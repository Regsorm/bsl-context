//! Разбор главы «Доступность:» страницы синтакс-помощника.
//!
//! Глава выглядит так (8.3.27.1786, `shcntx_ru.hbk`):
//!
//! ```html
//! <p>Тонкий клиент, веб-клиент, мобильный клиент, толстый клиент, мобильное приложение (клиент).</p>
//! ```
//!
//! Берётся только ПЕРВЫЙ `<p>` главы: и после него, и внутри него (после
//! `<br>`) бывает текст, который списком меток не является:
//!
//! ```html
//! <p>Тонкий клиент, …, мобильный автономный сервер.</p><br /><b>Внимание: </b>…
//! <p>Сервер, …, мобильный автономный сервер. <BR>Возможен обмен с сервером. …</p>
//! ```
//!
//! Метки сверяются со словарём без учёта регистра (справка пишет первую метку
//! списка с заглавной, остальные — строчными) и без учёта `ё`/`е`. Первый
//! нераспознанный токен обрывает список и уходит в `note`, а `has_unknown`
//! ставится в `true` — даже если предыдущие метки распознаны: список неполон, и
//! потребитель обязан молчать, а не делать вывод из него. О неизвестной метке
//! пишется `warn`: дрейф справки не должен быть невидимым.

use scraper::{Html, Node};

use crate::blocks::non_empty;
use crate::html::{extract_text, serialize_node, to_markdown};
use crate::models::AvailabilityInfo;

/// Словарь меток главы «Доступность:» в нижнем регистре — полный набор
/// 8.3.27.1786 по 181 странице с этой главой (см. §3 плана).
///
/// «Толстый клиент» — отдельный контекст, а не «любой клиент»: он стоит в одном
/// списке рядом с тонким и веб-клиентом, а единственной клиентской меткой
/// бывает только у методов обычного приложения. Толкование — дело потребителя,
/// парсер лишь передаёт метки как есть.
const KNOWN_LABELS: [&str; 10] = [
    "сервер",
    "внешнее соединение",
    "мобильное приложение (сервер)",
    "мобильный автономный сервер",
    "тонкий клиент",
    "веб-клиент",
    "мобильный клиент",
    "мобильное приложение (клиент)",
    "толстый клиент",
    "интеграция",
];

/// Разобрать тело главы «Доступность:». `None` — данных о доступности в главе
/// нет (пустая глава): это не то же самое, что «доступно везде».
pub fn parse_availability(body_html: &str) -> Option<AvailabilityInfo> {
    let (paragraph_html, rest_html) = split_first_paragraph(body_html);
    let (list_html, after_break) = split_at_first_br(&paragraph_html);

    // Список кончается завершающей точкой; всё, что после неё, — хвостовой
    // текст, а не метка.
    let text = extract_text(list_html);
    let (list_text, after_period) = match text.find('.') {
        Some(index) => (&text[..index], text[index + 1..].trim()),
        None => (text.as_str(), ""),
    };

    let tokens: Vec<&str> = list_text
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect();

    let mut labels: Vec<String> = Vec::new();
    let mut unknown_from: Option<usize> = None;
    for (position, token) in tokens.iter().enumerate() {
        if is_known_label(token) {
            labels.push((*token).to_string());
        } else {
            unknown_from = Some(position);
            break;
        }
    }

    let mut note_parts: Vec<String> = Vec::new();
    if let Some(position) = unknown_from {
        note_parts.push(tokens[position..].join(", "));
    }
    if !after_period.is_empty() {
        note_parts.push(after_period.to_string());
    }
    note_parts.push(to_markdown(after_break));
    note_parts.push(to_markdown(&rest_html));

    let note = non_empty(
        note_parts
            .iter()
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
    );

    let has_unknown = unknown_from.is_some();
    if has_unknown {
        tracing::warn!(
            labels = labels.len(),
            text = %note.as_deref().unwrap_or(""),
            "глава «Доступность:» содержит метку, которой нет в словаре"
        );
    }

    if labels.is_empty() && note.is_none() {
        return None;
    }

    Some(AvailabilityInfo {
        labels,
        note,
        has_unknown,
    })
}

fn is_known_label(token: &str) -> bool {
    let normalized = normalize_label(token);
    KNOWN_LABELS.contains(&normalized.as_str())
}

/// Метка к виду словаря: без регистра и без различия `ё`/`е`.
fn normalize_label(label: &str) -> String {
    label.trim().to_lowercase().replace('ё', "е")
}

/// Тело главы → (html первого `<p>`, html всего остального).
///
/// Главы без `<p>` в 8.3.27 нет, но список важнее формы разметки: тогда
/// разбирается весь текст главы.
fn split_first_paragraph(body_html: &str) -> (String, String) {
    let doc = Html::parse_fragment(body_html);
    let mut paragraph: Option<String> = None;
    let mut rest = String::new();

    for child in doc.tree.root().children() {
        let is_paragraph =
            matches!(child.value(), Node::Element(el) if el.name().eq_ignore_ascii_case("p"));
        if paragraph.is_none() && is_paragraph {
            paragraph = scraper::ElementRef::wrap(child).map(|el| el.inner_html());
            if paragraph.is_some() {
                continue;
            }
        }
        if let Some(html) = serialize_node(child) {
            rest.push_str(&html);
        }
    }

    match paragraph {
        Some(html) => (html, rest),
        None => (body_html.to_string(), String::new()),
    }
}

/// Отделить список от хвоста по первому `<br>` в любом написании.
fn split_at_first_br(html: &str) -> (&str, &str) {
    // `to_ascii_lowercase` не меняет длину строки в байтах — индексы ниже
    // годятся и для исходного html.
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(relative) = lower[from..].find("<br") {
        let start = from + relative;
        // `<b>` — другой тег: после имени обязан идти разделитель.
        let is_break = html[start + 3..]
            .chars()
            .next()
            .is_some_and(|c| matches!(c, ' ' | '/' | '>'));
        if is_break {
            let end = match lower[start..].find('>') {
                Some(relative_end) => start + relative_end + 1,
                None => html.len(),
            };
            return (&html[..start], &html[end..]);
        }
        from = start + 3;
    }
    (html, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels_of(info: &AvailabilityInfo) -> Vec<&str> {
        info.labels.iter().map(String::as_str).collect()
    }

    #[test]
    fn plain_list_is_split_into_labels() {
        let info = parse_availability("<p>Тонкий клиент, веб-клиент, мобильный клиент.</p>")
            .expect("глава не пуста");
        assert_eq!(
            labels_of(&info),
            ["Тонкий клиент", "веб-клиент", "мобильный клиент"]
        );
        assert!(info.note.is_none(), "{:?}", info.note);
        assert!(!info.has_unknown);
    }

    #[test]
    fn single_label_with_trailing_period_is_recognized() {
        let info = parse_availability("<p>Толстый клиент.</p>").expect("глава не пуста");
        assert_eq!(labels_of(&info), ["Толстый клиент"]);
        assert!(info.note.is_none());
        assert!(!info.has_unknown);
    }

    #[test]
    fn tail_after_paragraph_goes_to_note() {
        // Глобальный `Найти`: `<br /><b>Внимание: </b>…` стоит ПОСЛЕ `</p>`.
        let html = "<p>Тонкий клиент, веб-клиент, мобильный автономный сервер.</p>\
                    <br /><b>Внимание: </b>Данный метод не поддерживается при работе через COM-соединение";
        let info = parse_availability(html).expect("глава не пуста");
        assert_eq!(
            labels_of(&info),
            ["Тонкий клиент", "веб-клиент", "мобильный автономный сервер"]
        );
        let note = info.note.expect("хвост обязан попасть в note");
        assert!(note.contains("Внимание"), "{note}");
        assert!(note.contains("COM-соединение"), "{note}");
        assert!(!info.has_unknown);
    }

    #[test]
    fn tail_after_br_inside_paragraph_goes_to_note() {
        // Тип `ТаблицаЗначений`: хвост начинается после `<BR>` внутри `<p>`.
        let html = "<p>Сервер, толстый клиент, мобильный автономный сервер. \
                    <BR>Возможен обмен с сервером. Сериализуется.</p>";
        let info = parse_availability(html).expect("глава не пуста");
        assert_eq!(
            labels_of(&info),
            ["Сервер", "толстый клиент", "мобильный автономный сервер"]
        );
        let note = info.note.expect("хвост обязан попасть в note");
        assert!(note.contains("Возможен обмен с сервером"), "{note}");
        assert!(note.contains("Сериализуется."), "{note}");
        assert!(!info.has_unknown);
    }

    #[test]
    fn label_case_and_yo_do_not_matter() {
        let info = parse_availability("<p>СЕРВЕР, ВЕБ-КЛИЕНТ, мобильное приложение (клиент).</p>")
            .expect("глава не пуста");
        assert_eq!(
            labels_of(&info),
            ["СЕРВЕР", "ВЕБ-КЛИЕНТ", "мобильное приложение (клиент)"]
        );
        assert!(!info.has_unknown);
    }

    #[test]
    fn empty_chapter_is_no_data_not_empty_list() {
        assert!(parse_availability("").is_none());
        assert!(parse_availability("<p></p>").is_none());
        assert!(parse_availability("<p>.</p>").is_none());
    }

    #[test]
    fn unknown_label_marks_the_list_incomplete() {
        // Дрейф справки: метки нет в словаре — список обрывается, остаток в
        // note, has_unknown ставится (потребитель обязан молчать).
        let info =
            parse_availability("<p>Сервер, неведомая метка, толстый клиент.</p>").expect("глава");
        assert_eq!(labels_of(&info), ["Сервер"]);
        assert!(info.has_unknown);
        let note = info.note.expect("нераспознанный остаток — в note");
        assert!(note.contains("неведомая метка"), "{note}");
    }

    #[test]
    fn first_token_unknown_leaves_no_labels() {
        let info = parse_availability("<p>Клиент на телефоне.</p>").expect("глава");
        assert!(info.labels.is_empty());
        assert!(info.has_unknown);
        assert!(info.note.expect("note").contains("Клиент на телефоне"));
    }

    #[test]
    fn break_like_other_tag_is_not_a_break() {
        // `<b>` не должен считаться `<br>`: иначе список обрезался бы по
        // первому же жирному фрагменту.
        let info = parse_availability("<p>Сервер, <b>толстый клиент</b>.</p>").expect("глава");
        assert_eq!(labels_of(&info), ["Сервер", "толстый клиент"]);
        assert!(!info.has_unknown);
    }
}
