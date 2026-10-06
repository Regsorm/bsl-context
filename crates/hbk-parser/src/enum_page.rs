//! Парсер страницы системного перечисления (тип-перечисление, например
//! `ТипРазмещенияТекстаТабличногоДокумента`).
//!
//! Сама страница содержит описание, пример, ссылки «См. также», «Примечание:»
//! и главу «Значения» со ссылками на дочерние страницы `/properties/...`.
//! Значения собирает PagesVisitor из TOC-детей (глава — дублирующая
//! навигация); здесь парсим только саму страницу-родитель.
//!
//! Порт `EnumPageParser.kt` (alkoleft).

use crate::blocks::{
    non_empty, parse_description, parse_example, parse_head_name, parse_note, parse_related_objects,
};
use crate::html::split_chapters;
use crate::models::EnumInfo;

pub fn parse_enum_page(html: &str) -> EnumInfo {
    let chapters = split_chapters(html);

    let head_html = chapters.first().map(|c| c.body_html.as_str()).unwrap_or("");
    let (name_ru, name_en) = parse_head_name(head_html);

    let mut description = String::new();
    let mut example: Option<String> = None;
    let mut note: Option<String> = None;
    let mut related = Vec::new();

    for ch in chapters.iter().skip(1) {
        match ch.title.as_str() {
            "Описание:" => description = parse_description(&ch.body_html),
            "Пример:" => example = non_empty(parse_example(&ch.body_html)),
            "См. также:" => related = parse_related_objects(&ch.body_html),
            "Примечание:" => note = non_empty(parse_note(&ch.body_html)),
            // Игнорируемые главы (как в апстриме):
            "Значения"
            | "Свойства:"
            | "Доступность:"
            | "Использование в версии:"
            | "Использование в интерфейсе:" => {}
            _ => {
                if !ch.title.is_empty() {
                    tracing::warn!(
                        chapter = ch.title,
                        "неизвестная глава страницы перечисления"
                    );
                }
            }
        }
    }

    EnumInfo {
        name_ru,
        name_en,
        description,
        example,
        note,
        related_objects: related,
        // values заполняется через PagesVisitor отдельным проходом по детям TOC.
        values: Vec::new(),
    }
}
