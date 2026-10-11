"""Safe-by-default raster optimization and conservative SVG minification."""

from __future__ import annotations

import hashlib
import math
import os
import re
import shutil
import tempfile
import warnings
from typing import Any

RASTER_FORMATS: tuple[str, ...] = (
    ".jpg",
    ".jpeg",
    ".png",
    ".webp",
    ".avif",
    ".tiff",
    ".gif",
)
ALL_FORMATS: tuple[str, ...] = (*RASTER_FORMATS, ".svg")

_FORMAT_TO_PIL: dict[str, tuple[str, str]] = {
    "jpeg": ("JPEG", ".jpg"),
    "jpg": ("JPEG", ".jpg"),
    "png": ("PNG", ".png"),
    "webp": ("WEBP", ".webp"),
    "avif": ("AVIF", ".avif"),
    "tiff": ("TIFF", ".tiff"),
    "gif": ("GIF", ".gif"),
}

DEFAULT_QUALITY = 82
DEFAULT_MAX_PIXELS = 50_000_000
DEFAULT_MAX_SVG_BYTES = 10 * 1024 * 1024


def format_size(num_bytes: int) -> str:
    """Return a compact human-readable byte size."""
    if num_bytes < 1024:
        return f"{num_bytes} B"
    kibibytes = num_bytes / 1024
    if kibibytes < 1024:
        return f"{kibibytes:.1f} KB"
    return f"{kibibytes / 1024:.2f} MB"


def clamp_quality(quality: Any) -> int:
    """Clamp requested quality to the supported 10-100 range."""
    try:
        normalized = int(quality)
    except (TypeError, ValueError):
        return DEFAULT_QUALITY
    return min(100, max(10, normalized))


def _ext_of(path: str) -> str:
    return os.path.splitext(path)[1].lower()


def resolve_target_format(source_extension: str, requested: Any) -> str | None:
    """Resolve ``keep`` or an explicit target to a Pillow format key."""
    target = str(requested or "keep").lower()
    if target in {"", "keep"}:
        target = source_extension.lstrip(".")
    if target == "jpg":
        target = "jpeg"
    return target if target in _FORMAT_TO_PIL else None


def compute_resize(
    width: int,
    height: int,
    settings: dict[str, Any] | None,
) -> tuple[int, int]:
    """Fit dimensions inside optional bounds without changing aspect ratio."""
    settings = settings or {}
    if width <= 0 or height <= 0:
        return width, height

    scale = 1.0
    if settings.get("max_width"):
        scale = min(scale, float(settings["max_width"]) / width)
    if settings.get("max_height"):
        scale = min(scale, float(settings["max_height"]) / height)
    if scale >= 1.0:
        return width, height
    return max(1, round(width * scale)), max(1, round(height * scale))


_SVG_TEXT_ELEMENT_RE = re.compile(r"<text\b[^>]*>.*?</text\s*>", re.DOTALL | re.IGNORECASE)
# foreignObject embeds arbitrary XHTML (e.g. <pre>) where whitespace can be
# semantically significant, exactly like <text>/<tspan> -- protected the same way.
_SVG_FOREIGN_OBJECT_RE = re.compile(
    r"<foreignObject\b[^>]*>.*?</foreignObject\s*>", re.DOTALL | re.IGNORECASE
)


def minify_svg(text: str) -> str:
    """Apply a conservative, text-preserving SVG whitespace pass.

    Whitespace between and inside tags is only ever collapsing noise outside of
    <text> and <foreignObject> elements. Inside a <text>, a run of spaces can be
    the only thing separating two <tspan>s, and xml:space="preserve" can make
    repeated spaces significant. Inside a <foreignObject>, the embedded XHTML can
    contain elements like <pre> where whitespace is part of the content. So every
    <text>...</text> and <foreignObject>...</foreignObject> block is protected
    before the collapsing regexes run, and restored byte-for-byte afterward.
    """
    output = re.sub(r"<!--.*?-->", "", text, flags=re.DOTALL)

    protected: list[str] = []

    def _protect(match: re.Match[str]) -> str:
        protected.append(match.group(0))
        return f"\0{len(protected) - 1}\0"

    output = _SVG_FOREIGN_OBJECT_RE.sub(_protect, output)
    output = _SVG_TEXT_ELEMENT_RE.sub(_protect, output)
    output = re.sub(r">\s+<", "><", output)
    output = re.sub(r"\s{2,}", " ", output)
    output = output.strip()

    for index, block in enumerate(protected):
        output = output.replace(f"\0{index}\0", block)
    return output


def scan_paths(paths: list[str]) -> list[str]:
    """Expand files and directories into a deterministic, de-duplicated list."""
    return [path for path, _relative in _scan_targets(paths)]


def _scan_targets(
    paths: list[str],
    *,
    exclude_dir: str | None = None,
) -> list[tuple[str, str]]:
    targets: list[tuple[str, str]] = []
    seen_sources: set[str] = set()
    seen_relative: set[str] = set()
    excluded = os.path.abspath(exclude_dir) if exclude_dir else None

    def add(source: str, relative: str) -> None:
        absolute = os.path.abspath(source)
        if absolute in seen_sources:
            return
        seen_sources.add(absolute)

        normalized = relative.replace(os.sep, "/")
        if normalized in seen_relative:
            stem, extension = os.path.splitext(normalized)
            suffix = hashlib.sha256(absolute.encode("utf-8")).hexdigest()[:8]
            normalized = f"{stem}--{suffix}{extension}"
        seen_relative.add(normalized)
        targets.append((source, normalized))

    for value in paths or []:
        try:
            if os.path.isdir(value):
                root_path = os.path.abspath(value)
                for root, directories, files in os.walk(value):
                    if excluded:
                        directories[:] = [
                            directory
                            for directory in directories
                            if os.path.abspath(os.path.join(root, directory)) != excluded
                        ]
                    directories.sort()
                    for name in sorted(files):
                        source = os.path.join(root, name)
                        if _ext_of(source) in ALL_FORMATS:
                            add(source, os.path.relpath(os.path.abspath(source), root_path))
            elif os.path.isfile(value) and _ext_of(value) in ALL_FORMATS:
                add(value, os.path.basename(value))
        except OSError:
            continue
    return targets


def _pil_save_kwargs(pillow_format: str, quality: int) -> dict[str, Any]:
    if pillow_format == "JPEG":
        return {"quality": quality, "optimize": True, "progressive": True}
    if pillow_format == "PNG":
        return {"optimize": True, "compress_level": 9}
    if pillow_format == "WEBP":
        return {"quality": quality, "method": 6}
    if pillow_format == "AVIF":
        return {"quality": quality}
    if pillow_format == "TIFF":
        return {"compression": "tiff_lzw"}
    if pillow_format == "GIF":
        return {"optimize": True}
    return {}


def _backup_file(file_path: str) -> str:
    backup_dir = os.path.join(os.path.dirname(os.path.abspath(file_path)), "backup")
    os.makedirs(backup_dir, exist_ok=True)
    base = os.path.basename(file_path)
    candidate = os.path.join(backup_dir, base)
    index = 1
    while os.path.exists(candidate):
        candidate = os.path.join(backup_dir, f"{base}.{index}.bak")
        index += 1
    shutil.copy2(file_path, candidate)
    return candidate


def _output_path(
    source: str,
    relative: str,
    target_extension: str,
    output_dir: str | None,
) -> str:
    if output_dir:
        relative_stem = os.path.splitext(relative)[0]
        return os.path.join(output_dir, relative_stem + target_extension)
    source_stem = os.path.splitext(os.path.basename(source))[0]
    return os.path.join(os.path.dirname(source), source_stem + target_extension)


def _encode_raster(
    source: str,
    destination: str,
    target_format: str,
    settings: dict[str, Any],
) -> str:
    from PIL import Image, ImageOps

    pillow_format, _extension = _FORMAT_TO_PIL[target_format]
    quality = clamp_quality(settings.get("quality"))
    max_pixels = max(1, int(settings.get("max_pixels", DEFAULT_MAX_PIXELS)))

    with warnings.catch_warnings():
        warnings.simplefilter("error", Image.DecompressionBombWarning)
        with Image.open(source) as image:
            frame_count = int(getattr(image, "n_frames", 1))
            if frame_count != 1:
                raise ValueError(
                    "animated or multipage images are not rewritten; "
                    "extract a frame explicitly first"
                )
            if image.width * image.height > max_pixels:
                raise ValueError(f"image exceeds the {max_pixels:,}-pixel safety limit")

            image.load()
            icc_profile = image.info.get("icc_profile")
            image = ImageOps.exif_transpose(image)
            new_width, new_height = compute_resize(image.width, image.height, settings)
            if (new_width, new_height) != image.size:
                image = image.resize((new_width, new_height), Image.Resampling.LANCZOS)

            if pillow_format == "JPEG" and image.mode in {"RGBA", "LA", "P"}:
                rgba = image.convert("RGBA")
                background = Image.new("RGBA", rgba.size, (255, 255, 255, 255))
                background.alpha_composite(rgba)
                image = background.convert("RGB")

            save_kwargs = _pil_save_kwargs(pillow_format, quality)
            if settings.get("preserve_icc", True) and icc_profile:
                save_kwargs["icc_profile"] = icc_profile
            if not settings.get("strip_exif", False):
                exif = image.getexif()
                if exif:
                    save_kwargs["exif"] = exif.tobytes()
            image.save(destination, format=pillow_format, **save_kwargs)

    with Image.open(destination) as verification:
        verification.verify()
    return pillow_format.lower()


def _encode_svg(source: str, destination: str, settings: dict[str, Any]) -> str:
    from lxml import etree

    max_bytes = max(1, int(settings.get("max_svg_bytes", DEFAULT_MAX_SVG_BYTES)))
    if os.path.getsize(source) > max_bytes:
        raise ValueError(f"SVG exceeds the {max_bytes:,}-byte safety limit")
    with open(source, encoding="utf-8") as stream:
        text = stream.read(max_bytes + 1)
    if len(text.encode("utf-8")) > max_bytes:
        raise ValueError(f"SVG exceeds the {max_bytes:,}-byte safety limit")
    upper = text.upper()
    if "<!DOCTYPE" in upper or "<!ENTITY" in upper:
        raise ValueError("SVG with DTD or entity declarations is not accepted")

    output = minify_svg(text)
    parser = etree.XMLParser(resolve_entities=False, no_network=True, recover=False)
    root = etree.fromstring(output.encode("utf-8"), parser=parser)
    if etree.QName(root).localname.lower() != "svg":
        raise ValueError("file is not an SVG document")
    with open(destination, "w", encoding="utf-8") as stream:
        stream.write(output)
    return "svg"


def _ssim_score(source: str, candidate: str) -> float:
    """Compute bounded-memory RGB SSIM over aligned 1024px thumbnails."""
    import numpy as np
    from PIL import Image, ImageOps

    with Image.open(source) as original, Image.open(candidate) as optimized:
        source_image = ImageOps.exif_transpose(original).convert("RGB")
        candidate_image = ImageOps.exif_transpose(optimized).convert("RGB")
        if source_image.size != candidate_image.size:
            candidate_image = candidate_image.resize(source_image.size, Image.Resampling.LANCZOS)
        source_image.thumbnail((1024, 1024), Image.Resampling.LANCZOS)
        candidate_image.thumbnail((1024, 1024), Image.Resampling.LANCZOS)
        left = np.asarray(source_image, dtype=np.float32)
        right = np.asarray(candidate_image, dtype=np.float32)

    window = min(11, min(source_image.size))
    if window > 1 and window % 2 == 0:
        window -= 1

    def moments(values):
        integral = np.pad(values, ((1, 0), (1, 0), (0, 0))).cumsum(
            0, dtype=np.float64
        ).cumsum(1, dtype=np.float64)
        return (
            integral[window:, window:]
            - integral[:-window, window:]
            - integral[window:, :-window]
            + integral[:-window, :-window]
        ) / (window * window)

    mean_left, mean_right = moments(left), moments(right)
    var_left = np.maximum(moments(left * left) - mean_left * mean_left, 0)
    var_right = np.maximum(moments(right * right) - mean_right * mean_right, 0)
    covariance = moments(left * right) - mean_left * mean_right
    c1, c2 = (0.01 * 255) ** 2, (0.03 * 255) ** 2
    score = ((2 * mean_left * mean_right + c1) * (2 * covariance + c2)) / (
        (mean_left * mean_left + mean_right * mean_right + c1)
        * (var_left + var_right + c2)
    )
    return float(score.mean())


def _encode_raster_auto(
    source: str, destination: str, target_format: str, settings: dict[str, Any]
) -> tuple[str, int, float, bool]:
    """Select a bounded-search quality and report whether thumbnail SSIM meets target."""
    pillow_format = _FORMAT_TO_PIL[target_format][0]
    if pillow_format not in {"JPEG", "WEBP", "AVIF"}:
        raise ValueError("quality=auto requires a lossy JPEG, WebP, or AVIF output format")
    threshold = settings.get("ssim_threshold", 0.98)
    if (
        isinstance(threshold, bool)
        or not isinstance(threshold, int | float)
        or not math.isfinite(threshold)
        or not 0 < threshold <= 1
    ):
        raise ValueError("ssim_threshold must be a finite number in (0, 1]")

    low, high, best = 10, 100, None
    while low <= high:
        quality = (low + high) // 2
        _encode_raster(source, destination, target_format, {**settings, "quality": quality})
        score = _ssim_score(source, destination)
        if score >= threshold:
            best = (quality, score)
            high = quality - 1
        else:
            low = quality + 1
    quality = best[0] if best else 100
    _encode_raster(source, destination, target_format, {**settings, "quality": quality})
    score = _ssim_score(source, destination)
    return pillow_format.lower(), quality, score, score >= threshold


def _result(source: str) -> dict[str, Any]:
    return {
        "file": source,
        "out": None,
        "format": _ext_of(source).lstrip("."),
        "before_bytes": 0,
        "after_bytes": 0,
        "saved_bytes": 0,
        "saved_pct": 0.0,
        "source_retained": True,
        "ok": False,
    }


def _optimize_one(
    source: str,
    relative: str,
    settings: dict[str, Any],
) -> dict[str, Any]:
    result = _result(source)
    extension = _ext_of(source)
    if extension not in ALL_FORMATS:
        result["error"] = "unsupported image format"
        return result

    try:
        before = os.path.getsize(source)
    except OSError as exc:
        result["error"] = f"file is unavailable: {exc}"
        return result
    result["before_bytes"] = before

    requested_format = str(settings.get("format") or "keep").lower()
    if extension == ".svg":
        if requested_format not in {"", "keep", "svg"}:
            result["error"] = "SVG conversion to a raster format is not supported"
            return result
        target_extension = ".svg"
        target_format = "svg"
    else:
        target_format = resolve_target_format(extension, requested_format)
        if target_format is None:
            result["error"] = "unsupported target format"
            return result
        _pillow_format, target_extension = _FORMAT_TO_PIL[target_format]
        if requested_format in {"", "keep"}:
            target_extension = extension

    output_dir = settings.get("out_dir")
    in_place = bool(settings.get("in_place"))
    overwrite = bool(settings.get("overwrite"))
    destination = _output_path(source, relative, target_extension, output_dir)
    same_file = os.path.abspath(destination) == os.path.abspath(source)

    if same_file and not in_place:
        result["error"] = "destination equals source; pass in_place=true explicitly"
        return result
    if os.path.exists(destination) and not same_file and not overwrite:
        result["error"] = "destination already exists; pass overwrite=true explicitly"
        return result

    destination_dir = os.path.dirname(os.path.abspath(destination))
    os.makedirs(destination_dir, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(
        prefix=".seohead-",
        suffix=target_extension,
        dir=destination_dir,
    )
    os.close(descriptor)

    backup_path: str | None = None
    quality_report = None
    try:
        if target_format == "svg":
            encoded_format = _encode_svg(source, temporary, settings)
        elif str(settings.get("quality", "")).lower() == "auto":
            encoded_format, chosen_quality, ssim, threshold_met = _encode_raster_auto(
                source, temporary, target_format, settings
            )
            quality_report = {
                "mode": "auto-ssim",
                "quality": chosen_quality,
                "ssim": ssim,
                "threshold": settings.get("ssim_threshold", 0.98),
                "threshold_met": threshold_met,
            }
        else:
            encoded_format = _encode_raster(source, temporary, target_format, settings)
        if same_file and settings.get("backup", True):
            backup_path = _backup_file(source)
        os.replace(temporary, destination)

        after = os.path.getsize(destination)
        saved = before - after
        result.update(
            {
                "out": destination,
                "format": encoded_format,
                "after_bytes": after,
                "saved_bytes": saved,
                "saved_pct": round((saved / before * 100.0) if before else 0.0, 1),
                "source_retained": not same_file,
                "backup": backup_path,
                "ok": True,
            }
        )
        if quality_report is not None:
            result["quality_selection"] = quality_report
    except Exception as exc:  # Per-file errors do not abort the batch.
        result["error"] = str(exc)
    finally:
        try:
            if os.path.exists(temporary):
                os.remove(temporary)
        except OSError:
            pass
    return result


def optimize_files(
    files: list[str],
    settings: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Optimize images without mutating sources by default.

    Supply ``out_dir`` for the recommended workflow. Source mutation is allowed
    only with ``in_place=true`` and creates a backup by default. Format conversion
    never deletes the source file.
    """
    normalized = dict(settings or {})
    output_dir = normalized.get("out_dir")
    if not output_dir and not normalized.get("in_place"):
        return {
            "ok": False,
            "error": "out_dir is required unless in_place=true is explicitly set",
            "results": [],
            "total_before": 0,
            "total_after": 0,
            "total_saved": 0,
            "count": 0,
        }

    if output_dir:
        try:
            os.makedirs(str(output_dir), exist_ok=True)
        except OSError as exc:
            return {
                "ok": False,
                "error": f"cannot create out_dir: {exc}",
                "results": [],
                "total_before": 0,
                "total_after": 0,
                "total_saved": 0,
                "count": 0,
            }

    targets = _scan_targets(
        list(files or []),
        exclude_dir=str(output_dir) if output_dir else None,
    )
    if not targets:
        return {
            "ok": False,
            "error": "no supported images were found",
            "results": [],
            "total_before": 0,
            "total_after": 0,
            "total_saved": 0,
            "count": 0,
        }

    results = [_optimize_one(source, relative, normalized) for source, relative in targets]
    successful = [record for record in results if record.get("ok")]
    total_before = sum(int(record["before_bytes"]) for record in successful)
    total_after = sum(int(record["after_bytes"]) for record in successful)
    return {
        "ok": all(record.get("ok") for record in results),
        "results": results,
        "total_before": total_before,
        "total_after": total_after,
        "total_saved": total_before - total_after,
        "count": len(results),
    }
