//! Derived groups: the content type of a requested path and the browser of a human
//! user agent.
//!
//! Log lines rarely carry a MIME type, so the file type comes from the path extension:
//! no extension, or a page extension (`.php`, `.aspx`, ...), means `html`. Query string and
//! fragment are ignored and the match is case-insensitive.
//!
//! Browser order matters because engines impersonate each other: Edge, Opera and Samsung
//! carry `Chrome/` too, and Chrome carries `Safari/`.

/// File-type group of a request path.
pub fn file_type(path: &str) -> &'static str {
    let bare = path.split(['?', '#']).next().unwrap_or("");
    let name = bare.rsplit('/').next().unwrap_or("");
    let Some((_, ext)) = name.rsplit_once('.') else {
        return "html";
    };
    match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" | "xhtml" | "php" | "asp" | "aspx" | "jsp" | "cfm" | "shtml" => "html",
        "js" | "mjs" | "cjs" => "javascript",
        "css" => "css",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "svg" | "ico" | "bmp" | "tif"
        | "tiff" | "heic" => "image",
        "pdf" => "pdf",
        "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "rtf" | "txt"
        | "csv" | "md" => "document",
        "woff" | "woff2" | "ttf" | "otf" | "eot" => "font",
        "mp4" | "webm" | "mov" | "avi" | "mkv" | "mp3" | "ogg" | "wav" | "m4a" | "flac" => "media",
        "json" | "xml" | "rss" | "atom" | "map" | "webmanifest" => "data",
        "swf" => "flash",
        "zip" | "gz" | "tar" | "rar" | "7z" | "tgz" => "archive",
        _ => "other",
    }
}

/// Browser family of a user agent; `other_browser` when none of the known ones matches.
pub fn browser_of(user_agent: &str) -> &'static str {
    let has = |s: &str| user_agent.contains(s);
    if has("Edg/") || has("EdgA/") || has("EdgiOS/") || has("Edge/") {
        "edge"
    } else if has("OPR/") || has("Opera") {
        "opera"
    } else if has("SamsungBrowser") {
        "samsung"
    } else if has("YaBrowser") {
        "yandex_browser"
    } else if has("Firefox/") || has("FxiOS/") {
        "firefox"
    } else if has("Chrome/") || has("CriOS/") {
        "chrome"
    } else if has("Safari/") && has("Version/") {
        "safari"
    } else {
        "other_browser"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_types_follow_extension() {
        assert_eq!(file_type("/"), "html");
        assert_eq!(file_type("/shop/item"), "html");
        assert_eq!(file_type("/a.PHP?x=1"), "html");
        assert_eq!(file_type("/static/app.min.js"), "javascript");
        assert_eq!(file_type("/s.css#x"), "css");
        assert_eq!(file_type("/i/logo.WebP"), "image");
        assert_eq!(file_type("/doc.pdf"), "pdf");
        assert_eq!(file_type("/f.woff2"), "font");
        assert_eq!(file_type("/x.weird"), "other");
        assert_eq!(file_type("/dir.v2/page"), "html");
    }

    #[test]
    fn browsers_resolve_engine_impersonation() {
        let chrome = "Mozilla/5.0 (X11) AppleWebKit/537.36 Chrome/120.0 Safari/537.36";
        assert_eq!(browser_of(chrome), "chrome");
        assert_eq!(browser_of(&format!("{chrome} Edg/120.0")), "edge");
        assert_eq!(browser_of(&format!("{chrome} OPR/100")), "opera");
        assert_eq!(
            browser_of("Mozilla/5.0 Gecko/20100101 Firefox/121.0"),
            "firefox"
        );
        assert_eq!(
            browser_of("Mozilla/5.0 (iPhone) AppleWebKit Version/17.0 Mobile Safari/604.1"),
            "safari"
        );
        assert_eq!(browser_of("curl/8"), "other_browser");
    }
}
