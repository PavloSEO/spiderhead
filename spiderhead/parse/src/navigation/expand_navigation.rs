//! Expands stored navigation evidence into the bounded public read shape.

use super::*;

/// Resolve retained identities for bounded public reads and exports.
pub fn expand_navigation(
    stored: &Json,
    resolve_url: &dyn Fn(u64) -> Option<String>,
) -> Result<Json> {
    validate_navigation(stored, true, Some(resolve_url))?;
    let Some(map) = stored.as_object() else {
        return invalid("retained navigation fields are invalid");
    };
    let mut result = map.clone();
    let mut events = Vec::new();
    for event in map["events"].as_array().into_iter().flatten() {
        let Some(fields) = event.as_object() else {
            continue;
        };
        let mut out: Map<String, Value> = fields
            .iter()
            .filter(|(k, _)| k.as_str() != "source_url_id" && k.as_str() != "destination_url_id")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (key, id_key) in [
            ("source", "source_url_id"),
            ("destination", "destination_url_id"),
        ] {
            let id = py_int(&fields[id_key]).unwrap_or_default() as u64;
            let Some(url) = resolve_url(id) else {
                return invalid("navigation URL is invalid");
            };
            out.insert(key.to_owned(), Value::String(url));
        }
        events.push(Value::Object(out));
    }
    result.insert("events".to_owned(), Value::Array(events));
    Ok(Value::Object(result))
}

/// A CDP/Playwright signal the render layer feeds into [`NavigationCapture`].
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureSignal {
    /// `Page.frameRequestedNavigation` for the main frame: `(url, reason)`.
    Requested {
        /// Target URL.
        url: String,
        /// CDP reason (`anchorClick`, `scriptInitiated`, ...).
        reason: String,
    },
    /// `Page.frameNavigated` for the main frame.
    Document {
        /// Committed URL.
        url: String,
    },
    /// `Page.navigatedWithinDocument`: `navigation_type` is `historyApi`, `fragment` or other.
    SameDocument {
        /// New URL.
        url: String,
        /// CDP navigation type.
        navigation_type: String,
    },
    /// Playwright `framenavigated` for the main frame (used only when CDP is unavailable).
    PlaywrightFrame {
        /// Frame URL.
        url: String,
    },
    /// A navigation-request response of the main frame.
    Response {
        /// Request URL.
        request_url: String,
        /// HTTP status.
        status: u16,
        /// `Location` header, if any.
        location: Option<String>,
    },
}

/// Observe the main document only, with a constant event and cause budget.
#[derive(Debug, Clone)]
pub struct NavigationCapture {
    /// Evidence under construction.
    pub data: NavigationEvidence,
    committed: bool,
    cdp: bool,
    pending: Option<(String, String)>,
}

impl NavigationCapture {
    /// Start a capture for `target`. `cdp` records whether CDP was attached, which sets the state
    /// to `partial/capture_in_progress`.
    pub fn new(target: &str, wait: &str, timeout: f64, cdp: bool) -> Self {
        Self {
            data: NavigationEvidence {
                schema: SCHEMA.to_owned(),
                requested_url: target.to_owned(),
                final_url: target.to_owned(),
                wait_until: wait.to_owned(),
                timeout_seconds: timeout,
                interaction_policy: "no_clicks".to_owned(),
                policy: "pinned_http".to_owned(),
                events: Vec::new(),
                events_omitted: 0,
                state: "partial".to_owned(),
                reason: "capture_in_progress".to_owned(),
            },
            committed: false,
            cdp,
            pending: None,
        }
    }

    /// Record one navigation unless the URL is unsafe or the budget is spent.
    /// The current URL is `data.final_url`, which is kept equal to the `current` field. A
    /// malformed netloc is counted as omitted.
    pub fn add(
        &mut self,
        destination: &str,
        kind: &str,
        observation: &str,
        source: Option<&str>,
        status: Option<u16>,
        elapsed_ms: u64,
    ) {
        let rejected = destination.chars().count() > MAX_URL_CHARS
            || url_is_unsafe(destination).unwrap_or(true);
        if rejected {
            self.data.events_omitted += 1;
            return;
        }
        if self.data.events.len() >= EVENT_CAP {
            self.data.events_omitted += 1;
        } else {
            let source = match source {
                Some(text) if !text.is_empty() => text.to_owned(),
                _ => self.data.final_url.clone(),
            };
            self.data.events.push(NavigationEvent {
                source,
                destination: destination.to_owned(),
                elapsed_ms,
                kind: kind.to_owned(),
                observation: observation.to_owned(),
                status_code: status,
                user_click: None,
            });
        }
        self.data.final_url = destination.to_owned();
    }

    /// Feed one signal (`requested`, `document`, `same_document`, `frame`, `response`).
    /// Frame-identity filtering is the render layer's job, as the signal is already
    /// about the main frame.
    pub fn signal(&mut self, signal: CaptureSignal, elapsed_ms: u64) {
        match signal {
            CaptureSignal::Requested { url, reason } => self.pending = Some((url, reason)),
            CaptureSignal::Document { url } => {
                let reason = match &self.pending {
                    Some((pending_url, reason)) if *pending_url == url => reason.as_str(),
                    _ => "",
                };
                let mapped = mapped_document_kind(reason);
                let kind = mapped.unwrap_or(if self.committed {
                    "document_navigation"
                } else {
                    "initial_http_navigation"
                });
                let observation = if mapped.is_some() {
                    "cdp_requested"
                } else {
                    "cdp_frame"
                };
                self.add(&url, kind, observation, None, None, elapsed_ms);
                self.committed = true;
                self.pending = None;
            }
            CaptureSignal::SameDocument {
                url,
                navigation_type,
            } => {
                let kind = match navigation_type.as_str() {
                    "historyApi" => "spa_history_change",
                    "fragment" => "fragment_navigation",
                    _ => "document_navigation",
                };
                self.add(&url, kind, "cdp_history", None, None, elapsed_ms);
                self.pending = None;
            }
            CaptureSignal::PlaywrightFrame { url } => {
                if self.cdp {
                    return;
                }
                let kind = if self.committed {
                    "document_navigation"
                } else {
                    "initial_http_navigation"
                };
                self.add(&url, kind, "playwright_frame", None, None, elapsed_ms);
                self.committed = true;
            }
            CaptureSignal::Response {
                request_url,
                status,
                location,
            } => {
                let Some(location) = location.filter(|value| !value.is_empty()) else {
                    return;
                };
                if !(300..400).contains(&status) {
                    return;
                }
                match urlutil::urljoin(&request_url, &location) {
                    Ok(target) => self.add(
                        &target,
                        "http_redirect",
                        "http_response",
                        Some(&request_url),
                        Some(status),
                        elapsed_ms,
                    ),
                    // A handler error keeps the capture running, but the lost hop still counts as omitted
                    // so state cannot read complete.
                    Err(_) => self.data.events_omitted += 1,
                }
            }
        }
    }

    /// Settle `state`/`reason` after the load.
    pub fn finish(&mut self, success: bool) {
        let mut reason = if success && self.cdp {
            ""
        } else if success {
            "cause_observation_unsupported"
        } else {
            "render_incomplete"
        };
        if self.data.events_omitted > 0 {
            reason = "event_budget_exhausted";
        }
        if self.data.events.is_empty() {
            reason = "navigation_not_observed";
        }
        self.data.state = if !reason.is_empty() && !self.data.events.is_empty() {
            "partial"
        } else if !reason.is_empty() {
            "unavailable"
        } else {
            "complete"
        }
        .to_owned();
        self.data.reason = reason.to_owned();
    }
}

/// Event kind named by a CDP `Page.frameNavigated` reason, when the reason is one of the mapped
/// ones.
pub(super) fn mapped_document_kind(reason: &str) -> Option<&'static str> {
    match reason {
        "scriptInitiated" => Some("script_navigation"),
        "anchorClick" => Some("anchor_navigation"),
        "formSubmissionGet" | "formSubmissionPost" => Some("form_navigation"),
        "metaTagRefresh" | "httpHeaderRefresh" | "reload" => Some("refresh_navigation"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Location` that cannot be resolved still counts as omitted, so `finish` can no
    /// longer settle to `complete` with a silently dropped redirect hop.
    #[test]
    pub(super) fn unresolvable_redirect_location_is_counted_as_omitted() {
        let mut capture = NavigationCapture::new("https://ex.com", "load", 30.0, false);
        capture.signal(
            CaptureSignal::Response {
                request_url: "https://ex.com".to_owned(),
                status: 301,
                location: Some("//[::1".to_owned()),
            },
            0,
        );
        assert!(capture.data.events.is_empty());
        assert_eq!(capture.data.events_omitted, 1);
        capture.finish(true);
        assert_eq!(capture.data.state, "unavailable");
        assert_eq!(capture.data.reason, "navigation_not_observed");
    }
}
