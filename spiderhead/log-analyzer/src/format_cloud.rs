//! Load-balancer logs: HAProxy HTTP logs and AWS Classic ELB / Application Load Balancer.

use std::sync::OnceLock;

use regex::Regex;

use crate::format::Row;
use crate::format_fields::{parse_time, path_of, split_request, tokens};
use crate::text::{parse_float, parse_int};

fn haproxy_re() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(
            r#"(\S+?):\d+ \[(\d{2}/[A-Za-z]{3}/\d{4}:\d{2}:\d{2}:\d{2})(?:\.\d+)?\] \S+ \S+ -?\d+/-?\d+/-?\d+/-?\d+/\+?(\d+) (\d{3}) (\d+) \S+ \S+ \S{4} \S+ \S+ (?:\{([^}]*)\} )?(?:\{([^}]*)\} )?"([^"]*)""#,
        )
        .ok()
    })
    .as_ref()
}

/// Parse one HAProxy HTTP log line. Timestamps carry no zone and are read as UTC. Captured
/// request headers (`{a|b|c}`) are scanned for a referrer URL and a user-agent product.
pub fn parse_haproxy(line: &str) -> Option<Row> {
    let caps = haproxy_re()?.captures(line)?;
    let (method, target) = split_request(&caps[8])?;
    let headers: Vec<&str> = [caps.get(6), caps.get(7)]
        .into_iter()
        .flatten()
        .flat_map(|m| m.as_str().split('|'))
        .map(str::trim)
        .collect();
    let referer = headers
        .iter()
        .find(|h| h.starts_with("http://") || h.starts_with("https://"))
        .map(|h| (*h).to_string());
    let agent = headers
        .iter()
        .find(|h| h.contains('/') && !h.starts_with("http"))
        .map(|h| (*h).to_string());
    let total_ms = parse_float(&caps[3]);
    Some(Row {
        ip: caps[1].to_string(),
        time: parse_time(&caps[2]),
        method,
        path: path_of(&target),
        status: parse_int(&caps[4])?,
        bytes: parse_int(&caps[5]).unwrap_or(0),
        time_taken: total_ms.map(|m| m / 1000.0),
        response_ms: total_ms,
        referer,
        user_agent: agent,
    })
}

/// Which AWS balancer layout a log uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElbKind {
    /// Classic Load Balancer: the timestamp is the first token.
    Classic,
    /// Application Load Balancer: a type token precedes the timestamp.
    Alb,
}

const ALB_TYPES: [&str; 6] = ["http", "https", "h2", "grpcs", "ws", "wss"];

fn looks_iso(token: &str) -> bool {
    let b = token.as_bytes();
    b.len() >= 19 && b[4] == b'-' && b[7] == b'-' && b[10] == b'T'
}

/// Layout of a tokenised line, if it is an ELB or ALB record.
pub fn elb_kind(parts: &[&str]) -> Option<ElbKind> {
    if parts.len() >= 15 && looks_iso(parts[0]) && parts[2].contains(':') && parts[11].contains(' ')
    {
        return Some(ElbKind::Classic);
    }
    (parts.len() >= 14
        && ALB_TYPES.contains(&parts[0])
        && looks_iso(parts[1])
        && parts[12].contains(' '))
    .then_some(ElbKind::Alb)
}

fn without_port(client: &str) -> String {
    match client.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host.to_string(),
        _ => client.to_string(),
    }
}

/// Parse one ELB/ALB line of the given kind. The response time is the sum of the three
/// processing times, which the balancer writes in seconds.
pub fn parse_elb(line: &str, kind: ElbKind) -> Option<Row> {
    let parts = tokens(line);
    if elb_kind(&parts)? != kind {
        return None;
    }
    let shift = usize::from(kind == ElbKind::Alb);
    let at = |i: usize| parts.get(i + shift).copied();
    let (method, target) = split_request(at(11)?)?;
    let seconds: Option<f64> = (4..=6)
        .map(|i| at(i).and_then(parse_float).filter(|v| *v >= 0.0))
        .sum();
    let agent = at(12).filter(|a| !a.is_empty() && *a != "-");
    Some(Row {
        ip: without_port(at(2)?),
        time: parse_time(at(0)?),
        method,
        path: path_of(&target),
        status: parse_int(at(7)?)?,
        bytes: parse_int(at(10)?).unwrap_or(0).max(0),
        time_taken: seconds,
        response_ms: seconds.map(|s| s * 1000.0),
        referer: None,
        user_agent: agent.map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haproxy_line_with_captured_headers() {
        let line = r#"Oct  1 10:00:00 lb haproxy[1]: 192.0.2.7:33317 [01/Oct/2026:10:00:00.655] http-in static/srv1 10/0/30/69/109 200 2750 - - ---- 1/1/1/1/0 0/0 {example.test|Mozilla/5.0 (compatible; Googlebot/2.1)|https://example.test/} "GET /index.html HTTP/1.1""#;
        let row = parse_haproxy(line).expect("row");
        assert_eq!(
            (row.ip.as_str(), row.status, row.bytes),
            ("192.0.2.7", 200, 2750)
        );
        assert_eq!(row.response_ms, Some(109.0));
        assert_eq!(row.referer.as_deref(), Some("https://example.test/"));
        assert!(row.user_agent.is_some_and(|a| a.contains("Googlebot")));
    }

    #[test]
    fn classic_and_application_balancers() {
        let classic = r#"2026-10-01T10:00:00.945958Z lb 192.0.2.9:2817 10.0.0.1:80 0.000073 0.001048 0.000057 200 200 0 29 "GET http://example.test:80/a?b=1 HTTP/1.1" "curl/7.38.0" - -"#;
        let row = parse_elb(classic, ElbKind::Classic).expect("classic");
        assert_eq!(
            (row.ip.as_str(), row.path.as_str(), row.bytes),
            ("192.0.2.9", "/a?b=1", 29)
        );
        let alb = r#"https 2026-10-01T10:00:00.1Z app/x/1 192.0.2.9:2817 10.0.0.1:80 0.001 0.002 0.000 404 404 0 77 "GET https://example.test:443/m HTTP/2.0" "Bingbot/2.0" ECDHE TLSv1.2 arn "Root=1" "-" "-" 0 2026-10-01T10:00:00.000000Z "forward" "-" "-""#;
        let row = parse_elb(alb, ElbKind::Alb).expect("alb");
        assert_eq!((row.status, row.path.as_str()), (404, "/m"));
        assert_eq!(row.response_ms.map(|m| m.round()), Some(3.0));
        assert!(parse_elb(alb, ElbKind::Classic).is_none());
    }
}
