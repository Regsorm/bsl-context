//! Обход TOC платформы.
//!
//! Порт `PlatformContextPagesVisitor.kt` + `PlatformContextReader.Context`.
//! Главное — точно определить тип корневой страницы (`GLOBAL_CONTEXT`,
//! `ENUMS_CATALOG`, `TYPES_CATALOG`), а внутри типа дойти до листовых страниц
//! (drill-down через `catalog\d+\.html`).

use hbk_reader::{HbkContent, Page};
use regex::Regex;

use hbk_parser::{
    parse_constructor_page, parse_enum_page, parse_enum_value_page, parse_method_page,
    parse_object_page, parse_property_page, ConstructorInfo, EnumInfo, MethodInfo, ObjectInfo,
    PropertyInfo,
};

/// Источник html-страниц справки.
///
/// Абстракция введена ради параллельной сборки индекса: `HbkContent` — это
/// zip-архив с `&mut self`, и из нескольких потоков к нему ходят через обёртку
/// с блокировкой (см. `LockedSource` в `loader`). Публично, потому что
/// `visit_*` — тоже публичный API крейта.
pub trait HtmlSource {
    /// Прочитать страницу по её `htmlPath` из TOC.
    ///
    /// `None` — страницы нет (узел TOC бывает «каталогом» без html), это штатный
    /// случай и он молчит. Настоящий отказ чтения попадает в журнал: пропуск
    /// страницы не должен быть невидимым (см. `try_read_html`).
    fn read_html(&mut self, html_path: &str) -> Option<String>;
}

impl HtmlSource for HbkContent {
    fn read_html(&mut self, html_path: &str) -> Option<String> {
        try_read_html(self, html_path)
    }
}

/// Категория корневой страницы TOC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    GlobalContext,
    EnumsCatalog,
    TypesCatalog,
}

fn is_global_context(page: &Page) -> bool {
    page.html_path.contains("Global context.html")
}

fn is_enum_catalog(page: &Page) -> bool {
    // У апстрима фильтр идёт по `title.en`, что фактически содержит русские
    // строки (см. реальные данные TOC). Дублируем по обеим веткам для
    // надёжности — реальный TOC платформы хранит русские названия в `ru`.
    let names = ["Системные наборы значений", "Системные перечисления"];
    names.contains(&page.title.ru.as_str()) || names.contains(&page.title.en.as_str())
}

pub fn classify_root(page: &Page) -> RootKind {
    if is_global_context(page) {
        RootKind::GlobalContext
    } else if is_enum_catalog(page) {
        RootKind::EnumsCatalog
    } else {
        RootKind::TypesCatalog
    }
}

fn catalog_pattern() -> &'static Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"/catalog\d+\.html").unwrap())
}

fn is_catalog_page(page: &Page) -> bool {
    catalog_pattern().is_match(&page.html_path)
}

/// Рекурсивно собрать все «листовые» страницы (не каталог) под `base`.
/// Соответствует `drillDown` в апстриме.
pub fn drill_down<'a>(base: &'a Page, out: &mut Vec<&'a Page>) {
    for child in &base.children {
        if child.html_path.is_empty() {
            // Узел-группировка без собственной страницы («Таблицы запросов»
            // с 59 листьями): под ним лежат настоящие страницы — идём внутрь,
            // иначе теряются целые типы.
            if !child.children.is_empty() {
                drill_down(child, out);
            }
            continue;
        }
        if is_catalog_page(child) {
            drill_down(child, out);
        } else {
            out.push(child);
        }
    }
}

/// Разобрать корневые страницы TOC.
pub struct RootPages<'a> {
    pub global_context: Option<&'a Page>,
    pub enums: Vec<&'a Page>,
    pub types: Vec<&'a Page>,
}

pub fn collect_root_pages(pages: &[Page]) -> RootPages<'_> {
    let mut global_context: Option<&Page> = None;
    let mut enums = Vec::new();
    let mut types = Vec::new();
    for p in pages {
        if p.html_path.is_empty() && p.children.is_empty() {
            continue;
        }
        match classify_root(p) {
            RootKind::GlobalContext => global_context = Some(p),
            RootKind::EnumsCatalog => enums.push(p),
            RootKind::TypesCatalog => types.push(p),
        }
    }
    RootPages {
        global_context,
        enums,
        types,
    }
}

/// Прочитать html-страницу через `HbkContent`. `None` — страницы нет (узел
/// TOC бывает «каталогом» без html), это штатный случай и он молчит.
///
/// А вот НАСТОЯЩИЙ отказ чтения (текст не в UTF-8, битый deflate, ошибка
/// ввода-вывода) раньше тоже сворачивался в `None` тем же `.ok()`. Такой
/// пропуск незаметен: индекс собирается, `load_from_hbk` возвращает `Ok`, а у
/// пользователя недостающий метод оборачивается находкой «метод не найден в
/// платформенном контексте» на законном вызове. Теперь он попадает в журнал.
fn try_read_html(content: &mut HbkContent, html_path: &str) -> Option<String> {
    if html_path.is_empty() {
        return None;
    }
    match content.get_entry_text(html_path) {
        Ok(text) => Some(text),
        Err(hbk_reader::HbkError::HtmlEntryNotFound(_)) => {
            // Штатно это каталог без html, но бывает и битый путь TOC: debug —
            // чтобы такие потери были видны при отладке, не засоряя журнал.
            tracing::debug!(page = html_path, "страница не найдена в FileStorage");
            None
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                page = html_path,
                "страница справки не прочитана — её содержимое в индекс не попадёт"
            );
            None
        }
    }
}

/// Распарсить страницу системного перечисления + значения из потомков `/properties/`.
pub fn visit_enum_page<H: HtmlSource>(content: &mut H, page: &Page) -> Option<EnumInfo> {
    let html = content.read_html(&page.html_path)?;
    let mut info = parse_enum_page(&html);

    // Значения — все потомки с `/properties/`: у части перечислений они лежат
    // под пустым узлом-группировкой «Свойства» (5 перечислений, 28 значений
    // в 8.3.27 — раньше терялись).
    let mut value_pages = Vec::new();
    collect_property_descendants(page, &mut value_pages);
    let mut seen_html = std::collections::HashSet::new();
    for child in value_pages {
        if !seen_html.insert(child.html_path.as_str()) {
            continue;
        }
        if let Some(child_html) = content.read_html(&child.html_path) {
            info.values.push(parse_enum_value_page(&child_html));
        }
    }
    if info.values.is_empty() {
        tracing::warn!(
            page = %page.html_path,
            name = %page.title.ru,
            "у страницы-перечисления не найдено ни одного значения"
        );
    }
    Some(info)
}

/// Все потомки с `/properties/` в пути — включая проход через узлы-группировки
/// без собственной html-страницы.
fn collect_property_descendants<'a>(page: &'a Page, out: &mut Vec<&'a Page>) {
    for child in &page.children {
        if child.html_path.contains("/properties/") {
            out.push(child);
        }
        if child.html_path.is_empty() || child.html_path.contains("/properties/") {
            collect_property_descendants(child, out);
        }
    }
}

/// Отображаемое имя узла TOC: русское, при пустом — английское.
fn page_label(page: &Page) -> &str {
    if !page.title.ru.is_empty() {
        page.title.ru.as_str()
    } else {
        page.title.en.as_str()
    }
}

/// Распарсить страницу типа (объекта) + properties/methods/constructors из дочерних разделов.
///
/// В TOC у типа дочерние страницы — «Свойства», «Методы», «Конструкторы»
/// (по русскому `title.ru`; апстрим читает их через `title.en`, но фактически
/// в `en` у этого подмножества лежат русские строки). Внутри каждой —
/// листовые страницы конкретных членов.
pub fn visit_type_page<H: HtmlSource>(content: &mut H, page: &Page) -> Option<ObjectInfo> {
    let html = content.read_html(&page.html_path)?;
    let mut info = parse_object_page(&html);

    for sub in &page.children {
        let label = page_label(sub);
        match label {
            "Свойства" => info.properties = visit_properties_page(content, sub),
            "Методы" => info.methods = visit_methods_page(content, sub),
            "Конструкторы" => info.constructors = visit_constructors_page(content, sub),
            _ => {}
        }
    }

    Some(info)
}

pub fn visit_properties_page<H: HtmlSource>(content: &mut H, page: &Page) -> Vec<PropertyInfo> {
    let mut out = Vec::new();
    for child in &page.children {
        if !child.html_path.contains("/properties/") {
            continue;
        }
        if child.title.ru.starts_with('<') || child.title.en.starts_with('<') {
            // Апстрим фильтрует псевдо-имена в угловых скобках (например, `<Свойство>`).
            continue;
        }
        if let Some(html) = content.read_html(&child.html_path) {
            out.push(parse_property_page(&html));
        }
    }
    out
}

pub fn visit_methods_page<H: HtmlSource>(content: &mut H, page: &Page) -> Vec<MethodInfo> {
    let mut out = Vec::new();
    for child in &page.children {
        if let Some(html) = content.read_html(&child.html_path) {
            let info = parse_method_page(&html);
            // У страниц-«каталогов» внутри `/methods/` нет блока «Синтаксис:» —
            // парсер вернёт пустые сигнатуры. Отбрасываем такие записи, чтобы
            // в storage не попадали псевдо-методы.
            if !info.signatures.is_empty() {
                out.push(info);
            }
        }
    }
    out
}

pub fn visit_constructors_page<H: HtmlSource>(
    content: &mut H,
    page: &Page,
) -> Vec<ConstructorInfo> {
    let mut out = Vec::new();
    for child in &page.children {
        if !child.html_path.contains("/ctors/") {
            continue;
        }
        if let Some(html) = content.read_html(&child.html_path) {
            out.push(parse_constructor_page(&html));
        }
    }
    out
}

/// Глобальные методы: разделы-каталоги `Global context` с путём `/methods/`
/// (реальные методы — в их детях), а также прямые дети-методы.
pub fn collect_global_methods<H: HtmlSource>(content: &mut H, global: &Page) -> Vec<MethodInfo> {
    let mut out = Vec::new();
    for child in &global.children {
        if child.html_path.is_empty() {
            continue;
        }
        if child.html_path.contains("/methods/") {
            // Это раздел («Функции работы со строками» и т.п.); реальные методы — в его детях.
            out.extend(visit_methods_page(content, child));
            continue;
        }
        if page_label(child) == "Свойства" {
            continue; // раздел свойств глобального контекста — не методы
        }
        // 8.5.1: некоторые методы — прямые дети Global context, и путь TOC
        // указывает на страницу без сегмента `/methods/`, тогда как entry в
        // архиве лежит в `/methods/`. Пробуем оба варианта.
        let mut html = content.read_html(&child.html_path);
        if html.is_none() {
            html = content.read_html(&insert_methods_segment(&child.html_path));
        }
        if let Some(html) = html {
            let info = parse_method_page(&html);
            if !info.signatures.is_empty() {
                out.push(info);
            }
        }
    }
    out
}

/// `/objects/Global context/Name.html` → `/objects/Global context/methods/Name.html`.
fn insert_methods_segment(html_path: &str) -> String {
    match html_path.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/methods/{file}"),
        None => html_path.to_string(),
    }
}

/// Глобальные свойства: подстраница «Свойства» у `Global context`, в её детях — реальные свойства.
pub fn collect_global_properties<H: HtmlSource>(
    content: &mut H,
    global: &Page,
) -> Vec<PropertyInfo> {
    for child in &global.children {
        if page_label(child) == "Свойства" {
            return visit_properties_page(content, child);
        }
    }
    Vec::new()
}
