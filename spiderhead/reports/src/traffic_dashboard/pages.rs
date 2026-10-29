//! The dashboard pages — one `page()` call per printed A4 landscape sheet.

use std::collections::HashSet;

use serde_json::Value;

use super::{
    delta_html, label, label_for, legend, num, table, text_of, tpl, unavailable, Renderer,
    GSC_ROWS_PER_PAGE, OTHER_COLOR, PX_PER_MM,
};
use crate::py::{html_escape, str_value};
use crate::svg_charts::{bar_chart, donut_chart, line_chart, ChartOpts, Series};

impl Renderer<'_> {
    fn label(&self, key: &str) -> &str {
        label(self.fmt.labels, key)
    }

    fn title_with(&self, key: &str, values: &[(&str, &str)]) -> Result<String, String> {
        self.title(key, values)
    }

    fn plain_title(&self, key: &str) -> Result<String, String> {
        self.title(key, &[])
    }

    /// `str.format_map`-safe lookups into a nested label dictionary.
    fn months_label(&self, months: Option<&Value>) -> String {
        let raw = months.map(str_value).unwrap_or_default();
        self.fmt
            .labels
            .get("months")
            .and_then(|m| m.get(&raw))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or(raw)
    }

    // pages
    pub(super) fn cover(&self) -> Result<String, String> {
        let fmt = &self.fmt;
        let logo = if self.brand.logo_text.is_empty() {
            String::new()
        } else {
            format!(
                "<div class=\"logo\">{}</div>",
                html_escape(&self.brand.logo_text)
            )
        };
        let site = if self.site.is_empty() {
            String::new()
        } else {
            format!(
                "<div class=\"cover-site\">{}</div>",
                html_escape(&self.site)
            )
        };
        let generated = self
            .doc
            .get("generated_at")
            .and_then(Value::as_str)
            .unwrap_or("");
        let mut prepared = String::new();
        if !generated.is_empty() {
            // `datetime.fromisoformat(generated).date()` — the leading YYYY-MM-DD
            // of a datetime string, or the date itself.
            let prefix = if generated.len() == 10 {
                Some(generated)
            } else if generated.len() > 10 && matches!(generated.as_bytes()[10], b'T' | b' ') {
                Some(&generated[..10])
            } else {
                None
            };
            if let Some(iso) = prefix {
                if let Ok(day) = fmt.day(iso, true) {
                    prepared = tpl(self.label("prepared"), &[("date", &day)]);
                }
            }
        }
        let attribution = self
            .fmt
            .labels
            .get("attribution")
            .and_then(|a| a.get(text_of(self.doc.get("attribution")).as_str()))
            .and_then(Value::as_str)
            .unwrap_or("");
        Ok(format!(
            "<section class=\"page cover\"><div class=\"cover-bar\"></div>\
             <div class=\"cover-inner\">{logo}<h1>{}</h1>{site}\
             <div class=\"cover-meta\"><div><span>{}</span><b>{}</b></div>\
             <div>{}</div><div>{}</div><div>{}</div></div></div></section>",
            html_escape(&self.plain_title("report_title")?),
            html_escape(self.label("period")),
            html_escape(&fmt.span(self.period)?),
            html_escape(&tpl(self.label("data_source"), &[("ids", &self.ids)])),
            html_escape(&tpl(
                self.label("method_attribution"),
                &[("value", attribution)]
            )),
            html_escape(&prepared),
        ))
    }

    pub(super) fn summary(&self) -> Result<String, String> {
        let summary = self.block("summary");
        let metrics = summary
            .and_then(|s| s.get("metrics"))
            .unwrap_or(&Value::Null);
        let mut cards = String::new();
        for key in ["visits", "users", "pageviews", "visits_per_day"] {
            cards += &self.kpi_card(key, metrics.get(key), true);
        }
        let bullets = self.key_points()?;
        let method = self.method_lines()?;
        let warnings = self
            .doc
            .get("warnings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut warn_html = String::new();
        if !warnings.is_empty() {
            let items: String = warnings
                .iter()
                .map(|w| format!("<li>{}</li>", html_escape(&self.warning_text(w))))
                .collect();
            warn_html = format!(
                "<div class=\"card note warn\"><h3>{}</h3><ul>{items}</ul></div>",
                html_escape(self.label("warnings"))
            );
        }
        let body = format!(
            "<div class=\"kpis four\">{cards}</div><div class=\"split\">\
             <div class=\"card note\"><h3>{}</h3><ul>{}</ul></div>\
             <div class=\"stack\"><div class=\"card note\"><h3>{}</h3><ul>{}</ul></div>\
             {warn_html}</div></div>",
            html_escape(self.label("key_points")),
            bullets
                .iter()
                .map(|b| format!("<li>{b}</li>"))
                .collect::<String>(),
            html_escape(self.label("method")),
            method
                .iter()
                .map(|m| format!("<li>{}</li>", html_escape(m)))
                .collect::<String>(),
        );
        self.page(self.label("summary"), &body, None)
    }

    fn key_points(&self) -> Result<Vec<String>, String> {
        let fmt = &self.fmt;
        let mut out: Vec<String> = Vec::new();
        let metrics = self
            .block("summary")
            .and_then(|s| s.get("metrics"))
            .cloned()
            .unwrap_or(Value::Null);

        let change = |entry: &Value, key: &str| -> String {
            let value = num(entry.get(format!("{key}_delta_pct")));
            let status = entry
                .get(format!("{key}_delta_status"))
                .and_then(Value::as_str);
            if status == Some("new") {
                return self.label("new").to_owned();
            }
            match value {
                None => self.label("na").to_owned(),
                Some(v) => fmt.signed_pct(v),
            }
        };
        let strong = |text: &str| format!("<b>{}</b>", html_escape(text));

        if let Some(visits) = metrics.get("visits") {
            if visits.get("status").and_then(Value::as_str) == Some("ok") {
                out.push(
                    html_escape(self.label("bullet_visits"))
                        .replace("{value}", &strong(&fmt.number(num(visits.get("value")), 0)))
                        .replace("{prev}", &html_escape(&change(visits, "previous")))
                        .replace("{year}", &html_escape(&change(visits, "year_ago"))),
                );
            }
        }
        let users = metrics.get("users");
        let per_day = metrics.get("visits_per_day");
        if let Some(users) = users {
            if users.get("status").and_then(Value::as_str) == Some("ok") {
                out.push(
                    html_escape(self.label("bullet_users"))
                        .replace("{value}", &strong(&fmt.number(num(users.get("value")), 0)))
                        .replace("{prev}", &html_escape(&change(users, "previous")))
                        .replace(
                            "{per_day}",
                            &html_escape(&fmt.number(per_day.and_then(|p| num(p.get("value"))), 1)),
                        ),
                );
            }
        }
        for (key, template) in [
            ("search_engines", "bullet_engine"),
            ("cities", "bullet_city"),
            ("devices", "bullet_device"),
            ("landing_pages", "bullet_page"),
        ] {
            let block = self.block(key).unwrap_or(&Value::Null);
            let rows = block.get("rows").and_then(Value::as_array);
            if block.get("status").and_then(Value::as_str) != Some("ok")
                || !rows.is_some_and(|r| !r.is_empty())
            {
                continue;
            }
            let row = &rows.unwrap()[0];
            out.push(
                html_escape(self.label(template))
                    .replace("{name}", &strong(&label_for(fmt, key, row)))
                    .replace(
                        "{value}",
                        &html_escape(&fmt.number(num(row.get("visits")), 0)),
                    )
                    .replace(
                        "{share}",
                        &html_escape(&fmt.pct(num(row.get("share_pct")), 1)),
                    ),
            );
        }
        let items = self
            .block("windows")
            .and_then(|w| w.get("items"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for item in &items {
            let months = item.get("months").and_then(Value::as_i64);
            if item.get("status").and_then(Value::as_str) != Some("ok")
                || !matches!(months, Some(3) | Some(12))
            {
                continue;
            }
            let entry = item
                .get("metrics")
                .and_then(|m| m.get("visits"))
                .cloned()
                .unwrap_or(Value::Null);
            let months_text = self.months_label(item.get("months"));
            let delta = if entry.get("delta_status").and_then(Value::as_str) == Some("new") {
                self.label("new").to_owned()
            } else {
                match num(entry.get("delta_pct")) {
                    None => self.label("na").to_owned(),
                    Some(v) => fmt.signed_pct(v),
                }
            };
            out.push(
                html_escape(self.label("bullet_window"))
                    .replace("{months}", &html_escape(&months_text))
                    .replace("{value}", &strong(&fmt.number(num(entry.get("value")), 0)))
                    .replace("{delta}", &html_escape(&delta)),
            );
        }
        let channels = self.block("channels").unwrap_or(&Value::Null);
        if channels.get("status").and_then(Value::as_str) == Some("ok") {
            let organic = channels
                .get("rows")
                .and_then(Value::as_array)
                .and_then(|rows| {
                    rows.iter()
                        .find(|r| r.get("id").and_then(Value::as_str) == Some("organic"))
                });
            if let Some(organic) = organic {
                out.push(
                    html_escape(self.label("bullet_channel"))
                        .replace(
                            "{share}",
                            &strong(&fmt.pct(num(organic.get("share_pct")), 1)),
                        )
                        .replace(
                            "{value}",
                            &html_escape(&fmt.number(num(organic.get("visits")), 0)),
                        ),
                );
            }
        }
        Ok(out)
    }

    fn method_lines(&self) -> Result<Vec<String>, String> {
        let fmt = &self.fmt;
        let doc = self.doc;
        let attribution = self
            .fmt
            .labels
            .get("attribution")
            .and_then(|a| a.get(text_of(doc.get("attribution")).as_str()))
            .and_then(Value::as_str)
            .unwrap_or("");
        let scope = self
            .fmt
            .labels
            .get("scope")
            .and_then(|s| s.get(&self.scope))
            .and_then(Value::as_str)
            .unwrap_or("");
        let mut lines = vec![
            tpl(self.label("method_attribution"), &[("value", attribution)]),
            tpl(self.label("method_scope"), &[("value", scope)]),
        ];
        let empty = Value::Null;
        let robots = doc
            .get("methodology")
            .and_then(|m| m.get("robots"))
            .unwrap_or(&empty);
        if robots.get("status").and_then(Value::as_str) == Some("ok") {
            lines.push(tpl(
                self.label("method_robots"),
                &[("value", &fmt.pct(num(robots.get("robot_pct")), 1))],
            ));
        } else {
            let reason = robots
                .get("reason")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| self.label("na"));
            lines.push(tpl(self.label("method_robots_na"), &[("reason", reason)]));
        }
        let comparisons = doc.get("comparisons").unwrap_or(&empty);
        if crate::py::truthy(comparisons.get("previous"))
            && crate::py::truthy(comparisons.get("year_ago"))
        {
            lines.push(tpl(
                self.label("method_compare"),
                &[
                    ("previous", &fmt.span(&comparisons["previous"])?),
                    ("year", &fmt.span(&comparisons["year_ago"])?),
                ],
            ));
        }
        lines.push(self.label("method_share").to_owned());
        let gsc = self.block("gsc_queries").unwrap_or(&empty);
        if gsc.get("status").and_then(Value::as_str) != Some("ok") {
            let key = if gsc.get("status").and_then(Value::as_str) == Some("skipped") {
                "method_gsc_skipped"
            } else {
                "method_gsc_unavailable"
            };
            let reason = gsc
                .get("reason")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| self.label("na"));
            lines.push(tpl(self.label(key), &[("reason", reason)]));
        }
        Ok(lines)
    }

    /// Localise a document warning by its code; unknown codes keep the
    /// document's text.
    fn warning_text(&self, warning: &Value) -> String {
        let template = warning
            .get("code")
            .and_then(Value::as_str)
            .and_then(|code| {
                self.fmt
                    .labels
                    .get("warning_text")
                    .and_then(|w| w.get(code))
                    .and_then(Value::as_str)
            });
        let Some(template) = template else {
            return warning.get("message").map(str_value).unwrap_or_default();
        };
        let names = self.fmt.labels.get("block_names");
        let blocks = warning
            .get("blocks")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .map(|b| {
                        let raw = b.as_str().unwrap_or_default();
                        names
                            .and_then(|n| n.get(raw))
                            .and_then(Value::as_str)
                            .unwrap_or(raw)
                            .to_owned()
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let value = match num(warning.get("value")).filter(|v| *v != 0.0) {
            Some(v) => self.fmt.number(Some(v), 1),
            None => String::new(),
        };
        tpl(template, &[("blocks", &blocks), ("value", &value)])
    }

    fn kpi_card(&self, key: &str, entry: Option<&Value>, big: bool) -> String {
        let fmt = &self.fmt;
        let label = self
            .fmt
            .labels
            .get("kpi")
            .and_then(|k| k.get(key))
            .and_then(Value::as_str)
            .unwrap_or("");
        let css = if big { "card kpi big" } else { "card kpi" };
        if entry.and_then(|e| e.get("status")).and_then(Value::as_str) != Some("ok") {
            let reason = entry
                .and_then(|e| e.get("reason"))
                .map(str_value)
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| self.label("na").to_owned());
            return format!(
                "<div class=\"{css} off\"><div class=\"kpi-label\">{}</div>\
                 <div class=\"kpi-value\">—</div><div class=\"kpi-sub\">{}: {}</div></div>",
                html_escape(label),
                html_escape(self.label("unavailable")),
                html_escape(&reason)
            );
        }
        let entry = entry.unwrap_or(&Value::Null);
        let lower = entry
            .get("lower_is_better")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let prev = delta_html(
            fmt,
            num(entry.get("previous_delta_pct")),
            entry.get("previous_delta_status").and_then(Value::as_str),
            lower,
        );
        let year = delta_html(
            fmt,
            num(entry.get("year_ago_delta_pct")),
            entry.get("year_ago_delta_status").and_then(Value::as_str),
            lower,
        );
        format!(
            "<div class=\"{css}\"><div class=\"kpi-label\">{}</div>\
             <div class=\"kpi-value\">{}</div>\
             <div class=\"kpi-delta\">{prev}<span class=\"vs\">{}</span></div>\
             <div class=\"kpi-sub\">{year}<span class=\"vs\">{}</span></div></div>",
            html_escape(label),
            html_escape(&fmt.kpi(key, num(entry.get("value")))),
            html_escape(self.label("vs_previous")),
            html_escape(self.label("vs_year")),
        )
    }

    pub(super) fn detail(&self) -> Result<String, String> {
        let summary = self.block("summary");
        let grid = if summary
            .and_then(|s| s.get("status"))
            .and_then(Value::as_str)
            != Some("ok")
        {
            unavailable(&self.fmt, summary, true)
        } else {
            let empty = Value::Null;
            let metrics = summary.and_then(|s| s.get("metrics")).unwrap_or(&empty);
            let order = [
                "users",
                "visits",
                "pageviews",
                "page_depth",
                "visits_per_day",
                "bounce_rate",
                "avg_visit_duration_seconds",
                "repeat_visitors_2_3_pct",
                "new_visitors_pct",
            ];
            let cards: String = order
                .iter()
                .map(|k| self.kpi_card(k, metrics.get(k), false))
                .collect();
            format!("<div class=\"kpis\">{cards}</div>")
        };
        let tables = format!(
            "<div class=\"two\">{}{}</div>",
            table(
                &self.fmt,
                "search_engines",
                self.block("search_engines"),
                7,
                false,
                false,
                false,
                false
            ),
            table(
                &self.fmt,
                "cities",
                self.block("cities"),
                7,
                false,
                false,
                false,
                false
            ),
        );
        self.page(&self.plain_title("detail")?, &(grid + &tables), None)
    }

    pub(super) fn daily(&self) -> Result<String, String> {
        let fmt = &self.fmt;
        let block = self.block("daily");
        if block.and_then(|b| b.get("status")).and_then(Value::as_str) != Some("ok") {
            return self.page(
                &self.plain_title("daily")?,
                &unavailable(fmt, block, true),
                None,
            );
        }
        let empty: Vec<Value> = Vec::new();
        let points = block
            .and_then(|b| b.get("points"))
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let one_month = points
            .iter()
            .filter_map(|p| p.get("date").and_then(Value::as_str))
            .map(|d| &d[..7.min(d.len())])
            .collect::<HashSet<_>>()
            .len()
            == 1;
        let mut labels: Vec<String> = Vec::with_capacity(points.len());
        for p in points {
            let date = p.get("date").and_then(Value::as_str).unwrap_or("");
            labels.push(if one_month {
                match crate::join::IsoDate::parse(date) {
                    Some(d) => d.day().to_string(),
                    None => String::new(),
                }
            } else {
                fmt.day(date, false)?
            });
        }
        let width = 263.0 * PX_PER_MM;
        let mut current = Series::new(
            self.label("current_series"),
            points.iter().map(|p| num(p.get("visits"))).collect(),
            &self.series_colors[0],
        );
        current.width = 2.8;
        let mut previous = Series::new(
            &tpl(
                self.label("previous_series"),
                &[("days", &points.len().to_string())],
            ),
            points.iter().map(|p| num(p.get("previous"))).collect(),
            &self.series_colors[1],
        );
        previous.labels = false;
        let mut year = Series::new(
            self.label("year_series"),
            points.iter().map(|p| num(p.get("year_ago"))).collect(),
            &self.series_colors[2],
        );
        year.dashed = true;
        year.width = 1.8;
        year.labels = false;
        let mut line_opts = ChartOpts::new(width, 58.0 * PX_PER_MM, self.number_fmt());
        line_opts.ink = self.brand.ink.clone();
        let line = line_chart(
            &labels,
            &[current.clone(), previous.clone(), year.clone()],
            &line_opts,
        );
        let mut bar_opts = ChartOpts::new(width, 60.0 * PX_PER_MM, self.number_fmt());
        bar_opts.ink = self.brand.ink.clone();
        let bars = bar_chart(
            &labels,
            &[Series::new(
                self.label("current_series"),
                points.iter().map(|p| num(p.get("visits"))).collect(),
                &self.brand.accent,
            )],
            &bar_opts,
        );
        let legend_items: Vec<(String, String)> = self
            .series_colors
            .iter()
            .zip([current, previous, year].iter())
            .map(|(c, s)| (c.clone(), s.name.clone()))
            .collect();
        let legend_html = legend(&legend_items, &HashSet::from([2]));
        let body = format!(
            "<div class=\"card panel\"><div class=\"panel-head\">{legend_html}</div>{line}</div>\
             <div class=\"card panel\"><div class=\"panel-head\">{}</div>{bars}</div>",
            legend(
                &[(
                    self.brand.accent.clone(),
                    self.label("current_series").to_owned(),
                )],
                &HashSet::new(),
            )
        );
        self.page(&self.plain_title("daily")?, &body, None)
    }

    pub(super) fn window(&self, item: &Value) -> Result<String, String> {
        let fmt = &self.fmt;
        let months = self.months_label(item.get("months"));
        let title = self.title_with("window", &[("months", &months)])?;
        let current_span = match item.get("current") {
            Some(v) if !v.is_null() => v,
            _ => self.period,
        };
        if item.get("status").and_then(Value::as_str) != Some("ok") {
            return self.page(
                &title,
                &unavailable(fmt, Some(item), true),
                Some(fmt.span(current_span)?),
            );
        }
        let mut cards = String::new();
        for key in ["visits", "users", "pageviews"] {
            let entry = item
                .get("metrics")
                .and_then(|m| m.get(key))
                .unwrap_or(&Value::Null);
            let vs = format!(
                "{}: {}",
                tpl(self.label("window_previous_series"), &[("months", &months)]).to_lowercase(),
                fmt.number(num(entry.get("previous")), 0)
            );
            cards += &format!(
                "<div class=\"card kpi\"><div class=\"kpi-label\">{}</div>\
                 <div class=\"kpi-value\">{}</div>\
                 <div class=\"kpi-delta\">{}<span class=\"vs\">{}</span></div></div>",
                html_escape(
                    self.fmt
                        .labels
                        .get("kpi")
                        .and_then(|k| k.get(key))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                ),
                html_escape(&fmt.number(num(entry.get("value")), 0)),
                delta_html(
                    fmt,
                    num(entry.get("delta_pct")),
                    entry.get("delta_status").and_then(Value::as_str),
                    false
                ),
                html_escape(&vs),
            );
        }
        let empty: Vec<Value> = Vec::new();
        let series = item.get("series").unwrap_or(&Value::Null);
        let current = series
            .get("current")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let previous = series
            .get("previous")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let width = 263.0 * PX_PER_MM;
        let height = 116.0 * PX_PER_MM;
        let name_current = self.label("current_series");
        let name_previous = tpl(self.label("window_previous_series"), &[("months", &months)]);
        let chart = if item.get("granularity").and_then(Value::as_str) == Some("month") {
            let mut labels: Vec<String> = Vec::with_capacity(current.len());
            for p in current {
                labels.push(fmt.month(p.get("label").and_then(Value::as_str).unwrap_or(""))?);
            }
            let mut opts = ChartOpts::new(width, height, self.number_fmt());
            opts.ink = self.brand.ink.clone();
            bar_chart(
                &labels,
                &[
                    Series::new(
                        name_current,
                        current.iter().map(|p| num(p.get("visits"))).collect(),
                        &self.series_colors[0],
                    ),
                    Series::new(
                        &name_previous,
                        previous[..previous.len().min(current.len())]
                            .iter()
                            .map(|p| num(p.get("visits")))
                            .collect(),
                        &self.series_colors[1],
                    ),
                ],
                &opts,
            )
        } else {
            let mut labels: Vec<String> = Vec::with_capacity(current.len());
            for p in current {
                labels.push(fmt.day(p.get("label").and_then(Value::as_str).unwrap_or(""), false)?);
            }
            let mut opts = ChartOpts::new(width, height, self.number_fmt());
            opts.ink = self.brand.ink.clone();
            let mut cur = Series::new(
                name_current,
                current.iter().map(|p| num(p.get("visits"))).collect(),
                &self.series_colors[0],
            );
            cur.width = 2.4;
            let mut prev = Series::new(
                &name_previous,
                previous.iter().map(|p| num(p.get("visits"))).collect(),
                &self.series_colors[1],
            );
            prev.width = 1.8;
            prev.labels = false;
            line_chart(&labels, &[cur, prev], &opts)
        };
        let legend = legend(
            &[
                (
                    self.series_colors[0].clone(),
                    format!(
                        "{name_current} · {}",
                        fmt.span(item.get("current").unwrap_or(&Value::Null))?
                    ),
                ),
                (
                    self.series_colors[1].clone(),
                    format!(
                        "{name_previous} · {}",
                        fmt.span(item.get("previous").unwrap_or(&Value::Null))?
                    ),
                ),
            ],
            &HashSet::new(),
        );
        let body = format!(
            "<div class=\"kpis three\">{cards}</div>\
             <div class=\"card panel grow\"><div class=\"panel-head\">{legend}</div>{chart}</div>"
        );
        self.page(&title, &body, Some(fmt.span(current_span)?))
    }

    pub(super) fn donut_panel(
        &self,
        block_key: &str,
        limit: usize,
        size_mm: f64,
    ) -> Result<String, String> {
        let fmt = &self.fmt;
        let block = self.block(block_key);
        if block.and_then(|b| b.get("status")).and_then(Value::as_str) != Some("ok") {
            return Ok(unavailable(fmt, block, true));
        }
        let block = block.unwrap_or(&Value::Null);
        let empty: Vec<Value> = Vec::new();
        let all_rows = block
            .get("rows")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let rows = &all_rows[..all_rows.len().min(limit)];
        let mut values: Vec<f64> = rows
            .iter()
            .map(|r| num(r.get("visits")).unwrap_or(0.0))
            .collect();
        let mut colours = self.palette(rows.len());
        let short = if block_key == "channels" {
            self.fmt.labels.get("channel_short")
        } else {
            None
        };
        let mut labels: Vec<String> = rows
            .iter()
            .map(|row| {
                let id = row.get("id").and_then(Value::as_str);
                id.and_then(|i| short.and_then(|s| s.get(i)).and_then(Value::as_str))
                    .map(str::to_owned)
                    .unwrap_or_else(|| label_for(fmt, block_key, row))
            })
            .collect();
        let mut shares: Vec<Option<f64>> = rows.iter().map(|r| num(r.get("share_pct"))).collect();
        let other = block
            .get("other")
            .and_then(|o| num(o.get("visits")))
            .unwrap_or(0.0);
        if other > 0.0 {
            values.push(other);
            colours.push(OTHER_COLOR.to_owned());
            labels.push(self.label("other").to_owned());
            shares.push(block.get("other").and_then(|o| num(o.get("share_pct"))));
        }
        let ring_labels: Vec<String> = shares
            .iter()
            .map(|s| match s {
                Some(v) => fmt.pct(Some(*v), if *v >= 10.0 { 0 } else { 1 }),
                None => String::new(),
            })
            .collect();
        let svg = donut_chart(
            &values,
            &colours,
            &ring_labels,
            size_mm * PX_PER_MM,
            Some(&fmt.number(num(block.get("total")), 0)),
            Some(&self.label("visits").to_lowercase()),
            &self.brand.ink,
            11.0,
            0.045,
            None,
        );
        let items: String = colours
            .iter()
            .zip(labels.iter())
            .zip(shares.iter())
            .map(|((c, n), s)| {
                format!(
                    "<li><i style=\"background:{c}\"></i><span class=\"lg-name\">{}</span>\
                     <span class=\"lg-val\">{}</span></li>",
                    html_escape(n),
                    html_escape(&fmt.share(*s))
                )
            })
            .collect();
        let title = self
            .fmt
            .labels
            .get("dimension")
            .and_then(|d| d.get(block_key))
            .and_then(Value::as_str)
            .unwrap_or("");
        Ok(format!(
            "<div class=\"card panel donut-panel\"><div class=\"panel-title\">{}</div>\
             <div class=\"donut-row\"><div class=\"donut\" style=\"width:{}mm\">{svg}</div>\
             <ul class=\"donut-legend\">{items}</ul></div></div>",
            html_escape(title),
            mm_g(size_mm),
        ))
    }

    pub(super) fn geo(&self) -> Result<String, String> {
        let body = format!(
            "<div class=\"two\">{}{}</div><div class=\"two\">{}{}</div>",
            self.donut_panel("devices", 10, 58.0)?,
            self.donut_panel("cities", 10, 58.0)?,
            table(
                &self.fmt,
                "search_engines",
                self.block("search_engines"),
                7,
                false,
                false,
                false,
                false
            ),
            table(
                &self.fmt,
                "countries",
                self.block("countries"),
                7,
                false,
                false,
                false,
                false
            ),
        );
        self.page(&self.plain_title("geo")?, &body, None)
    }

    pub(super) fn demographics(&self) -> Result<String, String> {
        let body = format!(
            "<div class=\"two\">{}{}</div><div class=\"two\">{}{}</div>",
            self.donut_panel("age", 10, 58.0)?,
            self.donut_panel("gender", 10, 58.0)?,
            table(
                &self.fmt,
                "age",
                self.block("age"),
                6,
                false,
                true,
                false,
                false
            ),
            table(
                &self.fmt,
                "gender",
                self.block("gender"),
                6,
                false,
                true,
                false,
                false
            ),
        );
        self.page(&self.plain_title("demo")?, &body, None)
    }

    pub(super) fn ranked(
        &self,
        key: &str,
        title_key: &str,
        note: Option<&str>,
    ) -> Result<String, String> {
        let block = self.block(key);
        let table = table(&self.fmt, key, block, 15, true, true, true, false);
        let extra = match note {
            Some(note) => format!("<div class=\"footnote\">{}</div>", html_escape(note)),
            None => String::new(),
        };
        self.page(&self.plain_title(title_key)?, &(table + &extra), None)
    }

    pub(super) fn channels(&self) -> Result<String, String> {
        let fmt = &self.fmt;
        let block = self.block("channels");
        if block.and_then(|b| b.get("status")).and_then(Value::as_str) != Some("ok") {
            return self.page(self.label("channels"), &unavailable(fmt, block, true), None);
        }
        let table = table(fmt, "channels", block, 20, false, true, true, true);
        let body = format!(
            "<div class=\"split\"><div>{table}</div>{}</div>",
            self.donut_panel("channels", 10, 48.0)?,
        );
        self.page(self.label("channels"), &body, None)
    }

    pub(super) fn gsc_pages(&self) -> Result<Vec<String>, String> {
        let fmt = &self.fmt;
        let block = self.block("gsc_queries");
        if block.and_then(|b| b.get("status")).and_then(Value::as_str) != Some("ok") {
            return Ok(Vec::new());
        }
        let block = block.unwrap_or(&Value::Null);
        let rows = block
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut pages = Vec::new();
        let totals = block.get("totals").cloned().unwrap_or(Value::Null);
        let summary = tpl(
            self.label("gsc_totals"),
            &[
                ("count", &fmt.number(num(block.get("count")), 0)),
                ("clicks", &fmt.number(num(totals.get("clicks")), 0)),
                (
                    "impressions",
                    &fmt.number(num(totals.get("impressions")), 0),
                ),
            ],
        );
        let notes = format!(
            "{}{}",
            self.label("gsc_note"),
            if block
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                format!(" {}", self.label("gsc_truncated"))
            } else {
                String::new()
            }
        );
        for (page_index, chunk) in rows.chunks(GSC_ROWS_PER_PAGE).enumerate() {
            let start = page_index * GSC_ROWS_PER_PAGE;
            let body_rows: String = chunk
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let ctr = num(r.get("ctr")).map(|c| c * 100.0);
                    let query = html_escape(r.get("query").and_then(Value::as_str).unwrap_or(""));
                    format!(
                        "<tr><td class=\"num idx\">{}.</td>\
                         <td class=\"label\" title=\"{query}\">{query}</td>\
                         <td class=\"num\">{}</td><td class=\"num\">{}</td>\
                         <td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                        start + i + 1,
                        html_escape(&fmt.number(num(r.get("clicks")), 0)),
                        html_escape(&fmt.number(num(r.get("impressions")), 0)),
                        html_escape(&fmt.pct(ctr, 2)),
                        html_escape(&fmt.number(num(r.get("position")), 1)),
                    )
                })
                .collect();
            let head = format!(
                "<th class=\"num idx\">#</th><th>{}</th><th class=\"num\">{}</th>\
                 <th class=\"num\">{}</th><th class=\"num\">{}</th><th class=\"num\">{}</th>",
                html_escape(self.label("gsc_query")),
                html_escape(self.label("gsc_clicks")),
                html_escape(self.label("gsc_impressions")),
                html_escape(self.label("gsc_ctr")),
                html_escape(self.label("gsc_position")),
            );
            let table = format!(
                "<div class=\"card table-card\"><table class=\"data wide gsc\"><colgroup>\
                 <col class=\"c-num\"><col class=\"c-label\"><col class=\"c-val\"><col class=\"c-val\">\
                 <col class=\"c-delta\"><col class=\"c-delta\"></colgroup>\
                 <thead><tr>{head}</tr></thead><tbody>{body_rows}</tbody></table></div>"
            );
            let lead = format!(
                "<div class=\"footnote top\">{} · {}</div>",
                html_escape(&summary),
                html_escape(&notes)
            );
            pages.push(self.page(self.label("gsc"), &(lead + &table), None)?);
        }
        Ok(pages)
    }

    pub(super) fn render(&self) -> Result<String, String> {
        let mut pages = vec![
            self.cover()?,
            self.summary()?,
            self.detail()?,
            self.daily()?,
        ];
        let items = self
            .block("windows")
            .and_then(|w| w.get("items"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for item in &items {
            pages.push(self.window(item)?);
        }
        let windows = self.block("windows");
        if items.is_empty() {
            pages.push(self.page(
                &self.title_with("window", &[("months", "—")])?,
                &unavailable(&self.fmt, windows, true),
                None,
            )?);
        }
        pages.push(self.geo()?);
        pages.push(self.demographics()?);
        pages.push(self.ranked("landing_pages", "pages", None)?);
        pages.push(self.ranked(
            "search_phrases",
            "phrases",
            Some(self.label("phrases_note")),
        )?);
        pages.push(self.channels()?);
        pages.extend(self.gsc_pages()?);
        let total = pages.len();
        let numbered: Vec<String> = pages
            .iter()
            .enumerate()
            .map(|(index, page)| {
                page.replace(
                    "{PAGE}",
                    &html_escape(&tpl(
                        self.label("page_of"),
                        &[
                            ("page", &(index + 1).to_string()),
                            ("pages", &total.to_string()),
                        ],
                    )),
                )
            })
            .collect();
        let mut title = self.plain_title("report_title")?;
        if !self.site.is_empty() {
            title += &format!(" — {}", self.site);
        }
        Ok(format!(
            "<!doctype html><html lang=\"{}\"><head><meta charset=\"utf-8\">\
             <title>{}</title><style>{}</style></head><body>{}</body></html>",
            self.fmt.lang,
            html_escape(&title),
            super::css::css(self.brand),
            numbered.join("")
        ))
    }
}

/// `f"{size_mm:g}"` — strip the trailing `.0` from integral millimetre sizes.
fn mm_g(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}
