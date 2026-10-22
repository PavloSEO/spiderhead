PRAGMA application_id = 1397051208; -- ASCII SEOH, identifies the application, not compatibility.
PRAGMA user_version = 1;
PRAGMA foreign_keys = ON;

CREATE TABLE scan (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  scan_uuid TEXT NOT NULL UNIQUE,
  format_version TEXT NOT NULL CHECK (format_version = 'scan.v1'),
  evidence_version TEXT NOT NULL,
  writer_version TEXT NOT NULL,
  writer_revision TEXT NOT NULL,
  runtime_versions_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  finished_at TEXT,
  source_kind TEXT NOT NULL CHECK (source_kind IN ('native','legacy_import','reanalysis')),
  parent_scan_uuid TEXT,
  start_url TEXT,
  config_json TEXT NOT NULL,
  config_fingerprint TEXT NOT NULL,
  lifecycle TEXT NOT NULL CHECK (lifecycle IN ('running','interrupted','finished','failed')),
  finish_reason TEXT NOT NULL,
  crawl_partial INTEGER NOT NULL CHECK (crawl_partial IN (0,1)),
  corpus_partial INTEGER NOT NULL CHECK (corpus_partial IN (0,1)),
  evidence_revision INTEGER NOT NULL CHECK (evidence_revision >= 0),
  limitations_json TEXT NOT NULL,
  capabilities_json TEXT NOT NULL,
  retention_json TEXT NOT NULL,
  pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0,1))
);
CREATE TABLE urls (
  url_id INTEGER PRIMARY KEY,
  url TEXT NOT NULL UNIQUE
);
CREATE TABLE bodies (
  sha256 TEXT PRIMARY KEY NOT NULL CHECK (length(sha256) = 64),
  codec TEXT NOT NULL CHECK (codec IN ('identity','zlib')),
  decoded_bytes INTEGER NOT NULL CHECK (decoded_bytes >= 0),
  stored_bytes INTEGER NOT NULL CHECK (stored_bytes >= 0),
  data BLOB NOT NULL,
  CHECK (stored_bytes = length(data))
);
CREATE TABLE responses (
  response_id INTEGER PRIMARY KEY,
  request_url_id INTEGER NOT NULL REFERENCES urls(url_id),
  request_ordinal INTEGER NOT NULL UNIQUE,
  effective_url_id INTEGER REFERENCES urls(url_id),
  redirect_chain_json TEXT NOT NULL,
  method TEXT NOT NULL,
  purpose TEXT NOT NULL CHECK (purpose IN ('page','script','stylesheet','robots','sitemap')),
  requested_at TEXT NOT NULL,
  received_at TEXT,
  request_headers_redacted_json TEXT NOT NULL,
  credentials_used INTEGER NOT NULL CHECK (credentials_used IN (0,1)),
  variant_key TEXT NOT NULL,
  status_code INTEGER,
  effective_status_code INTEGER,
  response_headers_redacted_json TEXT NOT NULL,
  effective_headers_redacted_json TEXT NOT NULL,
  content_type TEXT NOT NULL,
  charset TEXT NOT NULL,
  content_encoding TEXT NOT NULL,
  reported_size_bytes INTEGER,
  response_time REAL,
  transport_source TEXT NOT NULL CHECK (transport_source IN ('network','cache','legacy_import')),
  cache_status TEXT NOT NULL,
  source_response_id INTEGER REFERENCES responses(response_id),
  body_sha256 TEXT REFERENCES bodies(sha256),
  body_fidelity TEXT NOT NULL CHECK (body_fidelity IN ('entity_bytes','reencoded_text','unavailable')),
  body_state TEXT NOT NULL CHECK (body_state IN ('complete','truncated','omitted','unavailable')),
  body_reason TEXT NOT NULL CHECK (body_reason IN ('none','not_enabled','not_fetched','not_in_corpus','legacy_not_retained','cache_control_no_store','credentialed','unsupported_media','fetch_failed','truncated','body_budget_exhausted','resource_budget_exhausted','preexisting_cache_snapshot')),
  error TEXT NOT NULL,
  error_kind TEXT NOT NULL
);
CREATE TABLE documents (
  document_id INTEGER PRIMARY KEY,
  url_id INTEGER NOT NULL REFERENCES urls(url_id),
  representation TEXT NOT NULL CHECK (representation IN ('static','rendered','legacy_fragment')),
  source_response_id INTEGER REFERENCES responses(response_id),
  body_sha256 TEXT REFERENCES bodies(sha256),
  captured_at TEXT NOT NULL,
  decoder_version TEXT NOT NULL CHECK (decoder_version = 'scan_decoder.v1'),
  decoder_source TEXT NOT NULL CHECK (decoder_source IN ('content_type_charset','utf8_fallback','renderer_utf8','legacy_unknown','not_applicable')),
  decoder_charset TEXT NOT NULL,
  decoder_errors TEXT NOT NULL CHECK (decoder_errors IN ('replace','unknown','not_applicable')),
  fidelity TEXT NOT NULL CHECK (fidelity IN ('entity_bytes','reencoded_text','serialized_dom','unavailable')),
  body_state TEXT NOT NULL CHECK (body_state IN ('complete','truncated','omitted','unavailable')),
  body_reason TEXT NOT NULL CHECK (body_reason IN ('none','not_enabled','not_fetched','not_in_corpus','legacy_not_retained','cache_control_no_store','credentialed','unsupported_media','fetch_failed','truncated','body_budget_exhausted','resource_budget_exhausted','preexisting_cache_snapshot')),
  renderer_json TEXT NOT NULL
);
CREATE TABLE pages (
  url_id INTEGER PRIMARY KEY REFERENCES urls(url_id),
  page_ordinal INTEGER NOT NULL UNIQUE,
  document_id INTEGER REFERENCES documents(document_id),
  status_code INTEGER,
  content_type TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  response_time REAL,
  redirect_url TEXT NOT NULL,
  title TEXT NOT NULL,
  meta_description TEXT NOT NULL,
  h1 TEXT NOT NULL,
  h1_2 TEXT NOT NULL,
  h2 TEXT NOT NULL,
  canonical TEXT NOT NULL,
  meta_robots TEXT NOT NULL,
  x_robots TEXT NOT NULL,
  og_title TEXT NOT NULL,
  og_description TEXT NOT NULL,
  og_image TEXT NOT NULL,
  -- The page's own <meta property="og:url">, the fact a Screaming Frog export
  -- has always carried in its "OG:URL" column. Nullable, because a crawl written
  -- before it was collected never read the tag, which is not the same as a page
  -- that declares none -- and check_og names "og:url" missing from what it reads
  -- here, so the two must stay distinguishable.
  og_url TEXT,
  -- Count of <link rel="alternate" media=...> tags without an href. Nullable: a crawl
  -- written before this check never read them, which is not the same as zero.
  mobile_alternate_broken INTEGER,
  word_count INTEGER NOT NULL,
  text_ratio REAL,
  content_frames INTEGER,
  content_frames_same_origin INTEGER,
  crawl_depth INTEGER NOT NULL,
  content_encoding TEXT NOT NULL,
  charset TEXT NOT NULL,
  doctype TEXT NOT NULL,
  viewport TEXT NOT NULL,
  meta_refresh TEXT,
  http_refresh TEXT,
  meta_description_count INTEGER,
  h1_alt_text TEXT,
  lorem_ipsum_count INTEGER,
  images_total INTEGER,
  images_missing_alt_attr INTEGER,
  images_max_alt_length INTEGER,
  plugin_elements INTEGER,
  meta_fragment TEXT,
  amphtml TEXT,
  ajax_scheme_outlinks INTEGER,
  title_outside_head INTEGER CHECK (title_outside_head IN (0,1)),
  meta_description_outside_head INTEGER CHECK (meta_description_outside_head IN (0,1)),
  canonical_outside_head INTEGER CHECK (canonical_outside_head IN (0,1)),
  directives_outside_head INTEGER CHECK (directives_outside_head IN (0,1)),
  hreflang_outside_head INTEGER CHECK (hreflang_outside_head IN (0,1)),
  hreflang_json TEXT,
  -- The page's h1-h6 outline in DOM order: level, text and page region per
  -- heading. Nullable, because a scan written before headings were recorded
  -- in order has no outline to state, which is not an empty outline.
  heading_outline_json TEXT,
  -- Hrefs that could not become fetchable link targets, each {raw_href, reason}
  -- (issue #1530). Nullable: recording is opt-in
  -- (discovery.hyperlinks.record_invalid_targets), and a crawl run without it
  -- measured nothing here -- not the same as a page with no broken href.
  invalid_link_targets_json TEXT,
  -- The anchors this page wraps in an h1-h6, and the image links on it that
  -- name nothing. Nullable, because a scan written before link placement was
  -- recorded measured neither, which is not the same as finding none.
  link_placement_json TEXT,
  -- The page's authorship/date/article-scope markup signals (issue #823), as
  -- the parser's {author, dates, article} object. Nullable: a page whose body
  -- was never parsed, or a scan written before the signals were recorded, is
  -- unmeasured -- which is not the same as a measured empty object.
  trust_signals_json TEXT,
  -- Bounded repeated literal DOM ids (issue #828). NULL means the HTML body
  -- was not parsed or this older source did not record the observation.
  duplicate_ids_json TEXT,
  -- List mode's bounded canonical walk: the chain it inspected and the target it
  -- settled on. Nullable, because a run that never walked canonicals recorded neither.
  canonical_chain_json TEXT,
  final_canonical TEXT,
  head_count INTEGER NOT NULL,
  body_count INTEGER NOT NULL,
  head_not_first INTEGER NOT NULL CHECK (head_not_first IN (0,1)),
  invalid_head_elements TEXT NOT NULL,
  outlinks INTEGER NOT NULL,
  external_outlinks INTEGER NOT NULL,
  jsonld_blocks_found INTEGER NOT NULL,
  jsonld_blocks_parsed INTEGER NOT NULL,
  error TEXT NOT NULL,
  error_kind TEXT NOT NULL,
  cache_status TEXT NOT NULL,
  body_unavailable TEXT,
  representation TEXT NOT NULL CHECK (representation IN ('static','rendered','legacy_fragment')),
  redirect_chain_json TEXT NOT NULL,
  final_url TEXT NOT NULL
);
CREATE TABLE links (
  link_id INTEGER PRIMARY KEY,
  source_url_id INTEGER NOT NULL REFERENCES pages(url_id),
  destination_url_id INTEGER NOT NULL REFERENCES urls(url_id),
  source_document_id INTEGER REFERENCES documents(document_id),
  evidence_representation TEXT NOT NULL CHECK (evidence_representation IN ('static','rendered','legacy_fragment','legacy_unknown')),
  ordinal INTEGER NOT NULL,
  anchor TEXT NOT NULL,
  nofollow INTEGER NOT NULL CHECK (nofollow IN (0,1)),
  position TEXT NOT NULL,
  rel_json TEXT NOT NULL,
  target TEXT NOT NULL,
  raw_href TEXT NOT NULL,
  UNIQUE (source_url_id, evidence_representation, ordinal)
);
CREATE TABLE forms (
  form_id INTEGER PRIMARY KEY,
  page_url_id INTEGER NOT NULL REFERENCES pages(url_id),
  ordinal INTEGER NOT NULL,
  source_document_id INTEGER REFERENCES documents(document_id),
  evidence_representation TEXT NOT NULL CHECK (evidence_representation IN ('static','rendered','legacy_fragment','legacy_unknown')),
  method TEXT NOT NULL,
  action TEXT NOT NULL,
  has_password INTEGER NOT NULL CHECK (has_password IN (0,1)),
  UNIQUE (page_url_id, evidence_representation, ordinal)
);
CREATE TABLE decisions (
  decision_id INTEGER PRIMARY KEY,
  url TEXT NOT NULL,
  reason TEXT NOT NULL,
  source TEXT NOT NULL,
  depth INTEGER,
  occurrence_key TEXT NOT NULL UNIQUE
);
CREATE TABLE frontier (
  url_id INTEGER PRIMARY KEY REFERENCES urls(url_id),
  queue_ordinal INTEGER NOT NULL UNIQUE,
  depth INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('queued','inflight','done','excluded'))
);
CREATE TABLE query_variants (
  path_key TEXT NOT NULL,
  query_key TEXT NOT NULL,
  PRIMARY KEY (path_key, query_key)
);
CREATE TABLE resume_state (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  state_version TEXT NOT NULL CHECK (state_version = 'scan_resume.v1'),
  max_depth_reached INTEGER NOT NULL CHECK (max_depth_reached >= 0),
  elapsed_seconds REAL NOT NULL CHECK (elapsed_seconds >= 0),
  circuit_timeout_streak INTEGER NOT NULL CHECK (circuit_timeout_streak >= 0),
  circuit_server_error_streak INTEGER NOT NULL CHECK (circuit_server_error_streak >= 0),
  crawl_delay_applied REAL CHECK (crawl_delay_applied >= 0),
  throttle_state_json TEXT NOT NULL
);
CREATE TABLE context_items (
  kind TEXT NOT NULL,
  item_key TEXT NOT NULL,
  payload_version TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  completeness TEXT NOT NULL CHECK (completeness IN ('complete','partial','unavailable')),
  reason TEXT NOT NULL,
  PRIMARY KEY (kind, item_key)
);
CREATE TABLE resource_refs (
  resource_ref_id INTEGER PRIMARY KEY,
  page_url_id INTEGER NOT NULL REFERENCES pages(url_id),
  ordinal INTEGER NOT NULL,
  resource_url_id INTEGER NOT NULL REFERENCES urls(url_id),
  source_document_id INTEGER REFERENCES documents(document_id),
  kind TEXT NOT NULL CHECK (kind IN ('script','stylesheet')),
  representation TEXT NOT NULL,
  raw_url TEXT NOT NULL,
  response_id INTEGER REFERENCES responses(response_id),
  capture_state TEXT NOT NULL CHECK (capture_state IN ('measured','resources_disabled','not_fetched','excluded_scope','excluded_robots','resource_budget_exhausted','body_budget_exhausted','fetch_failed','body_unavailable')),
  reason TEXT NOT NULL,
  UNIQUE (page_url_id, representation, kind, ordinal)
);
CREATE TABLE audit (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  schema_version TEXT NOT NULL,
  evidence_revision INTEGER NOT NULL,
  analyzer_version TEXT NOT NULL,
  analyzer_revision TEXT NOT NULL,
  created_at TEXT NOT NULL,
  sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
  document_json TEXT NOT NULL
);
CREATE INDEX links_destination_source_position
  ON links(destination_url_id, source_url_id, position);
CREATE INDEX pages_status ON pages(status_code, url_id);
CREATE INDEX responses_url_variant ON responses(request_url_id, variant_key, request_ordinal);
CREATE INDEX documents_url_representation ON documents(url_id, representation, document_id);
CREATE INDEX frontier_work ON frontier(state, queue_ordinal);
CREATE INDEX resource_refs_resource ON resource_refs(resource_url_id, response_id);
