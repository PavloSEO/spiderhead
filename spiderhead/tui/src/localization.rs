//! Terminal UI labels; user evidence and command identifiers remain verbatim.
//!
use std::sync::RwLock;

/// Currently selected UI language.
static LANGUAGE: RwLock<&'static str> = RwLock::new("en");

/// Select the UI language for this process.
pub fn set_language(code: &'static str) {
    *LANGUAGE.write().unwrap_or_else(|e| e.into_inner()) = code;
}

/// The active language code.
pub fn language() -> String {
    LANGUAGE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .to_string()
}

/// Reset the language to English; test helper for the global-state fixture.
pub fn reset_language() {
    set_language("en");
}

static RU: &[(&str, &str)] = &[
    ("Finalizing", "Завершение"),
    ("Spider", "Сайт целиком"),
    ("List", "Список URL"),
    ("SF scans", "Сканы SF"),
    ("Mode", "Режим"),
    ("In flight", "В работе"),
    ("critical", "критично"),
    ("warning", "внимание"),
    ("notice", "замечания"),
    ("no active workflow", "нет активного сценария"),
    ("Next", "Далее"),
    ("rate unavailable", "скорость не измерялась"),
    (
        "initialize the checklist and record the agreed audit plan",
        "создать список задач и записать согласованный план аудита",
    ),
    ("Finished", "Завершён"),
    ("Retained", "Сохранённое наблюдение"),
    ("interactive shell", "интерактивный терминал"),
    ("PROJECT OBSERVER", "НАБЛЮДЕНИЕ ПРОЕКТА"),
    (
        "SEOHEAD  /  PROJECT OBSERVER",
        "SEOHEAD  /  НАБЛЮДЕНИЕ ПРОЕКТА",
    ),
    ("READ ONLY  ·  LOCAL", "ТОЛЬКО ЧТЕНИЕ  ·  ЛОКАЛЬНО"),
    ("NOTE DRAFT  ·  LOCAL", "ЧЕРНОВИК ЗАМЕТКИ  ·  ЛОКАЛЬНО"),
    ("WORKSPACE", "ПРОЕКТ"),
    ("Local evidence", "Сохранённые данные"),
    ("n  Add note", "n  Заметка"),
    ("g  Propose goal", "g  Предложить цель"),
    ("Pages retained", "Сохранено страниц"),
    ("Findings", "Находки"),
    ("Saved scans", "Сканы"),
    ("Collection & scope", "Скан сейчас"),
    ("Scenarios & skills", "Сценарии и методы"),
    ("Findings & notes", "Проблемы и заметки"),
    ("Scenarios", "Сценарии"),
    ("Skills", "Методы"),
    ("CURRENT WORK", "ЗАДАЧИ В РАБОТЕ"),
    ("NEXT ACTIONS", "СЛЕДУЮЩИЕ ДЕЙСТВИЯ"),
    ("FINDINGS BY SEVERITY", "ПРОБЛЕМЫ ПО ВАЖНОСТИ"),
    ("PROJECT NOTES", "ВХОДЯЩИЕ"),
    ("COMPETITORS", "КОНКУРЕНТЫ"),
    ("WORKFLOW", "СЦЕНАРИЙ"),
    ("PROJECT LOG", "ЖУРНАЛ"),
    ("No competitors configured", "Конкуренты не указаны"),
    ("n  Write or dictate a note", "n  Написать заметку"),
    (
        "No agent task recorded · 2 opens the complete checklist",
        "Задач агента нет · 2 открывает все задачи",
    ),
    ("No project run recorded", "Запусков проекта нет"),
    ("Not measured", "Не измерялось"),
    ("None in agreed scope", "Нет в согласованной области"),
    (
        "Discovered URL coverage · scope can grow",
        "Обработано найденных URL · область может расти",
    ),
    (
        "Run URL budget · not whole-site completion",
        "Лимит URL запуска · полнота сайта не измерена",
    ),
    ("Retained evidence", "Сохранённые данные"),
    ("Stale sample", "Устаревшее наблюдение"),
    (
        "Loading retained project evidence…",
        "Чтение данных проекта…",
    ),
    ("Loading retained entries…", "Чтение записей…"),
    ("Project", "Проект"),
    ("Compose note", "Заметка"),
    ("Overview", "Обзор"),
    ("Tasks", "Задачи"),
    ("Methods", "Методы"),
    ("Scans", "Сканы"),
    ("Views", "Виды"),
    ("Activity", "Агент"),
    ("Log", "Журнал"),
    ("Inbox", "Входящие"),
    ("Sites", "Сайты"),
    ("Schema", "Разметка"),
    ("Not recorded", "Нет данных"),
    ("Not started", "Не запускался"),
    ("In progress", "В работе"),
    ("Complete", "Завершён"),
    ("Partial", "Частичный"),
    ("Succeeded", "Успешно"),
    ("Blocked", "Заблокирован"),
    ("Failed", "Ошибка"),
    ("Unavailable", "Недоступно"),
    ("Native crawler", "Встроенный краулер"),
    ("Evidence needs refresh", "Данные устарели"),
    ("Needs review", "Нужно проверить"),
    ("Ready to start", "Готово к запуску"),
    ("Evidence recorded", "Данные сохранены"),
    ("Outside agreed scope", "Вне согласованной области"),
    ("Not applicable", "Не применяется"),
    ("Proposed goal", "Предложенная цель"),
    ("Fresh", "Актуально"),
    ("Stale", "Устарело"),
    ("Stopped", "Остановлен"),
    ("Goal", "Цель"),
    ("Scan now", "Скан сейчас"),
    ("Queue", "Очередь"),
    ("Notes", "Заметки"),
    ("Competitors", "Конкуренты"),
    ("Discovered URL coverage", "Обработано найденных URL"),
    ("Sitemap", "Карта сайта"),
    ("Errors", "Ошибки"),
    ("Excluded", "Исключено"),
    ("No scan recorded", "Сканов нет"),
    ("No matching scan", "Запуск не найден"),
    ("Loading…", "Чтение…"),
    ("unknown", "нет данных"),
    ("observed active", "наблюдается активный запуск"),
    (
        "n Note   g Goal   ? Keys   q Quit",
        "n Заметка   g Цель   ? Клавиши   q Выход",
    ),
    (
        "2 Task details   3 Methods   a Structured data   p Sites\n",
        "2 Задачи   3 Методы   a Разметка   p Сайты\n",
    ),
    (
        "↑ ↓ Browse   Enter Details   PgUp/PgDn Page   f Filter\n",
        "↑ ↓ Выбор   Enter Детали   PgUp/PgDn Страница   f Фильтр\n",
    ),
];

/// `resolve_language`: explicit value wins, else the process locale; `ru`/`be` map to `ru`.
///
/// The env chain is `LC_ALL`, `LC_MESSAGES`, `LANG`, then the process locale
/// (always `C` here — a Rust process never calls `setlocale`), then `en`.
pub fn resolve_language(value: Option<&str>) -> Result<String, crate::error::TuiError> {
    if let Some(v) = value {
        if v != "ru" && v != "en" {
            return Err(crate::error::TuiError::Language(
                "language must be ru or en".into(),
            ));
        }
        return Ok(v.to_string());
    }
    let local = std::env::var("LC_ALL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("LC_MESSAGES").ok().filter(|s| !s.is_empty()))
        .or_else(|| std::env::var("LANG").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "en".into());
    Ok(
        if local.to_lowercase().starts_with("ru") || local.to_lowercase().starts_with("be") {
            "ru"
        } else {
            "en"
        }
        .to_string(),
    )
}

/// Group thousands: `1,330` in English, `1 330` in Russian.
pub fn number(value: i64) -> String {
    let grouped = group_thousands(value);
    if language() == "ru" {
        grouped.replace(',', " ")
    } else {
        grouped
    }
}

/// `f"{value:,}"` — comma-separated groups of three, sign kept.
fn group_thousands(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    format!("{sign}{out}")
}

/// UI string for the current language.
pub fn ui(value: &str) -> String {
    if language() != "ru" {
        return value.to_string();
    }
    RU.iter()
        .find(|(key, _)| *key == value)
        .map(|(_, v)| (*v).to_string())
        .unwrap_or_else(|| value.to_string())
}
