//! Парсеры отдельных типов блоков html-страницы.
//!
//! Каждая функция — порт одного `BlockHandler` из `BlockHandler.kt`.
//! На вход — html-фрагмент главы, на выход — структурированное значение.

use scraper::{Html, Selector};

use crate::html::{collapse_whitespace, extract_text, to_markdown};
use crate::models::{MethodParameterInfo, RelatedObject, ValueInfo};

// Регулярка из BlockHandler.kt: NAMES_PATTERN.
// Шаблон вида "RuName(EnName)" — извлекает русское и английское имя.
fn split_dual_name(text: &str) -> (String, String) {
    if let Some(open) = text.rfind('(') {
        if let Some(close) = text.rfind(')') {
            if close > open {
                let ru = text[..open].trim().to_string();
                let en = text[open + 1..close].trim().to_string();
                if !en.is_empty() && !en.contains(' ') || (en.contains(' ') && en.is_ascii()) {
                    // Если содержимое скобок похоже на английское имя (только латиница/пробелы) —
                    // считаем парой ru/en. Иначе — оставляем всё имя как ru.
                    // Подчёркивание — законная часть английского имени
                    // (`Windows_x86`, `Version8_2`); без него такие значения
                    // оставались с синонимом в скобках прямо в `name_ru`.
                    if en.chars().all(|c| {
                        c.is_ascii_alphanumeric()
                            || c.is_ascii_whitespace()
                            || c == '.'
                            || c == '_'
                    }) {
                        return (ru, en);
                    }
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
    let heading_sel = Selector::parse("p.V8SH_heading").expect("V8SH_heading selector");
    let title_sel = Selector::parse("p.V8SH_title").expect("V8SH_title selector");

    let raw = doc
        .select(&heading_sel)
        .next()
        .map(|el| el.text().collect::<String>())
        .or_else(|| {
            doc.select(&title_sel)
                .next()
                .map(|el| el.text().collect::<String>())
        })
        .unwrap_or_default();
    let normalized = collapse_whitespace(raw.trim());
    if normalized.is_empty() {
        return (String::new(), String::new());
    }
    split_dual_name(&normalized)
}

/// Описание (`Описание:`) — html → Markdown.
pub fn parse_description(body_html: &str) -> String {
    to_markdown(body_html)
}

/// Пример (`Пример:`) — текст с сохранением `<br>` → `\n`.
///
/// Порт `ExampleBlockHandler.kt`: только текстовый контент + переносы по `<br>`.
pub fn parse_example(body_html: &str) -> String {
    // Заменим <br>/<BR> на \n и вытащим текст.
    let normalized = body_html
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<BR>", "\n")
        .replace("<BR/>", "\n");
    let text = extract_text_keep_breaks(&normalized);
    text.trim().replace('\u{00a0}', " ")
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
    let a_sel = Selector::parse("a").expect("a selector");
    let mut out = Vec::new();
    for a in doc.select(&a_sel) {
        let text_raw: String = a.text().collect();
        let text = collapse_whitespace(text_raw.trim().replace(" ,", ",").as_str());
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

/// «Возвращаемое значение:» / «Описание:» свойства — извлекает тип + описание.
///
/// Структура текста (порт `ValueInfoBlockHandler.kt`):
/// - блок начинается с «Тип:»
/// - дальше идёт имя типа, затем точка
/// - после точки начинается описание (произвольный html → Markdown)
///
/// Возвращает `None` если блок пустой или нет «Тип:».
pub fn parse_value_info(body_html: &str) -> Option<ValueInfo> {
    // Стратегия: получаем чистый markdown всего тела, потом пытаемся выделить
    // префикс «Тип: <тип>.» и остаток превратить в описание.
    let md = to_markdown(body_html);
    // Найти начало «Тип:»
    let after_marker = md.strip_prefix("Тип:").or_else(|| {
        // иногда «Тип:» идёт после whitespace или с дефисом
        md.find("Тип:").map(|idx| &md[idx + "Тип:".len()..])
    });
    let Some(rest) = after_marker else { return None };
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }

    // Попытка взять имя типа до первой «.» (в hbk описание начинается после точки).
    let (type_name, description) = match rest.find('.') {
        Some(idx) => {
            let name = rest[..idx].trim().to_string();
            let desc = rest[idx + 1..].trim().to_string();
            (name, desc)
        }
        None => (rest.trim().to_string(), String::new()),
    };

    if type_name.is_empty() {
        return None;
    }
    Some(ValueInfo {
        type_name,
        description,
    })
}

/// Параметры метода (`Параметры:`).
///
/// Каждый параметр представлен в html как `<div class="V8SH_rubric"><p>...</p></div>`
/// (имя в формате `<имя> (необязательный)`), затем абзацы с типом и описанием.
/// Порт `ParametersBlockHandler.kt`.
///
/// Минимальная реализация: ищем все `div.V8SH_rubric`, для каждого вытаскиваем
/// имя/optional, затем собираем «следующий sibling-блок» как тип+описание до
/// следующего rubric.
pub fn parse_parameters(body_html: &str) -> Vec<MethodParameterInfo> {
    // Аппроксимация (детальный порт со всем стейт-машинами оставлен на Phase 3):
    // для каждого rubric извлекаем имя и optional, тип/описание берём из текста
    // до следующего rubric (в простом случае).
    let doc = Html::parse_fragment(body_html);
    let rubric_sel =
        Selector::parse("div.V8SH_rubric").expect("V8SH_rubric selector");

    let mut params = Vec::new();
    for rubric in doc.select(&rubric_sel) {
        let raw: String = rubric.text().collect();
        let text = collapse_whitespace(raw.trim());
        if text.is_empty() {
            continue;
        }
        let (name, is_optional) = parse_parameter_header(&text);
        // Тип/описание — пока пустые, расширим в следующих итерациях Phase 2,
        // когда сделаем sibling-обход для DOM (детальный аналог Kotlin state-machine).
        params.push(MethodParameterInfo {
            name,
            type_name: String::new(),
            is_optional,
            description: String::new(),
        });
    }
    params
}

/// Разобрать заголовок параметра: `<имя> (необязательный)` → (имя, true).
fn parse_parameter_header(text: &str) -> (String, bool) {
    // Шаблон: возможны кавычки/скобки. PARAMETER_NAME_PATTERN = `<([^&]+)>\s*(?:\(([^)]+)\))?`
    let trimmed = text.trim();
    let stripped = trimmed.strip_prefix('<').and_then(|s| s.find('>').map(|i| &s[..i]));
    if let Some(inner) = stripped {
        let name = inner.trim().to_string();
        let after = trimmed
            .splitn(2, '>')
            .nth(1)
            .unwrap_or("")
            .trim();
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

#[cfg(test)]
mod tests {
    use super::split_dual_name;

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
    fn pseudo_value_in_angle_brackets_stays_whole() {
        // `<Имя картинки> (<Icon name>)` — не пара имён, а описание открытой
        // коллекции; оставляем целиком, чтобы признак `<` сохранился.
        let (ru, en) = split_dual_name("<Имя картинки> (<Icon name>)");
        assert!(ru.starts_with('<'));
        assert!(en.is_empty());
    }
}
