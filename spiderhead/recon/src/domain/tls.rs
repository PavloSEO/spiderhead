//! TLS certificate inspection for domain profiling.

use super::*;

/// Inspect certificate issuer, covered names, protocol, and expiry through the system
/// `openssl` binary. The command seam is `run(argv, timeout) -> stdout`, like
/// [`crate::net_client::whois_text`].
pub fn tls_with(
    domain: &str,
    timeout: f64,
    run: &dyn Fn(&[String], f64) -> Option<String>,
) -> Json {
    let argv = |args: &[&str]| args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let session = run(
        &argv(&[
            "s_client",
            "-connect",
            &format!("{domain}:443"),
            "-servername",
            domain,
            "-verify_return_error",
        ]),
        timeout,
    );
    let Some(session) = session else {
        return json!({"ok": false, "error": "TLS connection failed"});
    };
    let verify_line = session
        .lines()
        .find(|l| l.contains("Verify return code"))
        .unwrap_or("");
    if let Some(code) = verify_line.split(':').next_back() {
        let code = code.trim();
        if !code.is_empty() && !code.starts_with("0") {
            return json!({
                "ok": false,
                "valid": false,
                "error": format!("certificate verification failed: {code}"),
            });
        }
    }
    let protocol = ["Protocol", "Negotiated cipher is"]
        .iter()
        .find_map(|key| {
            session
                .lines()
                .map(str::trim)
                .find(|l| l.contains(*key) && !l.contains("NEGOTIATED"))
                .and_then(|l| l.rsplit([':', ' ']).next())
        })
        .map(|v| {
            if v.starts_with("TLSv") {
                v.to_string()
            } else {
                format!("TLS{v}")
            }
        });
    let details = run(
        &argv(&[
            "x509",
            "-noout",
            "-enddate",
            "-issuer",
            "-ext",
            "subjectAltName",
        ]),
        timeout,
    )
    .unwrap_or_else(|| session.clone());
    let get = |prefix: &str| -> Option<String> {
        details
            .lines()
            .find(|l| {
                l.trim_start()
                    .to_lowercase()
                    .starts_with(&prefix.to_lowercase())
            })
            .map(|l| l.split('=').nth(1).unwrap_or("").trim().to_string())
    };
    let not_after = get("notafter").and_then(|v| parse_cert_date(&v));
    let issuer_line = get("issuer").unwrap_or_default();
    let issuer = ["O=", "CN=", "organizationName="]
        .iter()
        .find_map(|key| {
            issuer_line
                .split(',')
                .find_map(|part| part.trim().strip_prefix(key).map(str::to_string))
        })
        .filter(|s| !s.is_empty());
    let san: Vec<String> = get("x509v3 subject alternative name")
        .or_else(|| get("subjectAltName"))
        .map(|line| {
            line.split(',')
                .filter_map(|part| part.trim().strip_prefix("DNS:").map(str::to_string))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .take(20)
                .collect()
        })
        .unwrap_or_default();
    if issuer.is_none() && san.is_empty() && not_after.is_none() {
        return json!({"ok": false, "error": "the server did not provide a certificate"});
    }
    let days_left = not_after.map(|d| d - today_days());
    json!({
        "ok": true,
        "valid": true,
        "protocol": protocol,
        "issuer": issuer,
        "expires": not_after.map(civil_from_days),
        "days_left": days_left,
        "san": san,
    })
}

/// Default TLS probe: system `openssl`; a missing binary degrades to `{"ok": false, ...}`.
pub fn tls(domain: &str, timeout: f64) -> Json {
    if resolve_socket_addresses(domain, 443, &spiderhead_core::host::SystemHost, None).is_err() {
        return json!({"ok": false, "error": "TLS connection failed"});
    }
    tls_with(domain, timeout, &|argv, timeout| {
        let child = std::process::Command::new("openssl")
            .args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });
        let output = rx
            .recv_timeout(std::time::Duration::from_secs_f64(timeout.max(0.1)))
            .ok()?
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        (!text.is_empty()).then_some(text)
    })
}
