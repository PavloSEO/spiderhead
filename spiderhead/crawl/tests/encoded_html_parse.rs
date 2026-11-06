//! A content-encoded HTML page must parse like its plain twin: the crawler
//! hands the decoded entity, not the wire bytes, to charset detection and the
//! parser. The fixture carries a large inline bootstrap script before `<title>`.

use std::io::{Read, Write};
use std::net::TcpListener;

use flate2::write::GzEncoder;
use flate2::Compression;
use spiderhead_crawl::collect::fetch_one;
use spiderhead_fetch::throttle::Throttle;
use spiderhead_parse::ParseOptions;

const FIXTURE: &str = include_str!("fixtures/inline_bootstrap_head.html");

fn serve(encoding: Option<&'static str>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let body = match encoding {
                Some(_) => {
                    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
                    enc.write_all(FIXTURE.as_bytes()).expect("gzip");
                    enc.finish().expect("finish")
                }
                None => FIXTURE.as_bytes().to_vec(),
            };
            let mut head = String::from("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n");
            if let Some(name) = encoding {
                head.push_str(&format!("Content-Encoding: {name}\r\n"));
            }
            head.push_str(&format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            ));
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    format!("http://{addr}/")
}

async fn crawl_one(url: &str) -> spiderhead_crawl::collect::Fetched {
    let client = spiderhead_fetch::client::build_client(10.0).expect("client");
    let mut throttle = Throttle::new(0.0, 60.0, 1);
    fetch_one(
        &client,
        &mut throttle,
        url,
        "test-agent",
        0,
        0,
        5_242_880,
        5_242_880,
        &ParseOptions::default(),
        &|_| None,
    )
    .await
}

#[tokio::test]
async fn gzip_page_with_inline_bootstrap_script_parses_head_and_links() {
    let plain = crawl_one(&serve(None)).await;
    let gzip = crawl_one(&serve(Some("gzip"))).await;
    let parsed = gzip.parsed.expect("gzip page parsed");
    assert_eq!(parsed.title.as_deref(), Some("Router Bootstrap Title"));
    assert_eq!(parsed.links.len(), 2);
    assert!(
        gzip.size_bytes < plain.size_bytes,
        "size_bytes stays the wire size"
    );
    assert_eq!(gzip.html, plain.html);
    assert_eq!(plain.parsed.expect("plain parsed").links.len(), 2);
}
