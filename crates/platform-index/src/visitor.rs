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
        Err(hbk_reader::HbkError::HtmlEntryNotFound(_)) => None,
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

/// Распарсить страницу системного перечисления + значения из её детей `/properties/`.
pub fn visit_enum_page(content: &mut HbkContent, page: &Page) -> Option<EnumInfo> {
    let html = try_read_html(content, &page.html_path)?;
    let mut info = parse_enum_page(&html);

    for child in &page.children {
        if !child.html_path.contains("/properties/") {
            continue;
        }
        if let Some(child_html) = try_read_html(content, &child.html_path) {
            info.values.push(parse_enum_value_page(&child_html));
        }
    }
    Some(info)
}

/// Распарсить страницу типа (объекта) + properties/methods/constructors из дочерних разделов.
///
/// В TOC у типа дочерние страницы — «Свойства», «Методы», «Конструкторы»
/// (по русскому `title.ru`; апстрим читает их через `title.en`, но фактически
/// в `en` у этого подмножества лежат русские строки). Внутри каждой —
/// листовые страницы конкретных членов.
pub fn visit_type_page(content: &mut HbkContent, page: &Page) -> Option<ObjectInfo> {
    let html = try_read_html(content, &page.html_path)?;
    let mut info = parse_object_page(&html);

    for sub in &page.children {
        let label = if !sub.title.ru.is_empty() {
            sub.title.ru.as_str()
        } else {
            sub.title.en.as_str()
        };
        match label {
            "Свойства" => info.properties = visit_properties_page(content, sub),
            "Методы" => info.methods = visit_methods_page(content, sub),
            "Конструкторы" => info.constructors = visit_constructors_page(content, sub),
            _ => {}
        }
    }

    Some(info)
}

pub fn visit_properties_page(content: &mut HbkContent, page: &Page) -> Vec<PropertyInfo> {
    let mut out = Vec::new();
    for child in &page.children {
        if !child.html_path.contains("/properties/") {
            continue;
        }
        if child.title.ru.starts_with('<') {
            // Апстрим фильтрует псевдо-имена в угловых скобках (например, `<Свойство>`).
            continue;
        }
        if let Some(html) = try_read_html(content, &child.html_path) {
            out.push(parse_property_page(&html));
        }
    }
    out
}

pub fn visit_methods_page(content: &mut HbkContent, page: &Page) -> Vec<MethodInfo> {
    let mut out = Vec::new();
    for child in &page.children {
        if let Some(html) = try_read_html(content, &child.html_path) {
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

pub fn visit_constructors_page(content: &mut HbkContent, page: &Page) -> Vec<ConstructorInfo> {
    let mut out = Vec::new();
    for child in &page.children {
        if !child.html_path.contains("/ctors/") {
            continue;
        }
        if let Some(html) = try_read_html(content, &child.html_path) {
            out.push(parse_constructor_page(&html));
        }
    }
    out
}

/// Глобальные методы: дочерние страницы у `Global context` с путём `/methods/`.
/// Каждая такая страница — это раздел-каталог с настоящими методами внутри.
pub fn collect_global_methods(content: &mut HbkContent, global: &Page) -> Vec<MethodInfo> {
    let mut out = Vec::new();
    for child in &global.children {
        if !child.html_path.contains("/methods/") {
            continue;
        }
        // Это раздел («Функции работы со строками» и т.п.); реальные методы — в его детях.
        out.extend(visit_methods_page(content, child));
    }
    out
}

/// Глобальные свойства: подстраница «Свойства» у `Global context`, в её детях — реальные свойства.
pub fn collect_global_properties(content: &mut HbkContent, global: &Page) -> Vec<PropertyInfo> {
    for child in &global.children {
        let label = if !child.title.ru.is_empty() {
            child.title.ru.as_str()
        } else {
            child.title.en.as_str()
        };
        if label == "Свойства" {
            return visit_properties_page(content, child);
        }
    }
    Vec::new()
}
