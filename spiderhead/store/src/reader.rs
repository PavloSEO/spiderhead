//! Read-only reader of a `scan.v1` file: header, URL table and page rows.
//!
//! Reads only what the desktop URL table needs: the header, the URL table and the page rows.
//! The file is opened read-only and never migrated. Full validation (object inventory, scalar
//! storage checks) is done elsewhere; this reader checks only the header identity.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;
use spiderhead_core::sqlite::open_readonly;

use crate::error::{StoreError, StoreResult};
use crate::rows::scan::{PagesRow, ScanRow, UrlsRow};
use crate::schema_sql::{SCAN_APPLICATION_ID, SCAN_USER_VERSION};

/// An open, read-only scan.v1 database.
pub struct ScanReader {
    conn: Connection,
    path: PathBuf,
}

impl ScanReader {
    /// Open `path` read-only and check `application_id` and `user_version`.
    pub fn open(path: &Path) -> StoreResult<Self> {
        let conn = open_readonly(path, Duration::from_secs(5), &[])?;
        let app: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let ver: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if app != SCAN_APPLICATION_ID || ver != SCAN_USER_VERSION {
            return Err(StoreError::Invalid(format!(
                "not a scan.v1 file: application_id={app} user_version={ver}"
            )));
        }
        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    /// Path the reader was opened on.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The singleton `scan` header row.
    pub fn scan(&self) -> StoreResult<ScanRow> {
        let sql = format!("SELECT {} FROM scan", ScanRow::COLUMNS);
        Ok(self.conn.query_row(&sql, [], ScanRow::from_row)?)
    }

    /// Number of rows of `table` (a fixed scan.v1 table name; not user input).
    fn count(&self, table: &str) -> StoreResult<i64> {
        Ok(self
            .conn
            .query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
                r.get(0)
            })?)
    }

    /// Number of interned URLs.
    pub fn url_count(&self) -> StoreResult<i64> {
        self.count("urls")
    }

    /// Number of page rows.
    pub fn page_count(&self) -> StoreResult<i64> {
        self.count("pages")
    }

    /// All interned URLs ordered by `url_id`.
    pub fn urls(&self) -> StoreResult<Vec<UrlsRow>> {
        let sql = format!("SELECT {} FROM urls ORDER BY url_id", UrlsRow::COLUMNS);
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], UrlsRow::from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// A window of page rows ordered by `page_ordinal` (crawl order).
    pub fn pages_window(&self, offset: i64, limit: i64) -> StoreResult<Vec<PagesRow>> {
        let sql = format!(
            "SELECT {} FROM pages ORDER BY page_ordinal LIMIT ?1 OFFSET ?2",
            PagesRow::COLUMNS
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([limit, offset], PagesRow::from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// All page rows ordered by `page_ordinal`.
    pub fn pages(&self) -> StoreResult<Vec<PagesRow>> {
        self.pages_window(0, -1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(site: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../oracle/raw-scans")
            .join(site)
            .join("scan.sqlite")
    }

    // (site, urls, pages, first url, last url id, first page title, sum word_count)
    // Expected values were computed independently from the same files.
    #[test]
    fn raw_scans_match_python_sqlite3() {
        let cases = [
            (
                "basic",
                12,
                10,
                "http://127.0.0.1:18701/",
                "Basic site home",
                28955,
            ),
            (
                "errors",
                9,
                9,
                "http://127.0.0.1:18702/",
                "Errors site home",
                50,
            ),
        ];
        for (site, urls, pages, first, title, words) in cases {
            let r = ScanReader::open(&raw(site)).expect("open");
            let scan = r.scan().expect("scan");
            assert_eq!(scan.format_version, "scan.v1");
            assert_eq!(scan.lifecycle, "finished");
            assert_eq!(scan.source_kind, "native");
            assert_eq!(scan.crawl_partial, 0);
            assert_eq!(r.url_count().expect("n"), urls);
            assert_eq!(r.page_count().expect("n"), pages);
            let u = r.urls().expect("urls");
            assert_eq!(u.len() as i64, urls);
            assert_eq!(u[0].url_id, 1);
            assert_eq!(u[0].url, first);
            let p = r.pages().expect("pages");
            assert_eq!(p.len() as i64, pages);
            assert_eq!(p[0].title, title);
            assert_eq!(p[0].status_code, Some(200));
            assert_eq!(p[0].crawl_depth, 0);
            assert_eq!(p.iter().map(|x| x.word_count).sum::<i64>(), words);
        }
    }

    #[test]
    fn basic_scan_header_and_second_page() {
        let r = ScanReader::open(&raw("basic")).expect("open");
        let scan = r.scan().expect("scan");
        assert_eq!(scan.scan_uuid, "ea01145e-be8b-4c8f-893c-583d2a0c517e");
        assert_eq!(scan.start_url.as_deref(), Some("http://127.0.0.1:18701/"));
        assert_eq!(scan.evidence_revision, 10);
        let w = r.pages_window(1, 1).expect("window");
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].title, "About the basic site");
        assert_eq!(w[0].word_count, 17);
        assert_eq!(w[0].crawl_depth, 1);
        assert_eq!(
            r.urls().expect("u")[11].url,
            "http://127.0.0.1:18701/about.html#team"
        );
    }

    #[test]
    fn all_six_raw_scans_open() {
        for (site, urls, pages) in [
            ("canonical-hreflang", 10, 10),
            ("redirects", 14, 14),
            ("robots-sitemap", 7, 5),
            ("schema", 6, 6),
        ] {
            let r = ScanReader::open(&raw(site)).expect("open");
            assert_eq!(r.url_count().expect("n"), urls);
            assert_eq!(r.pages().expect("p").len(), pages);
        }
    }

    #[test]
    fn rejects_non_scan_file() {
        let a = raw("basic").with_file_name("scan.sqlite.audit-v2.sqlite");
        assert!(matches!(ScanReader::open(&a), Err(StoreError::Invalid(_))));
    }
}
