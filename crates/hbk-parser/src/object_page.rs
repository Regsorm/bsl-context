//! Парсер страницы типа платформы (объекта). Не собирает properties/methods/
//! constructors — эти дочерние коллекции собираются на уровне visitor (Phase 3).
//!
//! Главы: «Описание:», «Пример:», «См. также:», «Примечание:». Игнорируем
//! «Свойства:», «Методы:», «Конструкторы:» — их данные лежат в дочерних
//! страницах TOC, которые визитор обходит отдельно. «События:» визитором НЕ
//! собираются, а «Элементы коллекции:»/«Параметры формы:» в модели `ObjectInfo`
//! не представлены — эти главы сознательно пропускаются.
//!
//! Порт `ObjectPageParser.kt`.

use crate::blocks::{
    non_empty, parse_description, parse_example, parse_head_name, parse_note, parse_related_objects,
};
use crate::html::split_chapters;
use crate::models::ObjectInfo;

pub fn parse_object_page(html: &str) -> ObjectInfo {
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
            // Игнорируемые главы (как в апстриме). «Элементы коллекции:» и
            // «Параметры формы:» в модели `ObjectInfo` не представлены — в
            // апстриме они пропускаются молча, и предупреждение на каждую
            // страницу коллекции/формы (а их сотни) только засоряло журнал.
            "Свойства:"
            | "Методы:"
            | "События:"
            | "Конструкторы:"
            | "Доступность:"
            | "Использование в версии:"
            | "Использование в интерфейсе:"
            | "Элементы коллекции:"
            | "Параметры формы:" => {}
            _ => {
                if !ch.title.is_empty() {
                    tracing::warn!(chapter = ch.title, "неизвестная глава страницы типа");
                }
            }
        }
    }

    ObjectInfo {
        name_ru,
        name_en,
        description,
        example,
        note,
        related_objects: related,
        properties: Vec::new(),
        methods: Vec::new(),
        constructors: Vec::new(),
    }
}
