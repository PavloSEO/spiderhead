"""SSIM-guided image quality and EXIF stripping fixtures."""

from PIL import Image

from seohead.checks.optimizer import optimize_files


def test_auto_quality_reports_measured_ssim_and_threshold(tmp_path):
    source = tmp_path / "source.png"
    output = tmp_path / "optimized"
    image = Image.new("RGB", (64, 64))
    image.putdata([(x * 4, y * 4, (x + y) * 2) for y in range(64) for x in range(64)])
    image.save(source)

    result = optimize_files(
        [str(source)],
        {"out_dir": str(output), "format": "jpeg", "quality": "auto", "ssim_threshold": 0.9},
    )
    selected = result["results"][0]["quality_selection"]
    assert result["ok"] is True
    assert selected["mode"] == "auto-ssim"
    assert 10 <= selected["quality"] <= 100
    assert selected["threshold_met"] is True
    assert selected["ssim"] >= selected["threshold"]


def test_strip_exif_removes_metadata_from_optimized_jpeg(tmp_path):
    source = tmp_path / "source.jpg"
    output = tmp_path / "optimized"
    image = Image.new("RGB", (32, 32), "navy")
    exif = Image.Exif()
    exif[270] = "synthetic metadata"
    image.save(source, exif=exif)

    result = optimize_files(
        [str(source)], {"out_dir": str(output), "format": "keep", "strip_exif": True}
    )
    optimized = Image.open(result["results"][0]["out"])
    assert result["ok"] is True
    assert 270 not in optimized.getexif()
