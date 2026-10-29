//! Alias for `spiderhead mcp`: the stdio MCP server as a stand-alone command.

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(spiderhead_mcp::stdio::run(
        "spiderhead-mcp",
        std::env::args().skip(1),
    ))
}
