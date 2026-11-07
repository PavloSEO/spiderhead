//! URL-list inputs for `crawl-site` list mode: the `--urls-file` reader shared
//! by the CLI and MCP surfaces.
//!
//! One list, four container shapes — TXT (one URL per line, `#` comments and
//! blank lines skipped), CSV and XLSX (a header row with a `url` column) and
//! XML (every `<loc>` element, sitemap-shaped or bare). The reader returns the
//! URLs in file order with their spelling preserved; validation and budgeting
//! stay with the caller.

use std::path::Path;

/// Read one URL-list file. The extension picks the container; an unknown
/// extension is refused rather than guessed.
pub fn read_url_list(path: &str) -> Result<Vec<String>, String> {
    let file = Path::new(path);
    let ext = file
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "txt" | "text" | "" => read_txt(file),
        "csv" => read_csv(file),
        "xlsx" | "xlsm" => read_xlsx(file),
        "xml" => read_xml(file),
        other => Err(format!(
            "urls_file {other} is not a supported URL list (TXT, CSV, XLSX or XML)"
        )),
    }
}

/// TXT: one URL per line, `#` comments and blank lines skipped — the same
/// shape [`crate::markdown_export::read_urls_file`] reads.
fn read_txt(file: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(file).map_err(io(path_of(file)))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

/// CSV: a header row whose `url` column (case-insensitive) carries the list.
fn read_csv(file: &Path) -> Result<Vec<String>, String> {
    let bytes = std::fs::read(file).map_err(io(path_of(file)))?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(text.as_bytes());
    let headers = reader
        .headers()
        .map_err(|e| format!("urls_file needs a header row ({e})"))?
        .clone();
    let column = url_column(&headers.iter().map(str::to_owned).collect::<Vec<_>>())
        .ok_or_else(|| "urls_file CSV needs a url column".to_owned())?;
    let mut urls = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| format!("urls_file row could not be read ({e})"))?;
        if let Some(value) = record.get(column) {
            let value = value.trim();
            if !value.is_empty() {
                urls.push(value.to_owned());
            }
        }
    }
    Ok(urls)
}

/// XLSX/XLSM: the first worksheet's header row, `url` column like the CSV.
fn read_xlsx(file: &Path) -> Result<Vec<String>, String> {
    use calamine::Reader;
    let mut workbook: calamine::Xlsx<_> = calamine::open_workbook(file)
        .map_err(|e| format!("urls_file workbook could not be opened ({e})"))?;
    let title = workbook
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| "urls_file workbook has no worksheet".to_owned())?;
    let range = workbook
        .worksheet_range(&title)
        .map_err(|e| format!("urls_file worksheet could not be read ({e})"))?;
    let mut rows = range.rows();
    let header_row = rows
        .next()
        .ok_or_else(|| "urls_file worksheet needs a header row".to_owned())?;
    let headers: Vec<String> = header_row
        .iter()
        .map(|cell| cell.to_string().trim().to_lowercase())
        .collect();
    let column =
        url_column(&headers).ok_or_else(|| "urls_file worksheet needs a url column".to_owned())?;
    let mut urls = Vec::new();
    for row in rows {
        if let Some(cell) = row.get(column) {
            let value = cell.to_string();
            let value = value.trim();
            if !value.is_empty() {
                urls.push(value.to_owned());
            }
        }
    }
    Ok(urls)
}

/// XML: every `<loc>` element's text — sitemap documents and bare lists alike.
/// The scan is deliberately flat (template chrome is not expected here): a
/// `<location>` element is not a `<loc>`, and entity escapes are undone for
/// the one form a URL carries.
fn read_xml(file: &Path) -> Result<Vec<String>, String> {
    let bytes = std::fs::read(file).map_err(io(path_of(file)))?;
    let text = String::from_utf8_lossy(&bytes).to_lowercase();
    let mut urls = Vec::new();
    let mut from = 0usize;
    while let Some(at) = text[from..].find("<loc") {
        let start = from + at;
        // The element is a location only when the tag itself is exactly `loc`.
        let after_tag = &text[start + 4..];
        let first = after_tag.chars().next();
        if !matches!(
            first,
            Some('>') | Some(' ') | Some('\t') | Some('\n') | Some('\r') | Some('/')
        ) {
            from = start + 4;
            continue;
        }
        let Some(open_end) = after_tag.find('>') else {
            break;
        };
        let body_start = start + 4 + open_end + 1;
        let Some(close) = text[body_start..].find("</loc>") else {
            break;
        };
        let url = text[body_start..body_start + close]
            .trim()
            .replace("&amp;", "&");
        if !url.is_empty() {
            urls.push(url);
        }
        from = body_start + close + "</loc>".len();
    }
    Ok(urls)
}

/// Index of the `url` column in a header row, case-insensitive.
fn url_column(headers: &[String]) -> Option<usize> {
    headers
        .iter()
        .position(|name| name.eq_ignore_ascii_case("url"))
}

/// The `no such file` shape every reader's IO error carries.
fn io(path: &str) -> impl Fn(std::io::Error) -> String + '_ {
    move |error| format!("urls_file {path} could not be read ({error})")
}

fn path_of(file: &Path) -> &str {
    file.to_str().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(name: &str, body: &str) -> String {
        let dir = std::env::temp_dir().join(format!("sh-url-lists-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write");
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn txt_skips_comments_and_blank_lines() {
        let path = write_temp(
            "list.txt",
            "# comment\n\nhttps://a.test/1\n  https://a.test/2  \n",
        );
        assert_eq!(
            read_url_list(&path).expect("txt"),
            vec!["https://a.test/1".to_owned(), "https://a.test/2".to_owned()]
        );
    }

    #[test]
    fn csv_reads_the_url_column_by_header() {
        let path = write_temp(
            "list.csv",
            "URL,note\nhttps://a.test/1,home\nhttps://a.test/2,\n",
        );
        assert_eq!(
            read_url_list(&path).expect("csv"),
            vec!["https://a.test/1".to_owned(), "https://a.test/2".to_owned()]
        );
    }

    #[test]
    fn csv_without_a_url_column_is_refused() {
        let path = write_temp("bad.csv", "address\nhttps://a.test/1\n");
        let error = read_url_list(&path).expect_err("refused");
        assert_eq!(error, "urls_file CSV needs a url column");
    }

    #[test]
    fn xml_reads_every_loc() {
        let path = write_temp(
            "list.xml",
            "<?xml version=\"1.0\"?>\n<urlset><url><loc>https://a.test/1</loc></url>\
             <url><loc>https://a.test/2?x=1&amp;y=2</loc></url><loc>https://a.test/3</loc>\
             <location>https://a.test/not-a-loc</location></urlset>",
        );
        assert_eq!(
            read_url_list(&path).expect("xml"),
            vec![
                "https://a.test/1".to_owned(),
                "https://a.test/2?x=1&y=2".to_owned(),
                "https://a.test/3".to_owned(),
            ]
        );
    }

    #[test]
    fn an_unknown_extension_is_refused() {
        let path = write_temp("list.json", "[]");
        let error = read_url_list(&path).expect_err("refused");
        assert!(error.contains("not a supported URL list"), "{error}");
    }

    #[test]
    fn a_missing_file_names_the_path() {
        let error = read_url_list("/no/such/list.txt").expect_err("refused");
        assert!(error.starts_with("urls_file /no/such/list.txt"), "{error}");
    }
}
