# Command inputs

This reference is generated from `seohead.core.input_contracts`. Each row inventories consumed
source inputs rather than inferring them from a command's output. Forms on one row can be
required together; the notes name those relationships.

A **scan artifact** is a retained local `scan.v1` SQLite file. Read-only analysis and
history operations do not replay a crawl or promise retained page bodies. `crawl-site --resume`
is the explicit exception: it continues network collection. `duplicate-check` and
`boilerplate-report` may instead read one retained scan corpus with `--scan`.

## Operational-store decision

`scan.v1` is the authoritative retained evidence store for an artifact-mode crawl.
The HTTP cache remains sharded `http_cache.v3` files because concurrent crawl workers
can independently replace one cache entry. In live cache modes a cache miss is a safe
network fallback; a replay-mode miss remains offline and is reported. The run
journal remains an append-only local `runs.jsonl` record. Neither store is a second scan corpus,
and this decision makes no backend migration.

## Catalogue

| Command | Accepted input forms | Notes |
| --- | --- | --- |
| `bi-filter` | Local directory (`package, out_dir`)<br>Selector (`dataset, columns`)<br>Inline JSON (`where, max_rows_per_file, max_bytes_per_file, max_output_bytes, xlsx_max_rows_per_sheet`)<br>Local file (`xlsx_out`) | — |
| `scan-navigation` | Scan artifact (`input_path`)<br>Selector (`document_id, limit, offset`) | — |
| `doctor` | Project directory (`projects_directory`)<br>Local configuration (`config_path`)<br>Local file (`sf_cli`) | Optional directory access check.; Optional Screaming Frog executable path. |
| `update-check` | Selector (`consent, automatic, max_releases`) | Network access requires consent; no scheduler starts. |
| `update-download` | Selector (`consent, tag, asset_name, max_bytes`)<br>Local file (`out`) | Downloads only an asset from a previously checked release.; New destination file; the package is not installed. |
| `project-activity` | Project directory (`directory`)<br>Selector (`since_revision`) | Optional opaque observer revision for a compact unchanged response. |
| `project-checklist-page` | Project directory (`directory`)<br>Selector (`offset, limit, query, kind, state, sort, descending, states`) | — |
| `project-task-detail` | Project directory (`directory`)<br>Selector (`item_id`) | — |
| `project-scans` | Project directory (`directory`)<br>Selector (`offset, limit`) | — |
| `provider-auth` | Inline JSON (`provider, action, grant_file, confirm`) | GSC private grant import/status/refresh; cancel a pending browser flow; confirmed disconnect or remote revoke. No secret values returned. |
| `provider-replay` | Scan artifact (`input_path`); requires `evidence_file, out_dir` | Offline join with a private saved provider envelope; raw joins remain in restricted output. |
| `parse` | Live URL (`url`)<br>URL list (`urls`) | — |
| `crawl-site` | Live URL (`url`)<br>URL list (`urls`)<br>Local file (`urls_file`)<br>Scan artifact (`resume`)<br>Local configuration (`config`)<br>Saved crawl profile (`profile`)<br>Project directory (`project`) | TXT, CSV, XLSX, or XML URL input; Resumes retained crawl evidence and continues network collection.; Selects a saved local crawl profile by name.; Defaults the target and scans/ path; explicit paths, legacy output and resume keep their route. |
| `crawl-describe-settings` | No direct input | — |
| `crawl-control` | Project directory (`project`)<br>Selector (`run_id, action`) | — |
| `scan-reanalyze` | Scan artifact (`scan`) | — |
| `scan-graph` | Scan artifact (`scan`)<br>Scan artifact (`compare_to`)<br>Selector (`positions, section, around, depth, aggregate, representation, node_offset, edge_offset, node_limit, edge_limit, max_nodes, max_edges, timeout_seconds, max_bytes`) | Reads a bounded retained graph offline; missing evidence and clipping stay explicit.; Optional retained comparison source. |
| `scan-sitemap-generate` | Scan artifact (`scan`); requires `out_dir, sitemap_base_url`<br>Local directory (`out_dir`)<br>Selector (`sitemap_base_url, include_images, exclude`) | Deployment base URL sets output scope; it is never fetched.; Fresh exclusive output directory; never deploys. |
| `log-scan` | Local directory (`run`)<br>Scan artifact (`run`) | — |
| `crawl-diagnose` | Scan artifact (`scan`)<br>Local directory (`run`) | Choose one retained source; diagnosis is offline and read-only. |
| `crawl-diagnose-export` | Scan artifact (`scan`); requires `export`<br>Local directory (`run`); requires `export`<br>Local file (`export`) | Choose one retained source and a new redacted export destination; refuses overwrite. |
| `compare-crawls` | Audit document (`before, after`)<br>Local file (`correspondence`)<br>Local directory (`out_dir`)<br>Selector (`compression, resume`) | Each path may be audit JSON or scan.v1.; Optional closed url-correspondence.v1 JSON declaration; exact URL comparison remains the default.; Optional new compare.v2 package; required for audit.v2 populations above 10,000 pages or issues. Refuses overwrite.; none (default) or explicit gzip with out_dir; every row is retained and compressed/uncompressed bytes are declared. Resume requires an exact matching persistent checkpoint. |
| `compare-read` | Local file (`manifest_path`)<br>Selector (`file`)<br>Selector (`offset`)<br>Selector (`limit`)<br>Selector (`sort`)<br>Selector (`direction`)<br>Selector (`contains`)<br>Selector (`segment`)<br>Selector (`change`)<br>Selector (`max_bytes`) | compare.v2 compare.json manifest; one retained comparison stream; optional URL substring filter; named segment; only available for segment_changes; change type; only available for segment_changes |
| `compare-report` | Local file (`manifest_path`)<br>Local file (`out_file`)<br>Selector (`max_bytes`) | compare.v2 compare.json manifest; new Markdown path; refuses overwrite |
| `compare-status` | Local directory (`out_dir`) | comparison output path; reads active checkpoint or completed cache |
| `compare-url` | Audit document (`before, after`)<br>Selector (`url`)<br>Selector (`max_bytes`)<br>Selector (`max_lines`)<br>Selector (`left_representation, right_representation`) | Each source may be audit JSON or scan.v1 SQLite.; Exact retained URL text. |
| `verify-fixes` | Audit document (`baseline`); requires `out_dir`<br>Audit document (`after`); requires `baseline, out_dir`<br>Selector (`finding_ids`); requires `baseline, out_dir`<br>Local file (`view`); requires `baseline, out_dir`<br>URL list (`urls`); requires `baseline, out_dir`<br>Local file (`urls_file`); requires `baseline, out_dir`<br>Local configuration (`config`) | Offline verification without recrawling.; Saved verification_view.v1 selection.; Required when the baseline redacted credentials. |
| `crawl-enrich` | Audit document (`audit`); requires `external_csv`<br>Local file (`external_csv`); requires `audit` | — |
| `crawl-import` | Local file (`manifest_path`) | third_party_crawl_manifest.v1 with manifest-relative CSV datasets; output remains foreign crawl evidence |
| `segment-diff` | Audit document (`audit`) | — |
| `redirects-generate` | Inline JSON (`redirects`) | — |
| `redirects-check` | Live URL (`url`) | — |
| `sitemap-crawl` | Live URL (`url`)<br>Project directory (`project`) | Optional explicit local observation; same origin only. Declared URLs are not fetched HTML pages. |
| `images-download` | URL list (`urls`) | — |
| `images-optimize` | Local file (`files`)<br>Scan artifact (`scan`)<br>Selector (`max_images`) | — |
| `keywords-cluster` | Inline JSON (`params`) | — |
| `robots-check` | Live URL (`url`) | — |
| `headers-check` | Live URL (`url`) | — |
| `asset-weight-check` | Live URL (`url`) | — |
| `links-check` | Live URL (`url`) | — |
| `hreflang-check` | Live URL (`url`) | — |
| `domain-profile` | Domain (`domain`) | — |
| `cdn-check` | Live URL (`url`) | — |
| `tech-detect` | Live URL (`url`) | — |
| `security-check` | Live URL (`url`) | — |
| `backlinks-check` | Domain (`target`); requires `donors`<br>URL list (`donors`); requires `target`<br>Local file (`donors_file`); requires `target` | — |
| `schema-check` | Live URL (`url`)<br>Inline HTML (`html`) | — |
| `schema-build` | Live URL (`url`)<br>Inline HTML (`html`) | — |
| `duplicate-check` | Inline corpus (`items`)<br>Scan artifact (`scan`) | — |
| `ai-bots-check` | Live URL (`url`)<br>Inline text (`robots_text`) | — |
| `mirror-check` | Live URL (`url`) | — |
| `llms-txt-check` | Live URL (`url`) | — |
| `citability-check` | Live URL (`url`)<br>Inline text (`text`) | — |
| `markdown-extract` | Live URL (`url`)<br>Inline HTML (`html`) | — |
| `boilerplate-report` | Inline corpus (`pages`)<br>Scan artifact (`scan`) | — |
| `semantic-inputs` | Inline corpus (`items`)<br>Scan artifact (`scan`) | — |
| `semantic-similarity` | Inline corpus (`items`); requires `embeddings, adapter, cache_path`<br>Scan artifact (`scan`); requires `embeddings, adapter, cache_path`<br>Inline JSON (`embeddings, adapter`)<br>Local file (`cache_path`)<br>Selector (`threshold, max_candidate_comparisons`) | Supplied normalized-vector evidence; no model is loaded or called.; Uses the retained semantic corpus and its recorded normalization policy.; Local SQLite embedding cache. |
| `meta-description-drafts` | Inline corpus (`items`)<br>Scan artifact (`scan`)<br>Inline JSON (`context`)<br>Inline JSON (`drafts, executor`); requires `checkpoint_path`<br>Local file (`checkpoint_path, json_path, csv_path`)<br>Selector (`batch_size`) | Dry-run needs only supplied page HTML.; Uses retained normalized page content offline.; Optional versioned site instructions.; Optional structured caller/delegated-agent results; no provider call. |
| `ai-column` | Inline corpus (`items`)<br>Scan artifact (`scan`)<br>Inline text (`prompt`)<br>Inline JSON (`urls, rows`)<br>Local file (`csv_path`)<br>Selector (`column, max_pages`) | Dry-run needs only supplied normalized page text.; Uses retained normalized page content offline.; Required instruction, at most 2000 characters.; Optional URL selection and caller-supplied values; no provider call.; Formula-safe CSV of the column values. |
| `social-meta-check` | Live URL (`url`)<br>Inline JSON (`og, twitter`) | — |
| `soft404-check` | Live URL (`url`) | — |
| `log-analyze` | Local log (`path`)<br>Selector (`verify_bots, max_lines, offset, limit, max_dns, bot, ip`) | — |
| `logs-import` | Local log (`path`)<br>Project directory (`project`)<br>Selector (`max_lines, max_files`) | — |
| `logs-summary` | Local file (`source`)<br>Selector (`view, limit, offset, bot, family, ip, date_from, date_to, status, section`) | — |
| `logs-join` | Local file (`source`)<br>Scan artifact (`scan`)<br>Selector (`view, base_url, family, bot, date_from, date_to, limit, offset`) | — |
| `logs-bots` | Local file (`source`)<br>Selector (`verify_bots, limit, offset, bot, family, ip, verdict, max_dns`) | — |
| `regions-check` | Live URL (`url`) | — |
| `render-check` | Live URL (`url`)<br>Inline JSON (`transport_config`) | Optional local/remote Playwright transport selection; endpoint is named by environment variable. |
| `site-audit` | Live URL (`url`)<br>URL list (`urls`) | — |
| `report-build` | Audit document (`audit`)<br>Project directory (`project`)<br>Selector (`view`)<br>Selector (`offset`)<br>Selector (`pdf_policy`) | Audit JSON or a retained scan.v1 artifact.; Includes validated checklist coverage and optionally applies a saved finding view.; Optional saved project finding view; requires project.; Optional stable finding-view page offset.; Explicit overview-v1 for PDF + retained audit.v2 only: bounded PDF with complete mandatory JSON/CSV/manifest companions; never inferred. |
| `facts-export` | Inline JSON (`sites`) | — |
| `marketing-inventory` | Inline JSON (`documents`)<br>Selector (`cta_selector, form_selector, id_attributes, id_parameters`)<br>Local directory (`out_dir`) | — |
| `keywords-expand` | Provider query (`phrase`) | — |
| `keywords-seasonality` | Provider query (`phrase`) | — |
| `keywords-exact` | Provider query (`keywords`) | — |
| `serp-fetch` | Provider query (`query`)<br>Provider query (`queries`) | — |
| `spend-report` | Local log | Configured local spend log. |
| `sources-doctor` | Local configuration | — |
| `sources-sync` | Provider query (`source, resource`)<br>Local file (`db`)<br>Project directory (`project`) | Explicit provider read and local SQLite write; records requested coverage and replaces eligible days atomically. |
| `sources-status` | Local file (`db`)<br>Project directory (`project`) | — |
| `sources-export` | Local file (`db`)<br>Project directory (`project`) | — |
| `regions-tree` | Local configuration | — |
| `topvisor-read` | Inline JSON (`operation, params`) | One bounded page of existing Topvisor data. Default operation is projects; other operations require params.project_id. Follow the provider's nextOffset for further pages. No paid checks or mutations. |
| `metrika-counters` | Local configuration | — |
| `metrika-setup` | Provider query (`counter_id`) | — |
| `metrika-report` | Provider query (`counter_id`) | — |
| `metrika-traffic-pdf` | Provider query (`counter_id`); requires `date1, date2, out_dir`<br>Local file (`document`); requires `out_dir`<br>Local configuration (`brand`)<br>Inline JSON (`gsc_rows`) | Renders an existing traffic document offline; no Metrika request.; Optional brand JSON file or inline object.; Optional Search Console query rows. |
| `google-keywords` | Provider query (`keywords`)<br>Provider query (`seed`) | — |
| `dataforseo-estimate` | Inline JSON (`operation, query_count, row_limit, cache_hits, prices, env, query`) | — |
| `backlinks-profile` | Provider query (`domain`)<br>Inline JSON (`imports, evidence, sources`)<br>Local file (`db`)<br>Project directory (`project`) | — |
| `backlinks-disavow` | URL list (`urls, domains`)<br>Local file (`output`) | — |
| `google-serp` | Provider query (`query`) | — |
| `wayback-history` | Live URL (`url`) | — |
| `cloudflare-traffic` | Provider query (`zone`) | — |
| `crtsh-subdomains` | Domain (`domain`) | — |
| `gsc-query` | Provider query (`site_url`) | — |
| `webmaster-url-queries` | Provider query (`host_id`) | Bounded Yandex URL-to-query evidence. |
| `miratext-analyze` | Inline JSON | Miratext analysis; paid modes require confirmation. |
| `gsc-archive` | Local file (`database`)<br>Inline JSON (`database, action, site_url, start_date, end_date, max_requests, pause, backup_path`) | Explicit SQLite archive. Only prepare creates a missing file; status and backup are offline.; prepare queues inclusive dates; run makes bounded Google API calls; backup requires a new destination. |
| `crux-report` | Provider query (`url`)<br>Provider query (`origin`) | — |
| `indexnow-submit` | URL list (`urls`) | — |
| `scan-list` | Local directory (`directory`)<br>Project directory (`project`) | — |
| `project-new` | Project directory (`directory`)<br>Live URL (`target`) | — |
| `project-open` | Project directory (`directory`) | — |
| `project-archive` | Project directory (`project`)<br>Local file (`out`)<br>Selector (`dry_run, exclude_bodies, exclude_bodies_older_than_days, keep_newest_scans`) | — |
| `project-restore` | Local file (`archive, to`) | — |
| `project-sources-link` | Project directory (`directory`) | Binds a provider resource ID (--service, --resource, optional --label) to the project in sources.json; no network. |
| `project-sources-unlink` | Project directory (`directory`) | Removes one (--service, --resource) binding from sources.json; no network. |
| `project-sources-list` | Project directory (`directory`) | — |
| `project-status` | Project directory (`directory`) | — |
| `project-progress` | Project directory (`directory`)<br>Inline JSON (`limit, offset`) | Optional bounded progress pagination. |
| `project-observe` | Project directory (`directory`)<br>Selector (`consumer`)<br>Inline JSON (`scan_limit`)<br>Inline JSON (`run_offset, run_limit`)<br>Selector (`since_revision`) | Optional scoped unread-notice recipient.; Bounded retained scan-history page.; Per-site terminal run page; all stored running records remain visible.; Optional opaque observer revision for a compact unchanged response. |
| `project-inbox-submit` | Project directory (`directory`)<br>Inline text (`text`)<br>Selector (`kind, references, author_role`) | — |
| `project-inbox-list` | Project directory (`directory`)<br>Selector (`consumer, offset, limit, include_acknowledged`) | — |
| `project-inbox-read` | Project directory (`directory`)<br>Selector (`consumer, entry_ids, expected_revision`) | — |
| `project-inbox-acknowledge` | Project directory (`directory`)<br>Selector (`consumer, entry_ids, expected_revision`) | — |
| `project-inbox-goal` | Project directory (`directory`)<br>Selector (`entry_id, state, expected_revision`) | — |
| `project-inbox-triage` | Project directory (`directory`)<br>Inline JSON (`entry_id, outcome, actor, expected_revision`) | Explicit controller outcome for a specialist note; it never reads, acknowledges, or executes the note. |
| `project-inbox-unread` | Project directory (`directory`)<br>Selector (`consumer, limit`) | — |
| `remediation-create` | Local file (`path`)<br>Project directory (`project_dir`)<br>Selector (`producer_build`) | New ledger path; an existing file is refused.; Full lowercase 40-character Git SHA. |
| `remediation-ingest` | Local file (`ledger`)<br>Local file (`scan`) | Read-only saved scan.v1 SQLite artifact. |
| `project-event-append` | Project directory (`directory`)<br>Selector (`source, actor`)<br>Inline text (`text`) | — |
| `project-event-page` | Project directory (`directory`)<br>Selector (`source, offset, limit, query`) | — |
| `remediation-summary` | Local file (`ledger`) | — |
| `remediation-cases` | Local file (`ledger`)<br>Selector (`check, url, finding_key, limit, offset, source_scan_id, group_ref, max_bytes`) | Use source_scan_id and group_ref together for a complete ordered group member page; mutually exclusive with finding selectors. |
| `remediation-transition` | Local file (`ledger`)<br>Selector (`occurrence_key, state, actor, reason, expected_revision`)<br>Inline JSON (`observation_id, decided_at`) | — |
| `remediation-record-verification` | Local file (`ledger, verification_path`)<br>Selector (`actor, expected_revision`) | — |
| `remediation-recheck` | Local file (`ledger, baseline, out_dir`)<br>Audit document (`after`)<br>Selector (`occurrence_keys, actor, expected_revision, task_id`)<br>Local configuration (`config`) | Optional retained later audit for offline verification.; Required when the baseline redacted credentials. |
| `remediation-report` | Local file (`ledger`)<br>Local directory (`out_dir`)<br>Selector (`limit, offset`) | — |
| `project-facts` | Project directory (`directory`)<br>Inline JSON (`facts`) | detect fetches the project's own target once after robots.txt; it is never implicit.; Operator-entered facts; preview by default, recorded with apply. |
| `project-checklist-init` | Project directory (`directory`)<br>Inline JSON (`template`)<br>Inline JSON (`plan`) | Optional reusable data-only checklist template.; Optional agreed scope plan fixing the URL-population and task denominators. |
| `project-checklist-update` | Project directory (`directory`)<br>Inline JSON (`item`) | Requires expected_revision for optimistic concurrency. |
| `project-checklist-record` | Project directory (`directory`)<br>Selector (`item_id`)<br>Inline JSON (`record`) | Requires expected_revision; records supplied evidence only. Automatic checks take status succeeded, reason and a saved scan artifact; manual items take a reviewer with signoff or an approved reviewed artifact; not_applicable needs reason, reviewer and an evidence basis. |
| `project-view-list` | Project directory (`directory`) | — |
| `project-view-show` | Project directory (`directory`)<br>Selector (`name`) | — |
| `project-view-save` | Project directory (`directory`)<br>Inline JSON (`view`)<br>Selector (`expected_revision`) | Required; use 0 for the first saved view. |
| `project-view-delete` | Project directory (`directory`)<br>Selector (`name`)<br>Selector (`expected_revision`) | Required; current view config revision. |
| `project-view-rename` | Project directory (`directory`)<br>Selector (`name`)<br>Selector (`new_name`)<br>Selector (`expected_revision`) | Required; current view config revision. |
| `findings-view` | Project directory (`directory`)<br>Selector (`name`)<br>Audit document (`audit`)<br>Selector (`offset`) | Audit JSON, inline audit object, or retained scan.v1.; Optional stable finding-view page offset. |
| `project-priorities` | Project directory (`directory`)<br>Inline JSON (`policy`) | Optional data-only priority policy; preview by default. Apply requires expected_revision. |
| `project-policy` | Project directory (`directory`)<br>Inline JSON (`policy`) | Optional data-only policy; preview by default. Apply requires expected_revision. |
| `project-prepare` | Project directory (`directory`)<br>Inline JSON (`template`)<br>Inline JSON (`competitors`) | Optional data-only project template.; Optional bounded competitor inputs. |
| `project-start` | Project directory (`directory`)<br>Live URL (`target`)<br>Inline JSON (`facts`)<br>Inline JSON (`template`)<br>Inline JSON (`competitors`) | Optional supplied project facts.; Optional data-only project template.; Optional bounded competitor inputs. |
| `project-competitors-add` | Project directory (`directory`)<br>Inline JSON (`competitors`) | Required list of candidates with url, source and observed_at. |
| `schedule-show` | Project directory (`directory`) | — |
| `schedule-set` | Project directory (`directory`)<br>Selector (`expected_revision`)<br>Inline JSON (`settings`) | Required; current schedule revision.; Partial schedule.json settings to merge. |
| `schedule-run-due` | Project directory (`directory`) | — |
| `schedule-install` | Project directory (`directory`)<br>Selector (`kind`)<br>Local file (`dest, python`) | launchd, cron or windows. |
| `schedule-uninstall` | Project directory (`directory`)<br>Selector (`kind`)<br>Local file (`dest`) | — |
| `project-competitors-list` | Project directory (`directory`) | — |
| `project-competitors-remove` | Project directory (`directory`)<br>Live URL (`url`) | The recorded competitor URL to drop from the shortlist. |
| `skill-list` | No direct input | — |
| `skill-show` | Selector (`name`) | — |
| `scenario-list` | No direct input | — |
| `scenario-show` | Selector (`name`) | — |
| `provider-registry` | No direct input | — |
| `provider-readiness` | Inline JSON (`provider, operation`) | Offline readiness and operation discovery; no provider requests. |
| `provider-verify` | Provider identifier (`provider`)<br>Inline JSON (`request`) | Optional read-only target-access request. |
| `provider-collect` | Provider identifier (`provider`)<br>Selector (`operation`)<br>Inline JSON (`request`)<br>Local directory (`artifact_dir`) | Optional restricted raw-evidence location. |
| `provider-join` | Inline JSON (`crawl_pages`)<br>Inline JSON (`evidence_rows`)<br>Inline JSON (`adjustments`) | Optional evidence-backed priority adjustments. |
| `evidence-normalize` | Local file (`file`)<br>Inline JSON (`mapping`)<br>Local directory (`out_dir`) | Supplied CSV/XLSX/JSON rows or a saved provider-evidence envelope; fully offline.; Optional seohead.evidence-mapping.v1 manifest, inline or file path.; Optional restricted normalized artifact directory. |
| `evidence-join` | Scan artifact (`scan`)<br>Audit document (`audit`)<br>Inline JSON (`pages`)<br>Local file (`evidence, compare`)<br>Inline JSON (`mapping, compare_mapping, policy`)<br>Local directory (`out_dir`) | Alternative crawl side; offline scan read like provider-replay.; Alternative crawl side.; Alternative crawl side, page objects.; CSV/XLSX/JSON or saved provider envelope; inline JSON also accepted.; Mapping manifests and the declared comparison policy, inline or file path.; Optional private join/compatibility artifact directory. |
| `bi-export` | Scan artifact (`scan`)<br>Audit document (`audit`)<br>Local file (`provider_joins`)<br>Local directory (`out_dir`)<br>Inline JSON (`max_rows_per_file, max_bytes_per_file, max_output_bytes, max_scan_bytes, search_metric, xlsx_max_rows_per_sheet`)<br>Local file (`xlsx_out`); requires `xlsx_dataset`<br>Selector (`xlsx_dataset`); requires `xlsx_out` | Reads saved artifacts only; no provider calls, crawl, or remote writes.; Alternative validated scan.v1 source.; Alternative supported audit JSON source.; Optional saved issue #781 evidence-join/normalized-evidence JSON files; repeatable.; Required new local package directory; existing output is refused.; Optional positive partition/total-output bounds and explicit Search Console clicks or impressions axis; exceeding a hard limit fails without publishing a package.; Optional new split XLSX consumer output from one verified package dataset. |
| `publication-cohorts` | Inline JSON (`document`)<br>Local file (`file`)<br>Local directory (`out_dir`) | Reads saved normalized evidence only; no provider calls or causal inference.; seohead.publication-cohort-input.v1 document.; Alternative versioned offline cohort input JSON.; Required new local package directory; existing output is refused. |
| `gsc-progress` | Inline JSON (`document`)<br>Local file (`file`)<br>Local directory (`out_dir`) | Reads saved normalized GSC evidence only; no provider calls or rank-placement claims.; seohead.gsc-progress-input.v1 document.; Alternative versioned offline GSC input JSON.; Required new local package directory; existing output is refused. |
| `bi-sheets-plan` | Local directory (`package`)<br>Inline JSON (`max_cells`) | Offline package/checksum/capacity preflight; no Google authentication or write.; Optional declared capacity, never an API quota check. |
| `bi-bigquery-plan` | Local directory (`package`)<br>Selector (`dataset, operation`) | Offline optional-load plan; no project selection, billing, authentication, or write. |
| `bi-destination-apply` | Local directory (`package`)<br>Selector (`target, destination, operation`)<br>Inline JSON (`apply`)<br>Inline JSON (`reconcile`) | Never accepts credentials; a missing host client fails before any destination write.; Requires true and a host-injected authorized client.; Explicitly reconciles a retained uncertain request before any retry; requires apply=true. |
| `inspect-url` | Live URL (`url`)<br>Inline JSON (`checks`) | Optional bounded selection of closed investigation checks. |
| `audit-workflow` | Project directory (`directory`)<br>Selector (`action`)<br>Live URL (`target`)<br>Audit document (`audit`) | status, start, prepare, or report.; Required only for action=start.; Optional for action=report; omitted, the project's latest complete audited scan is used. |
| `workflow-status` | Project directory (`directory`) | — |
| `workflow-start` | Project directory (`directory`); requires `scenario_id, steps` | — |
| `workflow-checkpoint` | Project directory (`directory`); requires `run_id, step_id, state, expected_revision` | — |
| `workflow-execute` | Project directory (`directory`); requires `scenario_id, steps, outcomes` | — |
| `workflow-resume` | Project directory (`directory`); requires `run_id, expected_revision` | — |
| `monitor-status` | Project directory (`directory`) | — |
| `monitor-configure` | Project directory (`directory`); requires `policy` | — |
| `monitor-run` | Project directory (`directory`); requires `scan_id, observations, expected_revision` | Records supplied retained-scan differences only; it starts no schedule or delivery. |
| `monitor-collect` | Project directory (`directory`); requires `expected_revision`<br>Selector (`expected_revision, apply`) | Preview by default; explicit apply collects only an existing validated bounded claim. No timer or delivery starts. |
| `monitor-local-deliver` | Project directory (`directory`); requires `scan_id, expected_revision`<br>Selector (`scan_id, expected_revision`) | Records a local:receipt only; no network, external transport or recipient delivery claim. |
| `monitor-schedule` | Project directory (`directory`); requires `action, expected_revision` | Claims, cancels, backs off, or recovers a local bounded pass; it starts no timer. |
| `tool-catalog` | Inline text (`query`)<br>Selector (`limit, offset, include_arguments`) | Optional bounded discovery query. |
| `url-check-batch` | URL list (`urls`)<br>Selector (`checks`)<br>Inline JSON (`options`)<br>Scan artifact (`scan`)<br>Selector (`offset, max_urls`) | Shared options for the selected checks. |
| `scan-inspect` | Scan artifact (`scan`) | — |
| `scan-content-search` | Scan artifact (`input_path`)<br>Inline text (`query`)<br>Selector (`url`) | Offline retained-content query; package output is bounded and source-identified.; Optional exact logical URL filter. |
| `scan-content-search-page` | Local file (`package`) | Read one bounded page from a prior offline search package. |
| `scan-url-detail` | Scan artifact (`input_path`)<br>Selector (`url, body_offset, body_length, max_body_bytes`) | Exact retained native URL; bounded source HTML and redacted query values. |
| `scan-body-read` | Scan artifact (`input_path`)<br>Selector (`url, representation, variant_key, offset, length, max_decoded_bytes`) | Exact retained URL; body output is bounded and read-only. |
| `scan-findings-page` | Project directory (`project`)<br>Selector (`scan_uuid, offset, limit, query, url, sort, descending`) | — |
| `scan-url-counts` | Scan artifact (`input_path`) | — |
| `extraction-presets` | No direct input | — |
| `scan-url-query` | Scan artifact (`scan`) | — |
| `scan-directory-tree` | Scan artifact (`input_path`) | Retained URL paths only; no network access. |
| `headers-analyze` | Scan artifact (`input_path`) | Reads saved redacted response headers only. |
| `scan-url-history` | Project directory (`project`)<br>Selector (`url`) | Exact retained URL text, as scan-url-detail takes it. |
| `scan-link-inspect` | Scan artifact (`scan`) | Offline path, inlinks, occurrence context or one URL's paged links selected by view; mode-specific selectors and limits are required. |
| `scan-status` | Scan artifact (`scan`) | — |
| `scan-rendered-routes` | Scan artifact (`scan`) | — |
| `scan-snapshot` | Scan artifact (`scan`) | — |
| `scan-export` | Scan artifact (`input_path`)<br>Selector (`records, fields`) | Also accepts an SF Analyzer audit.json document; links are unavailable there.; Optional record-type and field projection validated before any file is written. |
| `scan-pin` | Scan artifact (`scan`) | — |
| `scan-prune` | Local directory (`directory`)<br>Local file (`plan`)<br>Project directory (`project`) | Defaults the directory to project scans/; apply remains explicit. |
| `scan-body-diff` | Scan artifact (`left, right`)<br>Selector (`url, representation, left_representation, right_representation, variant_key`) | Selects one exact URL and retained representation from both scans. |
| `scan-evidence` | Scan artifact (`input_path`)<br>Selector (`section`) | capabilities, corpus, structured, routes, resources, or timeline. |
| `scan-extract` | Scan artifact (`input_path`)<br>Inline JSON (`rules`)<br>Selector (`url`) | Closed declarative rules over retained complete bodies.; Optional exact logical URL. |
| `custom-extract-sample` | Inline text (`html`)<br>Inline JSON (`extractors`)<br>Selector (`url`) | Supplied HTML; never fetched.; Explicit extractor specifications.; Optional output label only; never fetched. |
| `scan-structured-blocks` | Scan artifact (`input_path`)<br>Selector (`url`)<br>Selector (`representation`)<br>Selector (`variant_key`) | Exact retained logical URL.; static, rendered, or legacy_fragment source.; Optional exact retained HTTP variant key. |
| `scan-fragment-links` | Scan artifact (`input_path`)<br>Selector (`state`)<br>Selector (`representation`) | Optional resolved, missing, or skipped occurrence filter.; Optional static, rendered, or legacy_fragment source filter. |
| `scan-requeue` | Scan artifact (`input_path`)<br>Selector (`where`)<br>Local file (`backup_path`)<br>Scan artifact (`from_scan`) | Restricted saved URL/page predicate.; Mandatory new verified backup destination.; Optional alternate saved selection source. |
| `scan-import-urls` | Scan artifact (`input_path`)<br>Local file (`urls_file`)<br>Local file (`backup_path`) | Explicit TXT, CSV, XLSX, or XML URL source.; Mandatory new verified backup destination. |
| `cloudflare-history` | Project directory (`project`)<br>Provider identifier (`zone_id`)<br>Selector (`snapshot_id, limit, offset, include_facts, facts_offset, facts_limit`) | Reads retained snapshots only; facts are opt-in and bounded.; Required local project with retained Cloudflare history. |
| `cloudflare-instant-logs` | Project directory (`project`)<br>Selector (`confirm_live_capture, max_seconds, max_events, max_bytes, sample`) | Uses the project's selected Cloudflare zone binding.; Live capture requires explicit --confirm-live-capture and remains duration/event/byte bounded. |
| `cloudflare-logpush-import` | Local file (`input_path`)<br>Project directory (`project`)<br>Selector (`max_events`) | Caller-supplied UTF-8 Logpush JSON Lines file.; Project owning the local logstore.; Maximum imported records. |
| `project-action-log` | Project directory (`directory`)<br>Selector (`offset, limit, client, outcome, query`) | Bounded local read-only history. |
| `project-agent-heartbeat` | Project directory (`directory`) | Records a timestamped interface event; it does not prove the client remains connected. |
| `project-competitors-scan` | Project directory (`directory`)<br>Selector (`task_id, expected_revision, confirm, concurrency, approve_large_crawl, producer_build, run_id`) | Network scans require explicit confirmation, current task revision, and any large-crawl approval; URL and concurrency caps remain enforced. |
| `project-competitors-scan-cancel` | Project directory (`directory`)<br>Selector (`run_id`) | Requests checkpointed cancellation of the named run. |
| `project-competitors-scan-status` | Project directory (`directory`)<br>Selector (`run_id`) | Reads status for the named durable run. |
| `project-delete` | Project directory (`directory`) | Moves the project to sibling Trash; active project/job writers are refused. |
| `project-export` | Project directory (`directory`)<br>Selector (`dataset, fmt, compare_manifest, job_id`)<br>Local file (`output`) | compare_manifest is required for the compare dataset.; New output beneath the project's reports directory; refuses overwrite. |
| `project-export-cancel` | Project directory (`directory`)<br>Selector (`job_id`) | Requests cancellation at an export checkpoint. |
| `project-export-status` | Project directory (`directory`)<br>Selector (`job_id`) | Reads bounded status for the named export job. |
| `project-exports-page` | Project directory (`directory`)<br>Selector (`offset, limit`) | Reads a bounded page of retained export receipts. |
| `project-settings` | Project directory (`directory`) | Reads local project settings and their revision. |
| `project-settings-proposals` | Project directory (`directory`)<br>Selector (`offset, limit`) | Reads a bounded page of setting proposals. |
| `project-settings-propose` | Project directory (`directory`)<br>Inline JSON (`changes`)<br>Selector (`expected_revision`) | Proposed settings only; proposal does not apply the changes.; Current proposal revision for compare-and-swap. |
| `project-settings-review` | Project directory (`directory`)<br>Selector (`proposal_id, decision, expected_revision`) | Explicit local owner decision against the current proposal revision. |
| `project-task-comment` | Project directory (`directory`)<br>Inline text (`text`)<br>Selector (`item_id, actor, expected_revision`)<br>Inline JSON (`reference`) | Optional retained scan/finding reference. |
| `project-update` | Project directory (`directory`)<br>Selector (`label, expected_revision`) | Label update uses the current settings revision. |
| `provider-artifact-read` | Local directory (`artifact_dir`)<br>Selector (`artifact_ref, offset, limit`) | Caller-owned directory containing the saved provider artifact.; Reads only a bounded sanitized sample; no provider request. |
| `provider-credential` | Provider identifier (`provider`)<br>Selector (`component, action, confirm`)<br>Secret from stdin (`value`)<br>Selector (`replace`) | Revocation requires explicit confirmation.; Only action=set; the CLI requires --value-stdin. Secret bytes never belong in argv, logs, or catalogue examples.; Only action=set; replacement is explicit. |
| `provider-discover` | Provider identifier (`provider`)<br>Domain (`domain`) | Optional domain-match hint; discovery does not bind or select resources. |
| `sf run` | Live URL (`crawl`)<br>Local file (`load_crawl`)<br>Local file (`crawl_list`)<br>Local directory (`exports_dir`)<br>Local configuration (`config`)<br>Local file (`auth_config`)<br>Inline text (`auth`)<br>Local file (`sf_cli`)<br>Live URL (`sitemap`)<br>Local directory (`out`) | Saved .seospider crawl; requires licensed SF CLI; URL-list file for licensed SF live traversal; SF audit configuration; semantic_similarity requires an explicit adapter declaration and supplied vectors. No model is selected by default.; Optional SF authentication profile.; Optional HTTP Basic credentials; do not persist or log them.; Optional explicit licensed SF CLI path.; Optional explicit sitemap URL for live rechecks.; Local audit and optional task output directory. |
| `sf tasks` | Audit document (`audit_json`)<br>Local configuration (`config`)<br>Local directory (`out`) | Audit JSON or a retained scan.v1 artifact.; Local task output directory. |
| `sf doctor` | Local configuration (`config`)<br>Local file (`sf_cli`) | Optional explicit licensed SF CLI path. |
| `sf save-config` | Local file (`out`) | — |
| `sf snippet-edits` | Local file (`csv`)<br>Local file (`out`)<br>Local file (`metrics_json`)<br>Scan artifact (`scan`)<br>Local file (`content_area_json`)<br>Selector (`query, representation, variant_key, max_body_bytes`) | Supplied CSV; requires url and title/description columns.; New JSON review-task destination; refuses overwrite.; Optional named local font profiles for width measurement.; Optional retained context for exact source URLs; read-only.; Optional JSON content-area rules for retained context.; query requires scan; measurements are local font widths, not search-engine truncation predictions. |
| `mcp` | No direct input | Starts the local stdio server; profile and progress-notification behavior are startup options. |
