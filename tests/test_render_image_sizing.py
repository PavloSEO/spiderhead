"""Intrinsic image size against the rendered box (#1049, first slice).

Offline: the pure comparison is tested directly, and the browser-side collector
is checked for the fields the comparison needs.
"""

from __future__ import annotations

from seohead.checks.render import (
    _IMAGE_BOXES_JS,
    IMAGE_OVERSIZED_MIN_BYTES,
    image_sizing_findings,
)

BIG = IMAGE_OVERSIZED_MIN_BYTES * 4


def _img(src, nat_w, box_w, box_h=100, nat_h=100, bytes_=BIG):
    return {
        "src": src,
        "natural_width": nat_w,
        "natural_height": nat_h,
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
