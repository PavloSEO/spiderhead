"""Retained scan image batches preserve source occurrence and byte evidence."""

from seohead.mcp import handlers


def test_scan_image_batch_maps_download_and_optimization_back_to_occurrence(monkeypatch, tmp_path):
    monkeypatch.setattr(
        "seohead.mcp.evidence_handlers.scan_evidence",
        lambda *args, **kwargs: {
            "ok": True,
            "section": "resources",
            "evidence": {
                "state": "complete",
                "total": 1,
                "has_more": False,
                "occurrences": [
                    {
                        "occurrence_id": 17,
                        "page_url_id": 3,
                        "source_document_id": 8,
                        "representation": "static",
                        "ordinal": 0,
                        "kind": "image",
                        "state": "complete",
                        "resolved_url": "https://example.test/image.jpg",
                    }
                ],
            },
        },
    )
    monkeypatch.setattr(
        handlers.downloader,
        "download_images",
        lambda urls, output_dir, options: [
            {
                "url": urls[0],
                "path": "/synthetic/temp/image.jpg",
                "bytes": 900,
                "content_type": "image/jpeg",
                "ok": True,
            }
        ],
    )
    monkeypatch.setattr(
        handlers.optimizer,
        "optimize_files",
        lambda paths, settings: {
            "ok": True,
            "count": 1,
            "total_before": 900,
            "total_after": 500,
            "results": [
                {
                    "file": paths[0],
                    "out": str(tmp_path / "image.jpg"),
                    "before_bytes": 900,
                    "after_bytes": 500,
                    "ok": True,
                }
            ],
        },
    )

    result = handlers.images_optimize(scan="scan.sqlite", settings={"out_dir": str(tmp_path)})
    assert result["ok"] is True
    assert result["total_before"] == 900 and result["total_after"] == 500
    assert result["total_saved"] == 400
    assert result["results"][0]["instances"] == [
        {
            "occurrence_id": 17,
            "source_page_url_id": 3,
            "source_document_id": 8,
            "representation": "static",
            "ordinal": 0,
        }
    ]
