//! Парсеры отдельных типов блоков html-страницы.
//!
//! Каждая функция — порт одного `BlockHandler` из `BlockHandler.kt`.
//! На вход — html-фрагмент главы, на выход — структурированное значение.

use ego_tree::NodeRef;
use scraper::{Html, Node, Selector};
use std::sync::OnceLock;

use crate::html::{collapse_whitespace, extract_text, serialize_node, to_markdown};
use crate::models::{MethodParameterInfo, RelatedObject, ValueInfo};

/// Селекторы блоков — статические. `Selector::parse` компилирует CSS-строку при
/// каждом вызове, а парсеры блоков зовутся на каждую страницу справки (десятки
/// тысяч раз за сборку). Компиляция не зависит от содержимого страницы.
fn heading_selector() -> &'static Selector {
    static SEL: OnceLock<Selector> = OnceLock::new();
    // 8.3.17 отдаёт заголовок как `div`, остальные версии — как `p`.
    SEL.get_or_init(|| {
        Selector::parse("p.V8SH_heading, div.V8SH_heading").expect("V8SH_heading selector")
    })
}

fn title_selector() -> &'static Selector {
    static SEL: OnceLock<Selector> = OnceLock::new();
    SEL.get_or_init(|| {
        Selector::parse("p.V8SH_title, div.V8SH_title").expect("V8SH_title selector")
    })
}

fn page_title_selector() -> &'static Selector {
    static SEL: OnceLock<Selector> = OnceLock::new();
    SEL.get_or_init(|| {
        Selector::parse("h1.V8SH_pagetitle, p.V8SH_pagetitle, div.V8SH_pagetitle")
            .expect("V8SH_pagetitle selector")
    })
}

fn anchor_selector() -> &'static Selector {
    static SEL: OnceLock<Selector> = OnceLock::new();
    SEL.get_or_init(|| Selector::parse("a").expect("a selector"))
}

fn rubric_selector() -> &'static Selector {
    static SEL: OnceLock<Selector> = OnceLock::new();
    SEL.get_or_init(|| Selector::parse("div.V8SH_rubric").expect("V8SH_rubric selector"))
}

// Регулярка из BlockHandler.kt: NAMES_PATTERN.
// Шаблон вида "RuName(EnName)" — извлекает русское и английское имя.
fn split_dual_name(text: &str) -> (String, String) {
    if let Some(open) = text.rfind('(') {
        if let Some(close) = text.rfind(')') {
            if close > open {
                let ru = text[..open].trim().to_string();
                let en = text[open + 1..close].trim().to_string();
                // Английское имя — только латиница/цифры/пробелы/`._-:`
                // (`HTTP-service module`, `MetadataObject: HTTPService`).
                // Кириллица или угловые скобки означают, что в скобках не
                // синоним, а псевдо-имя открытой коллекции
                // (`<Имя картинки> (<Icon name>)`) — оставляем как есть.
                if !en.is_empty()
                    && en.chars().all(|c| {
                        c.is_ascii_alphanumeric()
                            || c.is_ascii_whitespace()
                            || matches!(c, '.' | '_' | '-' | ':')
                    })
                {
                    return (ru, en);
                }
            }
        }
    }
    (text.trim().to_string(), String::new())
}

/// Парсер «головы» страницы: ищет `<p class="V8SH_heading">` или `<p class="V8SH_title">`,
/// извлекает текст и разделяет на русское/английское имя по шаблону «RuName(EnName)».
///
/// Порт `NameBlockHandler` из `BlockHandler.kt`.
pub fn parse_head_name(html: &str) -> (String, String) {
    let doc = Html::parse_fragment(html);

    // Первый НЕПУСТОЙ заголовок: heading → title → pagetitle. Важно не
    // `or_else`: заголовок может существовать, но быть пустым.
    let raw = [heading_selector(), title_selector(), page_title_selector()]
        .iter()
        .filter_map(|sel| doc.select(sel).next())
        .map(|el| collapse_whitespace(el.text().collect::<String>().trim()))
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    if raw.is_empty() {
        return (String::new(), String::new());
    }
    split_dual_name(&raw)
}

/// Описание (`Описание:`) — html → Markdown.
pub fn parse_description(body_html: &str) -> String {
    to_markdown(body_html)
}

/// Пример (`Пример:`) — текст с сохранением `<br>` → `\n`.
///
/// Порт `ExampleBlockHandler.kt`: только текстовый контент + переносы по `<br>`.
pub fn parse_example(body_html: &str) -> String {
    // Любые формы `<br>` (`<BR />`, `<br class=…>`) — перевод строки.
    let normalized = br_to_newline(body_html);
    let text = extract_text_keep_breaks(&normalized);
    text.trim().replace('\u{00a0}', " ")
}

/// Заменить `<br>` в любом регистре и с любыми атрибутами на `\n`.
fn br_to_newline(html: &str) -> String {
    let bytes = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        if lower[i..].starts_with("<br") {
            let after = i + 3;
            let is_tag = bytes
                .get(after)
                .is_some_and(|b| matches!(b, b' ' | b'/' | b'>'));
            if is_tag {
                if let Some(rel) = html[i..].find('>') {
                    out.push('\n');
                    i += rel + 1;
                    continue;
                }
            }
        }
        let ch = html[i..]
            .chars()
            .next()
            .expect("byte index is a char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Извлечь текст из html, сохраняя \n.
fn extract_text_keep_breaks(html: &str) -> String {
    let doc = Html::parse_fragment(html);
    let mut buf = String::new();
    for node in doc.tree.nodes() {
        if let scraper::Node::Text(text) = node.value() {
            buf.push_str(text);
        }
    }
    buf
}

/// «См. также:» — список ссылок `<a href="...">текст</a>`.
///
/// Порт `RelatedObjectsBlockHandler.kt`: для каждого `<a>` берём текст и href.
/// `v8help://` ссылки сохраняем как есть (используется консьюмерами).
pub fn parse_related_objects(body_html: &str) -> Vec<RelatedObject> {
    let doc = Html::parse_fragment(body_html);
    let mut out = Vec::new();
    for a in doc.select(anchor_selector()) {
        let text_raw: String = a.text().collect();
        // Свернуть пробелы ДО замены « ,»: NBSP перед запятой сворачивается
        // collapse_whitespace только здесь.
        let text = collapse_whitespace(text_raw.trim()).replace(" ,", ",");
        let href = a.value().attr("href").unwrap_or("").to_string();
        if !text.is_empty() {
            out.push(RelatedObject { name: text, href });
        }
    }
    out
}

/// Примечание (`Примечание:`) — html → Markdown (так же как описание).
pub fn parse_note(body_html: &str) -> String {
    to_markdown(body_html)
}

/// Использование (`Использование:`) — флаг «Только чтение».
///
/// Порт `ReadOnlyBlockHandler.kt`: ищет текст начинающийся с «Только чтение».
pub fn parse_readonly(body_html: &str) -> bool {
    extract_text(body_html).starts_with("Только чтение")
}

/// «Возвращаемое значение:» / «Описание:» свойства / параметр — тип + описание.
///
/// Структура текста (порт `ValueInfoBlockHandler.kt`): «Тип:» → имя типа (может
/// состоять из нескольких `v8help`-ссылок и содержать точки внутри имён, как
/// `ХранилищеНастроекМенеджер.<Имя хранилища>`) → завершающая точка ВНЕ ссылок →
/// описание.
///
/// Если маркера «Тип:» нет, но текст главы непуст — возвращаем описание с пустым
/// `type_name`: так устроены тысячи страниц свойств, и терять их текст нельзя.
pub fn parse_value_info(body_html: &str) -> Option<ValueInfo> {
    let md = to_markdown(body_html);
    if md.trim().is_empty() {
        return None;
    }

    let Some(rest) = md
        .strip_prefix("Тип:")
        .or_else(|| md.find("Тип:").map(|idx| &md[idx + "Тип:".len()..]))
    else {
        // Главы без «Тип:» — целиком описание.
        return Some(ValueInfo {
            type_name: String::new(),
            description: md.trim().to_string(),
        });
    };
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }

    let (type_raw, description) = split_type_and_description(rest);
    let type_name = clean_markdown_inline(type_raw);
    if type_name.is_empty() && description.is_empty() {
        return None;
    }
    Some(ValueInfo {
        type_name,
        description,
    })
}

/// Отделить тип от описания: тип кончается первой точкой ВНЕ backtick-спанов.
/// Точки внутри спанов принадлежат именам типов (`БизнесПроцессМенеджер.<Имя>`),
/// первая же точка внутри такого имени не должна резать тип (аудит: 199 страниц).
fn split_type_and_description(rest: &str) -> (&str, String) {
    let mut in_code = false;
    for (idx, ch) in rest.char_indices() {
        match ch {
            '`' => in_code = !in_code,
            '.' if !in_code => {
                let after = &rest[idx + ch.len_utf8()..];
                if after.is_empty() || after.starts_with(char::is_whitespace) {
                    return (&rest[..idx], after.trim().to_string());
                }
            }
            _ => {}
        }
    }
    (rest, String::new())
}

/// Убрать markdown-разметку типа: обрамляющие `*`/`` ` `` и пробелы после
/// запятых (`A, B` → `A,B`, как в контракте апстрима).
fn clean_markdown_inline(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c: char| c == '*' || c == '`' || c.is_whitespace())
        .replace('`', "")
        .replace(", ", ",")
        .trim()
        .to_string()
}

/// Параметры метода/конструктора (`Параметры:`).
///
/// Каждый параметр представлен в html как `<div class="V8SH_rubric">…</div>`
/// (внутри — имя вида `<имя> (необязательный)`), а тип и описание лежат
/// СЛЕДОМ за `div` до следующего `rubric`:
/// `Тип: <a …>Строка</a>. <br>Имя панели.`
///
/// Порт `ParametersBlockHandler.kt`.
pub fn parse_parameters(body_html: &str) -> Vec<MethodParameterInfo> {
    let doc = Html::parse_fragment(body_html);
    let rubrics: Vec<_> = doc.select(rubric_selector()).collect();

    let mut params = Vec::new();
    for rubric in rubrics {
        let raw: String = rubric.text().collect();
        let text = collapse_whitespace(raw.trim());
        if text.is_empty() {
            continue;
        }
        let (name, is_optional) = parse_parameter_header(&text);

        // Собираем html всех sibling-узлов до следующего rubric.
        let mut chunk = String::new();
        let mut node = rubric.next_sibling();
        while let Some(n) = node {
            if is_rubric_node(n) {
                break;
            }
            if let Some(html) = serialize_node(n) {
                chunk.push_str(&html);
            }
            node = n.next_sibling();
        }

        let (type_name, description) = match parse_value_info(&chunk) {
            Some(info) => (info.type_name, info.description),
            None => (String::new(), String::new()),
        };
        params.push(MethodParameterInfo {
            name,
            type_name,
            is_optional,
            description,
        });
    }
    params
}

/// Узел — `<div class="V8SH_rubric">` (начало следующего параметра).
fn is_rubric_node(node: NodeRef<Node>) -> bool {
    matches!(node.value(), Node::Element(el)
        if el.attr("class").is_some_and(|c| c.split_whitespace().any(|t| t == "V8SH_rubric")))
}

/// Разобрать заголовок параметра: `<имя> (необязательный)` → (имя, true).
fn parse_parameter_header(text: &str) -> (String, bool) {
    // Шаблон: возможны кавычки/скобки. PARAMETER_NAME_PATTERN = `<([^&]+)>\s*(?:\(([^)]+)\))?`
    let trimmed = text.trim();
    let stripped = trimmed
        .strip_prefix('<')
        .and_then(|s| s.find('>').map(|i| &s[..i]));
    if let Some(inner) = stripped {
        let name = inner.trim().to_string();
        let after = trimmed.split_once('>').map(|x| x.1).unwrap_or("").trim();
        let is_optional = after.contains("необязательный");
        (name, is_optional)
    } else {
        (trimmed.to_string(), false)
    }
}

/// Синтаксис метода/конструктора (`Синтаксис:`) — текст без html.
pub fn parse_syntax(body_html: &str) -> String {
    extract_text(body_html)
}

/// `Some(text)`, только если текст непустой: пустая глава — не значение.
pub fn non_empty(text: String) -> Option<String> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dual_name_with_underscore_is_split() {
        assert_eq!(
            split_dual_name("Windows_x86 (Windows_x86)"),
            ("Windows_x86".to_string(), "Windows_x86".to_string())
        );
        assert_eq!(
            split_dual_name("Версия8_2 (Version8_2)"),
            ("Версия8_2".to_string(), "Version8_2".to_string())
        );
    }

    #[test]
    fn dual_name_with_dash_and_colon_is_split() {
        assert_eq!(
            split_dual_name("Модуль HTTP-сервиса (HTTP-service module)"),
            (
                "Модуль HTTP-сервиса".to_string(),
                "HTTP-service module".to_string()
            )
        );
        assert_eq!(
            split_dual_name("ОбъектМетаданных: HTTPСервис (MetadataObject: HTTPService)"),
            (
                "ОбъектМетаданных: HTTPСервис".to_string(),
                "MetadataObject: HTTPService".to_string()
            )
        );
    }

    #[test]
    fn pseudo_value_in_angle_brackets_stays_whole() {
        // `<Имя картинки> (<Icon name>)` — не пара имён, а описание открытой
        // коллекции; оставляем целиком, чтобы признак `<` сохранился.
        let (ru, en) = split_dual_name("<Имя картинки> (<Icon name>)");
        assert!(ru.starts_with('<'));
        assert!(en.is_empty());
    }

    #[test]
    fn value_info_cleans_markdown_and_keeps_composite_type() {
        let html = r#"Тип: <a href="v8help://x">СтандартноеХранилищеНастроекМенеджер</a>, <a href="v8help://y">ХранилищеНастроекМенеджер.<Имя хранилища></a>. Описание далее."#;
        let info = parse_value_info(html).expect("value info");
        assert_eq!(
            info.type_name,
            "СтандартноеХранилищеНастроекМенеджер,ХранилищеНастроекМенеджер.<Имя хранилища>"
        );
        assert_eq!(info.description, "Описание далее.");
    }

    #[test]
    fn value_info_without_type_keeps_description() {
        let info = parse_value_info("Только описательный текст без типа.").expect("value info");
        assert!(info.type_name.is_empty());
        assert_eq!(info.description, "Только описательный текст без типа.");
        assert!(parse_value_info("").is_none());
    }

    #[test]
    fn value_info_with_bold_marker_is_clean() {
        let info = parse_value_info("<b>Тип:</b> Строка. Описание.").expect("value info");
        assert_eq!(info.type_name, "Строка");
        assert_eq!(info.description, "Описание.");
    }

    #[test]
    fn parameters_get_type_and_description() {
        let html = r#"<div class="V8SH_rubric"><p>&lt;Имя&gt; (необязательный)</p></div>Тип: <a href="v8help://x">Строка</a>. <br>Имя панели."#;
        let params = parse_parameters(html);
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "Имя");
        assert!(params[0].is_optional);
        assert_eq!(params[0].type_name, "Строка");
        assert!(
            params[0].description.contains("Имя панели"),
            "{:?}",
            params[0].description
        );
    }

    #[test]
    fn example_accepts_br_variants() {
        assert_eq!(parse_example("a<BR />b<br class=\"x\">c"), "a\nb\nc");
    }

    #[test]
    fn related_object_nbsp_before_comma_is_collapsed() {
        let objs = parse_related_objects("<a href=\"h\">См.\u{a0}, ещё</a>");
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0].name, "См., ещё");
    }
}
