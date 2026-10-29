//! The menu-bar spec: which menus exist, what each row is labelled, which
//! accelerator it shows and which control command it dispatches. The spec is
//! plain data — `ui::menubar` renders it natively on macOS and the tests can
//! check every item against the command registry without a window.

use serde_json::Value;

/// One top-level menu («Файл», «Вид», …).
pub struct MenuSpec {
    /// Visible submenu title.
    pub title: &'static str,
    /// Rows top to bottom.
    pub items: &'static [ItemSpec],
}

/// One menu row.
pub enum ItemSpec {
    /// A separator line.
    Sep,
    /// An action row.
    Item {
        /// Unique menu id — `menus::lookup` resolves it back to the call.
        id: &'static str,
        /// Visible label.
        label: &'static str,
        /// Accelerator in muda syntax (`"CmdOrCtrl+N"`); `""` shows none.
        accel: &'static str,
        /// Control method to dispatch.
        method: &'static str,
        /// Params as a static JSON object.
        params: &'static str,
    },
    /// A nested submenu.
    Sub {
        /// Visible label.
        label: &'static str,
        /// Rows of the submenu.
        items: &'static [ItemSpec],
    },
}

/// One action row of a menu spec.
const fn item(
    id: &'static str,
    label: &'static str,
    accel: &'static str,
    method: &'static str,
    params: &'static str,
) -> ItemSpec {
    ItemSpec::Item {
        id,
        label,
        accel,
        method,
        params,
    }
}

/// The whole menu bar, left to right.
pub fn menus() -> &'static [MenuSpec] {
    MENUS
}

/// Resolve a menu id to `(method, params)` for `AppModel::dispatch`.
/// Unknown ids return `None` — a menu click must never be a panic path.
pub fn lookup(id: &str) -> Option<(&'static str, Value)> {
    for menu in MENUS {
        if let Some(found) = lookup_in(menu.items, id) {
            return Some(found);
        }
    }
    None
}

fn lookup_in(items: &[ItemSpec], id: &str) -> Option<(&'static str, Value)> {
    for item in items {
        match item {
            ItemSpec::Item {
                id: mine,
                method,
                params,
                ..
            } if *mine == id => {
                let params = serde_json::from_str(params).unwrap_or(Value::Null);
                return Some((*method, params));
            }
            ItemSpec::Sub { items, .. } => {
                if let Some(found) = lookup_in(items, id) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

use ItemSpec::Sep;

static MENUS: &[MenuSpec] = &[
    MenuSpec {
        title: "SpiderHead",
        items: &[
            item(
                "app.about",
                "О программе",
                "",
                "ui.window",
                r#"{"id":"about"}"#,
            ),
            Sep,
            item(
                "app.settings",
                "Настройки…",
                "CmdOrCtrl+Comma",
                "ui.window",
                r#"{"id":"settings"}"#,
            ),
            Sep,
            item(
                "app.install_cli",
                "Установить команду в терминал",
                "",
                "app.install_cli",
                "{}",
            ),
            item(
                "app.connect_claude",
                "Подключить к Claude",
                "",
                "app.connect_claude",
                "{}",
            ),
            Sep,
            item(
                "app.quit",
                "Завершить SpiderHead",
                "CmdOrCtrl+Q",
                "app.quit",
                "{}",
            ),
        ],
    },
    MenuSpec {
        title: "Файл",
        items: &[
            item(
                "file.new_crawl",
                "Новый краул",
                "CmdOrCtrl+N",
                "ui.mode",
                r#"{"mode":"crawler","nav":"crawler"}"#,
            ),
            item(
                "file.open",
                "Открыть скан…",
                "CmdOrCtrl+O",
                "scan.open",
                r#"{"dialog":true}"#,
            ),
            Sep,
            item("file.close_scan", "Закрыть скан", "", "scan.close", "{}"),
            Sep,
            item(
                "file.export",
                "Экспорт таблицы в TSV",
                "CmdOrCtrl+E",
                "table.copy",
                "{}",
            ),
        ],
    },
    MenuSpec {
        title: "Правка",
        items: &[
            item(
                "edit.undo",
                "Отменить",
                "CmdOrCtrl+Z",
                "edit.clipboard",
                r#"{"action":"undo"}"#,
            ),
            Sep,
            item(
                "edit.cut",
                "Вырезать",
                "CmdOrCtrl+X",
                "edit.clipboard",
                r#"{"action":"cut"}"#,
            ),
            item(
                "edit.copy",
                "Копировать",
                "CmdOrCtrl+C",
                "edit.clipboard",
                r#"{"action":"copy"}"#,
            ),
            item(
                "edit.paste",
                "Вставить",
                "CmdOrCtrl+V",
                "edit.clipboard",
                r#"{"action":"paste"}"#,
            ),
            item(
                "edit.select_all",
                "Выбрать всё",
                "CmdOrCtrl+A",
                "edit.clipboard",
                r#"{"action":"select_all"}"#,
            ),
            Sep,
            item(
                "edit.find",
                "Найти в таблице",
                "CmdOrCtrl+F",
                "ui.find",
                "{}",
            ),
        ],
    },
    MenuSpec {
        title: "Вид",
        items: &[
            item(
                "view.sidebar",
                "Навигация",
                "CmdOrCtrl+0",
                "ui.sidebar",
                "{}",
            ),
            item(
                "view.details",
                "Детали",
                "Alt+CmdOrCtrl+D",
                "details.toggle",
                "{}",
            ),
            Sep,
            ItemSpec::Sub {
                label: "Тема",
                items: &[
                    item(
                        "view.theme.light",
                        "Светлая",
                        "",
                        "ui.theme",
                        r#"{"mode":"light"}"#,
                    ),
                    item(
                        "view.theme.dark",
                        "Тёмная",
                        "",
                        "ui.theme",
                        r#"{"mode":"dark"}"#,
                    ),
                    item(
                        "view.theme.hc",
                        "Контрастная",
                        "",
                        "ui.theme",
                        r#"{"mode":"hc"}"#,
                    ),
                ],
            },
            Sep,
            item(
                "view.refresh",
                "Обновить",
                "F5",
                "table.filter",
                r#"{"reload":true}"#,
            ),
        ],
    },
    MenuSpec {
        title: "Переход",
        items: &[
            item(
                "go.crawler",
                "Краулер",
                "CmdOrCtrl+1",
                "ui.mode",
                r#"{"mode":"crawler"}"#,
            ),
            item(
                "go.logs",
                "Анализ логов",
                "CmdOrCtrl+2",
                "ui.mode",
                r#"{"mode":"logs"}"#,
            ),
            item(
                "go.agent",
                "Агентский режим",
                "CmdOrCtrl+3",
                "ui.mode",
                r#"{"mode":"agent"}"#,
            ),
            Sep,
            item(
                "go.start",
                "Выбор режима",
                "",
                "ui.mode",
                r#"{"mode":"start"}"#,
            ),
            Sep,
            item(
                "go.palette",
                "Действия и переходы",
                "CmdOrCtrl+K",
                "ui.window",
                r#"{"id":"palette"}"#,
            ),
        ],
    },
    MenuSpec {
        title: "Окно",
        items: &[
            item(
                "win.minimize",
                "Свернуть",
                "CmdOrCtrl+M",
                "window.minimize",
                "{}",
            ),
            item(
                "win.fullscreen",
                "Во весь экран",
                "Ctrl+CmdOrCtrl+F",
                "window.fullscreen",
                "{}",
            ),
        ],
    },
    MenuSpec {
        title: "Справка",
        items: &[
            item(
                "help.help",
                "Справка SpiderHead",
                "F1",
                "ui.window",
                r#"{"id":"help"}"#,
            ),
            item(
                "help.shortcuts",
                "Горячие клавиши",
                "CmdOrCtrl+Slash",
                "ui.window",
                r#"{"id":"shortcuts"}"#,
            ),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_item_dispatches_a_registered_command() {
        let commands: std::collections::BTreeSet<String> = crate::commands::registry(true)
            .into_iter()
            .map(|c| c.id)
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        fn collect<'a>(items: &'a [ItemSpec], out: &mut Vec<&'a ItemSpec>) {
            for item in items {
                match item {
                    ItemSpec::Sub { items, .. } => collect(items, out),
                    other => out.push(other),
                }
            }
        }
        for menu in MENUS {
            let mut flat = Vec::new();
            collect(menu.items, &mut flat);
            for item in flat {
                let ItemSpec::Item {
                    id, method, params, ..
                } = item
                else {
                    continue;
                };
                assert!(seen.insert(*id), "duplicate menu id {id}");
                assert!(
                    commands.contains(*method),
                    "menu item {id} calls unregistered {method}"
                );
                assert!(
                    serde_json::from_str::<Value>(params).is_ok(),
                    "menu item {id} has bad params JSON"
                );
            }
        }
    }

    #[test]
    fn lookup_resolves_ids_and_params() {
        let (method, params) = lookup("go.logs").unwrap();
        assert_eq!(method, "ui.mode");
        assert_eq!(params["mode"], "logs");
        assert!(lookup("no.such.item").is_none());
    }
}
