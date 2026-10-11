"""Opt-in embedding checks over the same stored HTML used by offline heuristics."""

from __future__ import annotations

from collections import Counter
from contextlib import nullcontext
from tempfile import TemporaryDirectory
from typing import Any

from seohead.checks.semantic_similarity import (
    MAX_DOCUMENTS,
    DeclaredEmbeddingAdapter,
    EmbeddingCache,
    adapter_identity,
    analyze_semantic_documents,
    analyze_semantic_relevance,
)
from seohead.checks.text_normalize import normalize_document
from seohead.storage import ScanError
from seohead.storage.corpus_inputs import MAX_CORPUS_INPUT_BYTES

from .context import AuditContext

SEMANTIC_CHECKS = (
    "SEMANTIC_SIMILARITY_CANDIDATE",
    "TITLE_LOW_SEMANTIC_RELEVANCE",
    "H1_LOW_SEMANTIC_RELEVANCE",
)


def check_semantic_content(ctx: AuditContext) -> dict[str, Any]:
    """Measure only with an explicitly supplied adapter; never choose/load a model."""
    cfg = ctx.config.get("semantic_similarity", {})
    adapter = getattr(ctx, "semantic_adapter", None)
    if adapter is None and isinstance(cfg.get("adapter"), dict):
        adapter = DeclaredEmbeddingAdapter(cfg["adapter"])
    result: dict[str, Any] = {
        "coverage": {
            "state": "unavailable",
            "eligible_documents": 0,
            "analyzed_documents": 0,
            "omitted_documents": 0,
            "omission_reasons": {},
        },
    }

    def unavailable(reason):
        result["reason"] = reason
        result["coverage"]["state"] = "unavailable"
        for check in SEMANTIC_CHECKS:
            if ctx.enabled(check):
                ctx.skip(check, reason)
        ctx.semantic_evidence = result
        return result

    if not any(ctx.enabled(check) for check in SEMANTIC_CHECKS):
        return result
    if adapter is None:
        return unavailable("no semantic embedding adapter is selected")
    # Authorization precedes reading retained text and opening a cache.
    try:
        result["adapter"] = adapter_identity(adapter)
    except (ValueError, PermissionError):
        return unavailable(
            "semantic adapter declaration is invalid or external use is unauthorized"
        )
    except Exception as exc:
        return unavailable(f"semantic adapter declaration failed ({type(exc).__name__})")

    from .heuristics import _stored_html_reader

    read_html = _stored_html_reader(ctx)
    if read_html is None:
        return unavailable("normalized retained content is unavailable")
    limit = cfg.get("max_documents", 1000)
    if isinstance(limit, bool) or not isinstance(limit, int) or not 1 <= limit <= MAX_DOCUMENTS:
        return unavailable("semantic max_documents must be between 1 and 10000")
    supplied = {
        item["url"]: item
        for item in cfg.get("embeddings", [])
        if isinstance(item, dict) and isinstance(item.get("url"), str)
    }
    documents = []
    omissions: Counter[str] = Counter()
    eligible = 0
    input_bytes = 0
    byte_budget_exhausted = False
    for page in ctx.indexable_html_pages():
        eligible += 1
        if eligible > limit:
            omissions["semantic document limit exceeded"] += 1
            continue
        if byte_budget_exhausted:
            omissions["semantic corpus input-byte limit exceeded"] += 1
            continue
        try:
            html = read_html(page.url)
            size = len(html.encode("utf-8"))
            if input_bytes + size > MAX_CORPUS_INPUT_BYTES:
                byte_budget_exhausted = True
                omissions["semantic corpus input-byte limit exceeded"] += 1
                continue
            input_bytes += size
            document = normalize_document(html, ctx.config.get("content_area"))
        except (ScanError, OSError, ValueError) as exc:
            omissions[str(exc)] += 1
            continue
        if not document["text"]:
            omissions["normalized retained content is unavailable"] += 1
            continue
        document.update(
            url=page.url, title=page.metrics.get("title"), h1=" ".join(page.metrics.get("h1") or [])
        )
        vectors = supplied.get(page.url, {})
        for field in ("body", "title", "h1"):
            if vectors.get(field) is not None:
                key = "embedding" if field == "body" else f"{field}_embedding"
                document[key] = vectors[field]
        documents.append(document)
    result["coverage"].update(
        eligible_documents=eligible,
        omitted_documents=sum(omissions.values()),
        omission_reasons=dict(omissions),
        input_bytes=input_bytes,
        input_byte_limit=MAX_CORPUS_INPUT_BYTES,
    )
    if not documents:
        reason = "; ".join(f"{reason} ({count})" for reason, count in sorted(omissions.items()))
        return unavailable(reason or "normalized retained content is unavailable")

    cache = getattr(ctx, "semantic_cache", None)
    # A run-local cache keeps the default free of persistent side effects. A
    # caller can explicitly supply a durable cache for later offline reuse.
    temporary = (
        TemporaryDirectory(prefix="seohead-semantic-")
        if cache is None and not cfg.get("cache_path")
        else nullcontext(None)
    )
    try:
        with temporary as directory:
            cache = cache or EmbeddingCache(
                cfg.get("cache_path") or f"{directory}/embeddings.sqlite"
            )
            if ctx.enabled(SEMANTIC_CHECKS[0]):
                groups = analyze_semantic_documents(
                    documents,
                    adapter,
                    cache,
                    threshold=cfg.get("threshold", 0.82),
                    max_candidate_comparisons=cfg.get("max_candidate_comparisons", 250_000),
                )
            else:
                groups = {
                    "ok": True,
                    "state": "disabled",
                    "groups": [],
                    "candidate_comparisons": 0,
                    "coverage": {
                        "analyzed_documents": len(documents),
                        "omission_reasons": {},
                        "state": "complete",
                    },
                }
            group_coverage = groups["coverage"]
            omissions.update(group_coverage["omission_reasons"])
            result["coverage"].update(
                analyzed_documents=group_coverage["analyzed_documents"],
                omitted_documents=sum(omissions.values()),
                omission_reasons=dict(omissions),
                state="partial" if omissions else group_coverage["state"],
            )
            if not groups["ok"]:
                ctx.skip(SEMANTIC_CHECKS[0], groups["reason"])
            result["grouping"] = {
                name: groups[name]
                for name in ("ok", "state", "reason", "coverage")
                if name in groups
            }
            fields = tuple(
                field
                for field, check in zip(("title", "h1"), SEMANTIC_CHECKS[1:], strict=True)
                if ctx.enabled(check)
            )
            try:
                relevance = (
                    analyze_semantic_relevance(
                        documents,
                        adapter,
                        cache,
                        threshold=cfg.get("relevance_threshold", 0.35),
                        fields=fields,
                    )
                    if fields
                    else {"scores": [], "missing_headings": {}, "state": "disabled"}
                )
            except Exception as exc:
                relevance = {
                    "scores": [],
                    "missing_headings": {},
                    "state": "unavailable",
                    "reason": f"semantic heading adapter failed ({type(exc).__name__})",
                }
    except Exception as exc:
        # Provider exception messages may contain credentials or submitted text.
        return unavailable(f"semantic adapter or cache failed ({type(exc).__name__})")
    result.update(
        groups=groups["groups"],
        relevance=relevance,
        candidate_comparisons=groups.get("candidate_comparisons", 0),
    )
    for candidate in groups["groups"]:
        urls = [member["url"] for member in candidate["members"]]
        group = ctx.add_group(SEMANTIC_CHECKS[0], None, urls)
        ctx.add(
            SEMANTIC_CHECKS[0],
            target_url=urls[0],
            group_id=group.group_id if group else None,
            details={**candidate, "adapter": result["adapter"], "coverage": result["coverage"]},
        )
    for score in relevance["scores"]:
        if score["low_relevance"]:
            check = SEMANTIC_CHECKS[1] if score["field"] == "title" else SEMANTIC_CHECKS[2]
            ctx.add(
                check,
                target_url=score["url"],
                details={
                    **score,
                    "adapter": result["adapter"],
                    "coverage": result["coverage"],
                    "relevance_coverage": relevance.get("coverage", {}),
                    "conclusion": "review the heading against the retained content; similarity alone is not a relevance verdict",
                },
            )
    for field, missing in relevance["missing_headings"].items():
        check = SEMANTIC_CHECKS[1] if field == "title" else SEMANTIC_CHECKS[2]
        if missing == len(documents) and ctx.enabled(check):
            ctx.skip(check, f"no {field} text is available for semantic relevance")
        elif not any(score["field"] == field for score in relevance["scores"]) and ctx.enabled(
            check
        ):
            ctx.skip(check, f"no {field} embedding is available for semantic relevance")
        elif missing or relevance.get("unavailable_vectors", {}).get(field):
            unavailable_vectors = relevance.get("unavailable_vectors", {}).get(field, 0)
            measured = sum(score["field"] == field for score in relevance["scores"])
            if ctx.enabled(check):
                ctx.skip(
                    check,
                    f"semantic {field} comparisons prepared {measured} of {len(documents)}; "
                    f"heading text unavailable ({missing}); embedding unavailable ({unavailable_vectors})",
                )
    if relevance.get("state") == "unavailable":
        for check in SEMANTIC_CHECKS[1:]:
            if ctx.enabled(check):
                ctx.skip(check, relevance["reason"])
    if omissions:
        reason = "; ".join(f"{reason} ({count})" for reason, count in sorted(omissions.items()))
        reason = f"semantic bodies prepared {len(documents)} of {eligible}; {reason}"
        for check in SEMANTIC_CHECKS:
            if ctx.enabled(check):
                ctx.skip(check, reason)
    ctx.semantic_evidence = result
    return result
