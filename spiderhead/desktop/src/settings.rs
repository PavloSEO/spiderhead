//! Per-computer application settings: a typed schema-validated store persisted
//! to one JSON file (reference `settings_store.AppSettings` + the `ui/settings`
//! SCHEMA declarations). Values are stored only when explicitly set; `get`
//! returns the validated stored value or the declared default. Secrets never
//! live here — source keys go through `sources::credential_state`.

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;

/// Persistence and IO failures of the settings file.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The file could not be read, written or renamed.
    #[error("settings file error: {0}")]
    Io(#[from] std::io::Error),
    /// The file holds JSON that is not an object of values.
    #[error("settings file is not a JSON object")]
    Shape,
}

/// Value kind of one setting (reference `Setting.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// JSON boolean only.
    Bool,
    /// Integer within `min..=max`; strings of digits accepted.
    Int,
    /// Float within `min..=max`; `,` decimal separator accepted.
    Float,
    /// Free text, optionally restricted to `choices`.
    Text,
    /// `HH:MM` 24-hour clock; a single-digit hour is stored as two digits.
    Time,
}

/// One declared setting: key, default, validation and the Russian error shown
/// under the field when validation fails (reference `settings_store.Setting`).
pub struct Setting {
    /// Dotted key (`general.start`).
    pub key: String,
    /// Value returned while nothing is stored.
    pub default: Value,
    kind: Kind,
    minimum: Option<f64>,
    maximum: Option<f64>,
    choices: Option<Vec<String>>,
    error: Option<&'static str>,
}

impl Setting {
    fn bool(key: &str, default: bool) -> Self {
        Self::new(key, json!(default), Kind::Bool)
    }
    fn int(key: &str, default: i64, min: f64, max: f64, error: &'static str) -> Self {
        let mut s = Self::new(key, json!(default), Kind::Int);
        s.minimum = Some(min);
        s.maximum = Some(max);
        s.error = Some(error);
        s
    }
    fn float(key: &str, default: f64, min: f64, max: f64, error: &'static str) -> Self {
        let mut s = Self::new(key, json!(default), Kind::Float);
        s.minimum = Some(min);
        s.maximum = Some(max);
        s.error = Some(error);
        s
    }
    fn text(key: &str, default: &str) -> Self {
        Self::new(key, json!(default), Kind::Text)
    }
    fn choice(key: &str, default: &str, choices: &[&str]) -> Self {
        let mut s = Self::new(key, json!(default), Kind::Text);
        s.choices = Some(choices.iter().map(|c| c.to_string()).collect());
        s
    }
    fn time(key: &str, default: &str, error: &'static str) -> Self {
        let mut s = Self::new(key, json!(default), Kind::Time);
        s.error = Some(error);
        s
    }
    fn new(key: &str, default: Value, kind: Kind) -> Self {
        Self {
            key: key.to_string(),
            default,
            kind,
            minimum: None,
            maximum: None,
            choices: None,
            error: None,
        }
    }

    /// Return the normalised value or the declared Russian error message
    /// (reference `Setting.validate` → `(ok, value)`).
    pub fn validate(&self, raw: &Value) -> Result<Value, String> {
        let invalid = || self.error.unwrap_or("Недопустимое значение").to_string();
        match self.kind {
            Kind::Bool => raw.as_bool().map(Value::Bool).ok_or_else(invalid),
            Kind::Int | Kind::Float => {
                if raw.is_boolean() {
                    return Err(invalid());
                }
                let number = match raw {
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) => s
                        .replace([' ', '\u{00a0}', '\u{2009}'], "")
                        .replace(',', ".")
                        .parse::<f64>()
                        .ok(),
                    _ => None,
                };
                let Some(number) = number else {
                    return Err(invalid());
                };
                if !number.is_finite()
                    || self.minimum.is_some_and(|m| number < m)
                    || self.maximum.is_some_and(|m| number > m)
                {
                    return Err(invalid());
                }
                if self.kind == Kind::Int {
                    // reference `int(str)` rejects dots; `int(number)` truncates.
                    if let Value::String(s) = raw {
                        let t = s.replace(' ', "");
                        if !t.chars().all(|c| c.is_ascii_digit() || "+-".contains(c))
                            || t.contains('.')
                        {
                            return Err(invalid());
                        }
                    }
                    Ok(json!(number.trunc() as i64))
                } else {
                    Ok(json!(number))
                }
            }
            Kind::Text => {
                let text = match raw {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                if self
                    .choices
                    .as_ref()
                    .is_some_and(|c| !c.iter().any(|v| v == &text))
                {
                    return Err(invalid());
                }
                Ok(Value::String(text))
            }
            Kind::Time => {
                let text = raw.as_str().unwrap_or("").trim().to_string();
                let Some((h_str, m_str)) = text.split_once(':') else {
                    return Err(invalid());
                };
                // reference `(\d{1,2}):(\d{2})`: loose hour, exactly two minute digits.
                if m_str.len() != 2 {
                    return Err(invalid());
                }
                let (Ok(h), Ok(m)) = (h_str.parse::<u32>(), m_str.parse::<u32>()) else {
                    return Err(invalid());
                };
                if h > 23 || m > 59 {
                    return Err(invalid());
                }
                Ok(Value::String(format!("{h:02}:{m:02}")))
            }
        }
    }
}

/// Notification matrix rows (reference the reference suite):
/// (id, default for window/system/sound).
pub const NOTIFY_EVENTS: &[(&str, (bool, bool, bool))] = &[
    ("done", (true, true, false)),
    ("err", (true, true, true)),
    ("agent", (true, false, false)),
    ("pay", (true, true, true)),
    ("stale", (true, false, false)),
    ("exp", (true, false, false)),
    ("upd", (true, false, false)),
];
/// Notification channels in column order (reference `CHANNELS`).
const NOTIFY_CHANNELS: &[&str] = &["w", "s", "z"];

/// Shortcut actions of the Hotkeys page (reference the reference suite):
/// (id, default chord).
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("palette", "Ctrl+K"),
    ("new_scan", "Ctrl+N"),
    ("new_tab", "Ctrl+T"),
    ("close_tab", "Ctrl+W"),
    ("settings", "Ctrl+,"),
    ("stop_scan", "Ctrl+Backspace"),
    ("find_in_table", "Ctrl+F"),
    ("expand_table", "Ctrl+Alt+Return"),
    ("copy_tsv", "Ctrl+Alt+C"),
    ("copy_url", "Ctrl+Shift+C"),
    ("help", "F1"),
    ("all_shortcuts", "Ctrl+/"),
];

/// The whole schema: every section's SCHEMA merged (reference test
/// `test_settings_wiring` requires every declared key to be classified).
fn build_schema() -> Vec<Setting> {
    const TIME_ERROR: &str = "Время в формате ЧЧ:ММ";
    let mut schema = vec![
        // shell (the reference suite)
        Setting::choice("shell.mode", "crawler", &["crawler", "logs", "agent"]),
        Setting::bool("shell.onboarding_done", false),
        // general
        Setting::choice("general.start", "last", &["last", "list"]),
        Setting::bool("general.restore_tabs", true),
        Setting::int(
            "general.recent_count",
            20,
            5.0,
            50.0,
            "Введите число от 5 до 50",
        ),
        Setting::text("general.projects_folder", "~/Documents/SEOHEAD"),
        Setting::bool("general.confirm_close_active_scan", true),
        Setting::bool("general.stay_in_menu_bar", false),
        Setting::choice("general.link_action", "details", &["details", "browser"]),
        Setting::int(
            "general.autosave_seconds",
            30,
            5.0,
            600.0,
            "Не меньше 5 секунд — иначе файл проекта пишется слишком часто",
        ),
        Setting::choice(
            "general.date_format",
            "dd.MM.yyyy HH:mm",
            &["dd.MM.yyyy HH:mm", "yyyy-MM-dd HH:mm", "MM/dd/yyyy hh:mm a"],
        ),
        Setting::bool("general.crash_stats", false),
        // view
        Setting::choice("view.theme", "light", &["light", "dark", "hc", "system"]),
        Setting::choice("view.language", "en", &["en"]),
        Setting::choice(
            "view.density",
            "standard",
            &["compact", "standard", "comfortable"],
        ),
        Setting::choice("view.details_position", "bottom", &["bottom", "right"]),
        Setting::int("view.zoom", 100, 80.0, 150.0, "Масштаб — от 80 до 150 %"),
        Setting::bool("view.reduce_motion", false),
        Setting::bool("view.mono_urls", true),
        Setting::bool("view.rail_when_narrow", true),
        Setting::bool("view.focus_mode", false),
        Setting::bool("view.status_badges", true),
        // Big-table layout snapshot: column order/widths/hidden/freeze + zoom
        // (a JSON blob the grid writes through `persist_grid`).
        Setting::text("table.urls.layout", ""),
        // scan
        Setting::float(
            "scan.rate",
            2.0,
            0.1,
            10.0,
            "Максимум 10 запр/с; больше — только профиль «Свой сайт»",
        ),
        Setting::int(
            "scan.url_limit",
            1500,
            1.0,
            1_000_000.0,
            "Введите число от 1 до 1 000 000",
        ),
        Setting::int("scan.depth", 10, 0.0, 100.0, "Введите число от 0 до 100"),
        Setting::bool("scan.robots", true),
        Setting::bool("scan.save_html", true),
        Setting::int("scan.parallel", 3, 1.0, 4.0, "От 1 до 4"),
        Setting::text("scan.sf_path", ""),
        Setting::choice(
            "scan.default_profile",
            "quick",
            &["quick", "full", "sitemap", "js"],
        ),
        // core
        Setting::choice("core.poll", "0.5", &["0.25", "0.5", "1", "2"]),
        Setting::choice("core.log_level", "INFO", &["INFO", "DEBUG"]),
        // mcp
        Setting::bool("mcp.enabled", true),
        Setting::choice(
            "mcp.profile",
            "full",
            &["full", "audit", "quick-check", "router", "infra"],
        ),
        Setting::bool("mcp.journal", true),
        // agent
        Setting::text("agent.projects", ""),
        Setting::bool("agent.run_scans", false),
        Setting::bool("agent.change_tasks", true),
        Setting::bool("agent.show_actions", true),
        // data
        Setting::choice("data.keep_scans", "20", &["10", "20", "all"]),
        Setting::choice("data.html_age", "90", &["30", "90", "never"]),
        // updates
        Setting::choice("updates.channel", "stable", &["stable", "beta"]),
        Setting::bool("updates.auto_check", true),
        Setting::bool("updates.background_download", false),
    ];
    for &(event, (w, s, z)) in NOTIFY_EVENTS {
        for (i, channel) in NOTIFY_CHANNELS.iter().enumerate() {
            schema.push(Setting::bool(
                &format!("notify.{event}_{channel}"),
                [w, s, z][i],
            ));
        }
    }
    schema.push(Setting::bool("notify.quiet", true));
    schema.push(Setting::time("notify.quiet_from", "20:00", TIME_ERROR));
    schema.push(Setting::time("notify.quiet_to", "08:00", TIME_ERROR));
    for &(action, chord) in SHORTCUTS {
        schema.push(Setting::text(&format!("keys.{action}"), chord));
    }
    schema
}

/// The schema, built once.
pub fn schema() -> &'static [Setting] {
    static SCHEMA: OnceLock<Vec<Setting>> = OnceLock::new();
    SCHEMA.get_or_init(build_schema)
}

/// One declared setting by key.
pub fn definition(key: &str) -> Option<&'static Setting> {
    schema().iter().find(|s| s.key == key)
}

/// Where the desktop settings file lives: the credential config root's
/// `spiderhead/` area, never alongside project files.
pub fn default_path(host: &dyn Host) -> PathBuf {
    spiderhead_sources::credentials::default_config_root(host)
        .join("spiderhead")
        .join("desktop-settings.json")
}

/// The typed store. `values` holds only keys the user explicitly set; every
/// read validates so a corrupt file can never produce an out-of-range value.
pub struct AppSettings {
    path: Option<PathBuf>,
    values: BTreeMap<String, Value>,
}

impl AppSettings {
    /// A store with no file backend (tests, `--no-settings` runs).
    pub fn in_memory() -> Self {
        Self {
            path: None,
            values: BTreeMap::new(),
        }
    }

    /// Load from `path`; a missing file means all defaults, a corrupt one is
    /// treated as empty rather than crashing the shell.
    pub fn load(path: PathBuf) -> Self {
        let mut store = Self {
            path: Some(path),
            values: BTreeMap::new(),
        };
        if let Some(p) = store.path.clone() {
            if let Ok(text) = std::fs::read_to_string(&p) {
                if let Ok(doc) = serde_json::from_str::<Value>(&text) {
                    // The file is {"version":1,"settings":{…}}; a bare object is
                    // accepted as well so hand-edited files keep working.
                    let map = doc
                        .get("settings")
                        .and_then(Value::as_object)
                        .or_else(|| doc.as_object());
                    if let Some(map) = map {
                        store.values = map
                            .iter()
                            .filter(|(k, _)| definition(k).is_some())
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect();
                    }
                }
            }
        }
        store
    }

    /// Persist atomically (temp file + rename), like `credentials::write_secret`.
    pub fn save(&self) -> Result<(), SettingsError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut map = Map::new();
        map.insert("version".into(), json!(1));
        map.insert(
            "settings".into(),
            Value::Object(
                self.values
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
        );
        let body = serde_json::to_vec_pretty(&Value::Object(map)).map_err(|e| {
            SettingsError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// The effective value: validated stored value or the default.
    pub fn get(&self, key: &str) -> Value {
        let Some(setting) = definition(key) else {
            return Value::Null;
        };
        match self.values.get(key) {
            Some(raw) => setting
                .validate(raw)
                .unwrap_or_else(|_| setting.default.clone()),
            None => setting.default.clone(),
        }
    }

    /// The effective value as bool (`false` when the key is not boolean).
    pub fn get_bool(&self, key: &str) -> bool {
        self.get(key).as_bool().unwrap_or(false)
    }

    /// The effective value as string.
    pub fn get_str(&self, key: &str) -> String {
        self.get(key).as_str().unwrap_or("").to_string()
    }

    /// Whether the key was explicitly stored (drives «изменено» badges).
    pub fn is_set(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    /// Store a valid value. `Ok(changed)`; `Err` is the Russian validation
    /// message and nothing is stored (reference `AppSettings.set`).
    pub fn set(&mut self, key: &str, value: Value) -> Result<bool, String> {
        let Some(setting) = definition(key) else {
            return Err(format!("Неизвестная настройка: {key}"));
        };
        let normal = setting.validate(&value)?;
        if self.get(key) == normal && self.is_set(key) {
            return Ok(false);
        }
        self.values.insert(key.to_string(), normal);
        Ok(true)
    }

    /// Drop every stored key under `prefix` (reference `reset`); returns how many.
    pub fn reset(&mut self, prefix: &str) -> usize {
        let keys: Vec<String> = self
            .values
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        let n = keys.len();
        for k in keys {
            self.values.remove(&k);
        }
        n
    }

    /// `{key: {value, default, is_set}}` for `settings.list`.
    pub fn describe(&self) -> Value {
        let mut out = Map::new();
        for s in schema() {
            out.insert(
                s.key.clone(),
                json!({
                    "value": self.get(&s.key),
                    "default": s.default,
                    "is_set": self.is_set(&s.key),
                }),
            );
        }
        Value::Object(out)
    }

    /// Export document: effective values of every declared key; secrets are
    /// never part of the schema so nothing is redacted out.
    pub fn export(&self) -> Value {
        let mut settings = Map::new();
        for s in schema() {
            settings.insert(s.key.clone(), self.get(&s.key));
        }
        json!({"version": 1, "settings": settings})
    }

    /// Import an export document: apply the recognised, valid keys; return
    /// `(applied, rejected keys)` — a bad file never mutates half-way.
    pub fn import(&mut self, data: &Value) -> (usize, Vec<String>) {
        let Some(incoming) = data.get("settings").and_then(Value::as_object) else {
            return (0, vec!["file".to_string()]);
        };
        let mut staged = Vec::new();
        let mut rejected = Vec::new();
        for (key, value) in incoming {
            match definition(key) {
                Some(setting) => match setting.validate(value) {
                    Ok(v) => staged.push((key.clone(), v)),
                    Err(_) => rejected.push(key.clone()),
                },
                None => rejected.push(key.clone()),
            }
        }
        let applied = staged.len();
        for (k, v) in staged {
            self.values.insert(k, v);
        }
        (applied, rejected)
    }

    /// Scan defaults mapped onto `spiderhead_core` crawl settings paths
    /// (`settings.crawl_defaults`). reference `new_scan_draft._from_prefs`: the
    /// rate becomes a minimum delay, depth 0 means «no limit» (core −1), and
    /// `scan.parallel` stays app-level — it is not a crawl setting.
    pub fn crawl_defaults(&self) -> Value {
        let mut layer = serde_json::Map::new();
        let rate = self.get("scan.rate").as_f64().unwrap_or_default();
        if rate > 0.0 {
            layer.insert(
                "speed.min_delay_seconds".to_string(),
                json!(((1.0 / rate) * 1e6).round() / 1e6),
            );
        }
        let limit = self.get("scan.url_limit").as_i64().unwrap_or_default();
        if limit > 0 {
            layer.insert("limits.max_urls".to_string(), json!(limit));
        }
        let depth = self.get("scan.depth").as_i64().unwrap_or_default();
        if depth >= 0 {
            layer.insert(
                "limits.max_depth".to_string(),
                json!(if depth == 0 { -1 } else { depth }),
            );
        }
        layer.insert(
            "robots.policy".to_string(),
            json!(if self.get_bool("scan.robots") {
                "respect"
            } else {
                "ignore"
            }),
        );
        layer.insert(
            "storage.body_mode".to_string(),
            json!(if self.get_bool("scan.save_html") {
                "captured_entity_bytes"
            } else {
                "off"
            }),
        );
        Value::Object(layer)
    }
}

/// Credential component a provider accepts: the config-root-relative file plus
/// the overriding environment variable. The value itself is never read.
pub struct CredentialComponent {
    /// Component name shown in UI (`api_key`, `login`, `access_token`).
    pub name: &'static str,
    /// Path under the credential root (`arsenkin/token`).
    pub path: &'static str,
    /// Environment variable that overrides the file.
    pub env_var: &'static str,
}

/// How a provider is authorised (drives the detail page flow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Browser OAuth with token refresh.
    OAuth,
    /// A pasted API key or login pair.
    ApiKey,
    /// A service-account JSON file.
    ServiceAccount,
    /// No credential at all.
    Public,
}

/// One data provider of the Sources page (canvas `SetSources` list).
pub struct Provider {
    /// Registry id.
    pub id: &'static str,
    /// Russian display name.
    pub title: &'static str,
    /// Authorisation kind.
    pub access: Access,
    /// Paid API (shows in the spend section).
    pub paid: bool,
    /// Credential components.
    pub components: &'static [CredentialComponent],
}

const fn comp(
    name: &'static str,
    path: &'static str,
    env_var: &'static str,
) -> CredentialComponent {
    CredentialComponent {
        name,
        path,
        env_var,
    }
}

/// The provider catalogue the Sources page shows (canvas order).
pub fn providers() -> &'static [Provider] {
    &PROVIDERS
}

static PROVIDERS: [Provider; 15] = [
    Provider {
        id: "gsc",
        title: "Google Search Console",
        access: Access::OAuth,
        paid: false,
        components: &[
            comp("access_token", "gsc/access_token", "GSC_ACCESS_TOKEN"),
            comp(
                "service_account",
                "gsc/service-account.json",
                "GSC_SERVICE_ACCOUNT_FILE",
            ),
        ],
    },
    Provider {
        id: "ga4",
        title: "Google Analytics 4",
        access: Access::OAuth,
        paid: false,
        components: &[comp("access_token", "ga4/access_token", "GA4_ACCESS_TOKEN")],
    },
    Provider {
        id: "pagespeed",
        title: "PageSpeed / CrUX",
        access: Access::ApiKey,
        paid: false,
        components: &[
            comp("api_key", "pagespeed/api_key", "PAGESPEED_API_KEY"),
            comp("crux_key", "crux/api_key", "CRUX_API_KEY"),
        ],
    },
    Provider {
        id: "bing_webmaster",
        title: "Bing Webmaster",
        access: Access::ApiKey,
        paid: false,
        components: &[comp(
            "api_key",
            "bing-webmaster/api_key",
            "BING_WEBMASTER_API_KEY",
        )],
    },
    Provider {
        id: "yandex_webmaster",
        title: "Яндекс Вебмастер",
        access: Access::OAuth,
        paid: false,
        components: &[comp(
            "access_token",
            "yandex-webmaster/access_token",
            "YANDEX_WEBMASTER_TOKEN",
        )],
    },
    Provider {
        id: "yandex_metrika",
        title: "Яндекс Метрика",
        access: Access::OAuth,
        paid: false,
        components: &[comp(
            "token",
            "yandex-metrika/token",
            "YANDEX_METRIKA_TOKEN",
        )],
    },
    Provider {
        id: "yandex_wordstat",
        title: "Яндекс Облако / Wordstat",
        access: Access::ApiKey,
        paid: true,
        components: &[
            comp("api_key", "yandex-wordstat/api_key", "YANDEX_CLOUD_API_KEY"),
            comp(
                "folder_id",
                "yandex-wordstat/folder_id",
                "YANDEX_CLOUD_FOLDER_ID",
            ),
        ],
    },
    Provider {
        id: "topvisor",
        title: "Topvisor",
        access: Access::ApiKey,
        paid: true,
        components: &[
            comp("access_token", "topvisor/access_token", "TOPVISOR_TOKEN"),
            comp("user_id", "topvisor/user_id", "TOPVISOR_USER_ID"),
        ],
    },
    Provider {
        id: "dataforseo",
        title: "DataForSEO",
        access: Access::ApiKey,
        paid: true,
        components: &[
            comp("login", "dataforseo/login", "DATAFORSEO_LOGIN"),
            comp("password", "dataforseo/password", "DATAFORSEO_PASSWORD"),
        ],
    },
    Provider {
        id: "arsenkin",
        title: "Arsenkin",
        access: Access::ApiKey,
        paid: true,
        components: &[comp("token", "arsenkin/token", "ARSENKIN_TOKEN")],
    },
    Provider {
        id: "ahrefs",
        title: "Ahrefs",
        access: Access::ApiKey,
        paid: true,
        components: &[comp("token", "ahrefs/token", "AHREFS_TOKEN")],
    },
    Provider {
        id: "majestic",
        title: "Majestic",
        access: Access::ApiKey,
        paid: true,
        components: &[comp("api_key", "majestic/api_key", "MAJESTIC_API_KEY")],
    },
    Provider {
        id: "moz",
        title: "Moz",
        access: Access::ApiKey,
        paid: true,
        components: &[comp("api_key", "moz/api_key", "MOZ_API_KEY")],
    },
    Provider {
        id: "wayback",
        title: "Wayback / crt.sh",
        access: Access::Public,
        paid: false,
        components: &[],
    },
    Provider {
        id: "crawler",
        title: "Локальный краулер",
        access: Access::Public,
        paid: false,
        components: &[],
    },
];

/// Find a provider by id.
pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// `sources.list` payload: every provider with per-component set/not-set state
/// and the overall readiness. Values are never read or returned.
pub fn sources_list(host: &dyn Host, root: &Path) -> Value {
    let list: Vec<Value> = providers()
        .iter()
        .map(|p| {
            let components: Map<String, Value> = p
                .components
                .iter()
                .map(|c| {
                    (
                        c.name.to_string(),
                        json!(spiderhead_sources::credentials::available(
                            host, root, c.path, c.env_var
                        )),
                    )
                })
                .collect();
            let total = components.len();
            let set = components
                .values()
                .filter(|v| v == &&Value::Bool(true))
                .count();
            let state = match p.access {
                Access::Public => "not_required",
                _ if set == total && total > 0 => "configured_unverified",
                _ if set == 0 => "not_configured",
                _ => "invalid",
            };
            let access = match p.access {
                Access::OAuth => "oauth",
                Access::ApiKey => "api_key",
                Access::ServiceAccount => "service_account",
                Access::Public => "public",
            };
            json!({
                "id": p.id,
                "title": p.title,
                "access": access,
                "paid": p.paid,
                "state": state,
                "components": components,
            })
        })
        .collect();
    Value::Array(list)
}

/// One credential write or revoke through the credential API (`set` fails
/// closed on an empty value; `replace` keeps parity with the reference store).
pub fn credential_action(
    root: &Path,
    provider_id: &str,
    component: &str,
    action: &str,
    value: &str,
) -> Result<Value, String> {
    let p = provider(provider_id).ok_or_else(|| format!("unknown provider {provider_id:?}"))?;
    let c = p
        .components
        .iter()
        .find(|c| c.name == component)
        .ok_or_else(|| format!("provider {provider_id:?} has no component {component:?}"))?;
    match action {
        "set" => {
            let replace = root.join(c.path).is_file();
            spiderhead_sources::credentials::write_secret(root, c.path, value, replace)
                .map_err(|e| e.to_string())?;
            Ok(json!({"provider": p.id, "component": c.name, "set": true}))
        }
        "revoke" => {
            let removed = spiderhead_sources::credentials::revoke_secret(root, c.path)
                .map_err(|e| e.to_string())?;
            Ok(json!({"provider": p.id, "component": c.name, "set": !removed}))
        }
        other => Err(format!(
            "unknown credential action {other:?} (set or revoke)"
        )),
    }
}
