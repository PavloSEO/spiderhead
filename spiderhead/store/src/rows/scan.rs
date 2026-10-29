//! Row structs of the scan.v1 tables (and the scan.v2 retry tables).
//!
//! One struct per table, one field per column (`Option` for nullable columns, TEXT dates and JSON
//! kept as `String`, BLOB as `Vec<u8>`). Generated from the DDL in `sql/`; do not edit by hand.

/// Row of table `scan`.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanRow {
    /// Column `singleton`.
    pub singleton: i64,
    /// Column `scan_uuid`.
    pub scan_uuid: String,
    /// Column `format_version`.
    pub format_version: String,
    /// Column `evidence_version`.
    pub evidence_version: String,
    /// Column `writer_version`.
    pub writer_version: String,
    /// Column `writer_revision`.
    pub writer_revision: String,
    /// Column `runtime_versions_json`.
    pub runtime_versions_json: String,
    /// Column `created_at`.
    pub created_at: String,
    /// Column `finished_at`.
    pub finished_at: Option<String>,
    /// Column `source_kind`.
    pub source_kind: String,
    /// Column `parent_scan_uuid`.
    pub parent_scan_uuid: Option<String>,
    /// Column `start_url`.
    pub start_url: Option<String>,
    /// Column `config_json`.
    pub config_json: String,
    /// Column `config_fingerprint`.
    pub config_fingerprint: String,
    /// Column `lifecycle`.
    pub lifecycle: String,
    /// Column `finish_reason`.
    pub finish_reason: String,
    /// Column `crawl_partial`.
    pub crawl_partial: i64,
    /// Column `corpus_partial`.
    pub corpus_partial: i64,
    /// Column `evidence_revision`.
    pub evidence_revision: i64,
    /// Column `limitations_json`.
    pub limitations_json: String,
    /// Column `capabilities_json`.
    pub capabilities_json: String,
    /// Column `retention_json`.
    pub retention_json: String,
    /// Column `pinned`.
    pub pinned: i64,
}

impl ScanRow {
    /// Table name.
    pub const TABLE: &'static str = "scan";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "singleton, scan_uuid, format_version, evidence_version, writer_version, writer_revision, runtime_versions_json, created_at, finished_at, source_kind, parent_scan_uuid, start_url, config_json, config_fingerprint, lifecycle, finish_reason, crawl_partial, corpus_partial, evidence_revision, limitations_json, capabilities_json, retention_json, pinned";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            singleton: row.get(0)?,
            scan_uuid: row.get(1)?,
            format_version: row.get(2)?,
            evidence_version: row.get(3)?,
            writer_version: row.get(4)?,
            writer_revision: row.get(5)?,
            runtime_versions_json: row.get(6)?,
            created_at: row.get(7)?,
            finished_at: row.get(8)?,
            source_kind: row.get(9)?,
            parent_scan_uuid: row.get(10)?,
            start_url: row.get(11)?,
            config_json: row.get(12)?,
            config_fingerprint: row.get(13)?,
            lifecycle: row.get(14)?,
            finish_reason: row.get(15)?,
            crawl_partial: row.get(16)?,
            corpus_partial: row.get(17)?,
            evidence_revision: row.get(18)?,
            limitations_json: row.get(19)?,
            capabilities_json: row.get(20)?,
            retention_json: row.get(21)?,
            pinned: row.get(22)?,
        })
    }
}

/// Row of table `urls`.
#[derive(Debug, Clone, PartialEq)]
pub struct UrlsRow {
    /// Column `url_id`.
    pub url_id: i64,
    /// Column `url`.
    pub url: String,
}

impl UrlsRow {
    /// Table name.
    pub const TABLE: &'static str = "urls";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "url_id, url";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            url_id: row.get(0)?,
            url: row.get(1)?,
        })
    }
}

/// Row of table `bodies`.
#[derive(Debug, Clone, PartialEq)]
pub struct BodiesRow {
    /// Column `sha256`.
    pub sha256: String,
    /// Column `codec`.
    pub codec: String,
    /// Column `decoded_bytes`.
    pub decoded_bytes: i64,
    /// Column `stored_bytes`.
    pub stored_bytes: i64,
    /// Column `data`.
    pub data: Vec<u8>,
}

impl BodiesRow {
    /// Table name.
    pub const TABLE: &'static str = "bodies";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "sha256, codec, decoded_bytes, stored_bytes, data";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            sha256: row.get(0)?,
            codec: row.get(1)?,
            decoded_bytes: row.get(2)?,
            stored_bytes: row.get(3)?,
            data: row.get(4)?,
        })
    }
}

/// Row of table `responses`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResponsesRow {
    /// Column `response_id`.
    pub response_id: i64,
    /// Column `request_url_id`.
    pub request_url_id: i64,
    /// Column `request_ordinal`.
    pub request_ordinal: i64,
    /// Column `effective_url_id`.
    pub effective_url_id: Option<i64>,
    /// Column `redirect_chain_json`.
    pub redirect_chain_json: String,
    /// Column `method`.
    pub method: String,
    /// Column `purpose`.
    pub purpose: String,
    /// Column `requested_at`.
    pub requested_at: String,
    /// Column `received_at`.
    pub received_at: Option<String>,
    /// Column `request_headers_redacted_json`.
    pub request_headers_redacted_json: String,
    /// Column `credentials_used`.
    pub credentials_used: i64,
    /// Column `variant_key`.
    pub variant_key: String,
    /// Column `status_code`.
    pub status_code: Option<i64>,
    /// Column `effective_status_code`.
    pub effective_status_code: Option<i64>,
    /// Column `response_headers_redacted_json`.
    pub response_headers_redacted_json: String,
    /// Column `effective_headers_redacted_json`.
    pub effective_headers_redacted_json: String,
    /// Column `content_type`.
    pub content_type: String,
    /// Column `charset`.
    pub charset: String,
    /// Column `content_encoding`.
    pub content_encoding: String,
    /// Column `reported_size_bytes`.
    pub reported_size_bytes: Option<i64>,
    /// Column `response_time`.
    pub response_time: Option<f64>,
    /// Column `transport_source`.
    pub transport_source: String,
    /// Column `cache_status`.
    pub cache_status: String,
    /// Column `source_response_id`.
    pub source_response_id: Option<i64>,
    /// Column `body_sha256`.
    pub body_sha256: Option<String>,
    /// Column `body_fidelity`.
    pub body_fidelity: String,
    /// Column `body_state`.
    pub body_state: String,
    /// Column `body_reason`.
    pub body_reason: String,
    /// Column `error`.
    pub error: String,
    /// Column `error_kind`.
    pub error_kind: String,
}

impl ResponsesRow {
    /// Table name.
    pub const TABLE: &'static str = "responses";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "response_id, request_url_id, request_ordinal, effective_url_id, redirect_chain_json, method, purpose, requested_at, received_at, request_headers_redacted_json, credentials_used, variant_key, status_code, effective_status_code, response_headers_redacted_json, effective_headers_redacted_json, content_type, charset, content_encoding, reported_size_bytes, response_time, transport_source, cache_status, source_response_id, body_sha256, body_fidelity, body_state, body_reason, error, error_kind";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            response_id: row.get(0)?,
            request_url_id: row.get(1)?,
            request_ordinal: row.get(2)?,
            effective_url_id: row.get(3)?,
            redirect_chain_json: row.get(4)?,
            method: row.get(5)?,
            purpose: row.get(6)?,
            requested_at: row.get(7)?,
            received_at: row.get(8)?,
            request_headers_redacted_json: row.get(9)?,
            credentials_used: row.get(10)?,
            variant_key: row.get(11)?,
            status_code: row.get(12)?,
            effective_status_code: row.get(13)?,
            response_headers_redacted_json: row.get(14)?,
            effective_headers_redacted_json: row.get(15)?,
            content_type: row.get(16)?,
            charset: row.get(17)?,
            content_encoding: row.get(18)?,
            reported_size_bytes: row.get(19)?,
            response_time: row.get(20)?,
            transport_source: row.get(21)?,
            cache_status: row.get(22)?,
            source_response_id: row.get(23)?,
            body_sha256: row.get(24)?,
            body_fidelity: row.get(25)?,
            body_state: row.get(26)?,
            body_reason: row.get(27)?,
            error: row.get(28)?,
            error_kind: row.get(29)?,
        })
    }
}

/// Row of table `documents`.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentsRow {
    /// Column `document_id`.
    pub document_id: i64,
    /// Column `url_id`.
    pub url_id: i64,
    /// Column `representation`.
    pub representation: String,
    /// Column `source_response_id`.
    pub source_response_id: Option<i64>,
    /// Column `body_sha256`.
    pub body_sha256: Option<String>,
    /// Column `captured_at`.
    pub captured_at: String,
    /// Column `decoder_version`.
    pub decoder_version: String,
    /// Column `decoder_source`.
    pub decoder_source: String,
    /// Column `decoder_charset`.
    pub decoder_charset: String,
    /// Column `decoder_errors`.
    pub decoder_errors: String,
    /// Column `fidelity`.
    pub fidelity: String,
    /// Column `body_state`.
    pub body_state: String,
    /// Column `body_reason`.
    pub body_reason: String,
    /// Column `renderer_json`.
    pub renderer_json: String,
}

impl DocumentsRow {
    /// Table name.
    pub const TABLE: &'static str = "documents";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "document_id, url_id, representation, source_response_id, body_sha256, captured_at, decoder_version, decoder_source, decoder_charset, decoder_errors, fidelity, body_state, body_reason, renderer_json";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            document_id: row.get(0)?,
            url_id: row.get(1)?,
            representation: row.get(2)?,
            source_response_id: row.get(3)?,
            body_sha256: row.get(4)?,
            captured_at: row.get(5)?,
            decoder_version: row.get(6)?,
            decoder_source: row.get(7)?,
            decoder_charset: row.get(8)?,
            decoder_errors: row.get(9)?,
            fidelity: row.get(10)?,
            body_state: row.get(11)?,
            body_reason: row.get(12)?,
            renderer_json: row.get(13)?,
        })
    }
}

/// Row of table `pages`.
#[derive(Debug, Clone, PartialEq)]
pub struct PagesRow {
    /// Column `url_id`.
    pub url_id: i64,
    /// Column `page_ordinal`.
    pub page_ordinal: i64,
    /// Column `document_id`.
    pub document_id: Option<i64>,
    /// Column `status_code`.
    pub status_code: Option<i64>,
    /// Column `content_type`.
    pub content_type: String,
    /// Column `size_bytes`.
    pub size_bytes: i64,
    /// Column `response_time`.
    pub response_time: Option<f64>,
    /// Column `redirect_url`.
    pub redirect_url: String,
    /// Column `title`.
    pub title: String,
    /// Column `meta_description`.
    pub meta_description: String,
    /// Column `h1`.
    pub h1: String,
    /// Column `h1_2`.
    pub h1_2: String,
    /// Column `h2`.
    pub h2: String,
    /// Column `canonical`.
    pub canonical: String,
    /// Column `meta_robots`.
    pub meta_robots: String,
    /// Column `x_robots`.
    pub x_robots: String,
    /// Column `og_title`.
    pub og_title: String,
    /// Column `og_description`.
    pub og_description: String,
    /// Column `og_image`.
    pub og_image: String,
    /// Column `og_url`.
    pub og_url: Option<String>,
    /// Column `mobile_alternate_broken`.
    pub mobile_alternate_broken: Option<i64>,
    /// Column `word_count`.
    pub word_count: i64,
    /// Column `text_ratio`.
    pub text_ratio: Option<f64>,
    /// Column `content_frames`.
    pub content_frames: Option<i64>,
    /// Column `content_frames_same_origin`.
    pub content_frames_same_origin: Option<i64>,
    /// Column `crawl_depth`.
    pub crawl_depth: i64,
    /// Column `content_encoding`.
    pub content_encoding: String,
    /// Column `charset`.
    pub charset: String,
    /// Column `doctype`.
    pub doctype: String,
    /// Column `viewport`.
    pub viewport: String,
    /// Column `meta_refresh`.
    pub meta_refresh: Option<String>,
    /// Column `http_refresh`.
    pub http_refresh: Option<String>,
    /// Column `meta_description_count`.
    pub meta_description_count: Option<i64>,
    /// Column `h1_alt_text`.
    pub h1_alt_text: Option<String>,
    /// Column `lorem_ipsum_count`.
    pub lorem_ipsum_count: Option<i64>,
    /// Column `images_total`.
    pub images_total: Option<i64>,
    /// Column `images_missing_alt_attr`.
    pub images_missing_alt_attr: Option<i64>,
    /// Column `images_max_alt_length`.
    pub images_max_alt_length: Option<i64>,
    /// Column `plugin_elements`.
    pub plugin_elements: Option<i64>,
    /// Column `meta_fragment`.
    pub meta_fragment: Option<String>,
    /// Column `amphtml`.
    pub amphtml: Option<String>,
    /// Column `ajax_scheme_outlinks`.
    pub ajax_scheme_outlinks: Option<i64>,
    /// Column `title_outside_head`.
    pub title_outside_head: Option<i64>,
    /// Column `meta_description_outside_head`.
    pub meta_description_outside_head: Option<i64>,
    /// Column `canonical_outside_head`.
    pub canonical_outside_head: Option<i64>,
    /// Column `directives_outside_head`.
    pub directives_outside_head: Option<i64>,
    /// Column `hreflang_outside_head`.
    pub hreflang_outside_head: Option<i64>,
    /// Column `hreflang_json`.
    pub hreflang_json: Option<String>,
    /// Column `heading_outline_json`.
    pub heading_outline_json: Option<String>,
    /// Column `transport_upgraded`.
    pub transport_upgraded: i64,
    /// Column `sentence_count`.
    pub sentence_count: Option<i64>,
    /// Column `avg_words_per_sentence`.
    pub avg_words_per_sentence: Option<f64>,
    /// Column `flesch`.
    pub flesch: Option<f64>,
    /// Column `readability`.
    pub readability: Option<String>,
    /// Column `invalid_link_targets_json`.
    pub invalid_link_targets_json: Option<String>,
    /// Column `link_placement_json`.
    pub link_placement_json: Option<String>,
    /// Column `trust_signals_json`.
    pub trust_signals_json: Option<String>,
    /// Column `duplicate_ids_json`.
    pub duplicate_ids_json: Option<String>,
    /// Column `canonical_chain_json`.
    pub canonical_chain_json: Option<String>,
    /// Column `final_canonical`.
    pub final_canonical: Option<String>,
    /// Column `head_count`.
    pub head_count: i64,
    /// Column `body_count`.
    pub body_count: i64,
    /// Column `head_not_first`.
    pub head_not_first: i64,
    /// Column `invalid_head_elements`.
    pub invalid_head_elements: String,
    /// Column `outlinks`.
    pub outlinks: i64,
    /// Column `external_outlinks`.
    pub external_outlinks: i64,
    /// Column `jsonld_blocks_found`.
    pub jsonld_blocks_found: i64,
    /// Column `jsonld_blocks_parsed`.
    pub jsonld_blocks_parsed: i64,
    /// Column `error`.
    pub error: String,
    /// Column `error_kind`.
    pub error_kind: String,
    /// Column `cache_status`.
    pub cache_status: String,
    /// Column `body_unavailable`.
    pub body_unavailable: Option<String>,
    /// Column `representation`.
    pub representation: String,
    /// Column `redirect_chain_json`.
    pub redirect_chain_json: String,
    /// Column `final_url`.
    pub final_url: String,
}

impl PagesRow {
    /// Table name.
    pub const TABLE: &'static str = "pages";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "url_id, page_ordinal, document_id, status_code, content_type, size_bytes, response_time, redirect_url, title, meta_description, h1, h1_2, h2, canonical, meta_robots, x_robots, og_title, og_description, og_image, og_url, mobile_alternate_broken, word_count, text_ratio, content_frames, content_frames_same_origin, crawl_depth, content_encoding, charset, doctype, viewport, meta_refresh, http_refresh, meta_description_count, h1_alt_text, lorem_ipsum_count, images_total, images_missing_alt_attr, images_max_alt_length, plugin_elements, meta_fragment, amphtml, ajax_scheme_outlinks, title_outside_head, meta_description_outside_head, canonical_outside_head, directives_outside_head, hreflang_outside_head, hreflang_json, heading_outline_json, transport_upgraded, sentence_count, avg_words_per_sentence, flesch, readability, invalid_link_targets_json, link_placement_json, trust_signals_json, duplicate_ids_json, canonical_chain_json, final_canonical, head_count, body_count, head_not_first, invalid_head_elements, outlinks, external_outlinks, jsonld_blocks_found, jsonld_blocks_parsed, error, error_kind, cache_status, body_unavailable, representation, redirect_chain_json, final_url";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            url_id: row.get(0)?,
            page_ordinal: row.get(1)?,
            document_id: row.get(2)?,
            status_code: row.get(3)?,
            content_type: row.get(4)?,
            size_bytes: row.get(5)?,
            response_time: row.get(6)?,
            redirect_url: row.get(7)?,
            title: row.get(8)?,
            meta_description: row.get(9)?,
            h1: row.get(10)?,
            h1_2: row.get(11)?,
            h2: row.get(12)?,
            canonical: row.get(13)?,
            meta_robots: row.get(14)?,
            x_robots: row.get(15)?,
            og_title: row.get(16)?,
            og_description: row.get(17)?,
            og_image: row.get(18)?,
            og_url: row.get(19)?,
            mobile_alternate_broken: row.get(20)?,
            word_count: row.get(21)?,
            text_ratio: row.get(22)?,
            content_frames: row.get(23)?,
            content_frames_same_origin: row.get(24)?,
            crawl_depth: row.get(25)?,
            content_encoding: row.get(26)?,
            charset: row.get(27)?,
            doctype: row.get(28)?,
            viewport: row.get(29)?,
            meta_refresh: row.get(30)?,
            http_refresh: row.get(31)?,
            meta_description_count: row.get(32)?,
            h1_alt_text: row.get(33)?,
            lorem_ipsum_count: row.get(34)?,
            images_total: row.get(35)?,
            images_missing_alt_attr: row.get(36)?,
            images_max_alt_length: row.get(37)?,
            plugin_elements: row.get(38)?,
            meta_fragment: row.get(39)?,
            amphtml: row.get(40)?,
            ajax_scheme_outlinks: row.get(41)?,
            title_outside_head: row.get(42)?,
            meta_description_outside_head: row.get(43)?,
            canonical_outside_head: row.get(44)?,
            directives_outside_head: row.get(45)?,
            hreflang_outside_head: row.get(46)?,
            hreflang_json: row.get(47)?,
            heading_outline_json: row.get(48)?,
            transport_upgraded: row.get(49)?,
            sentence_count: row.get(50)?,
            avg_words_per_sentence: row.get(51)?,
            flesch: row.get(52)?,
            readability: row.get(53)?,
            invalid_link_targets_json: row.get(54)?,
            link_placement_json: row.get(55)?,
            trust_signals_json: row.get(56)?,
            duplicate_ids_json: row.get(57)?,
            canonical_chain_json: row.get(58)?,
            final_canonical: row.get(59)?,
            head_count: row.get(60)?,
            body_count: row.get(61)?,
            head_not_first: row.get(62)?,
            invalid_head_elements: row.get(63)?,
            outlinks: row.get(64)?,
            external_outlinks: row.get(65)?,
            jsonld_blocks_found: row.get(66)?,
            jsonld_blocks_parsed: row.get(67)?,
            error: row.get(68)?,
            error_kind: row.get(69)?,
            cache_status: row.get(70)?,
            body_unavailable: row.get(71)?,
            representation: row.get(72)?,
            redirect_chain_json: row.get(73)?,
            final_url: row.get(74)?,
        })
    }
}

/// Row of table `links`.
#[derive(Debug, Clone, PartialEq)]
pub struct LinksRow {
    /// Column `link_id`.
    pub link_id: i64,
    /// Column `source_url_id`.
    pub source_url_id: i64,
    /// Column `destination_url_id`.
    pub destination_url_id: i64,
    /// Column `source_document_id`.
    pub source_document_id: Option<i64>,
    /// Column `evidence_representation`.
    pub evidence_representation: String,
    /// Column `ordinal`.
    pub ordinal: i64,
    /// Column `anchor`.
    pub anchor: String,
    /// Column `anchor_source`.
    pub anchor_source: String,
    /// Column `nofollow`.
    pub nofollow: i64,
    /// Column `position`.
    pub position: String,
    /// Column `rel_json`.
    pub rel_json: String,
    /// Column `target`.
    pub target: String,
    /// Column `raw_href`.
    pub raw_href: String,
}

impl LinksRow {
    /// Table name.
    pub const TABLE: &'static str = "links";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "link_id, source_url_id, destination_url_id, source_document_id, evidence_representation, ordinal, anchor, anchor_source, nofollow, position, rel_json, target, raw_href";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            link_id: row.get(0)?,
            source_url_id: row.get(1)?,
            destination_url_id: row.get(2)?,
            source_document_id: row.get(3)?,
            evidence_representation: row.get(4)?,
            ordinal: row.get(5)?,
            anchor: row.get(6)?,
            anchor_source: row.get(7)?,
            nofollow: row.get(8)?,
            position: row.get(9)?,
            rel_json: row.get(10)?,
            target: row.get(11)?,
            raw_href: row.get(12)?,
        })
    }
}

/// Row of table `forms`.
#[derive(Debug, Clone, PartialEq)]
pub struct FormsRow {
    /// Column `form_id`.
    pub form_id: i64,
    /// Column `page_url_id`.
    pub page_url_id: i64,
    /// Column `ordinal`.
    pub ordinal: i64,
    /// Column `source_document_id`.
    pub source_document_id: Option<i64>,
    /// Column `evidence_representation`.
    pub evidence_representation: String,
    /// Column `method`.
    pub method: String,
    /// Column `action`.
    pub action: String,
    /// Column `has_password`.
    pub has_password: i64,
}

impl FormsRow {
    /// Table name.
    pub const TABLE: &'static str = "forms";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "form_id, page_url_id, ordinal, source_document_id, evidence_representation, method, action, has_password";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            form_id: row.get(0)?,
            page_url_id: row.get(1)?,
            ordinal: row.get(2)?,
            source_document_id: row.get(3)?,
            evidence_representation: row.get(4)?,
            method: row.get(5)?,
            action: row.get(6)?,
            has_password: row.get(7)?,
        })
    }
}

/// Row of table `decisions`.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionsRow {
    /// Column `decision_id`.
    pub decision_id: i64,
    /// Column `url`.
    pub url: String,
    /// Column `reason`.
    pub reason: String,
    /// Column `source`.
    pub source: String,
    /// Column `depth`.
    pub depth: Option<i64>,
    /// Column `occurrence_key`.
    pub occurrence_key: String,
}

impl DecisionsRow {
    /// Table name.
    pub const TABLE: &'static str = "decisions";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "decision_id, url, reason, source, depth, occurrence_key";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            decision_id: row.get(0)?,
            url: row.get(1)?,
            reason: row.get(2)?,
            source: row.get(3)?,
            depth: row.get(4)?,
            occurrence_key: row.get(5)?,
        })
    }
}

/// Row of table `frontier`.
#[derive(Debug, Clone, PartialEq)]
pub struct FrontierRow {
    /// Column `url_id`.
    pub url_id: i64,
    /// Column `queue_ordinal`.
    pub queue_ordinal: i64,
    /// Column `depth`.
    pub depth: i64,
    /// Column `state`.
    pub state: String,
}

impl FrontierRow {
    /// Table name.
    pub const TABLE: &'static str = "frontier";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "url_id, queue_ordinal, depth, state";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            url_id: row.get(0)?,
            queue_ordinal: row.get(1)?,
            depth: row.get(2)?,
            state: row.get(3)?,
        })
    }
}

/// Row of table `query_variants`.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryVariantsRow {
    /// Column `path_key`.
    pub path_key: String,
    /// Column `query_key`.
    pub query_key: String,
}

impl QueryVariantsRow {
    /// Table name.
    pub const TABLE: &'static str = "query_variants";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "path_key, query_key";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            path_key: row.get(0)?,
            query_key: row.get(1)?,
        })
    }
}

/// Row of table `resume_state`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResumeStateRow {
    /// Column `singleton`.
    pub singleton: i64,
    /// Column `state_version`.
    pub state_version: String,
    /// Column `max_depth_reached`.
    pub max_depth_reached: i64,
    /// Column `elapsed_seconds`.
    pub elapsed_seconds: f64,
    /// Column `circuit_timeout_streak`.
    pub circuit_timeout_streak: i64,
    /// Column `circuit_server_error_streak`.
    pub circuit_server_error_streak: i64,
    /// Column `crawl_delay_applied`.
    pub crawl_delay_applied: Option<f64>,
    /// Column `throttle_state_json`.
    pub throttle_state_json: String,
}

impl ResumeStateRow {
    /// Table name.
    pub const TABLE: &'static str = "resume_state";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "singleton, state_version, max_depth_reached, elapsed_seconds, circuit_timeout_streak, circuit_server_error_streak, crawl_delay_applied, throttle_state_json";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            singleton: row.get(0)?,
            state_version: row.get(1)?,
            max_depth_reached: row.get(2)?,
            elapsed_seconds: row.get(3)?,
            circuit_timeout_streak: row.get(4)?,
            circuit_server_error_streak: row.get(5)?,
            crawl_delay_applied: row.get(6)?,
            throttle_state_json: row.get(7)?,
        })
    }
}

/// Row of table `context_items`.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextItemsRow {
    /// Column `kind`.
    pub kind: String,
    /// Column `item_key`.
    pub item_key: String,
    /// Column `payload_version`.
    pub payload_version: String,
    /// Column `payload_json`.
    pub payload_json: String,
    /// Column `completeness`.
    pub completeness: String,
    /// Column `reason`.
    pub reason: String,
}

impl ContextItemsRow {
    /// Table name.
    pub const TABLE: &'static str = "context_items";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str =
        "kind, item_key, payload_version, payload_json, completeness, reason";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            kind: row.get(0)?,
            item_key: row.get(1)?,
            payload_version: row.get(2)?,
            payload_json: row.get(3)?,
            completeness: row.get(4)?,
            reason: row.get(5)?,
        })
    }
}

/// Row of table `resource_refs`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceRefsRow {
    /// Column `resource_ref_id`.
    pub resource_ref_id: i64,
    /// Column `page_url_id`.
    pub page_url_id: i64,
    /// Column `ordinal`.
    pub ordinal: i64,
    /// Column `resource_url_id`.
    pub resource_url_id: i64,
    /// Column `source_document_id`.
    pub source_document_id: Option<i64>,
    /// Column `kind`.
    pub kind: String,
    /// Column `representation`.
    pub representation: String,
    /// Column `raw_url`.
    pub raw_url: String,
    /// Column `response_id`.
    pub response_id: Option<i64>,
    /// Column `capture_state`.
    pub capture_state: String,
    /// Column `reason`.
    pub reason: String,
}

impl ResourceRefsRow {
    /// Table name.
    pub const TABLE: &'static str = "resource_refs";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "resource_ref_id, page_url_id, ordinal, resource_url_id, source_document_id, kind, representation, raw_url, response_id, capture_state, reason";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            resource_ref_id: row.get(0)?,
            page_url_id: row.get(1)?,
            ordinal: row.get(2)?,
            resource_url_id: row.get(3)?,
            source_document_id: row.get(4)?,
            kind: row.get(5)?,
            representation: row.get(6)?,
            raw_url: row.get(7)?,
            response_id: row.get(8)?,
            capture_state: row.get(9)?,
            reason: row.get(10)?,
        })
    }
}

/// Row of table `audit`.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditRow {
    /// Column `singleton`.
    pub singleton: i64,
    /// Column `schema_version`.
    pub schema_version: String,
    /// Column `evidence_revision`.
    pub evidence_revision: i64,
    /// Column `analyzer_version`.
    pub analyzer_version: String,
    /// Column `analyzer_revision`.
    pub analyzer_revision: String,
    /// Column `created_at`.
    pub created_at: String,
    /// Column `sha256`.
    pub sha256: String,
    /// Column `document_json`.
    pub document_json: String,
}

impl AuditRow {
    /// Table name.
    pub const TABLE: &'static str = "audit";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "singleton, schema_version, evidence_revision, analyzer_version, analyzer_revision, created_at, sha256, document_json";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            singleton: row.get(0)?,
            schema_version: row.get(1)?,
            evidence_revision: row.get(2)?,
            analyzer_version: row.get(3)?,
            analyzer_revision: row.get(4)?,
            created_at: row.get(5)?,
            sha256: row.get(6)?,
            document_json: row.get(7)?,
        })
    }
}

/// Row of table `retry_attempts`.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryAttemptsRow {
    /// Column `attempt_id`.
    pub attempt_id: i64,
    /// Column `attempt_uuid`.
    pub attempt_uuid: String,
    /// Column `operation`.
    pub operation: String,
    /// Column `requested_at`.
    pub requested_at: String,
    /// Column `where_json`.
    pub where_json: String,
    /// Column `backup_path`.
    pub backup_path: String,
    /// Column `backup_sha256`.
    pub backup_sha256: String,
    /// Column `source_scan_uuid`.
    pub source_scan_uuid: Option<String>,
}

impl RetryAttemptsRow {
    /// Table name.
    pub const TABLE: &'static str = "retry_attempts";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "attempt_id, attempt_uuid, operation, requested_at, where_json, backup_path, backup_sha256, source_scan_uuid";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            attempt_id: row.get(0)?,
            attempt_uuid: row.get(1)?,
            operation: row.get(2)?,
            requested_at: row.get(3)?,
            where_json: row.get(4)?,
            backup_path: row.get(5)?,
            backup_sha256: row.get(6)?,
            source_scan_uuid: row.get(7)?,
        })
    }
}

/// Row of table `retry_transitions`.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryTransitionsRow {
    /// Column `attempt_id`.
    pub attempt_id: i64,
    /// Column `url_id`.
    pub url_id: i64,
    /// Column `url`.
    pub url: String,
    /// Column `from_frontier_state`.
    pub from_frontier_state: String,
    /// Column `to_frontier_state`.
    pub to_frontier_state: String,
    /// Column `prior_page_json`.
    pub prior_page_json: Option<String>,
    /// Column `prior_document_id`.
    pub prior_document_id: Option<i64>,
    /// Column `prior_evidence_sha256`.
    pub prior_evidence_sha256: Option<String>,
    /// Column `removed_links`.
    pub removed_links: i64,
    /// Column `removed_forms`.
    pub removed_forms: i64,
    /// Column `removed_resource_refs`.
    pub removed_resource_refs: i64,
    /// Column `removed_contexts`.
    pub removed_contexts: i64,
}

impl RetryTransitionsRow {
    /// Table name.
    pub const TABLE: &'static str = "retry_transitions";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "attempt_id, url_id, url, from_frontier_state, to_frontier_state, prior_page_json, prior_document_id, prior_evidence_sha256, removed_links, removed_forms, removed_resource_refs, removed_contexts";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            attempt_id: row.get(0)?,
            url_id: row.get(1)?,
            url: row.get(2)?,
            from_frontier_state: row.get(3)?,
            to_frontier_state: row.get(4)?,
            prior_page_json: row.get(5)?,
            prior_document_id: row.get(6)?,
            prior_evidence_sha256: row.get(7)?,
            removed_links: row.get(8)?,
            removed_forms: row.get(9)?,
            removed_resource_refs: row.get(10)?,
            removed_contexts: row.get(11)?,
        })
    }
}

/// Row of table `resource_graph_occurrences`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceGraphOccurrencesRow {
    /// Column `occurrence_id`.
    pub occurrence_id: i64,
    /// Column `page_url_id`.
    pub page_url_id: i64,
    /// Column `source_document_id`.
    pub source_document_id: i64,
    /// Column `representation`.
    pub representation: String,
    /// Column `ordinal`.
    pub ordinal: i64,
    /// Column `kind`.
    pub kind: String,
    /// Column `carrier`.
    pub carrier: String,
    /// Column `raw_url`.
    pub raw_url: String,
    /// Column `resolved_url`.
    pub resolved_url: String,
    /// Column `integrity`.
    pub integrity: Option<String>,
    /// Column `integrity_state`.
    pub integrity_state: String,
    /// Column `nesting_depth`.
    pub nesting_depth: i64,
    /// Column `state`.
    pub state: String,
    /// Column `reason`.
    pub reason: String,
}

impl ResourceGraphOccurrencesRow {
    /// Table name.
    pub const TABLE: &'static str = "resource_graph_occurrences";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "occurrence_id, page_url_id, source_document_id, representation, ordinal, kind, carrier, raw_url, resolved_url, integrity, integrity_state, nesting_depth, state, reason";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            occurrence_id: row.get(0)?,
            page_url_id: row.get(1)?,
            source_document_id: row.get(2)?,
            representation: row.get(3)?,
            ordinal: row.get(4)?,
            kind: row.get(5)?,
            carrier: row.get(6)?,
            raw_url: row.get(7)?,
            resolved_url: row.get(8)?,
            integrity: row.get(9)?,
            integrity_state: row.get(10)?,
            nesting_depth: row.get(11)?,
            state: row.get(12)?,
            reason: row.get(13)?,
        })
    }
}

/// Row of table `resource_graph_fetches`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceGraphFetchesRow {
    /// Column `resolved_url`.
    pub resolved_url: String,
    /// Column `state`.
    pub state: String,
    /// Column `reason`.
    pub reason: String,
    /// Column `status_code`.
    pub status_code: Option<i64>,
    /// Column `content_type`.
    pub content_type: String,
    /// Column `bytes_received`.
    pub bytes_received: i64,
    /// Column `elapsed_seconds`.
    pub elapsed_seconds: Option<f64>,
    /// Column `origin_host`.
    pub origin_host: String,
    /// Column `redirects`.
    pub redirects: i64,
    /// Column `nesting_depth`.
    pub nesting_depth: i64,
    /// Column `final_url`.
    pub final_url: String,
    /// Column `compression`.
    pub compression: String,
    /// Column `cache_state`.
    pub cache_state: String,
    /// Column `integrity_state`.
    pub integrity_state: String,
    /// Column `width`.
    pub width: Option<i64>,
    /// Column `height`.
    pub height: Option<i64>,
    /// Column `body_state`.
    pub body_state: String,
}

impl ResourceGraphFetchesRow {
    /// Table name.
    pub const TABLE: &'static str = "resource_graph_fetches";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "resolved_url, state, reason, status_code, content_type, bytes_received, elapsed_seconds, origin_host, redirects, nesting_depth, final_url, compression, cache_state, integrity_state, width, height, body_state";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            resolved_url: row.get(0)?,
            state: row.get(1)?,
            reason: row.get(2)?,
            status_code: row.get(3)?,
            content_type: row.get(4)?,
            bytes_received: row.get(5)?,
            elapsed_seconds: row.get(6)?,
            origin_host: row.get(7)?,
            redirects: row.get(8)?,
            nesting_depth: row.get(9)?,
            final_url: row.get(10)?,
            compression: row.get(11)?,
            cache_state: row.get(12)?,
            integrity_state: row.get(13)?,
            width: row.get(14)?,
            height: row.get(15)?,
            body_state: row.get(16)?,
        })
    }
}

/// Row of table `discovery_occurrences`.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveryOccurrencesRow {
    /// Column `occurrence_key`.
    pub occurrence_key: String,
    /// Column `source_kind`.
    pub source_kind: String,
    /// Column `relation`.
    pub relation: String,
    /// Column `source_url_id`.
    pub source_url_id: Option<i64>,
    /// Column `source_document_id`.
    pub source_document_id: Option<i64>,
    /// Column `source_response_id`.
    pub source_response_id: Option<i64>,
    /// Column `representation`.
    pub representation: String,
    /// Column `carrier`.
    pub carrier: String,
    /// Column `raw_value`.
    pub raw_value: String,
    /// Column `resolved_value`.
    pub resolved_value: String,
    /// Column `target_url_id`.
    pub target_url_id: Option<i64>,
    /// Column `depth`.
    pub depth: Option<i64>,
    /// Column `outcome`.
    pub outcome: String,
    /// Column `reason`.
    pub reason: String,
    /// Column `attributes_json`.
    pub attributes_json: String,
}

impl DiscoveryOccurrencesRow {
    /// Table name.
    pub const TABLE: &'static str = "discovery_occurrences";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "occurrence_key, source_kind, relation, source_url_id, source_document_id, source_response_id, representation, carrier, raw_value, resolved_value, target_url_id, depth, outcome, reason, attributes_json";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            occurrence_key: row.get(0)?,
            source_kind: row.get(1)?,
            relation: row.get(2)?,
            source_url_id: row.get(3)?,
            source_document_id: row.get(4)?,
            source_response_id: row.get(5)?,
            representation: row.get(6)?,
            carrier: row.get(7)?,
            raw_value: row.get(8)?,
            resolved_value: row.get(9)?,
            target_url_id: row.get(10)?,
            depth: row.get(11)?,
            outcome: row.get(12)?,
            reason: row.get(13)?,
            attributes_json: row.get(14)?,
        })
    }
}

/// Row of table `discovery_ledger_coverage`.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveryLedgerCoverageRow {
    /// Column `source_document_id`.
    pub source_document_id: i64,
    /// Column `representation`.
    pub representation: String,
    /// Column `captured`.
    pub captured: i64,
    /// Column `omitted`.
    pub omitted: i64,
    /// Column `state`.
    pub state: String,
    /// Column `reason`.
    pub reason: String,
}

impl DiscoveryLedgerCoverageRow {
    /// Table name.
    pub const TABLE: &'static str = "discovery_ledger_coverage";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str =
        "source_document_id, representation, captured, omitted, state, reason";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            source_document_id: row.get(0)?,
            representation: row.get(1)?,
            captured: row.get(2)?,
            omitted: row.get(3)?,
            state: row.get(4)?,
            reason: row.get(5)?,
        })
    }
}

/// Row of table `response_transport_meta`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseTransportMetaRow {
    /// Column `response_id`.
    pub response_id: i64,
    /// Column `protocol`.
    pub protocol: Option<String>,
    /// Column `protocol_state`.
    pub protocol_state: String,
    /// Column `total_seconds`.
    pub total_seconds: Option<f64>,
    /// Column `dns_seconds`.
    pub dns_seconds: Option<f64>,
    /// Column `dns_state`.
    pub dns_state: String,
    /// Column `dns_reason`.
    pub dns_reason: String,
    /// Column `connect_seconds`.
    pub connect_seconds: Option<f64>,
    /// Column `connect_state`.
    pub connect_state: String,
    /// Column `connect_reason`.
    pub connect_reason: String,
    /// Column `tls_seconds`.
    pub tls_seconds: Option<f64>,
    /// Column `tls_state`.
    pub tls_state: String,
    /// Column `tls_reason`.
    pub tls_reason: String,
    /// Column `ttfb_seconds`.
    pub ttfb_seconds: Option<f64>,
    /// Column `ttfb_state`.
    pub ttfb_state: String,
    /// Column `ttfb_reason`.
    pub ttfb_reason: String,
    /// Column `download_seconds`.
    pub download_seconds: Option<f64>,
    /// Column `download_state`.
    pub download_state: String,
    /// Column `download_reason`.
    pub download_reason: String,
}

impl ResponseTransportMetaRow {
    /// Table name.
    pub const TABLE: &'static str = "response_transport_meta";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "response_id, protocol, protocol_state, total_seconds, dns_seconds, dns_state, dns_reason, connect_seconds, connect_state, connect_reason, tls_seconds, tls_state, tls_reason, ttfb_seconds, ttfb_state, ttfb_reason, download_seconds, download_state, download_reason";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            response_id: row.get(0)?,
            protocol: row.get(1)?,
            protocol_state: row.get(2)?,
            total_seconds: row.get(3)?,
            dns_seconds: row.get(4)?,
            dns_state: row.get(5)?,
            dns_reason: row.get(6)?,
            connect_seconds: row.get(7)?,
            connect_state: row.get(8)?,
            connect_reason: row.get(9)?,
            tls_seconds: row.get(10)?,
            tls_state: row.get(11)?,
            tls_reason: row.get(12)?,
            ttfb_seconds: row.get(13)?,
            ttfb_state: row.get(14)?,
            ttfb_reason: row.get(15)?,
            download_seconds: row.get(16)?,
            download_state: row.get(17)?,
            download_reason: row.get(18)?,
        })
    }
}

/// Row of table `scan_events`.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanEventsRow {
    /// Column `sequence`.
    pub sequence: i64,
    /// Column `event_type`.
    pub event_type: String,
    /// Column `occurred_at`.
    pub occurred_at: Option<String>,
    /// Column `timestamp_state`.
    pub timestamp_state: String,
    /// Column `payload_json`.
    pub payload_json: String,
}

impl ScanEventsRow {
    /// Table name.
    pub const TABLE: &'static str = "scan_events";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str =
        "sequence, event_type, occurred_at, timestamp_state, payload_json";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            sequence: row.get(0)?,
            event_type: row.get(1)?,
            occurred_at: row.get(2)?,
            timestamp_state: row.get(3)?,
            payload_json: row.get(4)?,
        })
    }
}

/// Row of table `scan_event_meta`.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanEventMetaRow {
    /// Column `singleton`.
    pub singleton: i64,
    /// Column `cap`.
    pub cap: i64,
    /// Column `captured`.
    pub captured: i64,
    /// Column `dropped`.
    pub dropped: i64,
}

impl ScanEventMetaRow {
    /// Table name.
    pub const TABLE: &'static str = "scan_event_meta";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "singleton, cap, captured, dropped";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            singleton: row.get(0)?,
            cap: row.get(1)?,
            captured: row.get(2)?,
            dropped: row.get(3)?,
        })
    }
}

/// Row of table `external_checks`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalChecksRow {
    /// Column `ordinal`.
    pub ordinal: i64,
    /// Column `outcome`.
    pub outcome: String,
    /// Column `payload_json`.
    pub payload_json: String,
}

impl ExternalChecksRow {
    /// Table name.
    pub const TABLE: &'static str = "external_checks";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "ordinal, outcome, payload_json";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            ordinal: row.get(0)?,
            outcome: row.get(1)?,
            payload_json: row.get(2)?,
        })
    }
}

/// Row of table `external_check_summary`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalCheckSummaryRow {
    /// Column `singleton`.
    pub singleton: i64,
    /// Column `payload_json`.
    pub payload_json: String,
}

impl ExternalCheckSummaryRow {
    /// Table name.
    pub const TABLE: &'static str = "external_check_summary";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "singleton, payload_json";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            singleton: row.get(0)?,
            payload_json: row.get(1)?,
        })
    }
}
