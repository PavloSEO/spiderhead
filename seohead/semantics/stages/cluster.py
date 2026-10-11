"""cluster: SERP clustering of the whole core (paid Yandex Search API, cached per phrase).

Cluster the whole core at once: phrases are linked by shared top-N result URLs, so cutting the
core into chunks with --limit splits clusters; --limit is for debugging only. One SERP is
fetched per phrase and cached in ``serp``, so a repeated run pays nothing. Phrases are joined
by union-find on >= ``serp_overlap`` shared pages (not domains: one large site ranks for many
subtopics with different pages). ``--method louvain`` re-clusters the cached SERP with Louvain
communities instead and needs the optional ``semantics`` extra.
"""

from __future__ import annotations

import collections
import itertools
import json
from datetime import datetime, timezone
from urllib.parse import urlsplit

from seohead.data_sources.yandex_cloud import WebSearch
from seohead.semantics.stages.graph import UF

# Hub domains (portals, wikis, social networks, marketplaces, government and education, stock
# media) appear in almost any SERP and carry no topic signal; single linkage would chain
# everything into one blob through them.
HUB_EXACT = {
    "yandex.ru",
    "ya.ru",
    "google.com",
    "mail.ru",
    "dzen.ru",
    "habr.com",
    "vc.ru",
    "pikabu.ru",
    "vk.com",
    "ok.ru",
    "t.me",
    "youtube.com",
    "rutube.ru",
    "music.yandex.ru",
    "music.apple.com",
    "ozon.ru",
    "aliexpress.ru",
    "wildberries.ru",
    "avito.ru",
    "market.yandex.ru",
    "pult.ru",
    "ru.freepik.com",
    "freepik.com",
    "shutterstock.com",
    "istockphoto.com",
    "gettyimages.com",
    "ru.wikipedia.org",
    "en.wikipedia.org",
    "ru.ruwiki.ru",
    "cyberleninka.ru",
    "moluch.ru",
    "rucont.ru",
    "otvet.mail.ru",
    "hh.ru",
    "superjob.ru",
    "2gis.ru",
    "zoon.ru",
    "tadviser.ru",
    "consultant.ru",
    "konsultant.ru",
    "regulation.gov.ru",
}
HUB_SUFFIX = (
    ".gov.ru",
    ".gov.by",
    ".gov.kz",
    ".gosuslugi.ru",
    ".mos.ru",
    ".edu.ru",
    ".edu.by",
    "wikipedia.org",
    "ruwiki.ru",
)


def is_hub(dom):
    host = (dom or "").lower().removeprefix("www.").rstrip(".")
    return bool(host) and (
        host in HUB_EXACT
        or any(
            host == suffix.lstrip(".") or host.endswith("." + suffix.lstrip("."))
            for suffix in HUB_SUFFIX
        )
    )


def is_hub_url(u):
    parsed = urlsplit(u if "://" in (u or "") else "https://" + (u or ""))
    return is_hub(parsed.hostname)


def url_key(u):
    """Normalize host and decorations, retaining the case-sensitive page path."""
    value = (u or "").strip()
    parsed = urlsplit(value if "://" in value else "https://" + value)
    host = (parsed.hostname or "").lower().removeprefix("www.")
    port = f":{parsed.port}" if parsed.port and parsed.port not in (80, 443) else ""
    return (host + port + parsed.path).rstrip("/")


def _louvain_groups(keys, shared, resolution):
    """Louvain communities over shared-page weights; breaks long single-linkage chains."""
    try:
        import community as community_louvain
        import networkx as nx
    except ImportError as exc:  # pragma: no cover - depends on the optional extra
        raise RuntimeError(
            "cluster --method louvain needs the optional extra: "
            "pip install 'seohead-seotools[semantics]'"
        ) from exc
    graph = nx.Graph()
    graph.add_nodes_from(keys)
    for (a, b), count in shared.items():
        if count >= 2:  # an edge needs two shared pages; one shared page chains weak links
            graph.add_edge(a, b, weight=count)
    partition = community_louvain.best_partition(
        graph, weight="weight", resolution=resolution, random_state=42
    )
    groups = collections.defaultdict(list)
    for node, community in partition.items():
        groups[community].append(node)
    return dict(groups)


def run(store, cfg, limit=0, top=10, method="serp"):
    if method not in ("serp", "louvain"):
        raise ValueError("cluster method must be serp or louvain")
    overlap = cfg.get("serp_overlap", 3)
    region = (cfg.get("regions") or ["225"])[0]
    # Every kept phrase: clean already applied the base cut; exact frequency is not required.
    rows = sorted(store.phrases(status="kept"), key=lambda r: -(r["exact"] or r["base"] or 0))
    if limit:
        rows = rows[:limit]
        print(
            f"warning: cluster --limit {limit} splits clusters; use it for debugging only "
            "and run the whole core without --limit."
        )
    if not rows:
        print("cluster: no kept phrases")
        return {"clusters": 0}

    store.db.execute("""CREATE TABLE IF NOT EXISTS serp_fetches(
        phrase_norm TEXT, region TEXT, top INTEGER, state TEXT, operation_id TEXT,
        raw_json TEXT, error TEXT, fetched_at TEXT, PRIMARY KEY(phrase_norm, region, top))""")
    receipt_columns = {row["name"] for row in store.db.execute("PRAGMA table_info(serp_fetches)")}
    if "fetched_at" not in receipt_columns:
        store.db.execute("ALTER TABLE serp_fetches ADD COLUMN fetched_at TEXT")
    n_req = 0
    doms = {}  # norm -> set of top-N page keys
    todo = []
    receipts = {
        r["phrase_norm"]: r
        for r in store.db.execute(
            "SELECT phrase_norm,state,fetched_at FROM serp_fetches WHERE region=? AND top=?",
            (str(region), top),
        )
    }
    has_receipt = {r[0] for r in store.db.execute("SELECT DISTINCT phrase_norm FROM serp_fetches")}
    for r in rows:
        receipt = receipts.get(r["norm"])
        cached = store.serp_for(r["norm"])
        if receipt and receipt["state"] != "complete":
            raise ValueError(
                f"unresolved paid SERP operation for {r['norm']!r}; "
                "recover the provider receipt before retrying"
            )
        if receipt and receipt["state"] == "complete":
            raw = store.db.execute(
                "SELECT raw_json FROM serp_fetches WHERE phrase_norm=? AND region=? AND top=?",
                (r["norm"], str(region), top),
            ).fetchone()[0]
            docs = json.loads(raw)["docs"][:top]
            if not cached:
                store.add_serp(r["norm"], docs, fetched_at=receipt["fetched_at"])
            doms[r["norm"]] = {url_key(d["url"]) for d in docs if d.get("url")}
        elif cached and r["norm"] not in has_receipt:
            # Legacy SERP has no region receipt; retain it until an explicit new fetch is recorded.
            doms[r["norm"]] = {url_key(d["url"]) for d in cached[:top] if d["url"]}
        else:
            todo.append(r["norm"])
    # Reserve before submission: an interrupted paid batch cannot be silently resubmitted.
    if todo:
        ys = WebSearch(rps=cfg.get("rps", 5))
    for i in range(0, len(todo), 40):
        batch = todo[i : i + 40]
        store.db.executemany(
            "INSERT OR REPLACE INTO serp_fetches(phrase_norm,region,top,state) VALUES(?,?,?,'pending')",
            [(p, str(region), top) for p in batch],
        )
        store.commit()
        res = ys.search_batch(batch, region=region, groups=top)
        unresolved = []
        submitted = 0
        for p in batch:
            result = res.get(p)
            state = "pending"
            if isinstance(result, dict):
                if result.get("status") == "rejected":
                    store.db.execute(
                        "DELETE FROM serp_fetches WHERE phrase_norm=? AND region=? AND top=?",
                        (p, str(region), top),
                    )
                    unresolved.append(p)
                    continue
                if result.get("operation_id"):
                    submitted += 1
                if not result.get("error") and isinstance(result.get("docs"), list):
                    state = "complete"
                    docs = result["docs"][:top]
                    store.add_serp(p, docs)
                    doms[p] = {url_key(d["url"]) for d in docs if d.get("url")}
            store.db.execute(
                "UPDATE serp_fetches SET state=?,operation_id=?,raw_json=?,error=?,fetched_at=? "
                "WHERE phrase_norm=? AND region=? AND top=?",
                (
                    state,
                    (result or {}).get("operation_id"),
                    json.dumps(result, ensure_ascii=False),
                    str((result or {}).get("error") or "") or None,
                    datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
                    if state == "complete"
                    else None,
                    p,
                    str(region),
                    top,
                ),
            )
            if state != "complete":
                unresolved.append(p)
        n_req += submitted
        store.add_run(
            "cluster",
            submitted,
            note=f"SERP batch: complete={len(batch) - len(unresolved)}, unresolved={len(unresolved)}",
        )
        if unresolved:
            raise ValueError(
                "SERP batch incomplete; retained receipts must be recovered before retrying"
            )
        print(f"  SERP batch {i // 40 + 1}: +{len(batch)} (total {len(doms)})", flush=True)

    # Inverted index URL -> phrases, shared-URL counts per pair, union at >= overlap. Hub URLs
    # and ubiquitous pages (> 4% of results) are noise. Cost is O(sum of pairs), not O(n^2).
    keys = list(doms)
    dom_index = collections.defaultdict(list)
    for p in keys:
        for d in doms[p]:
            dom_index[d].append(p)
    ubiq = max(4, int(0.04 * len(keys)))
    shared = collections.Counter()
    for d, plist in dom_index.items():
        if is_hub_url(d) or len(plist) > ubiq:
            continue
        for a, b in itertools.combinations(sorted(plist), 2):
            shared[(a, b)] += 1
    if method == "louvain":
        groups = _louvain_groups(keys, shared, float(cfg.get("louvain_resolution", 1.2)))
    else:
        uf = UF()
        for p in keys:
            uf.find(p)  # every phrase is a node, singletons included
        for (a, b), c in shared.items():
            if c >= overlap:
                uf.union(a, b)
        groups = {}
        for p in keys:
            groups.setdefault(uf.find(p), []).append(p)

    weights = {r["norm"]: r["exact"] if r["exact"] is not None else r["base"] or 0 for r in rows}
    protected = {
        r[0]
        for r in store.db.execute(
            "SELECT p.norm FROM phrases p JOIN clusters c ON c.id=p.cluster_id "
            "WHERE c.source NOT IN ('serp','louvain') OR c.landing_url IS NOT NULL "
            "OR (c.source IS NULL AND c.marker_norm IS NULL)"
        )
    }
    generated = {
        r["name"]: r["id"]
        for r in store.db.execute("SELECT name,id FROM clusters WHERE source=?", (method,))
    }
    made = 0
    for members in sorted(groups.values(), key=len, reverse=True):
        marker = max(members, key=weights.__getitem__)
        # coherence: mean pairwise Jaccard share of URLs
        if len(members) > 1:
            pairs = (
                len(doms[a] & doms[b]) / max(len(doms[a] | doms[b]), 1)
                for a, b in itertools.combinations(members, 2)
            )
            coh = round(sum(pairs) / (len(members) * (len(members) - 1) // 2), 3)
        else:
            coh = 1.0
        cid = generated.get(marker)
        if cid is None:
            cid = store.db.execute(
                "INSERT INTO clusters(name,source) VALUES(?,?)", (marker, method)
            ).lastrowid
            generated[marker] = cid
        store.db.execute(
            "UPDATE clusters SET marker_norm=?,coherence=?,is_real=? WHERE id=?",
            (marker, coh, 1 if coh >= 0.1 else 0, cid),
        )
        for p in members:
            if p in protected:
                continue
            store.set_fields(p, cluster_id=cid)
        made += 1
    store.commit()
    store.add_run("cluster", note=f"method={method}, clusters={made}, SERP requests={n_req}")
    print(f"cluster: method={method}, clusters={made} ({len(keys)} phrases, SERP requests={n_req})")
    return {"clusters": made, "requests": n_req, "method": method}
