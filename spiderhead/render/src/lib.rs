//! Headless rendering through CDP to an installed Chrome (`chromiumoxide`).
//!
//! No Node or Playwright: the JS render path drives the locally installed Chrome over the DevTools protocol.
//! The static crawl is unchanged; rendering folds the DOM back into the page record afterwards (representation `rendered`).

use futures::StreamExt;

/// Locates the Chrome binary: `SPIDERHEAD_CHROME` if set and non-empty, else the
/// first known install path that exists; `None` when neither is found.
pub fn chrome_executable() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("SPIDERHEAD_CHROME") {
        if !path.is_empty() {
            return Some(std::path::PathBuf::from(path));
        }
    }
    #[cfg(target_os = "macos")]
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ];
    #[cfg(not(target_os = "macos"))]
    let candidates = ["/usr/bin/google-chrome", "/usr/bin/chromium"];
    candidates
        .iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.exists())
}

/// Loads `url` in headless Chrome and returns the serialized DOM once navigation
/// finishes or `timeout_seconds` elapses. Errors if no Chrome is found or the
/// browser cannot launch or open the page.
pub async fn render_document(url: &str, timeout_seconds: f64) -> Result<String, String> {
    let executable = chrome_executable()
        .ok_or_else(|| "no Chrome installation found (set SPIDERHEAD_CHROME)".to_string())?;
    let config = chromiumoxide::BrowserConfig::builder()
        .chrome_executable(executable)
        .build()
        .map_err(|e| format!("browser config failed: {e}"))?;
    let (browser, mut handler) = chromiumoxide::Browser::launch(config)
        .await
        .map_err(|e| format!("launch failed: {e}"))?;
    let driver = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let page = browser
        .new_page(url)
        .await
        .map_err(|e| format!("page failed: {e}"))?;
    let loaded = tokio::time::timeout(
        std::time::Duration::from_secs_f64(timeout_seconds),
        page.wait_for_navigation(),
    )
    .await;
    let _ = loaded; // `load` may already have fired; the DOM is what counts
    let html = page
        .content()
        .await
        .map_err(|e| format!("content failed: {e}"))?;
    drop(page);
    let _ = {
        let mut browser = browser;
        browser.close().await
    };
    let _ = driver.await;
    Ok(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_detection_reports_missing_gracefully() {
        // Without asserting presence, the call must not panic.
        let _ = chrome_executable();
    }
}
