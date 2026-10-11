"""Intrinsic image size against the rendered box (#1049, first slice).

Offline: the pure comparison is tested directly, and the browser-side collector
is checked for the fields the comparison needs.
"""

from __future__ import annotations

import struct
import zlib
from io import BytesIO

import pytest
from PIL import Image

from seohead.checks.render import (
    _IMAGE_BOXES_JS,
    IMAGE_OVERSIZED_MIN_BYTES,
    image_sizing_findings,
)
from seohead.storage.resource_graph import _image_dimensions

BIG = IMAGE_OVERSIZED_MIN_BYTES * 4


def _img(src, nat_w, box_w, box_h=100, nat_h=100, bytes_=BIG):
    return {
        "src": src,
        "natural_width": nat_w,
        "natural_height": nat_h,
        "intrinsic_width": nat_w,
        "intrinsic_height": nat_h,
        "rendered_width": box_w,
        "rendered_height": box_h,
        "encoded_bytes": bytes_,
    }


def test_a_much_larger_heavy_file_is_oversized():
    result = image_sizing_findings([_img("https://ex.test/a.jpg", 3000, 300)], dpr=1.0)
    assert [e["src"] for e in result["oversized"]] == ["https://ex.test/a.jpg"]
    assert result["upscaled"] == []


def test_a_large_file_that_is_only_slightly_bigger_than_its_box_is_not_oversized():
    # 1.5x the box is inside the 2x tolerance: no finding.
    result = image_sizing_findings([_img("https://ex.test/a.jpg", 450, 300)], dpr=1.0)
    assert result == {"oversized": [], "upscaled": []}


def test_an_oversized_but_light_file_is_not_reported():
    # Pixels are wasteful, but the wire cost is below the threshold: no finding.
    light = _img("https://ex.test/icon.png", 3000, 300, bytes_=1024)
    assert image_sizing_findings([light], dpr=1.0)["oversized"] == []


def test_device_pixel_ratio_raises_the_pixels_a_box_needs():
    # A 600px-wide box on a 2x screen needs 1200 device pixels: 1500 is neither upscaled
    # nor oversized (1500 is under 2x of 1200).
    img = _img("https://ex.test/a.jpg", 1500, 600)
    result = image_sizing_findings([img], dpr=2.0)
    assert result["upscaled"] == []
    assert result["oversized"] == []


def test_density_corrected_browser_width_is_not_treated_as_file_pixels():
    img = _img("https://ex.test/a-2x.png", 600, 300)
    img["natural_width"] = 300
    assert image_sizing_findings([img], dpr=2.0) == {"upscaled": [], "oversized": []}


def test_browser_natural_size_without_proven_file_dimensions_is_unmeasured():
    img = _img("https://ex.test/a.svg", 300, 300)
    img.pop("intrinsic_width")
    img.pop("intrinsic_height")
    assert image_sizing_findings([img], dpr=2.0) == {"upscaled": [], "oversized": []}


def test_unproved_replaced_content_geometry_is_not_a_size_finding():
    img = _img("https://ex.test/a.png", 300, 600)
    img["geometry_state"] = "unavailable"
    assert image_sizing_findings([img], dpr=1.0) == {"upscaled": [], "oversized": []}


@pytest.mark.parametrize("orientation", range(1, 9))
@pytest.mark.parametrize("format", ["JPEG", "PNG"])
def test_file_dimensions_follow_exif_orientation_without_decoding_pixels(
    monkeypatch, orientation, format
):
    buffer = BytesIO()
    exif = Image.Exif()
    exif[274] = orientation
    Image.new("RGB", (300, 600), "red").save(buffer, format=format, exif=exif)
    monkeypatch.setattr(Image.Image, "load", lambda *args: pytest.fail("pixel decoding forbidden"))
    assert _image_dimensions(buffer.getvalue()) == (
        (600, 300) if orientation in {5, 6, 7, 8} else (300, 600)
    )


def test_png_exif_after_idat_is_read_without_decoding_pixels(monkeypatch):
    buffer = BytesIO()
    Image.new("RGB", (3, 6), "red").save(buffer, format="PNG")
    exif = Image.Exif()
    exif[274] = 6
    data = b"eXIf" + exif.tobytes()[6:]
    chunk = struct.pack(">I", len(data) - 4) + data + struct.pack(">I", zlib.crc32(data))
    png = buffer.getvalue()
    late = png[:-12] + chunk + png[-12:]
    monkeypatch.setattr(Image.Image, "load", lambda *args: pytest.fail("pixel decoding forbidden"))
    assert _image_dimensions(png) == (3, 6)
    assert _image_dimensions(late) == (6, 3)
    assert _image_dimensions(late[:-5]) == (None, None)


def test_invalid_exif_orientation_does_not_claim_unrotated_dimensions():
    buffer = BytesIO()
    exif = Image.Exif()
    exif[274] = 9
    Image.new("RGB", (300, 600), "red").save(buffer, format="JPEG", exif=exif)
    assert _image_dimensions(buffer.getvalue()) == (None, None)


def test_corrupt_exif_warning_makes_dimensions_unavailable(monkeypatch):
    import warnings

    buffer = BytesIO()
    Image.new("RGB", (300, 600), "red").save(buffer, format="JPEG")

    def corrupt(*args):
        warnings.warn("Corrupt EXIF data", UserWarning, stacklevel=1)
        return {}

    monkeypatch.setattr(Image.Image, "getexif", corrupt)
    assert _image_dimensions(buffer.getvalue()) == (None, None)


def test_proven_response_status_is_preserved_without_an_invented_default():
    img = _img("https://ex.test/a.png", 300, 600)
    img["status_code"] = 200
    assert image_sizing_findings([img], dpr=2.0)["upscaled"][0]["status_code"] == 200
    img.pop("status_code")
    assert "status_code" not in image_sizing_findings([img], dpr=2.0)["upscaled"][0]


def test_a_file_narrower_than_its_box_in_device_pixels_is_upscaled():
    result = image_sizing_findings([_img("https://ex.test/a.jpg", 300, 600)], dpr=2.0)
    assert [e["src"] for e in result["upscaled"]] == ["https://ex.test/a.jpg"]
    assert result["oversized"] == []


def test_not_loaded_or_not_rendered_images_are_skipped_not_measured_as_zero():
    images = [
        _img("https://ex.test/a.jpg", 0, 300),  # not loaded yet
        _img("https://ex.test/b.jpg", 3000, 0),  # display:none
        _img("https://ex.test/c.jpg", 3000, 300, box_h=0),  # zero height box
    ]
    assert image_sizing_findings(images, dpr=1.0) == {"oversized": [], "upscaled": []}


def test_one_entry_per_source_url():
    images = [
        _img("https://ex.test/a.jpg", 3000, 300),
        _img("https://ex.test/a.jpg", 3000, 300),
    ]
    assert len(image_sizing_findings(images, dpr=1.0)["oversized"]) == 1


def test_repeated_source_in_different_boxes_keeps_both_layout_observations():
    images = [_img("https://ex.test/a.jpg", 600, 600), _img("https://ex.test/a.jpg", 600, 100)]
    assert len(image_sizing_findings(images, dpr=1.0)["oversized"]) == 1


def test_entries_carry_the_numbers_a_reader_needs():
    entry = image_sizing_findings([_img("https://ex.test/a.jpg", 300, 600)], dpr=2.0)["upscaled"][0]
    assert entry == {
        "src": "https://ex.test/a.jpg",
        "natural": [300, 100],
        "rendered": [600, 100],
        "dpr": 2.0,
    }


def test_empty_input_is_an_empty_result():
    assert image_sizing_findings([], dpr=1.0) == {"oversized": [], "upscaled": []}


def test_collector_reads_the_fields_the_comparison_needs():
    for token in (
        "naturalWidth",
        "naturalHeight",
        "getBoundingClientRect",
        "devicePixelRatio",
        "encodedBodySize",
    ):
        assert token in _IMAGE_BOXES_JS
