//! A scripted HTTP server on 127.0.0.1 standing in for the provider APIs.
//!
//! Routes match on the original provider host (`X-Fake-Host`), method, path and
//! an optional body substring. Each route owns a response sequence: answers are
//! consumed in order and the last one repeats. Every request is recorded so the
//! parity tests can hold the Rust side to the same wire traffic as the oracle.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{json, Map, Value};

const RECORDED_HEADERS: [&str; 4] = [
    "authorization",
    "content-type",
    "x-auth-email",
    "x-auth-key",
];

struct Route {
    spec: Value,
    served: usize,
}

/// The running server; dropping it stops accepting.
pub struct FakeProviders {
    /// `http://127.0.0.1:<port>`.
    pub origin: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<Mutex<bool>>,
    port: u16,
}

impl FakeProviders {
    /// Start serving `routes` (a JSON array of route objects).
    pub fn start(routes: &Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
        let port = listener.local_addr().expect("addr").port();
        let routes: Arc<Mutex<Vec<Route>>> = Arc::new(Mutex::new(
            routes
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|spec| Route { spec, served: 0 })
                .collect(),
        ));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(Mutex::new(false));
        let (shared_routes, shared_requests, shared_stop) =
            (routes.clone(), requests.clone(), stop.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                if *shared_stop.lock().expect("stop") {
                    break;
                }
                if let Ok(stream) = stream {
                    serve(stream, &shared_routes, &shared_requests);
                }
            }
        });
        FakeProviders {
            origin: format!("http://127.0.0.1:{port}"),
            requests,
            stop,
            port,
        }
    }

    /// The recorded wire requests, in arrival order.
    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().expect("requests").clone()
    }
}

impl Drop for FakeProviders {
    fn drop(&mut self) {
        *self.stop.lock().expect("stop") = true;
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn serve(mut stream: TcpStream, routes: &Mutex<Vec<Route>>, requests: &Mutex<Vec<Value>>) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let Ok(read) = stream.read(&mut chunk) else {
            return;
        };
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    let length: usize = header("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    while buffer.len() < header_end + length {
        let Ok(read) = stream.read(&mut chunk) else {
            break;
        };
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = String::from_utf8_lossy(&buffer[header_end..]).into_owned();
    let host = header("x-fake-host").unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let mut recorded_headers = Map::new();
    for name in RECORDED_HEADERS {
        if let Some(value) = header(name) {
            recorded_headers.insert(name.to_owned(), json!(value));
        }
    }
    let mut record = json!({
        "host": host, "method": method, "path": path, "query": query,
        "body": body, "headers": recorded_headers,
    });
    let response = {
        let mut routes = routes.lock().expect("routes");
        let mut found = None;
        for route in routes.iter_mut() {
            let spec = &route.spec;
            if spec.get("host").and_then(Value::as_str).unwrap_or(&host) != host
                || spec.get("method").and_then(Value::as_str).unwrap_or("GET") != method
                || spec["path"].as_str() != Some(path)
            {
                continue;
            }
            if let Some(needle) = spec.get("body_contains").and_then(Value::as_str) {
                if !body.contains(needle) {
                    continue;
                }
            }
            let responses = spec["responses"].as_array().cloned().unwrap_or_default();
            let index = route.served.min(responses.len().saturating_sub(1));
            route.served += 1;
            found = responses.get(index).cloned();
            break;
        }
        found
    };
    let response = response.unwrap_or_else(|| {
        record["unmatched"] = json!(true);
        json!({"status": 404, "body": "{\"error\":\"no route\"}"})
    });
    requests.lock().expect("requests").push(record);
    let payload = match &response["body"] {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let status = response["status"].as_u64().unwrap_or(200);
    let mut out = format!("HTTP/1.0 {status} X\r\n");
    let headers = response
        .get("headers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(|| {
            Map::from_iter([("content-type".to_owned(), json!("application/json"))])
        });
    for (name, value) in &headers {
        out += &format!("{name}: {}\r\n", value.as_str().unwrap_or_default());
    }
    out += &format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(out.as_bytes());
    let _ = stream.write_all(payload.as_bytes());
    let _ = stream.flush();
}
