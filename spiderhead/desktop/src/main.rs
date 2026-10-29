//! `spiderhead-desktop` — the SpiderHead desktop window (egui).
//!
//! The window exposes the control channel from birth:
//! `spiderhead-desktop --control 7847` serves `{"id","method","params"}` JSON
//! lines on 127.0.0.1; `spiderhead-desktop-ctl` and widgets drive the same
//! `AppModel::dispatch`. Token authentication is always on.
//!
//! Flags: `--control <port>`, `--control-token <t>`, `--scan <scan.sqlite>`,
//! `--sample site-small|site-large [--sample-dir <dir>]`.

#![forbid(unsafe_code)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde_json::json;
use spiderhead_control::auth;
use spiderhead_control::server;
use spiderhead_desktop::app::AppModel;
use spiderhead_desktop::sample::{self, Sample};
use spiderhead_desktop::ui::{DesktopApp, Effects};

fn main() -> ExitCode {
    let mut port: Option<String> = std::env::var("SPIDERHEAD_CONTROL_PORT").ok();
    let mut token: Option<String> = std::env::var(auth::TOKEN_ENV)
        .ok()
        .filter(|t| !t.trim().is_empty());
    let mut scan: Option<PathBuf> = None;
    let mut sample: Option<Sample> = None;
    let mut sample_dir = PathBuf::from("samples");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let next = |args: &mut dyn Iterator<Item = String>| match args.next() {
            Some(v) => v,
            None => {
                eprintln!("spiderhead-desktop: {arg} needs a value");
                std::process::exit(2);
            }
        };
        match arg.as_str() {
            "--control" => port = Some(next(&mut args)),
            "--control-token" => token = Some(next(&mut args)),
            "--scan" => scan = Some(PathBuf::from(next(&mut args))),
            "--sample" => match Sample::parse(&next(&mut args)) {
                Some(s) => sample = Some(s),
                None => {
                    eprintln!("spiderhead-desktop: --sample wants site-small or site-large");
                    return ExitCode::from(2);
                }
            },
            "--sample-dir" => sample_dir = PathBuf::from(next(&mut args)),
            other => {
                eprintln!(
                    "spiderhead-desktop: unknown argument {other} \
                     (usage: spiderhead-desktop [--control <port>] [--scan <file>] \
                     [--sample site-small|site-large])"
                );
                return ExitCode::from(2);
            }
        }
    }
    let port: u16 = match port.as_deref().map(str::parse) {
        Some(Ok(p)) => p,
        Some(Err(_)) => {
            eprintln!("spiderhead-desktop: the control port is not a number");
            return ExitCode::from(2);
        }
        None => spiderhead_control::DEFAULT_PORT,
    };
    if let Some(t) = &token {
        if t.len() < auth::MIN_TOKEN_LEN {
            eprintln!(
                "spiderhead-desktop: the control token must have at least {} characters",
                auth::MIN_TOKEN_LEN
            );
            return ExitCode::from(2);
        }
    }
    let token: Arc<str> = match token {
        Some(t) => {
            eprintln!(
                "spiderhead-desktop: control channel on 127.0.0.1:{port} requires the token you set (clients: {})",
                auth::TOKEN_ENV
            );
            t.into()
        }
        None => {
            match auth::new_token()
                .and_then(|t| auth::write_token_file(port, &t).map(|path| (t, path)))
            {
                Ok((t, path)) => {
                    eprintln!("spiderhead-desktop: control channel on 127.0.0.1:{port}; its token is in {}", path.display());
                    t.into()
                }
                Err(e) => {
                    eprintln!("spiderhead-desktop: cannot prepare the control token: {e}");
                    return ExitCode::from(2);
                }
            }
        }
    };
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("spiderhead-desktop: cannot listen on 127.0.0.1:{port}: {e}");
            return ExitCode::from(2);
        }
    };

    // The model any autostart scan/sample opens before the window exists.
    let mut model = AppModel::new();
    if let Some(kind) = sample {
        match sample::generate(&sample_dir, kind) {
            Ok(path) => {
                eprintln!("spiderhead-desktop: sample at {}", path.display());
                scan = Some(path);
            }
            Err(e) => eprintln!("spiderhead-desktop: sample failed: {e}"),
        }
    }
    if let Some(path) = scan {
        let out = model.dispatch("scan.open", &json!({"path": path.display().to_string()}));
        match out.answer {
            Ok(_) => {
                eprintln!("spiderhead-desktop: opened {}", path.display());
                // A scan on the command line means «look at the crawl».
                model.dispatch("ui.mode", &json!({"mode": "crawler", "nav": "urls"}));
            }
            Err(e) => eprintln!("spiderhead-desktop: {e}"),
        }
    }

    let (tx, rx) = mpsc::channel::<server::ControlRequest>();
    let effects = Arc::new(Mutex::new(Effects::default()));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 875.0])
            .with_min_inner_size([960.0, 600.0])
            // The top bar lives under the traffic lights, like the Claude
            // desktop app: transparent titlebar, no title, one strip total.
            .with_title_shown(false)
            .with_titlebar_shown(false)
            .with_titlebar_buttons_shown(true)
            .with_fullsize_content_view(true)
            .with_title("SpiderHead"),
        ..Default::default()
    };
    let run = eframe::run_native(
        "SpiderHead",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            // Control messages must not wake the UI one by one: one repaint
            // at most every 250 ms (the spec's tick), only to serve them.
            let wake: Arc<dyn Fn() + Send + Sync> =
                Arc::new(move || ctx.request_repaint_after(std::time::Duration::from_millis(250)));
            std::thread::Builder::new()
                .name("control".into())
                .spawn(move || server::serve(listener, token, tx, Some(wake)))?;
            let mut app = DesktopApp::new(model, effects);
            app.requests = Some(rx);
            Ok(Box::new(app))
        }),
    );
    match run {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("spiderhead-desktop: {e}");
            ExitCode::from(1)
        }
    }
}
