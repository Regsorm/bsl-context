//! Парсер страницы конструктора типа.
//!
//! Главы: «Синтаксис:», «Параметры:», «Описание:», «Пример:», «См. также:»,
//! «Примечание:».
//!
//! Порт `ConstructorPageParser.kt`. Имя конструктора берётся из «головы»
//! страницы (NameBlockHandler); английская форма титула (`p.V8SH_title`
//! «Тип (TypeEn)») сохраняется в `name_en`.

use crate::blocks::{
    non_empty, parse_description, parse_example, parse_head_name, parse_note, parse_parameters,
    parse_related_objects, parse_syntax,
};
use crate::html::split_chapters;
use crate::models::ConstructorInfo;

pub fn parse_constructor_page(html: &str) -> ConstructorInfo {
    let chapters = split_chapters(html);

    let head_html = chapters.first().map(|c| c.body_html.as_str()).unwrap_or("");
    let (name_ru, name_en) = parse_head_name(head_html);

    let mut syntax = String::new();
    let mut parameters = Vec::new();
    let mut description = String::new();
    let mut example: Option<String> = None;
    let mut note: Option<String> = None;
    let mut related = Vec::new();

    for ch in chapters.iter().skip(1) {
        match ch.title.as_str() {
            "Синтаксис:" => syntax = parse_syntax(&ch.body_html),
            "Параметры:" => parameters = parse_parameters(&ch.body_html),
            "Описание:" => description = parse_description(&ch.body_html),
            "Пример:" => example = non_empty(parse_example(&ch.body_html)),
            "См. также:" => related = parse_related_objects(&ch.body_html),
            "Примечание:" => note = non_empty(parse_note(&ch.body_html)),
            "Доступность:" | "Использование в версии:" | "Использование в интерфейсе:" =>
            {
                // Известные главы, ничего не дающие правилам.
            }
            _ => {
                if !ch.title.is_empty() {
                    tracing::warn!(
                        chapter = ch.title,
                        "неизвестная глава страницы конструктора"
                    );
                }
            }
        }
    }

    ConstructorInfo {
        name: name_ru,
        name_en,
        syntax,
        parameters,
        description,
        example,
        note,
        related_objects: related,
    }
}
