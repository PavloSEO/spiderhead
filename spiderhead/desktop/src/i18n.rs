//! Russian UI strings in one place. The window is Russian-first; other locales
//! hang off the same keys later. Nothing here is user data: only static labels.

#![allow(dead_code)]

/// Application window title.
pub const APP_TITLE: &str = "SpiderHead";

// ---- shell / navigation ----------------------------------------------------

/// Left navigation, URL table entry.
pub const NAV_URLS: &str = "URL";
/// Left navigation, findings dashboard entry.
pub const NAV_DASHBOARD: &str = "Проблемы";
/// Left navigation, inbox placeholder.
pub const NAV_INBOX: &str = "Входящие";
/// Left navigation, projects placeholder.
pub const NAV_PROJECTS: &str = "Проекты";
/// Caption above the nav sections.
pub const NAV_SCAN_SECTION: &str = "СКАН";

/// Profile button name (bottom-left corner).
pub const PROFILE_NAME: &str = "Павел";
/// Profile menu location line.
pub const PROFILE_WHERE: &str = "локально · этот компьютер";
/// Profile menu: product-mode section header.
pub const PROFILE_MODE: &str = "РЕЖИМ";
/// Product mode: site crawler.
pub const MODE_CRAWLER: &str = "Краулер";
/// Badge on a launcher card for a mode that is not released yet.
pub const START_SOON: &str = "Скоро";
/// Launcher button that opens the test-build download page.
pub const START_DOWNLOAD: &str = "Скачать тестовую версию";
/// Product mode: server-log analysis.
pub const MODE_LOGS: &str = "Анализ логов";
/// Product mode: agent workspace (short label).
pub const MODE_AGENT: &str = "Проект";
/// Start-screen card: agent mode, full title.
pub const MODE_AGENT_CARD: &str = "Агентский режим (проект)";
/// Start screen: the chooser itself (`ui.mode` id `start`).
pub const MODE_START: &str = "Выбор режима";
/// Human label of a `ui.mode` id (unknown ids fall back to agent).
pub fn mode_label(mode: &str) -> &'static str {
    match mode {
        "crawler" => MODE_CRAWLER,
        "logs" => MODE_LOGS,
        "start" => MODE_START,
        _ => MODE_AGENT,
    }
}
/// Profile menu item: usage/limits window.
pub const MENU_USAGE: &str = "Использование";
/// Profile menu item: about/details window.
pub const MENU_MORE: &str = "Подробнее";
/// Profile menu item: sign out / quit.
pub const MENU_LOGOUT: &str = "Выйти";
/// Profile menu item: current workspace.
pub const MENU_WORKSPACE_LOCAL: &str = "Локальное · этот компьютер";
/// Profile menu item: settings.
pub const MENU_SETTINGS: &str = "Настройки";
/// Profile menu item: theme submenu.
pub const MENU_THEME: &str = "Тема";
/// Profile menu item: language submenu.
pub const MENU_LANGUAGE: &str = "Язык";
/// Profile menu item: MCP server.
pub const MENU_MCP: &str = "MCP-сервер";
/// Profile menu item: command line dialog.
pub const MENU_CLI: &str = "Командная строка";
/// Profile menu item: help.
pub const MENU_HELP: &str = "Справка";
/// Theme option: light.
pub const THEME_LIGHT: &str = "Светлая";
/// Theme option: dark.
pub const THEME_DARK: &str = "Тёмная";
/// Theme option: high contrast.
pub const THEME_HC: &str = "Контрастная";
/// Theme option: follow the OS appearance (`view.theme = system`).
pub const THEME_SYSTEM: &str = "Как в системе";
/// Label of a theme in menus and the status bar.
pub fn theme_label(theme: crate::theme::Theme) -> &'static str {
    use crate::theme::Theme;
    match theme {
        Theme::Light => THEME_LIGHT,
        Theme::Dark => THEME_DARK,
        Theme::HighContrast => THEME_HC,
    }
}
/// Language option: Russian.
pub const LANGUAGE_RU: &str = "Русский";

// ---- status bar ------------------------------------------------------------

/// Status bar when nothing is open.
pub const STATUS_NO_SCAN: &str = "Скан не открыт";
/// Status bar segment prefix for the control channel.
pub const STATUS_CONTROL: &str = "канал управления";

// ---- URL table screen ------------------------------------------------------

/// Empty state: no scan opened yet.
pub const TABLE_EMPTY_TITLE: &str = "Нет открытого скана";
/// Empty state: how to open one.
pub const TABLE_EMPTY_HINT: &str =
    "Откройте scan.sqlite: Файл ▸ Открыть скан, либо команда scan.open канала управления.";
/// Filter bar: preset combo caption.
pub const FILTER_PRESET: &str = "Предустановка";
/// Filter bar: free-text search placeholder.
pub const FILTER_SEARCH: &str = "Фильтр по URL…";
/// Filter bar: apply button.
pub const FILTER_APPLY: &str = "Применить";
/// Filter bar: reset button.
pub const FILTER_RESET: &str = "Сбросить";
/// Status segment: rows shown / total.
pub fn table_counts(filtered: Option<i64>, total: i64) -> String {
    match filtered {
        Some(f) if f != total => format!("Строк: {f} из {total}"),
        _ => format!("Строк: {total}"),
    }
}
/// Filtered-total state is capped by its count budget.
pub const FILTERED_CAPPED: &str = "точное число строк не известно (лимит времени)";
/// Sort on an unindexed column of a large set was refused.
pub fn sort_refused(column: &str) -> String {
    format!("Сортировка по «{column}» недоступна: нужен фильтр не более чем на 100 000 строк")
}

/// Column header: URL.
pub const COL_URL: &str = "Адрес";
/// Column header: HTTP status.
pub const COL_STATUS: &str = "Код";
/// Column header: content type.
pub const COL_CONTENT_TYPE: &str = "Тип";
/// Column header: crawl depth.
pub const COL_CRAWL_DEPTH: &str = "Глубина";
/// Column header: page title.
pub const COL_TITLE: &str = "Title";
/// Column header: first H1.
pub const COL_H1: &str = "H1";
/// Column header: word count.
pub const COL_WORDS: &str = "Слов";
/// Column header: size.
pub const COL_SIZE: &str = "Размер";
/// Column header: response time.
pub const COL_TIME: &str = "Время";
/// Column header: indexable flag.
pub const COL_INDEXABLE: &str = "Индекс.";
/// Column header: meta description.
pub const COL_META_DESC: &str = "Description";
/// Column header: canonical.
pub const COL_CANONICAL: &str = "Canonical";
/// Column header: inlinks.
pub const COL_INLINKS: &str = "Вх. ссылки";
/// Column header: outlinks.
pub const COL_OUTLINKS: &str = "Исх. ссылки";
/// Column header: issue count.
pub const COL_ISSUES: &str = "Находок";
/// Column header: page ordinal.
pub const COL_ORDINAL: &str = "№";

/// Human label of a `data::columns` id, following the table-column registry
/// vocabulary (Address, Status Code, Crawl Depth…) translated for the
/// Russian-first window. Unknown ids render as themselves — callers always
/// pass allow-listed names.
pub fn column_label(name: &str) -> &str {
    match name {
        "url_id" => "ID",
        "url" => COL_URL,
        "page_ordinal" => COL_ORDINAL,
        "status_code" => "Код ответа",
        "status_class" => "Класс ответа",
        "content_type" => "Тип содержимого",
        "size_bytes" => COL_SIZE,
        "response_time" => "Время ответа",
        "redirect_url" => "URL редиректа",
        "final_url" => "Финальный URL",
        "title" => "Title",
        "title_length" => "Длина Title",
        "title_duplicate" => "Дубль Title",
        "meta_description" => "Description 1",
        "meta_description_length" => "Длина Description",
        "meta_description_count" => "Description, шт.",
        "description_duplicate" => "Дубль Description",
        "h1" => "H1-1",
        "h1_2" => "H1-2",
        "h1_length" => "Длина H1",
        "h2" => "H2-1",
        "canonical" => "Canonical",
        "final_canonical" => "Canonical фин.",
        "canonical_self" => "Canonical на себя",
        "canonical_other" => "Canonical на другой",
        "canonical_non_indexable" => "Canonical неиндексируем",
        "meta_robots" => "Meta Robots",
        "x_robots" => "X-Robots-Tag",
        "og_title" => "OG Title",
        "og_description" => "OG Description",
        "og_image" => "OG Image",
        "word_count" => COL_WORDS,
        "text_ratio" => "Доля текста",
        "crawl_depth" => COL_CRAWL_DEPTH,
        "content_encoding" => "Сжатие",
        "charset" => "Кодировка",
        "outlinks" => COL_OUTLINKS,
        "external_outlinks" => "Внешние исх.",
        "inlinks" => COL_INLINKS,
        "images_total" => "Изображений",
        "images_missing_alt_attr" => "Без alt",
        "jsonld_blocks_found" => "Блоков JSON-LD",
        "has_hreflang" => "Hreflang",
        "in_sitemap" => "В sitemap",
        "robots_blocked" => "Запрет robots",
        "issue_count" => COL_ISSUES,
        "indexable" => "Индексируемость",
        "error" => "Ошибка",
        "error_kind" => "Вид ошибки",
        "cache_status" => "Кэш",
        "representation" => "Представление",
        _ => name,
    }
}

// ---- dashboard -------------------------------------------------------------

/// Dashboard: severity card, critical.
pub const SEV_CRITICAL: &str = "Критичные";
/// Dashboard: severity card, warning.
pub const SEV_WARNING: &str = "Предупреждения";
/// Dashboard: severity card, notice.
pub const SEV_NOTICE: &str = "Замечания";
/// Dashboard: severity card, opportunity.
pub const SEV_OPPORTUNITY: &str = "Возможности";
/// Dashboard: findings section caption.
pub const DASH_CHECKS: &str = "Проверки";
/// Dashboard: empty state without audit.
pub const DASH_NO_AUDIT: &str = "Нет сохранённого аудита";
/// Dashboard: hint when the companion is missing.
pub const DASH_NO_AUDIT_HINT: &str =
    "Рядом со scan.sqlite нет audit.v2-компаньона; показаны только данные скана.";
/// Dashboard: column, check name.
pub const DASH_COL_CHECK: &str = "Проверка";
/// Dashboard: column, severity.
pub const DASH_COL_SEVERITY: &str = "Важность";
/// Dashboard: column, findings.
pub const DASH_COL_FINDINGS: &str = "Находок";
/// Dashboard: column, state.
pub const DASH_COL_STATE: &str = "Состояние";
/// Dashboard: check state, fired.
pub const DASH_STATE_FIRED: &str = "сработала";
/// Dashboard: check state, silent.
pub const DASH_STATE_SILENT: &str = "без находок";
/// Dashboard: check state, skipped.
pub const DASH_STATE_SKIPPED: &str = "пропущена";
/// Dashboard: check state, disabled.
pub const DASH_STATE_DISABLED: &str = "выключена";
/// Dashboard: rollup-group counter label.
pub const DASH_GROUPS: &str = "групп";

// ---- details panel ---------------------------------------------------------

/// Details tab: name/value page fields.
pub const TAB_DETAILS: &str = "Сводка";
/// Details tab: inbound links.
pub const TAB_INLINKS: &str = "Входящие ссылки";
/// Details tab: outbound links.
pub const TAB_OUTLINKS: &str = "Исходящие ссылки";
/// Details tab: HTTP response.
pub const TAB_RESPONSE: &str = "Ответ";
/// Details tab: retained body.
pub const TAB_BODY: &str = "Тело";
/// Details: nothing selected.
pub const DETAILS_EMPTY: &str = "Выберите строку таблицы";
/// Details: no retained body.
pub const DETAILS_NO_BODY: &str = "Тело не сохранено в этом скане";
/// Details: no links in this direction.
pub const DETAILS_NO_LINKS: &str = "Ссылки не сохранены";
/// Links table column: counterpart URL.
pub const LINKS_COL_URL: &str = "Адрес";
/// Links table column: anchor text.
pub const LINKS_COL_ANCHOR: &str = "Анкор";
/// Links table column: link position.
pub const LINKS_COL_POSITION: &str = "Позиция";
/// Links table column: nofollow flag.
pub const LINKS_COL_NOFOLLOW: &str = "nofollow";
/// Links tab: the data cap kicked in, not all rows drawn.
pub fn links_shown(returned: usize, total: i64) -> String {
    format!("Показано {returned} из {total}")
}
/// Details: rows hidden by the panel's own display cap.
pub fn details_more(n: usize) -> String {
    format!("…и ещё {n}")
}
/// Response tab: request header group.
pub const RESP_REQUEST_HEADERS: &str = "Заголовки запроса";
/// Response tab: response header group.
pub const RESP_RESPONSE_HEADERS: &str = "Заголовки ответа";
/// Response tab: headers of the final (redirected) response.
pub const RESP_EFFECTIVE_HEADERS: &str = "Итоговые заголовки";
/// Response tab: redirect chain caption.
pub const RESP_REDIRECTS: &str = "Цепочка редиректов";
/// Body tab meta line: size, representation, display cap.
pub fn body_meta(bytes: usize, representation: &str, capped: bool) -> String {
    if capped {
        format!("{bytes} байт · {representation} · сохранено с обрезкой 512 КиБ")
    } else {
        format!("{bytes} байт · {representation}")
    }
}

/// Labels for details-payload keys that are not table-column ids
/// (`column_label` covers the rest; unknown names render as themselves).
pub fn field_label(name: &str) -> &str {
    match name {
        "method" => "Метод",
        "effective_url" => "Финальный URL",
        "effective_status_code" => "Итоговый код",
        "reported_size_bytes" => "Размер ответа",
        "requested_at" => "Запрошен",
        "received_at" => "Получен",
        _ => column_label(name),
    }
}

// ---- simple mode -----------------------------------------------------------

/// Simple page: field label for the start URL.
pub const SIMPLE_URL_LABEL: &str = "Адрес страницы";
/// Simple page: start button.
pub const SIMPLE_START: &str = "Проверить";
/// Simple page: what it does.
pub const SIMPLE_HINT: &str =
    "Введите адрес страницы — покажу, что о нём записано в открытом скане.";
/// Simple page heading.
pub const SIMPLE_TITLE: &str = "Проверка сайта";
/// Simple page: open-scans row caption.
pub const SIMPLE_OPEN_SCAN: &str = "Открыть файл скана…";
/// Simple page: nothing open to look into.
pub const SIMPLE_NO_SCAN: &str = "Нет открытого скана — откройте scan.sqlite в разделе «Краул».";
/// Simple page: the URL matched no row.
pub const SIMPLE_NOT_FOUND: &str = "Адреса нет в открытом скане.";
/// Simple page: near matches caption.
pub const SIMPLE_SIMILAR: &str = "Точного совпадения нет — похожие:";
/// Simple page: reveal the row in the agent table.
pub const SIMPLE_OPEN_TABLE: &str = "Показать в таблице";

// ---- profile-menu windows and the crawler page -----------------------------

/// Crawler page: heading (profile-menu target).
pub const CRAWLER_TITLE: &str = "Новый краул без проекта";
/// Crawler page: the engine is a later slice.
pub const CRAWLER_PENDING: &str = "Краул пока недоступен. Откройте готовый скан.";
/// Crawler page: scan-path field label.
pub const CRAWLER_OPEN_LABEL: &str = "Путь к scan.sqlite";
/// Crawler page: open button.
pub const CRAWLER_OPEN: &str = "Открыть скан";
/// Settings window: language row value.
pub const SETTINGS_LANGUAGE: &str = "Русский";
/// MCP window: bounded state.
pub const MCP_UNAVAILABLE: &str = "MCP-сервер в это окно не встроен.";
/// MCP window: how to start it.
pub const MCP_HINT: &str = "Запуск из терминала:";
/// CLI window: what the channel is.
pub const CLI_NOTE: &str =
    "Окном управляет канал команд; тот же интерфейс доступен в терминале как \
     spiderhead-desktop-ctl.";
/// CLI window: hint for the full command list.
pub const CLI_COMMANDS_HINT: &str = "Список команд канала: spiderhead-desktop-ctl commands";
/// CLI window: collapsed block with the resolved binary path.
pub const CLI_DETAILS: &str = "Подробнее";
/// CLI window: copy button.
pub const CLI_COPY: &str = "Скопировать";
/// Help window: about line.
pub fn help_version(version: &str) -> String {
    format!("SpiderHead Desktop · Rust · {version}")
}
/// Help window: command-surface hint.
pub const HELP_COMMANDS: &str =
    "Каждое действие окна — команда канала; полный список: commands.list";
/// Help window: the one agent hotkey.
pub const HELP_HOTKEY: &str = "F5 — обновить таблицу";
/// Status: a copy action succeeded.
pub const COPIED: &str = "Скопировано";

// ---- misc ------------------------------------------------------------------

/// Generic placeholder for unbuilt sections.
pub const SECTION_PENDING: &str = "Раздел готовится";
/// Generic placeholder detail.
pub const SECTION_PENDING_HINT: &str = "Экран появится в одной из следующих версий приложения.";

// ---- short keys used by the widgets ----------------------------------------

/// Status bar idle text.
pub const STATUS_READY: &str = "Готово";
/// Filter bar label.
pub const FILTER_LABEL: &str = "Фильтр";
/// Issue-facet chip noun (plural, genitive).
pub const FINDINGS_PLURAL: &str = "находок";

/// Russian plural of «находка» for a count: 1 находка, 2–4 находки, 5+ находок.
pub fn findings_noun(n: i64) -> &'static str {
    let n = n.unsigned_abs();
    if n % 100 / 10 == 1 {
        return "находок";
    }
    match n % 10 {
        1 => "находка",
        2..=4 => "находки",
        _ => "находок",
    }
}
/// Dashboard total counter label.
pub const ISSUES_TOTAL: &str = "Всего находок";

/// Field-access view over the constants above (`strs().nav_urls`).
pub struct Strings {
    /// `APP_TITLE`
    pub app_title: &'static str,
    /// `NAV_URLS`
    pub nav_urls: &'static str,
    /// `NAV_DASHBOARD`
    pub nav_dashboard: &'static str,
    /// `NAV_INBOX`
    pub nav_inbox: &'static str,
    /// `NAV_PROJECTS`
    pub nav_projects: &'static str,
    /// `PROFILE_NAME`
    pub profile: &'static str,
    /// `PROFILE_WHERE`
    pub local_machine: &'static str,
    /// `PROFILE_MODE`
    pub mode: &'static str,
    /// `MENU_SETTINGS`
    pub settings: &'static str,
    /// `MENU_THEME`
    pub theme: &'static str,
    /// `THEME_DARK`
    pub theme_dark: &'static str,
    /// `THEME_LIGHT`
    pub theme_light: &'static str,
    /// `MENU_LANGUAGE`
    pub language: &'static str,
    /// `MENU_MCP`
    pub mcp: &'static str,
    /// `MENU_CLI`
    pub cli_path: &'static str,
    /// `MENU_HELP`
    pub help: &'static str,
    /// `STATUS_READY`
    pub ready: &'static str,
    /// `SECTION_PENDING`
    pub not_implemented: &'static str,
    /// `TABLE_EMPTY_TITLE`
    pub no_scan: &'static str,
    /// `FILTER_LABEL`
    pub filter: &'static str,
    /// `FILTER_SEARCH`
    pub url_contains: &'static str,
    /// `FILTER_RESET`
    pub clear: &'static str,
    /// `FINDINGS_PLURAL`
    pub finding: &'static str,
    /// `TAB_DETAILS`
    pub details: &'static str,
    /// `TAB_DETAILS`
    pub tab_summary: &'static str,
    /// `TAB_INLINKS`
    pub tab_links_in: &'static str,
    /// `TAB_OUTLINKS`
    pub tab_links_out: &'static str,
    /// `TAB_RESPONSE`
    pub tab_response: &'static str,
    /// `TAB_BODY`
    pub tab_body: &'static str,
    /// `SEV_CRITICAL`
    pub sev_critical: &'static str,
    /// `SEV_WARNING`
    pub sev_warning: &'static str,
    /// `SEV_NOTICE`
    pub sev_notice: &'static str,
    /// `SEV_OPPORTUNITY`
    pub sev_opportunity: &'static str,
    /// `ISSUES_TOTAL`
    pub issues_total: &'static str,
    /// `DASH_NO_AUDIT`
    pub no_findings: &'static str,
    /// `SIMPLE_HINT`
    pub simple_hint: &'static str,
}

/// The one strings table the widgets read.
pub fn strs() -> Strings {
    Strings {
        app_title: APP_TITLE,
        nav_urls: NAV_URLS,
        nav_dashboard: NAV_DASHBOARD,
        nav_inbox: NAV_INBOX,
        nav_projects: NAV_PROJECTS,
        profile: PROFILE_NAME,
        local_machine: PROFILE_WHERE,
        mode: PROFILE_MODE,
        settings: MENU_SETTINGS,
        theme: MENU_THEME,
        theme_dark: THEME_DARK,
        theme_light: THEME_LIGHT,
        language: MENU_LANGUAGE,
        mcp: MENU_MCP,
        cli_path: MENU_CLI,
        help: MENU_HELP,
        ready: STATUS_READY,
        not_implemented: SECTION_PENDING,
        no_scan: TABLE_EMPTY_TITLE,
        filter: FILTER_LABEL,
        url_contains: FILTER_SEARCH,
        clear: FILTER_RESET,
        finding: FINDINGS_PLURAL,
        details: TAB_DETAILS,
        tab_summary: TAB_DETAILS,
        tab_links_in: TAB_INLINKS,
        tab_links_out: TAB_OUTLINKS,
        tab_response: TAB_RESPONSE,
        tab_body: TAB_BODY,
        sev_critical: SEV_CRITICAL,
        sev_warning: SEV_WARNING,
        sev_notice: SEV_NOTICE,
        sev_opportunity: SEV_OPPORTUNITY,
        issues_total: ISSUES_TOTAL,
        no_findings: DASH_NO_AUDIT,
        simple_hint: SIMPLE_HINT,
    }
}

// ---- design pass: categories, presets, empty states, status bar --------------

/// Check category: response codes.
pub const CAT_RESPONSE: &str = "Коды ответа";
/// Check category: titles and descriptions.
pub const CAT_TITLES: &str = "Title и Description";
/// Check category: headings.
pub const CAT_HEADINGS: &str = "Заголовки H1–H6";
/// Check category: canonicals and directives.
pub const CAT_DIRECTIVES: &str = "Canonical и директивы";
/// Check category: links and redirects.
pub const CAT_LINKS: &str = "Ссылки и редиректы";
/// Check category: images.
pub const CAT_IMAGES: &str = "Изображения";
/// Check category: URL structure.
pub const CAT_URL: &str = "Структура URL";
/// Check category: page content.
pub const CAT_CONTENT: &str = "Контент";
/// Check category: security.
pub const CAT_SECURITY: &str = "Безопасность";
/// Check category: hreflang and sitemaps.
pub const CAT_INTL: &str = "Hreflang и sitemap";
/// Check category: structured data and social markup.
pub const CAT_STRUCTURED: &str = "Разметка и соцсети";
/// Check category: rendering and resources.
pub const CAT_RENDER: &str = "Рендеринг и ресурсы";
/// Check category: everything else.
pub const CAT_OTHER: &str = "Прочее";

/// Dashboard subtitle: findings and the checks that produced them.
pub fn dash_subtitle(issues: i64, checks: usize) -> String {
    format!("{issues} находок в {checks} сработавших проверках")
}
/// KPI card caption: all crawled URLs.
pub const KPI_URLS: &str = "Всего URL";
/// KPI card caption: all findings.
pub const KPI_FINDINGS: &str = "Всего находок";
/// Dashboard: nothing selected in the list.
pub const DASH_PICK: &str = "Выберите проверку слева — здесь появятся описание и что делать.";
/// Dashboard detail: fix heading.
pub const DASH_FIX: &str = "Что делать";
/// Dashboard detail: message heading.
pub const DASH_WHAT: &str = "Что найдено";
/// Dashboard detail: affected URLs.
pub const DASH_AFFECTED: &str = "Затронуто URL";
/// Dashboard detail: button that opens the filtered table.
pub const DASH_OPEN_URLS: &str = "Показать URL в таблице";
/// Dashboard detail: why a check is not active.
pub const DASH_WHY: &str = "Почему нет данных";
/// Dashboard list: toggle for the quiet checks.
pub fn dash_quiet(n: usize) -> String {
    format!("Проверки без находок: {n}")
}
/// Dashboard detail: findings cut by a saved policy.
pub fn dash_suppressed(n: i64) -> String {
    format!("подавлено политикой: {n}")
}
/// Dashboard filter box hint.
pub const DASH_SEARCH: &str = "Найти проверку…";

/// URL table title.
pub const TABLE_TITLE: &str = "Адреса";
/// URL table subtitle: shown of total.
pub fn table_subtitle(shown: i64, total: i64) -> String {
    if shown == total {
        format!("{} URL", total)
    } else {
        format!("{shown} из {total} URL")
    }
}
/// Preset combo: no preset.
pub const PRESET_NONE: &str = "Все URL";
/// Preset combo: caption of the filter group.
pub const PRESET_CAPTION: &str = "Срез";
/// No rows match the filter.
pub const TABLE_NO_ROWS: &str = "Под фильтр ничего не попало";
/// No rows: hint.
pub const TABLE_NO_ROWS_HINT: &str = "Смягчите фильтр или сбросьте его, чтобы увидеть все адреса.";
/// Empty state action: open a scan.
pub const EMPTY_OPEN_SCAN: &str = "Открыть скан";
/// Details panel hint when no row is selected.
pub const DETAILS_PICK: &str =
    "Выберите строку — здесь появятся сводка, ссылки, ответ сервера и тело страницы.";
/// Boolean cell: yes.
pub const YES: &str = "Да";
/// Boolean cell: no.
pub const NO: &str = "Нет";
/// Response time unit.
pub const UNIT_MS: &str = "мс";
/// Status bar: URL counter.
pub fn status_urls(n: i64) -> String {
    format!("{n} URL")
}
/// Status bar: scan segment title.
pub const STATUS_SCAN: &str = "Скан";
/// Status bar: scan closed message.
pub const STATUS_CLOSED: &str = "Скан закрыт";
/// Nav caption above the project sections.
pub const NAV_WORKSPACE: &str = "РАБОЧЕЕ ПРОСТРАНСТВО";
/// Profile button subtitle.
pub const PROFILE_SUB: &str = "локально";
/// Inbox empty-state title.
pub const INBOX_TITLE: &str = "Во входящих пока пусто";
/// Inbox empty-state hint.
pub const INBOX_HINT: &str = "Сюда придут задачи от агента и результаты его проверок.";
/// Projects empty-state title.
pub const PROJECTS_TITLE: &str = "Проектов пока нет";
/// Projects empty-state hint.
pub const PROJECTS_HINT: &str =
    "Проект соберёт сканы, семантику и задачи по одному сайту. Для разового краула проект не нужен.";
/// Filter chip: finding facet.
pub const CHIP_FINDING: &str = "Находка";
/// Filter chip: severity facet.
pub const CHIP_SEVERITY: &str = "Важность";

/// Human label of a preset id (`status_4xx` → «Ошибки 4xx»).
pub fn preset_label(id: &str) -> &str {
    match id {
        "status_3xx" => "Редиректы 3xx",
        "status_4xx" => "Ошибки 4xx",
        "status_5xx" => "Ошибки сервера 5xx",
        "no_response" => "Нет ответа",
        "title_missing" => "Title отсутствует",
        "title_over_60" => "Title длиннее 60 символов",
        "meta_description_missing" => "Description отсутствует",
        "meta_description_over_160" => "Description длиннее 160 символов",
        "h1_missing" => "H1 отсутствует",
        "h1_multiple" => "Несколько H1",
        "canonical_missing" => "Canonical отсутствует",
        "images_missing_alt" => "Изображения без alt",
        "noindex_meta" => "Noindex в meta",
        "noindex_header" => "Noindex в заголовке",
        "no_outlinks" => "Без исходящих ссылок",
        "hreflang" => "С hreflang",
        "duplicate_titles" => "Дубли Title",
        "in_sitemap" => "В sitemap",
        "inlinks" => "С входящими ссылками",
        "robots_blocked" => "Закрыты robots.txt",
        "description_duplicate" => "Дубли Description",
        "canonical_non_indexable" => "Canonical на неиндексируемый",
        _ => id,
    }
}

// ---- shell: top bar, side navigation, palette ---------------------------------

/// Top bar chip: crawler mode without a project.
pub const SHELL_NO_PROJECT: &str = "Краулер · без проекта";
/// Top bar: command palette field caption.
pub const PALETTE_FIELD: &str = "Действия и переходы";
/// Top bar: start a fresh crawl.
pub const NEW_CRAWL: &str = "Новый краул";
/// Top bar: keep the crawl as a project.
pub const SAVE_AS_PROJECT: &str = "Сохранить как проект";
/// Top bar tooltip: fold the navigation.
pub const COLLAPSE_NAV: &str = "Свернуть навигацию";
/// Top bar tooltip: unfold the navigation.
pub const EXPAND_NAV: &str = "Развернуть навигацию";
/// Top bar state chip: scan is open and ready.
pub fn scan_ready(n: i64) -> String {
    format!("Готово · {n} URL")
}
/// Top bar state chip: nothing open.
pub const SCAN_NONE: &str = "Скан не открыт";
/// Top bar tooltip of the project chip.
pub const PROJECTS_TIP: &str = "Краул без проекта · к проектам";
/// Navigation: work section.
pub const NAV_WORK: &str = "Работа";
/// Navigation: crawler.
pub const NAV_CRAWLER: &str = "Краул";
/// Navigation: issues (findings).
pub const NAV_ISSUES: &str = "Проблемы";
/// Navigation: saved scans.
pub const NAV_SCANS: &str = "Сканы";
/// Navigation: compare.
pub const NAV_COMPARE: &str = "Сравнение";
/// Navigation: link graph.
pub const NAV_GRAPH: &str = "Граф ссылок";
/// Navigation: external links.
pub const NAV_BACKLINKS: &str = "Внешние ссылки";
/// Navigation: semantics.
pub const NAV_SEMANTICS: &str = "Семантика";
/// Navigation: HTML search.
pub const NAV_SEARCH: &str = "Поиск в HTML";
/// Navigation: methods.
pub const NAV_METHODS: &str = "Методы";
/// Navigation: reports.
pub const NAV_REPORTS: &str = "Отчёты";
/// Navigation: log.
pub const NAV_LOG: &str = "Журнал";
/// Navigation section caption: data.
pub const NAV_SEC_DATA: &str = "Данные";
/// Navigation section caption: result.
pub const NAV_SEC_RESULT: &str = "Результат";
/// Tooltip of a project-only item without a project.
pub const NEED_PROJECT: &str = "Нужен проект · «Сохранить как проект»";
/// Tooltip of an item whose screen is not built yet.
pub const NOT_IN_VERSION: &str = "Недоступно в этой версии";
/// Profile menu: theme row.
pub fn menu_theme(label: &str) -> String {
    format!("Тема · {label}")
}
/// Profile menu: language row.
pub const MENU_LANGUAGE_ROW: &str = "Язык · Русский";
/// Profile menu: MCP row.
pub const MENU_MCP_ROW: &str = "MCP-сервер · выключен";
/// Status bar: saved scans are read-only.
pub const STATUS_READONLY: &str = "Только чтение сохранённого скана";
/// Status bar: core version.
pub fn status_core(version: &str) -> String {
    format!("Ядро SpiderHead {version}")
}
/// Palette: placeholder of the search field.
pub const PALETTE_PLACEHOLDER: &str = "Найти действие или раздел…";
/// Palette: group of navigation targets.
pub const PALETTE_NAV: &str = "Переходы";
/// Palette: group of actions.
pub const PALETTE_ACTIONS: &str = "Действия";
/// Palette: nothing matched.
pub const PALETTE_EMPTY: &str = "Ничего не найдено";
/// Palette: footer hint.
pub const PALETTE_HINT: &str = "Стрелки: выбор · Enter: выполнить · Esc: закрыть";

// ---- group strip, condition builder and table chrome (canvas pass) ------------

/// Group-strip chip label of a facet id (`facet_groups()` vocabulary).
pub fn facet_label(id: &str) -> &str {
    match id {
        "all" => "Все URL",
        "status_2xx" => "2xx",
        "status_3xx" => "3xx",
        "status_4xx" => "4xx",
        "status_5xx" => "5xx",
        "title_missing" => "Без Title",
        "title_duplicate" => "Дубли Title",
        "title_long" => "Title длинный",
        "title_short" => "Title короткий",
        "canonical_missing" => "Без canonical",
        "canonical_other" => "Canonical на другой",
        "canonical_self" => "Canonical на себя",
        "directives_noindex" => "noindex",
        "directives_nofollow" => "nofollow",
        "with_outlinks" => "С исходящими",
        "with_inlinks" => "С входящими",
        "internal" => "Внутренние",
        "external" => "Внешние",
        "in_sitemap" => "В sitemap",
        "with_resources" => "С ресурсами",
        "with_images" => "С изображениями",
        "with_scripts" => "Со скриптами",
        "with_stylesheets" => "Со стилями",
        _ => id,
    }
}

/// Group chip without a countable facet (needs data beyond scan.v1).
pub const GROUP_NO_COUNT: &str = "счётчик недоступен в scan.v1";
/// `+ Условие` button of the filter bar.
pub const COND_ADD: &str = "+ Условие";
/// Condition dialog: column caption.
pub const COND_COLUMN: &str = "Колонка";
/// Condition dialog: operator caption.
pub const COND_OP: &str = "Условие";
/// Condition dialog: value caption.
pub const COND_VALUE: &str = "Значение";
/// Condition dialog: apply button.
pub const COND_APPLY: &str = "Добавить";
/// Column-picker menu tooltip.
pub const COLS_MENU: &str = "Колонки таблицы";
/// Export button tooltip: filtered rows as TSV.
pub const TSV_HINT: &str = "Скопировать строки как TSV";
/// Status strip: one click exports the visible query.
pub const TSV_COPIED: &str = "TSV скопирован в буфер";

/// Operator captions of the condition dialog (`data::filters` vocabulary).
pub fn op_label(op: &str) -> &str {
    match op {
        "eq" => "=",
        "ne" => "≠",
        "gt" => ">",
        "gte" => "≥",
        "lt" => "<",
        "lte" => "≤",
        "contains" => "содержит",
        "not_contains" => "не содержит",
        "starts_with" => "начинается с",
        "in" => "в списке",
        "not_in" => "не в списке",
        "between" => "между",
        "empty" => "пусто",
        "not_empty" => "не пусто",
        "is_null" => "нет данных",
        "not_null" => "есть данные",
        _ => op,
    }
}

/// Chip text of one active filter: «Колонка ≠ значение».
pub fn filter_chip(column: &str, op: &str, value: &str) -> String {
    let col = column_label(column);
    let op = op_label(op);
    if value.is_empty() {
        format!("{col} {op}")
    } else {
        format!("{col} {op} «{value}»")
    }
}

/// Status strip: visible range, scan total and the cached window size.
pub fn rows_line(filtered: i64, total: i64, cached: usize) -> String {
    let base = if filtered == total {
        format!("Строки 1–{filtered}")
    } else {
        format!("Строки 1–{filtered} из {filtered} · всего в запуске {total}")
    };
    format!("{base} · в памяти {cached} строк")
}
/// Status strip: the selected row.
pub fn selected_line(id: i64) -> String {
    format!("Выбрано: {id}")
}
/// Status strip: how many rows the selection covers.
pub fn selected_count_line(n: usize) -> String {
    format!("Выбрано: {n}")
}
/// Row context menu: copy the selected rows as TSV.
pub const GRID_MENU_COPY: &str = "Копировать";
/// Row context menu: copy the raw address of the active row.
pub const GRID_MENU_COPY_URL: &str = "Копировать адрес";
/// Row context menu: copy the percent-decoded address.
pub const GRID_MENU_COPY_PRETTY: &str = "Копировать декодированный адрес";
/// Row context menu: copy the percent-encoded address.
pub const GRID_MENU_COPY_UGLY: &str = "Копировать закодированный адрес";
/// Row context menu: open the address in the system browser.
pub const GRID_MENU_OPEN: &str = "Открыть в браузере";
/// Header menu: configure the columns (the chooser dialog).
pub const GRID_MENU_CONFIGURE: &str = "Настроить колонки…";
/// Header menu: hide this column.
pub const GRID_MENU_HIDE: &str = "Скрыть колонку";
/// Header menu: freeze the leading columns through this one.
pub const GRID_MENU_PIN: &str = "Закрепить колонки до этой";
/// Header menu: unfreeze everything.
pub const GRID_MENU_UNPIN: &str = "Открепить колонки";
/// Header menu: fit the column to its visible content.
pub const GRID_MENU_AUTOFIT: &str = "Подобрать ширину";
/// Header menu: sort by this column.
pub const GRID_MENU_SORT_ASC: &str = "Сортировать по возрастанию";
/// Header menu: sort back by this column.
pub const GRID_MENU_SORT_DESC: &str = "Сортировать по убыванию";
/// Header menu: drop the sort.
pub const GRID_MENU_SORT_NONE: &str = "Без сортировки";
/// Toast: rows went to the clipboard.
pub fn copied_toast(n: usize) -> String {
    format!("Скопировано: {n} стр.")
}
/// Toast: only the first five addresses opened in the browser.
pub const GRID_OPEN_CAP: &str = "Открыты первые 5 адресов из выделенных";
/// Toast: the address went to the clipboard.
pub const GRID_URL_COPIED: &str = "Адрес скопирован";
/// Column chooser: title.
pub const COLS_TITLE: &str = "Настроить колонки";
/// Column chooser: search placeholder.
pub const COLS_SEARCH: &str = "Поиск колонки…";
/// Column chooser: reset to defaults.
pub const COLS_RESET: &str = "Сбросить";
/// Column chooser: discard changes.
pub const COLS_CANCEL: &str = "Отмена";
/// Column chooser: apply changes.
pub const COLS_OK: &str = "ОК";
/// Status strip hint: what Ctrl+C does on the grid.
pub const GRID_COPY_HINT: &str = "⌘C копирует выделенные строки";
/// Status strip: zoom controls.
pub const GRID_ZOOM: &str = "Масштаб";
/// Table state: first page still loading.
pub const STATE_LOADING: &str = "Читаем таблицу…";
/// Table state: the query failed.
pub const STATE_ERR_TITLE: &str = "Не удалось прочитать данные скана";
/// Table state: retry button.
pub const STATE_RETRY: &str = "Повторить";

// ---- details panel tabs (canvas pass) ----------------------------------------

/// Details tab: overview/summary.
pub const TAB_OVERVIEW: &str = "Обзор";
/// URL table: the filter drop-down when the live query is no section filter.
pub const FILTER_CUSTOM: &str = "Свой фильтр";
/// «Обзор» tree: the crawl summary root.
pub const OVERVIEW_SUMMARY: &str = "Сводка краула";
/// Toolbar: unfold the «Обзор» panel.
pub const OVERVIEW_SHOW: &str = "Показать обзор";
/// Toolbar: fold the «Обзор» panel.
pub const OVERVIEW_HIDE: &str = "Скрыть обзор";
/// Details tab: request/response headers and timing.
pub const TAB_HEADERS: &str = "Заголовки";
/// Details tab: inbound/outbound links.
pub const TAB_LINKS: &str = "Ссылки";
/// Details tab: retained HTML source.
pub const TAB_HTML: &str = "HTML";
/// Details tab: resource inventory.
pub const TAB_RESOURCES: &str = "Ресурсы";
/// Details tab: redirect chain and canonical.
pub const TAB_REDIRECTS: &str = "Редиректы";
/// Details tab: cross-scan history.
pub const TAB_HISTORY: &str = "История";
/// Details tab: structured data.
pub const TAB_STRUCTURED: &str = "Разметка";

/// Panel header: copy the address to the clipboard.
pub const DETAILS_COPY: &str = "Копировать адрес";
/// Panel header: hide the details panel.
pub const DETAILS_HIDE: &str = "Скрыть панель";
/// Status strip: bring the details panel back.
pub const DETAILS_SHOW: &str = "Показать панель";
/// Panel tab bar: step back to the previously selected URL.
pub const DETAILS_BACK: &str = "Назад к ранее выбранному";

/// Headers tab: request headers card.
pub const HDR_REQUEST: &str = "Запрос";
/// Headers tab: response headers card.
pub const HDR_RESPONSE: &str = "Ответ";
/// Headers tab: timing rail caption.
pub const HDR_TIMING: &str = "Тайминги";
/// Headers tab: derived observations caption.
pub const HDR_INSIGHTS: &str = "Выводы";
/// Headers tab: the retained response has no header dictionaries.
pub const HDR_NO_HEADERS: &str = "Заголовки не сохранены";
/// Timing row: redirect hops of the recorded chain.
pub fn timing_hops(n: i64) -> String {
    format!("Прыжков: {n}")
}

/// Links tab: inbound segment.
pub const SEG_IN: &str = "Входящие";
/// Links tab: outbound segment.
pub const SEG_OUT: &str = "Исходящие";
/// Links grid: attribute column (rel, target, nofollow).
pub const LINKS_COL_ATTR: &str = "Атрибут";
/// Links tab: no inbound links retained.
pub const LINKS_EMPTY_IN: &str = "Входящих ссылок нет";
/// Links tab: no outbound links retained.
pub const LINKS_EMPTY_OUT: &str = "Исходящих ссылок нет";

/// HTML tab: in-source search field.
pub const HTML_FIND: &str = "Найти в HTML…";
/// HTML tab: line and match counters.
pub fn html_stats(lines: usize, matches: usize) -> String {
    if matches == 0 {
        format!("строк: {lines}")
    } else {
        format!("строк: {lines} · совпадений: {matches}")
    }
}
/// HTML tab: the source is longer than the display cap.
pub fn html_shown(shown: usize, total: usize) -> String {
    format!("показаны первые {shown} строк из {total}")
}

/// Resources tab: page-image counters caption.
pub const RES_IMAGES_TITLE: &str = "Изображения";
/// Resources tab: declared refs caption.
pub const RES_DECLARED: &str = "Заявленные ресурсы";
/// Resources tab: kind column.
pub const RES_COL_KIND: &str = "Тип";
/// Resources tab: address column.
pub const RES_COL_URL: &str = "Ресурс";
/// Resources tab: capture-state column.
pub const RES_COL_STATE: &str = "Состояние";
/// Resources tab: measured image counters.
pub fn res_images(total: i64, missing: i64) -> String {
    format!("изображений {total}, без alt {missing}")
}
/// Resources tab: nothing declared for the page.
pub const RES_EMPTY: &str = "У страницы нет сохранённых ссылок на ресурсы.";
/// Resources tab: why the inventory can be thin (capture state).
pub fn res_capture(state: &str, reason: &str) -> String {
    if reason.is_empty() {
        state.to_string()
    } else {
        format!("{state} · {reason}")
    }
}

/// Redirects tab: no chain was recorded for the URL.
pub const REDIRECT_NONE: &str = "URL пришёл напрямую — цепочки редиректов нет.";
/// Redirects tab: chain caption.
pub fn redirect_hops(n: usize) -> String {
    format!("Цепочка: {n} шаг(ов)")
}
/// Redirects tab: canonical card caption.
pub const CANON_TITLE: &str = "Canonical";
/// Redirects tab: canonical points at the URL itself.
pub const CANON_SELF: &str = "canonical указывает на себя";
/// Redirects tab: canonical points elsewhere.
pub const CANON_OTHER: &str = "canonical указывает на другой адрес";
/// Redirects tab: no canonical retained.
pub const CANON_MISSING: &str = "canonical не задан";

/// History tab: scan-number column.
pub const HIST_COL_SCAN: &str = "Скан";
/// History tab: finished-at column.
pub const HIST_COL_DATE: &str = "Завершён";
/// History tab: presence column.
pub const HIST_COL_STATE: &str = "Статус";
/// History tab: how many scan files sit beside the open one.
pub fn hist_files(n: i64) -> String {
    format!("сканов рядом: {n}")
}
/// History tab: unreadable neighbour files.
pub fn hist_errors(n: usize) -> String {
    format!("ошибок чтения соседних сканов: {n}")
}
/// History tab: per-field change list caption.
pub const HIST_TRANSITIONS: &str = "Изменения между сканами";
/// History state label of `url_history` entries.
pub fn hist_state(state: &str) -> &'static str {
    match state {
        "present" => "был в скане",
        "absent" => "не было в скане",
        "unavailable" => "скан не читается",
        _ => "нет данных",
    }
}

/// Structured-data tab: blocks list caption.
pub const SD_BLOCKS: &str = "Блоки разметки";
/// Structured-data tab: validation summary caption.
pub const SD_VALIDATION: &str = "Валидация";
/// Structured-data tab: nothing detected on the page.
pub const SD_ABSENT: &str = "Структурированные данные на странице не найдены.";
/// Structured-data tab: error/warning counters.
pub fn sd_counts(errors: i64, warnings: i64) -> String {
    format!("ошибок {errors} · предупреждений {warnings}")
}
/// Structured-data tab: one retained representation chip.
pub fn sd_representation(rep: &str) -> String {
    format!("представление: {rep}")
}

/// Settings window strings (dialog `settings.*` and `sources.*` commands).
pub mod settings {
    /// Dialog title.
    pub const TITLE: &str = "Настройки";
    /// Footer apply marker: everything is stored as soon as it changes.
    pub const APPLY_NOTE: &str = "Применяется сразу · хранится на этом компьютере";
    /// Footer close button.
    pub const DONE: &str = "Готово";
    /// Search field placeholder over the section list.
    pub const SEARCH: &str = "Поиск по настройкам…";
    /// Section: General.
    pub const NAV_GENERAL: &str = "Общие";
    /// Section: View.
    pub const NAV_VIEW: &str = "Вид";
    /// Section: Default scans.
    pub const NAV_SCAN: &str = "Сканы по умолчанию";
    /// Section: Core.
    pub const NAV_CORE: &str = "Ядро";
    /// Section: MCP server.
    pub const NAV_MCP: &str = "MCP-сервер";
    /// Section: Data sources.
    pub const NAV_SOURCES: &str = "Источники данных";
    /// Section: Agent.
    pub const NAV_AGENT: &str = "Агент";
    /// Section: Notifications.
    pub const NAV_NOTIFY: &str = "Уведомления";
    /// Section: Hotkeys.
    pub const NAV_KEYS: &str = "Горячие клавиши";
    /// Section: Storage and data.
    pub const NAV_DATA: &str = "Хранилище и данные";
    /// Section: Updates.
    pub const NAV_UPDATES: &str = "Обновления";
    /// Section: About.
    pub const NAV_ABOUT: &str = "О программе";

    /// General: startup group.
    pub const G_STARTUP: &str = "Запуск";
    /// General: what to open at start.
    pub const G_OPEN_AT_START: &str = "При запуске открывать";
    /// General: hint for the start choice.
    pub const G_OPEN_HINT: &str = "Последний проект с вкладками или список проектов";
    /// General: last project option.
    pub const G_LAST_PROJECT: &str = "Последний проект";
    /// General: project list option.
    pub const G_PROJECT_LIST: &str = "Список";
    /// General: restore tabs switch.
    pub const G_RESTORE_TABS: &str = "Восстанавливать вкладки";
    /// General: restore tabs hint.
    pub const G_RESTORE_TABS_HINT: &str = "Открытые таблицы, фильтры и позиции прокрутки";
    /// General: recent projects count.
    pub const G_RECENT: &str = "Недавние проекты";
    /// General: recent count hint.
    pub const G_RECENT_HINT: &str = "Сколько показывать на стартовом экране · 5–50";
    /// General: default projects folder.
    pub const G_FOLDER: &str = "Папка проектов по умолчанию";
    /// General: folder hint.
    pub const G_FOLDER_HINT: &str = "Куда создавать новые проекты";
    /// General: closing group.
    pub const G_CLOSING: &str = "Закрытие";
    /// General: confirm-close switch.
    pub const G_CONFIRM_CLOSE: &str = "Подтверждать закрытие при активном скане";
    /// General: confirm-close hint.
    pub const G_CONFIRM_CLOSE_HINT: &str = "Скан продолжится в ядре и после закрытия окна";
    /// General: stay-in-menu-bar switch.
    pub const G_STAY_MENU: &str = "Оставаться в строке меню";
    /// General: menu-bar hint.
    pub const G_STAY_MENU_HINT: &str = "Иконка с прогрессом сканов, когда окно закрыто";
    /// General: behaviour group.
    pub const G_BEHAVIOUR: &str = "Поведение";
    /// General: link action choice.
    pub const G_LINKS: &str = "Открывать ссылки на URL";
    /// General: link action hint.
    pub const G_LINKS_HINT: &str = "Клик по адресу в таблице";
    /// General: details option.
    pub const G_LINKS_DETAILS: &str = "Детали";
    /// General: browser option.
    pub const G_LINKS_BROWSER: &str = "Браузер";
    /// General: autosave interval.
    pub const G_AUTOSAVE: &str = "Автосохранение представлений";
    /// General: autosave hint.
    pub const G_AUTOSAVE_HINT: &str = "Интервал в секундах · 5–600";
    /// General: date format choice.
    pub const G_DATE_FORMAT: &str = "Формат дат";
    /// General: date format hint.
    pub const G_DATE_FORMAT_HINT: &str = "В таблицах, отчётах и журнале";
    /// General: crash stats switch.
    pub const G_CRASH: &str = "Анонимная статистика сбоев";
    /// General: crash stats hint.
    pub const G_CRASH_HINT: &str = "Только трассировка ошибки, без URL и данных проектов";
    /// General: machine-scope note.
    pub const G_NOTE: &str =
        "Настройки этого компьютера. В файл проекта не пишутся: коллега с тем же проектом увидит свои значения.";

    /// View: theme row.
    pub const V_THEME: &str = "Тема";
    /// View: language row.
    pub const V_LANGUAGE: &str = "Язык";
    /// View: density row.
    pub const V_DENSITY: &str = "Плотность";
    /// View: density options.
    pub const V_DENSITIES: [&str; 3] = ["Компактная", "Стандартная", "Свободная"];
    /// View: details position row.
    pub const V_DETAILS_POS: &str = "Панель деталей";
    /// View: details options.
    pub const V_DETAILS_POS_OPTS: [&str; 2] = ["Снизу", "Справа"];
    /// View: zoom row.
    pub const V_ZOOM: &str = "Масштаб интерфейса, %";
    /// View: zoom range hint.
    pub const V_ZOOM_RANGE: &str = "80–150";
    /// View: reduce motion.
    pub const V_REDUCE_MOTION: &str = "Меньше анимаций";
    /// View: monospace URLs.
    pub const V_MONO_URLS: &str = "Моноширинные URL";
    /// View: rail when narrow.
    pub const V_RAIL: &str = "Сворачивать навигацию в рейл";
    /// View: focus mode.
    pub const V_FOCUS: &str = "Фокус-режим таблицы";
    /// View: status badges.
    pub const V_BADGES: &str = "Бейджи статусов в таблице";

    /// Scan: profile row.
    pub const S_PROFILE: &str = "Профиль по умолчанию";
    /// Scan: request rate.
    pub const S_RATE: &str = "Скорость, запр/с";
    /// Scan: rate range hint.
    pub const S_RATE_RANGE: &str = "0,1–10";
    /// Scan: URL limit.
    pub const S_URL_LIMIT: &str = "Лимит URL";
    /// Scan: limit range hint.
    pub const S_LIMIT_RANGE: &str = "1–1 000 000";
    /// Scan: depth.
    pub const S_DEPTH: &str = "Глубина";
    /// Scan: depth range hint.
    pub const S_DEPTH_RANGE: &str = "0–100";
    /// Scan: robots switch.
    pub const S_ROBOTS: &str = "Учитывать robots.txt";
    /// Scan: store HTML switch.
    pub const S_SAVE_HTML: &str = "Сохранять HTML";
    /// Scan: parallel workers.
    pub const S_PARALLEL: &str = "Одновременных сканов";
    /// Scan: parallel range hint.
    pub const S_PARALLEL_RANGE: &str = "1–4";
    /// Scan: SF path.
    pub const S_SF_PATH: &str = "Путь к внешнему краулеру";
    /// Scan: external-crawler license row.
    pub const S_LICENSE: &str = "Лицензия внешнего краулера";
    /// Scan: license state chip when the check is not wired.
    pub const S_LICENSE_UNKNOWN: &str = "нет данных";
    /// Scan: license hint when the check is not wired.
    pub const S_LICENSE_HINT: &str = "Проверка лицензии недоступна в этой сборке";
    /// Scan: note that defaults map onto crawl settings.
    pub const S_NOTE: &str = "Значения для новых сканов на этом компьютере";

    /// Core: poll interval.
    pub const C_POLL: &str = "Опрос прогресса скана, с";
    /// Core: log level.
    pub const C_LOG: &str = "Уровень журнала";
    /// Core: diagnostics group.
    pub const C_DIAG: &str = "Диагностика";
    /// Core: diagnostics note.
    pub const C_NOTE: &str = "Приложение — оболочка над ядром; CLI той же версии";

    /// MCP: enabled switch.
    pub const M_ENABLED: &str = "Локальный MCP-сервер";
    /// MCP: profile row.
    pub const M_PROFILE: &str = "Профиль инструментов";
    /// MCP: journal switch.
    pub const M_JOURNAL: &str = "Журнал действий агента";
    /// MCP: profile hint.
    pub const M_PROFILE_HINT: &str = "Какие инструменты доступны агенту";
    /// MCP: note.
    pub const M_NOTE: &str = "Доступ агентов к проектам через локальный MCP";

    /// Agent: projects row.
    pub const A_PROJECTS: &str = "Проекты, доступные агенту";
    /// Agent: projects hint.
    pub const A_PROJECTS_HINT: &str = "Имена по одному в строке; пусто — ни одного";
    /// Agent: run scans.
    pub const A_RUN_SCANS: &str = "Запускать сканы";
    /// Agent: change tasks.
    pub const A_TASKS: &str = "Менять задачи";
    /// Agent: show actions.
    pub const A_SHOW: &str = "Показывать действия в ленте";
    /// Agent: config link.
    pub const A_CONFIG: &str = "Конфигурация агента";
    /// Agent: note.
    pub const A_NOTE: &str = "Что агент видит и что может делать через MCP";
    /// AgentConfig: quotas group.
    pub const A_QUOTAS: &str = "Квоты и предложения";
    /// AgentConfig: accept button.
    pub const A_ACCEPT: &str = "Принять";
    /// AgentConfig: reject button.
    pub const A_REJECT: &str = "Отклонить";
    /// AgentConfig: journal caption.
    pub const A_JOURNAL: &str = "Журнал изменений агента";
    /// AgentConfig: masked-key caption.
    pub const A_KEYS: &str = "Ключи хранятся в системном хранилище — значения не показываются";

    /// Notify: window column.
    pub const N_WINDOW: &str = "В окне";
    /// Notify: system column.
    pub const N_SYSTEM: &str = "Системные";
    /// Notify: sound column.
    pub const N_SOUND: &str = "Звук";
    /// Notify: quiet hours switch.
    pub const N_QUIET: &str = "Тихие часы";
    /// Notify: quiet range hint.
    pub const N_QUIET_HINT: &str = "Системные уведомления молчат в этот интервал";
    /// Notify: from field.
    pub const N_FROM: &str = "С";
    /// Notify: to field.
    pub const N_TO: &str = "До";
    /// Notify: time field format hint.
    pub const N_TIME_FORMAT: &str = "ЧЧ:ММ";
    /// Notify event label of one matrix row.
    pub fn n_event(id: &str) -> &'static str {
        match id {
            "done" => "Скан завершён или остановлен",
            "err" => "Ошибка ядра или скана",
            "agent" => "Агент предложил задачи или цель",
            "pay" => "Агент просит платный запрос",
            "stale" => "Наблюдение устарело",
            "exp" => "Экспорт и отчёт готовы",
            "upd" => "Доступно обновление",
            _ => "",
        }
    }
    /// Notify event hint of one matrix row.
    pub fn n_event_hint(id: &str) -> &'static str {
        match id {
            "done" => "С итогом и ссылкой",
            "err" => "Остаётся до закрытия",
            "agent" => "Ссылка во «Входящие»",
            "pay" => "Нужно ваше «да»",
            "stale" => "Только плашкой в окне",
            "exp" => "С кнопкой «Открыть папку»",
            "upd" => "Раз в день, не чаще",
            _ => "",
        }
    }

    /// Keys: conflict badge.
    pub const K_CONFLICT: &str = "конфликт";
    /// Keys: not set placeholder.
    pub const K_UNSET: &str = "Не задано";
    /// Keys: reset the whole section button.
    pub const K_RESET_SECTION: &str = "Сбросить раздел";
    /// Keys: import button (canvas footer).
    pub const K_IMPORT: &str = "Импорт";
    /// Keys: export button (canvas footer).
    pub const K_EXPORT: &str = "Экспорт";
    /// Keys: import/export group.
    pub const K_EXCHANGE: &str = "Импорт и экспорт сочетаний";
    /// Keys: action label.
    pub fn k_action(id: &str) -> &'static str {
        match id {
            "palette" => "Действия и переходы",
            "new_scan" => "Новый скан",
            "new_tab" => "Новая вкладка",
            "close_tab" => "Закрыть вкладку",
            "settings" => "Настройки",
            "stop_scan" => "Остановить скан",
            "find_in_table" => "Найти в таблице",
            "expand_table" => "Развернуть таблицу",
            "copy_tsv" => "Копировать как TSV",
            "copy_url" => "Копировать URL",
            "help" => "Справка",
            "all_shortcuts" => "Показать все сочетания",
            _ => "",
        }
    }

    /// Data: keep scans choice.
    pub const D_KEEP: &str = "Хранить сканов на проект";
    /// Data: keep options.
    pub const D_KEEP_OPTS: [&str; 3] = ["10", "20", "все"];
    /// Data: HTML age choice.
    pub const D_HTML_AGE: &str = "Хранить HTML, дней";
    /// Data: HTML age options.
    pub const D_HTML_OPTS: [&str; 3] = ["30", "90", "не удалять"];
    /// Data: reset group.
    pub const D_RESET: &str = "Сброс";
    /// Data: reset view settings button.
    pub const D_RESET_VIEW: &str = "Сбросить настройки вида";
    /// Data: reset all button.
    pub const D_RESET_ALL: &str = "Сбросить все настройки";
    /// Data: export/import group.
    pub const D_EXCHANGE: &str = "Настройки в файл";
    /// Data: export button.
    pub const D_EXPORT: &str = "Экспортировать…";
    /// Data: import button.
    pub const D_IMPORT: &str = "Импортировать…";
    /// Data: note that exports never carry secrets.
    pub const D_EXPORT_NOTE: &str =
        "Экспорт — JSON без ключей и токенов: они живут только в системном хранилище.";
    /// Data: note about scope.
    pub const D_NOTE: &str = "Сканы лежат в папке проекта; здесь — данные приложения";

    /// Updates: channel row.
    pub const U_CHANNEL: &str = "Канал обновлений";
    /// Updates: channel options.
    pub const U_CHANNELS: [&str; 2] = ["Стабильный", "Бета"];
    /// Updates: auto check.
    pub const U_AUTO: &str = "Проверять автоматически";
    /// Updates: background download.
    pub const U_DOWNLOAD: &str = "Скачивать в фоне";
    /// Updates: check button.
    pub const U_CHECK: &str = "Проверить сейчас";
    /// Updates: no-update line.
    pub const U_CURRENT: &str = "Установлена последняя версия";
    /// Updates: installed-version caption.
    pub const U_INSTALLED: &str = "Установлено";
    /// Updates: last-check caption.
    pub const U_LAST_CHECK: &str = "Последняя проверка";
    /// Updates: no check has run yet.
    pub const U_NEVER: &str = "—";
    /// Updates: check unavailable in this build.
    pub const U_UNAVAILABLE: &str = "Проверка обновлений недоступна в этой сборке";
    /// Updates: changelog section.
    pub const U_CHANGELOG: &str = "История изменений";
    /// Updates: note.
    pub const U_NOTE: &str = "Приложение и совместимое ядро обновляются вместе";

    /// About: version caption.
    pub const AB_VERSION: &str = "Версия";
    /// About: core caption.
    pub const AB_CORE: &str = "Ядро";
    /// About: licenses caption.
    pub const AB_LICENSES: &str = "Лицензии компонентов";
    /// About: license note.
    pub const AB_LICENSE_NOTE: &str = "Исходный код — MIT; словарь Schema.org — CC BY-SA 3.0";

    /// Sources: key column state — credential is stored.
    pub const SRC_SET: &str = "ключ задан";
    /// Sources: key column state — nothing stored.
    pub const SRC_UNSET: &str = "нет ключа";
    /// Sources: open detail button.
    pub const SRC_OPEN: &str = "Настроить";
    /// Sources: auth methods button.
    pub const SRC_METHODS: &str = "Способы подключения";
    /// Sources: spend button.
    pub const SRC_SPEND: &str = "Расходы и лимиты";
    /// Sources: save-key button.
    pub const SRC_SAVE: &str = "Сохранить ключ";
    /// Sources: revoke-key button.
    pub const SRC_REVOKE: &str = "Удалить ключ";
    /// Sources: key field placeholder.
    pub const SRC_KEY_PLACEHOLDER: &str = "Вставьте значение — оно не показывается";
    /// Sources: values-never-shown note.
    pub const SRC_SECRET_NOTE: &str =
        "Значения хранятся в защищённом хранилище и не показываются — видно только, задан ключ или нет.";
    /// Sources: provider detail back link.
    pub const SRC_BACK: &str = "← Все источники";
    /// Sources: doctor caption.
    pub const SRC_DOCTOR: &str = "Проверка источников";
    /// Sources: provider state label.
    pub fn src_state(state: &str) -> &'static str {
        match state {
            "configured_unverified" => "ключ задан",
            "not_configured" => "нет ключа",
            "invalid" => "неполный ключ",
            "not_required" => "без ключа",
            _ => "нет данных",
        }
    }
    /// Sources: access-kind label.
    pub fn src_access(access: &str) -> &'static str {
        match access {
            "oauth" => "OAuth",
            "api_key" => "API-ключ",
            "service_account" => "Service account",
            "public" => "Открытый",
            _ => "",
        }
    }

    /// Spend: monthly caption.
    pub const SP_MONTH: &str = "Платные API за месяц";
    /// Spend: provider budgets caption.
    pub const SP_BUDGETS: &str = "Лимиты по провайдерам";
    /// Spend: operations caption.
    pub const SP_OPS: &str = "Последние платные операции";
    /// Spend: empty line.
    pub const SP_EMPTY: &str = "Платных операций не записано";
    /// Spend: calls counter.
    pub fn sp_calls(n: i64) -> String {
        format!("вызовов: {n}")
    }

    /// Scan profile display name (reference the reference suite).
    pub fn s_profile_name(id: &str) -> &'static str {
        match id {
            "quick" => "Быстрый аудит",
            "full" => "Полный краул",
            "sitemap" => "Только sitemap",
            "js" => "JS-рендер",
            _ => "",
        }
    }
    /// Scan profile summary line (reference the reference suite).
    pub fn s_profile_hint(id: &str) -> &'static str {
        match id {
            "quick" => "1 500 URL · 2 запр/с · без HTML",
            "full" => "50 000 URL · 2 запр/с · HTML",
            "sitemap" => "список из sitemap.xml · 1 запр/с",
            "js" => "500 URL · Chromium · 1 запр/с",
            _ => "",
        }
    }
    /// Keys: group title of an action (reference the reference suite).
    pub fn k_group(id: &str) -> &'static str {
        match id {
            "palette" | "new_scan" | "new_tab" | "close_tab" | "settings" => "Навигация",
            "stop_scan" => "Сканы",
            "find_in_table" | "expand_table" | "copy_tsv" | "copy_url" => "Таблицы",
            _ => "Справка",
        }
    }
    /// Sources: connect verb on the detail page.
    pub const SRC_CONNECT: &str = "Подключить";
    /// Sources: OAuth flow line.
    pub const SRC_OAUTH_FLOW: &str =
        "Вход в браузере, выдача разрешения, токен обновляется автоматически.";
    /// Sources: API-key flow line.
    pub const SRC_KEY_FLOW: &str =
        "Ключ уходит в защищённое хранилище; приложение видит только, задан он или нет.";
    /// Sources: service-account flow line.
    pub const SRC_SA_FLOW: &str =
        "JSON-файл остаётся в защищённом хранилище конфигурации — значение не показывается.";
    /// Sources: component name on the detail page.
    pub fn src_component(name: &str) -> &'static str {
        match name {
            "access_token" => "Токен доступа",
            "token" => "Токен",
            "api_key" | "crux_key" => "API-ключ",
            "login" => "Логин",
            "password" => "Пароль",
            "user_id" => "ID пользователя",
            "folder_id" => "ID каталога",
            "service_account" => "Service account JSON",
            _ => "",
        }
    }
    /// Agent: empty proposals line.
    pub const A_NO_PROPOSALS: &str = "Предложений от агента сейчас нет";
    /// Settings: row-mark «changed» badge.
    pub const CHANGED: &str = "изменено";
}

// ---- slice D3: issues list pane ----------------------------------------------

/// Issues severity chip: every severity.
pub const ISSUES_ALL: &str = "Все";
/// Issues URL table: evidence column.
pub const ISSUES_COL_EVIDENCE: &str = "Доказательство";
/// Issues right pane: a fired check with no affected URLs.
pub const ISSUES_NO_URLS: &str = "У проверки нет затронутых URL";
/// Issues right pane: the check was not measured in the saved audit.
pub const ISSUES_UNMEASURED: &str = "Проверка не измерялась";
/// Issues right pane: why the URL table is empty for an unmeasured check.
pub const ISSUES_UNMEASURED_HINT: &str =
    "В сохранённом аудите нет измерения этой проверки — находок не видно.";
/// Issues footer: rows of the selected check and where they came from.
pub fn issues_footer(n: i64, check: &str) -> String {
    format!("{n} URL · проверка {check} · по сохранённому аудиту")
}
/// Issues pager strip: shown range of the total.
pub fn issues_range(offset: i64, shown: i64, total: i64) -> String {
    format!("{}–{} из {total}", offset + 1, offset + shown)
}
/// Issues empty state: the audit ran, nothing fired.
pub const ISSUES_EMPTY: &str = "Нарушений не найдено";
/// Issues empty-state hint.
pub const ISSUES_EMPTY_HINT: &str = "Сохранённый аудит не нашёл ни одной находки.";

// ---- slice D3: HTML content search -------------------------------------------

/// Search field caption.
pub const SEARCH_QUERY: &str = "Строка для поиска";
/// Search run button.
pub const SEARCH_RUN: &str = "Найти";
/// Scope chip: visible text.
pub const SEARCH_SCOPE_TEXT: &str = "Видимый текст";
/// Scope chip: raw HTML markup.
pub const SEARCH_SCOPE_HTML: &str = "HTML-код";
/// Scope chip: `<head>` markup.
pub const SEARCH_SCOPE_HEAD: &str = "Head";
/// Scope chip: CSS-selector markup.
pub const SEARCH_SCOPE_CSS: &str = "Элемент (CSS)";
/// Scope chip: rendered DOM after JavaScript.
pub const SEARCH_SCOPE_DOM: &str = "DOM после JS";
/// Option checkbox: case sensitivity.
pub const SEARCH_CASE: &str = "Учитывать регистр";
/// Option checkbox: only 2xx pages.
pub const SEARCH_ONLY_200: &str = "Только HTML 200";
/// Selector field hint of the CSS scope.
pub const SEARCH_SELECTOR: &str = "CSS-селектор…";
/// Idle state before the first run.
pub const SEARCH_IDLE_TITLE: &str = "Поиск по содержимому";
/// Idle-state hint.
pub const SEARCH_IDLE_HINT: &str = "Введите строку — поиск идёт по сохранённому HTML скана.";
/// No hits, the absence is confirmed.
pub const SEARCH_NONE: &str = "Ничего не найдено";
/// No-hit hint.
pub const SEARCH_NONE_HINT: &str = "Маркера нет ни в одном измеренном документе.";
/// Coverage unavailable: bodies were not retained.
pub const SEARCH_NO_HTML: &str = "HTML не сохранён в этом скане";
/// No-HTML hint.
pub const SEARCH_NO_HTML_HINT: &str =
    "Скан собран без сохранения тел документов — искать не по чему.";
/// Coverage unavailable for the chosen scope.
pub const SEARCH_UNAVAILABLE: &str = "Поиск по этому участку недоступен";
/// Hit list column: occurrences.
pub const SEARCH_COL_HITS: &str = "Совпадений";
/// Export affordance: hits as TSV.
pub const SEARCH_COPY: &str = "Скопировать результаты";
/// Open the hit's URL in the URL inspector.
pub const SEARCH_OPEN: &str = "Открыть в инспекторе";
/// Coverage line: measured of selected documents.
pub fn search_coverage(measured: i64, selected: i64) -> String {
    format!("Измерено документов: {measured} из {selected}")
}
/// Occurrence navigation: k of n.
pub fn search_match(k: usize, n: i64) -> String {
    format!("совпадение {k} из {n}")
}
/// Hit counter line.
pub fn search_hits(n: i64) -> String {
    format!("документов с совпадением: {n}")
}
/// Partial coverage prefix of the hits line.
pub const SEARCH_PARTIAL: &str = "покрытие частичное · ";

// ---- slice D3: scan comparison ------------------------------------------------

/// Picker label: the earlier scan.
pub const COMPARE_BEFORE: &str = "Было";
/// Picker label: the later scan.
pub const COMPARE_AFTER: &str = "Стало";
/// Swap affordance tooltip.
pub const COMPARE_SWAP: &str = "Поменять местами";
/// Run button.
pub const COMPARE_RUN: &str = "Сравнить";
/// Empty state: fewer than two saved scans.
pub const COMPARE_EMPTY: &str = "Сравнивать пока не с чем";
/// Empty-state hint.
pub const COMPARE_EMPTY_HINT: &str =
    "Рядом с открытым сканом нужен ещё один сохранённый scan.sqlite — тогда появится сравнение.";
/// Picker placeholder.
pub const COMPARE_PICK: &str = "Выберите скан";
/// Compatibility warning of a different crawl configuration.
pub const COMPARE_CONFIG_WARN: &str =
    "Сканы собраны с разными настройками — различия могут быть следствием конфигурации, а не сайта.";
/// Partition line: common URLs and the only-before/only-after tails.
pub fn compare_note(common: i64, only_b: i64, only_a: i64) -> String {
    format!("Общих URL: {common} · только в №1: {only_b} · только в №2: {only_a}")
}
/// KPI card: median words.
pub const COMPARE_KPI_WORDS: &str = "Слова (медиана)";
/// KPI card: median inbound links.
pub const COMPARE_KPI_INLINKS: &str = "Входящие ссылки (медиана)";
/// KPI card: median response time.
pub const COMPARE_KPI_RESPONSE: &str = "Медиана ответа";
/// KPI card: pages with title over 60 chars.
pub const COMPARE_KPI_TITLE60: &str = "Title > 60";
/// KPI card: canonical edits on shared URLs.
pub const COMPARE_KPI_CANONICAL: &str = "Изменения canonical";
/// KPI card: status-code changes on shared URLs.
pub const COMPARE_KPI_STATUS: &str = "Изменения кода";
/// Tab: pages whose status improved.
pub const COMPARE_TAB_FIXED: &str = "Исправлено";
/// Tab: pages that improved on the metric.
pub const COMPARE_TAB_BETTER: &str = "Улучшилось";
/// Tab: pages that regressed on the metric.
pub const COMPARE_TAB_WORSE: &str = "Ухудшилось";
/// Tab: URLs only in the first scan.
pub const COMPARE_TAB_GONE: &str = "Только в №1";
/// Tab: URLs only in the second scan.
pub const COMPARE_TAB_NEW: &str = "Только в №2";
/// Metric: word count.
pub const COMPARE_METRIC_WORDS: &str = "Слова";
/// Metric: inbound links.
pub const COMPARE_METRIC_INLINKS: &str = "Внутренние ссылки";
/// Metric: response time.
pub const COMPARE_METRIC_RESPONSE: &str = "Время ответа";
/// Metric picker caption of the better/worse tabs.
pub const COMPARE_METRIC: &str = "Метрика";
/// Rows column: value in the first scan.
pub const COMPARE_COL_BEFORE: &str = "Было";
/// Rows column: value in the second scan.
pub const COMPARE_COL_AFTER: &str = "Стало";
/// Rows column: difference.
pub const COMPARE_COL_DELTA: &str = "Δ";
/// Rows column: outcome.
pub const COMPARE_COL_OUTCOME: &str = "Итог";
/// Outcome: the URL is new in scan №2.
pub const OUTCOME_NEW: &str = "Новый URL";
/// Outcome: the URL disappeared after scan №1.
pub const OUTCOME_GONE: &str = "Удалена";
/// Outcome: the status rank improved.
pub const OUTCOME_FIXED: &str = "Код улучшился";
/// Outcome on a growing metric.
pub const OUTCOME_UP: &str = "Рост";
/// Outcome on a shrinking metric.
pub const OUTCOME_DOWN: &str = "Снижение";
/// Outcome: the page got faster.
pub const OUTCOME_FASTER: &str = "Быстрее";
/// Outcome: the page got slower.
pub const OUTCOME_SLOWER: &str = "Медленнее";
/// URL-compare screen title.
pub const CURL_TITLE: &str = "Сравнение URL";
/// URL-compare: back to the two-scan view.
pub const CURL_BACK: &str = "Назад к сравнению";
/// URL-compare column: field name.
pub const CURL_COL_FIELD: &str = "Поле";
/// URL-compare: the URL is in neither scan.
pub const CURL_ABSENT: &str = "URL нет ни в одном из сканов";
/// URL-compare: outlinks gained.
pub const CURL_LINKS_ADDED: &str = "Добавленные ссылки";
/// URL-compare: outlinks lost.
pub const CURL_LINKS_REMOVED: &str = "Удалённые ссылки";
/// State label of a compare field row.
pub fn curl_state(state: &str) -> &'static str {
    match state {
        "added" => "появилось",
        "removed" => "исчезло",
        "changed" => "изменилось",
        _ => "без изменений",
    }
}
/// Human label of a URL-compare field id.
pub fn compare_field_label(field: &str) -> &'static str {
    match field {
        "status_code" => "Код ответа",
        "title" => "Title",
        "meta_description" => "Description",
        "h1" => "H1",
        "word_count" => "Слов",
        "inlinks" => "Входящих ссылок",
        "outlinks" => "Исходящих ссылок",
        "canonical" => "Canonical",
        "indexable" => "Индексируемость",
        "response_time" => "Время ответа (мс)",
        "size_bytes" => "Размер HTML",
        _ => "Поле",
    }
}
/// Outcome label of a compare row.
pub fn compare_outcome(tab: &str, metric: &str) -> &'static str {
    match (tab, metric) {
        ("new", _) => OUTCOME_NEW,
        ("gone", _) => OUTCOME_GONE,
        ("fixed", _) => OUTCOME_FIXED,
        ("better", "response") => OUTCOME_FASTER,
        ("worse", "response") => OUTCOME_SLOWER,
        ("better", _) => OUTCOME_UP,
        ("worse", _) => OUTCOME_DOWN,
        _ => "",
    }
}
/// Compare tab label by id.
pub fn compare_tab_label(tab: &str) -> &'static str {
    match tab {
        "fixed" => COMPARE_TAB_FIXED,
        "better" => COMPARE_TAB_BETTER,
        "worse" => COMPARE_TAB_WORSE,
        "gone" => COMPARE_TAB_GONE,
        _ => COMPARE_TAB_NEW,
    }
}
/// Compare metric label by id.
pub fn compare_metric_label(metric: &str) -> &'static str {
    match metric {
        "inlinks" => COMPARE_METRIC_INLINKS,
        "response" => COMPARE_METRIC_RESPONSE,
        _ => COMPARE_METRIC_WORDS,
    }
}
/// Search scope chip label by id.
pub fn search_scope_label(scope: &str) -> &'static str {
    match scope {
        "html" => SEARCH_SCOPE_HTML,
        "head" => SEARCH_SCOPE_HEAD,
        "css" => SEARCH_SCOPE_CSS,
        "dom" => SEARCH_SCOPE_DOM,
        _ => SEARCH_SCOPE_TEXT,
    }
}

// ---- shell v2: start screen, three modes, menus ------------------------------

/// Start screen heading.
pub const START_TITLE: &str = "Что вы хотите сделать?";
/// Start screen subtitle.
pub const START_SUB: &str = "Три инструмента в одном окне — режим меняется в любой момент.";
/// Start card: crawler description.
pub const START_CRAWLER_D: &str =
    "Просканировать сайт: URL, проблемы, поиск и сравнение сканов. Без проекта.";
/// Start card: log-analysis description.
pub const START_LOGS_D: &str = "Логи сервера: боты, обход и сопоставление со сканом.";
/// Start card: agent-mode description.
pub const START_AGENT_D: &str = "Проект целиком: цель, задачи, входящие, семантика и отчёты.";
/// Start screen footer hint.
pub const START_HINT: &str = "⌘1 · ⌘2 · ⌘3 — режимы · ⌘K — действия и переходы";

/// Nav item: one-URL check (the former simple page).
pub const NAV_CHECK: &str = "Проверка страницы";
/// Nav item: log-analysis overview.
pub const NAV_LOGS: &str = "Логи";
/// Nav item: bots of the logs.
pub const NAV_LOGS_BOTS: &str = "Боты";
/// Nav item: logs × scan coverage.
pub const NAV_LOGS_X: &str = "Логи × скан";
/// Nav section caption of the log tool.
pub const NAV_SEC_LOGS: &str = "Анализ логов";

/// Log screens: empty-state title.
pub const LOGS_EMPTY_TITLE: &str = "Логов пока нет";
/// Log screens: empty-state hint.
pub const LOGS_EMPTY_HINT: &str =
    "Загрузка access-логов и разбор ботов появятся в следующей версии.";
/// Work screen: empty-state title.
pub const WORK_TITLE: &str = "Агентский режим — проект";
/// Work screen: empty-state hint.
pub const WORK_HINT: &str =
    "Здесь будут цель, задачи и отчёты проекта. Выберите проект или начните с краула.";

/// About window: product tagline.
pub const ABOUT_LINE: &str = "Краулер, анализ логов и проекты в одном окне.";
/// Usage window: no paid sources connected.
pub const USAGE_EMPTY: &str = "Платные источники не подключены — расходов нет.";
/// Shortcuts window: title row of a group.
pub const SHORTCUT_MODES: &str = "Режимы";
/// Shortcuts window: app-wide keys group.
pub const SHORTCUT_APP: &str = "Приложение";
/// Shortcuts window: edit menu keys group.
pub const SHORTCUT_EDIT: &str = "Правка";

#[cfg(test)]
mod tests {
    #[test]
    fn every_table_column_has_a_human_label() {
        for (name, _) in crate::data::columns::COLUMNS {
            let label = super::column_label(name);
            assert!(!label.is_empty(), "column {name} has an empty label");
            assert_ne!(label, *name, "column {name} lacks a human label");
        }
    }

    #[test]
    fn every_preset_has_a_human_label() {
        for id in crate::data::presets::PRESET_IDS {
            assert_ne!(super::preset_label(id), *id, "preset {id} lacks a label");
        }
    }
}
