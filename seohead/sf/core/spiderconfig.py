"""Apply a Screaming Frog crawl-rate limit in headless mode.

The SF CLI has no speed option. It only accepts
``--config <file.seospiderconfig>``, and the config itself must be created in the
GUI (Config → Speed → File → Config → Save As). The only safe headless approach
is to start from an existing config and update the two fields in
``seo.spider.config.SpiderPerformanceConfig`` precisely.

In the Java serialization stream, this class declares exactly two fields in the
following order::

    Z  mLimitPerformance        (boolean, 1 byte)
    D  mUrlRequestsPerSecond    (double,  8 bytes, big-endian)

Their values immediately follow ``TC_ENDBLOCKDATA TC_NULL`` (``b"xp"``), which
terminates the class descriptor. The patch changes exactly nine bytes and
nothing else, preserving the stream structure so SF can read its own file.

Interoperability only: the module edits a config file the user created with their own licensed
copy of SF and never ships one. The class and field names above are the ones written in plain
text inside that serialized file itself.

An explicit limitation: this code does not modify ``mMaxThreads``. That integer
is one of roughly ten primitive fields in another class, and its offset cannot
be determined without parsing that descriptor fully. Guessing an offset could
silently corrupt the config. The requests-per-second limit is already the more
restrictive control over crawl traffic.
"""

from __future__ import annotations

import glob
import os
import struct

PERF_FIELD = b"mUrlRequestsPerSecond"
PERF_CLASS = b"seo.spider.config.SpiderPerformanceConfig"
FIELDS_END = b"xp"  # TC_ENDBLOCKDATA + TC_NULL

# Match the practical range exposed by the SF GUI: 0.1 to 1,000 URLs per second.
MIN_RATE = 0.1
MAX_RATE = 1000.0

# SF stores the latest crawl config at these locations. The serialized format is
# the same as a saved .seospiderconfig file.
CRAWL_CONFIG_GLOBS = (
    os.path.expanduser(
        "~/.ScreamingFrogSEOSpider/ProjectInstanceData/*/serialised_data/spiderconfig"
    ),
    os.path.expanduser("~/.ScreamingFrogSEOSpider/spiderconfig"),
)


def _perf_offsets(blob: bytes) -> tuple[int, int]:
    """Return the flag and double offsets in SpiderPerformanceConfig.

    Raise :class:`ValueError` when the stream layout is not the exact structure
    this implementation can patch. Never guess offsets in an unknown config.
    """
    field = blob.find(PERF_FIELD)
    if field < 0:
        raise ValueError(
            f"config does not contain {PERF_FIELD.decode()} — "
            "it is not a serialized Screaming Frog config or uses another format version"
        )
    end = blob.find(FIELDS_END, field)
    if end < 0:
        raise ValueError("SpiderPerformanceConfig descriptor is unterminated — config is corrupt")
    flag_at = end + len(FIELDS_END)
    double_at = flag_at + 1
    if double_at + 8 > len(blob):
        raise ValueError("config ends inside the speed block — file is corrupt")
    if blob[flag_at] not in (0, 1):
        raise ValueError(
            f"mLimitPerformance contains {blob[flag_at]:#02x}, expected 0 or 1 — "
            "the config layout does not match and cannot be patched safely"
        )
    return flag_at, double_at


def read_speed(blob: bytes) -> tuple[bool, float]:
    """Read ``(limit_enabled, URLs_per_second)`` from a config blob."""
    flag_at, double_at = _perf_offsets(blob)
    return bool(blob[flag_at]), struct.unpack(">d", blob[double_at : double_at + 8])[0]


def patch_speed(blob: bytes, urls_per_second: float) -> bytes:
    """Enable and set the crawl-rate limit without performing I/O."""
    rate = float(urls_per_second)
    if not MIN_RATE <= rate <= MAX_RATE:
        raise ValueError(f"rate {rate} is outside the allowed {MIN_RATE}..{MAX_RATE} URLs/s")
    flag_at, double_at = _perf_offsets(blob)
    out = bytearray(blob)
    out[flag_at] = 1
    out[double_at : double_at + 8] = struct.pack(">d", rate)
    patched = bytes(out)
    # Trust the patch only after a round-trip through the same strict parser.
    enabled, written = read_speed(patched)
    if not enabled or abs(written - rate) > 1e-9:
        raise ValueError("speed patch failed round-trip validation — file was not written")
    return patched


def find_base_config(explicit: str | None = None) -> str | None:
    """Find an explicit base config, or fall back to the latest crawl config."""
    if explicit and os.path.isfile(explicit):
        return explicit
    found: list[str] = []
    for pattern in CRAWL_CONFIG_GLOBS:
        found.extend(p for p in glob.glob(pattern) if os.path.isfile(p))
    if not found:
        return None
    return max(found, key=os.path.getmtime)


def build_throttled_config(dest: str, *, urls_per_second: float, base: str | None = None) -> str:
    """Build a rate-limited .seospiderconfig and return its absolute path."""
    source = find_base_config(base)
    if not source:
        raise RuntimeError(
            "cannot apply a crawl-rate limit because no base Screaming Frog config was found. "
            "Save any config from the GUI (Config → File → Config → Save As) and set its "
            "path as sf_cli.seospiderconfig in config.json, or run one crawl in the GUI so "
            "SF creates a reusable config automatically. Without a base file, the limit "
            "cannot be set; the toolkit will not crawl a third-party site at full speed "
            "after a rate limit was explicitly requested."
        )
    with open(source, "rb") as fh:
        blob = fh.read()
    patched = patch_speed(blob, urls_per_second)
    os.makedirs(os.path.dirname(os.path.abspath(dest)) or ".", exist_ok=True)
    with open(dest, "wb") as fh:
        fh.write(patched)
    return os.path.abspath(dest)


# ── module flags ─────────────────────────────────────────────────────────────
#
# Which registry checks a crawl can satisfy is decided by module switches inside
# the same serialized config. Reading them is the difference between warning the
# operator before a crawl and telling them after an hour that checks were skipped.
#
# The stream is standard Java serialization, so the layout is parsed, never
# guessed: a class descriptor carries its fields in order (primitives first,
# alphabetically, then object fields), the class data follows the descriptor,
# and a primitive's offset is the sum of the sizes of the primitives before it.
# Anything that does not match exactly is reported as unknown, never as "off".

TC_CLASSDESC = 0x72
TC_ENDBLOCKDATA = 0x78
TC_NULL = 0x70
TC_STRING = 0x74
TC_REFERENCE = 0x71

_PRIMITIVE_SIZES = {"B": 1, "Z": 1, "C": 2, "S": 2, "I": 4, "F": 4, "J": 8, "D": 8}

# Module label -> (class name, boolean field). Every entry was read out of a real
# config; a name that stops matching makes the flag unknown rather than wrong.
MODULE_FLAG_FIELDS: dict[str, tuple[str, str]] = {
    "structured_data_json_ld": ("seo.spider.config.SpiderStructuredDataConfig", "mExtractJsonLd"),
    "structured_data_microdata": (
        "seo.spider.config.SpiderStructuredDataConfig",
        "mExtractMicrodata",
    ),
    "structured_data_rdfa": ("seo.spider.config.SpiderStructuredDataConfig", "mExtractRdfa"),
    "structured_data_google_validation": (
        "seo.spider.config.SpiderStructuredDataConfig",
        "mGoogleValidation",
    ),
    "structured_data_schema_org_validation": (
        "seo.spider.config.SpiderStructuredDataConfig",
        "mSchemaDotOrgValidation",
    ),
    "spelling": ("seo.spider.config.LanguageToolConfig", "mSpellCheckEnabled"),
    "grammar": ("seo.spider.config.LanguageToolConfig", "mGrammarCheckEnabled"),
    "store_html": ("seo.spider.config.SpiderCrawlConfig", "mStoreOriginalHtml"),
    "store_rendered_html": ("seo.spider.config.SpiderCrawlConfig", "mStoreRenderedHtml"),
    "crawl_linked_xml_sitemaps": ("seo.spider.config.SpiderCrawlConfig", "mCrawlSitemaps"),
    "auto_discover_sitemaps": ("seo.spider.config.SpiderCrawlConfig", "mAutoDiscoverSitemaps"),
    "near_duplicates": ("seo.spider.config.DuplicateConfig", "mNearDuplicateChecking"),
    "auto_crawl_analysis": ("seo.spider.config.CrawlAnalysisConfig", "mAutoAnalyse"),
}

# JavaScript rendering is an enum (SpiderCrawlConfig.mCrawlerMode), not a
# boolean, so it is deliberately absent: reporting it would mean decoding an
# enum reference, and a wrong answer here is worse than no answer.


def _read_class_descriptor(blob: bytes, at: int) -> tuple[str, list[tuple[str, str]], int]:
    """Parse one ``TC_CLASSDESC`` and return ``(name, fields, end)``.

    ``fields`` are ``(typecode, name)`` in stream order. Raise
    :class:`ValueError` on anything the layout does not explain.
    """
    if at < 0 or at >= len(blob) or blob[at] != TC_CLASSDESC:
        raise ValueError("not a class descriptor at the given offset")
    i = at + 1
    try:
        (name_len,) = struct.unpack(">H", blob[i : i + 2])
        i += 2
        name = blob[i : i + name_len].decode("utf-8")
        i += name_len
        i += 8  # serialVersionUID
        i += 1  # class flags
        (count,) = struct.unpack(">H", blob[i : i + 2])
        i += 2
        fields: list[tuple[str, str]] = []
        for _ in range(count):
            typecode = chr(blob[i])
            i += 1
            (field_len,) = struct.unpack(">H", blob[i : i + 2])
            i += 2
            field_name = blob[i : i + field_len].decode("utf-8")
            i += field_len
            if typecode in "L[":
                # Object fields carry a type signature: a string or a back-reference.
                if blob[i] == TC_STRING:
                    i += 1
                    (sig_len,) = struct.unpack(">H", blob[i : i + 2])
                    i += 2 + sig_len
                elif blob[i] == TC_REFERENCE:
                    i += 5
                else:
                    raise ValueError(f"unexpected type signature marker {blob[i]:#02x}")
            elif typecode not in _PRIMITIVE_SIZES:
                raise ValueError(f"unknown field typecode {typecode!r}")
            fields.append((typecode, field_name))
    except (struct.error, IndexError, UnicodeDecodeError) as exc:
        raise ValueError(f"class descriptor is malformed: {exc}") from exc
    return name, fields, i


def read_boolean_field(blob: bytes, class_name: str, field_name: str) -> bool:
    """Read one boolean field of a config class out of the serialized stream.

    Raise :class:`ValueError` when the class or field is absent, when the class
    has a superclass with its own data (the offset would not be computable
    here), or when the byte read is not a boolean.
    """
    marker = class_name.encode("utf-8")
    at = blob.find(marker)
    if at < 3:
        raise ValueError(f"{class_name} is not present in this config")
    name, fields, end = _read_class_descriptor(blob, at - 3)
    if name != class_name:
        raise ValueError(f"expected {class_name} at the descriptor, found {name}")

    # Class data starts after the class annotation and the superclass descriptor.
    # Only a null superclass ("xp") keeps the offset computable from here.
    if blob[end : end + 2] != bytes((TC_ENDBLOCKDATA, TC_NULL)):
        raise ValueError(
            f"{class_name} has a superclass or annotation data — offset is not computable"
        )
    data_at = end + 2

    offset = 0
    for typecode, name_in_stream in fields:
        if typecode not in _PRIMITIVE_SIZES:
            # Object fields follow every primitive, so the target is not a primitive.
            break
        if name_in_stream == field_name:
            if typecode != "Z":
                raise ValueError(f"{class_name}.{field_name} is {typecode!r}, not a boolean")
            position = data_at + offset
            if position >= len(blob):
                raise ValueError("config ends before the requested field")
            value = blob[position]
            if value not in (0, 1):
                raise ValueError(
                    f"{class_name}.{field_name} holds {value:#02x}, expected 0 or 1 — "
                    "the layout does not match and the value cannot be trusted"
                )
            return bool(value)
        offset += _PRIMITIVE_SIZES[typecode]
    raise ValueError(f"{class_name} has no primitive field {field_name}")


def read_module_flags(blob: bytes) -> dict[str, bool | None]:
    """Return the module switches, with ``None`` for anything unreadable.

    A module that cannot be read is reported as unknown on purpose. Reporting it
    as off would tell the operator to fix a config that may already be correct.
    """
    flags: dict[str, bool | None] = {}
    for label, (class_name, field_name) in MODULE_FLAG_FIELDS.items():
        try:
            flags[label] = read_boolean_field(blob, class_name, field_name)
        except ValueError:
            flags[label] = None
    return flags
