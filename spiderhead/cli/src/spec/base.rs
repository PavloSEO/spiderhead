//! base command declarations (the clap builder tree).

use clap::builder::Command;

use super::{c, posc, sw, v, ArgExt, Py};

/// Every top-level command of the bucket, fully nested.
pub(crate) fn commands() -> Vec<Command> {
    vec![
        c("version", "core version, formats and commands")
            .arg(sw("json", "--json", "print machine-readable core info"))
            .arg(v(
                "required_core_revision",
                "--required-core-revision",
                "compare this exact packaged Desktop core revision with the running core",
                Py::TEXT,
            )),
        c("doctor", "inspect local core prerequisites without writes")
            .arg(sw("json", "--json", "print machine-readable diagnostics"))
            .arg(v(
                "projects_directory",
                "--projects-directory",
                "existing projects directory to inspect",
                Py::TEXT,
            ))
            .arg(v("config", "--config", "SF configuration path", Py::TEXT).def("config.json"))
            .arg(v(
                "sf_cli",
                "--sf-cli",
                "explicit Screaming Frog CLI path",
                Py::TEXT,
            ))
            .arg(v(
                "mcp_http_port",
                "--mcp-http-port",
                "port the MCP HTTP bind check probes (default 8321)",
                Py::INT,
            )),
        c("update-check", "check core releases after explicit consent")
            .arg(sw("consent", "--consent", "").req())
            .arg(sw(
                "automatic",
                "--automatic",
                "reuse a fresh result for a consented daily check",
            ))
            .arg(v("max_releases", "--max-releases", "", Py::INT).def("5")),
        c(
            "update-download",
            "download a checksummed core release asset",
        )
        .arg(sw("consent", "--consent", "").req())
        .arg(v("tag", "--tag", "", Py::TEXT).req())
        .arg(v("asset", "--asset", "", Py::TEXT).id_("asset_name").req())
        .arg(v("out", "--out", "", Py::TEXT).req())
        .arg(v("max_bytes", "--max-bytes", "", Py::INT).def("262144000")),
        c("parse", "Parse.")
            .arg(v(
                "input",
                "--input",
                "JSON object mapped onto the handler arguments",
                Py::TEXT,
            ))
            .arg(v("url", "--url", "target URL", Py::TEXT))
            .arg(v("urls", "--urls", "comma-separated URL list", Py::TEXT)),
        c(
            "mcp",
            "run the MCP server (stdio default, --transport http)",
        )
        .arg(v(
            "profile",
            "--profile",
            "",
            Py::TEXT.ch(&["full", "audit", "infra", "quick-check", "router"]),
        ))
        .arg(sw(
            "no_progress",
            "--no-progress",
            "disable optional MCP progress notifications",
        ))
        .arg(
            v(
                "transport",
                "--transport",
                "server transport: stdio (default) or streamable HTTP",
                Py::TEXT.ch(&["stdio", "http"]),
            )
            .def("stdio"),
        )
        .arg(v(
            "host",
            "--host",
            "HTTP bind host (default 127.0.0.1; e.g. 0.0.0.0 explicitly exposes a listener)",
            Py::TEXT,
        ))
        .arg(v(
            "port",
            "--port",
            "HTTP bind port (default 8321)",
            Py::PORT,
        ))
        .arg(sw(
            "loopback_only",
            "--loopback-only",
            "refuse a non-loopback --host (loopback is already the default)",
        ))
        .arg(posc(
            "action",
            "",
            Py::TEXT.ch(&[
                "status",
                "enable",
                "disable",
                "install",
                "uninstall",
                "backups",
            ]),
        ))
        .arg(sw(
            "json",
            "--json",
            "return only SEOHEAD state or its registration plan",
        ))
        .arg(
            v(
                "actor",
                "--actor",
                "",
                Py::TEXT.ch(&["CLI", "SEOHEAD Desktop"]),
            )
            .def("CLI"),
        )
        .arg(v(
            "client",
            "--client",
            "",
            Py::TEXT.ch(&["claude-code", "claude-desktop", "codex", "cursor"]),
        ))
        .arg(v(
            "backup",
            "--backup",
            "adjacent backup path from the reviewed plan",
            Py::TEXT,
        ))
        .arg(sw(
            "dry_run",
            "--dry-run",
            "preview registration without writes",
        ))
        .arg(sw(
            "yes",
            "--yes",
            "explicitly allow client configuration mutation",
        ))
        .arg(
            v(
                "command",
                "--command",
                "absolute core executable for client registration",
                Py::TEXT,
            )
            .id_("install_command"),
        )
        .arg(v(
            "expected_sha256",
            "--expected-sha256",
            "refuse installation if configuration changed since preview",
            Py::TEXT,
        )),
        c(
            "tui",
            "interactive terminal shell (needs the optional 'tui' extra)",
        )
        .arg(sw(
            "no_color",
            "--no-color",
            "force the plain, unstyled shell",
        ))
        .arg(v(
            "lang",
            "--lang",
            "label language (default: locale)",
            Py::TEXT.ch(&["ru", "en"]),
        ))
        .arg(v(
            "scan",
            "--scan",
            "follow one retained scan/run UUID or id",
            Py::TEXT,
        ))
        .arg(sw(
            "compact",
            "--compact",
            "one-line read-only status for tmux; prints once when piped",
        ))
        .arg(v(
            "project",
            "--project",
            "local project workspace",
            Py::TEXT,
        )),
        c("watch", "observe a local project beside an AI chat")
            .arg(
                v(
                    "project",
                    "--project",
                    "validated local project workspace",
                    Py::TEXT,
                )
                .req(),
            )
            .arg(sw(
                "no_color",
                "--no-color",
                "force the plain, unstyled shell",
            ))
            .arg(v(
                "lang",
                "--lang",
                "label language (default: locale)",
                Py::TEXT.ch(&["ru", "en"]),
            ))
            .arg(v(
                "scan",
                "--scan",
                "follow one retained scan/run UUID or id",
                Py::TEXT,
            ))
            .arg(sw(
                "compact",
                "--compact",
                "one-line read-only status for tmux; prints once when piped",
            )),
    ]
}
