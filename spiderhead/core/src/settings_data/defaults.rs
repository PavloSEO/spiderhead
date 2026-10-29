// Generated from the crawl-settings defaults. Do not hand-edit.

/// Default values serialized as JSON; keys are sorted by the map type.
pub const DEFAULTS_JSON: &str = r#"{
  "scope": {
    "internal": "host",
    "start_folder_only": false,
    "include_patterns": [],
    "exclude_patterns": [],
    "include_extensions": [],
    "exclude_extensions": [],
    "include_media_types": [],
    "exclude_media_types": [],
    "exclude_hosts": [],
    "segments": [],
    "segments_only": [],
    "normalize_tracking_parameters": true
  },
  "analysis": {
    "segments": [],
    "canonical_policy": {
      "pagination": [],
      "filters": []
    },
    "asset_thresholds": {
      "page_weight_max_bytes": 2097152,
      "resource_max_bytes": 2097152,
      "network_payload_max_bytes": 4194304
    },
    "finding_exclusions": [],
    "redirect_hop_hsts_probe": false
  },
  "sitemaps": {
    "auto_discover": false
  },
  "discovery": {
    "hyperlinks": {
      "store": true,
      "crawl": true
    },
    "redirects": {
      "crawl": true
    },
    "external": {
      "store": true,
      "crawl": false
    },
    "follow_nofollow": false,
    "record_invalid_targets": false,
    "resolve_redirect_destination": false,
    "resolve_canonical_destination": false
  },
  "limits": {
    "max_urls": 200,
    "max_depth": 5,
    "max_query_variants_per_path": 5,
    "max_response_bytes": 5242880,
    "max_url_length": 2000,
    "max_crawl_seconds": 0,
    "max_requests": 20000
  },
  "external_checks": {
    "max_targets": 100,
    "max_hosts": 20,
    "max_requests": 0,
    "max_depth": 0,
    "max_redirects": 5
  },
  "fetch": {
    "retry_server_errors": 0
  },
  "http": {
    "timeout_seconds": 15.0,
    "user_agent": "",
    "headers": {},
    "retry_on_timeout": 0,
    "upgrade_to_https": false,
    "proxy": "",
    "proxy_allow_private": false,
    "proxy_identity": "",
    "proxy_authenticated": false,
    "credential_headers": [],
    "credentials_acknowledged": false,
    "auth": [],
    "auth_acknowledged": false,
    "zstd": false
  },
  "robots": {
    "policy": "respect",
    "user_agent_token": "",
    "unavailable_means_stop": true
  },
  "speed": {
    "min_delay_seconds": 0.5,
    "max_delay_seconds": 60.0,
    "adaptive": true,
    "stop_after_consecutive_timeouts": 5,
    "concurrency": 1
  },
  "output": {
    "dir": "",
    "write_pages_jsonl": true,
    "write_decisions_jsonl": true,
    "write_tasks": true
  },
  "link_position": {
    "classify": false,
    "rules": []
  },
  "link_attributes": {
    "capture": false
  },
  "cache": {
    "mode": "off",
    "invalidate": false
  },
  "resources": {
    "fetch": false,
    "max_requests": 20000,
    "max_response_bytes": 5242880,
    "graph": {
      "max_requests": 2000,
      "max_bytes": 104857600,
      "max_bytes_per_resource": 5242880,
      "max_seconds": 0,
      "max_origins": 1,
      "max_redirects": 0,
      "max_nesting": 2
    }
  },
  "evidence": {
    "retain_no_store_acknowledged": false,
    "content_area": {
      "include_selector": "",
      "root_selector": "",
      "exclude_tags": [
        "nav",
        "header",
        "aside",
        "footer"
      ],
      "exclude_selectors": []
    },
    "extraction_rules": []
  },
  "storage": {
    "format_version": "scan.v1",
    "capacity_profile": "stable",
    "body_mode": "captured_entity_bytes",
    "max_body_bytes": 5242880,
    "max_body_store_bytes": 10737418240,
    "min_free_bytes": 1073741824,
    "history_warning_bytes": 21474836480
  },
  "rendering": {
    "mode": "raw",
    "escalation": {
      "policy": "sampled",
      "sample_per_pattern": 2,
      "max_render_urls": 30,
      "max_render_seconds": 0
    },
    "rendered_links": {
      "store": false,
      "crawl": false
    },
    "actions": [],
    "presets": [],
    "browser": {
      "engine": "chromium",
      "transport": "local",
      "remote_protocol": "playwright",
      "remote_endpoint_env": "",
      "remote_playwright_version": "",
      "script_timeout_seconds": 10.0,
      "viewport": "desktop",
      "viewport_width": 0,
      "viewport_height": 0,
      "resize_to_content": false,
      "resize_to_content_max_height_px": 15000,
      "flatten_shadow_dom": false,
      "flatten_iframes": false,
      "device_pixel_ratio": 1.0,
      "mobile_emulation": false,
      "touch_emulation": false,
      "wait_until": "load",
      "page_concurrency": 1,
      "persistent_profile": false,
      "persistent_profile_dir": ""
    },
    "artifacts": {
      "screenshots": false,
      "console_errors": false,
      "screenshot_mode": "full_page"
    }
  }
}"#;
