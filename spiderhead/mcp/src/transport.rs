//! stdio transport: newline-delimited JSON-RPC in, JSON-RPC out.

use std::io::{BufRead, BufWriter, Write};

use crate::server::Server;

/// Serve `server` over stdio until the input closes.
///
/// Each input line is answered before the next is read; every outbound message
/// (progress notifications, then the response) is written and flushed as soon as
/// the line is handled, which keeps the wire order the oracle produces.
pub fn run(input: impl BufRead, output: impl Write, server: &Server) -> std::io::Result<()> {
    let mut output = BufWriter::new(output);
    for line in input.lines() {
        let line = line?;
        let mut messages = Vec::new();
        server.handle_line(&line, &mut messages);
        for message in messages {
            serde_json::to_writer(&mut output, &message)?;
            output.write_all(b"\n")?;
        }
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Profile;

    #[test]
    fn serves_lines_until_eof_and_flushes_each_answer() {
        let server = Server::new(Profile::Router);
        let input = "this is not json\n{\"jsonrpc\": \"2.0\", \"id\": 4, \"method\": \"ping\"}\n";
        let mut output: Vec<u8> = Vec::new();
        run(input.as_bytes(), &mut output, &server).expect("stdio loop must survive garbage");
        let lines = String::from_utf8(output).unwrap();
        let mut lines = lines.lines();
        let first: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(first["method"], "notifications/message");
        let second: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(second["result"], serde_json::json!({}));
        assert!(lines.next().is_none());
    }
}
