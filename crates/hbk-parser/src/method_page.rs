//! Парсер страницы метода. Поддерживает множественные перегрузки через
//! «Вариант синтаксиса:» в названии главы.
//!
//! Порт `MethodPageParser.kt`. Логика перегрузок: каждое появление главы,
//! заголовок которой начинается с `"Вариант синтаксиса:"`, начинает новую
//! `MethodSignatureInfo`. Если ни одной перегрузки не объявлено — единственная
//! сигнатура называется «Основная». Глава `"Синтаксис:"` идёт ДО любой
//! перегрузки и означает синтаксис основной (единственной) сигнатуры.
//!
//! Главы перегрузки внутри одной сигнатуры: «Параметры:», «Описание варианта
//! метода:». «Возвращаемое значение:» — ГЛОБАЛЬНАЯ глава метода: она одна на
//! все перегрузки (на реальных страницах их больше одной не встречается).
//!
//! Глобальные главы (применяются ко всему методу): «Описание:», «Пример:»,
//! «См. также:», «Примечание:».

use crate::blocks::{
    non_empty, parse_description, parse_example, parse_head_name, parse_note, parse_parameters,
    parse_related_objects, parse_syntax, parse_value_info,
};
use crate::html::split_chapters;
use crate::models::{MethodInfo, MethodSignatureInfo};

pub fn parse_method_page(html: &str) -> MethodInfo {
    let chapters = split_chapters(html);

    let head_html = chapters.first().map(|c| c.body_html.as_str()).unwrap_or("");
    let (name_ru, name_en) = parse_head_name(head_html);

    let mut signatures: Vec<MethodSignatureInfo> = Vec::new();
    let mut description = String::new();
    let mut example: Option<String> = None;
    let mut note: Option<String> = None;
    let mut related = Vec::new();
    let mut return_value = None;

    for ch in chapters.iter().skip(1) {
        let title = ch.title.as_str();

        if let Some(rest) = title.strip_prefix("Вариант синтаксиса:") {
            // Открываем новую сигнатуру с именем = текст после маркера.
            signatures.push(MethodSignatureInfo {
                name: rest.trim().to_string(),
                syntax: String::new(),
                parameters: Vec::new(),
                description: String::new(),
            });
            continue;
        }

        match title {
            "Синтаксис:" => {
                ensure_signature(&mut signatures);
                signatures.last_mut().unwrap().syntax = parse_syntax(&ch.body_html);
            }
            "Параметры:" => {
                ensure_signature(&mut signatures);
                signatures.last_mut().unwrap().parameters = parse_parameters(&ch.body_html);
            }
            "Возвращаемое значение:" => {
                return_value = parse_value_info(&ch.body_html);
            }
            "Описание варианта метода:" => {
                ensure_signature(&mut signatures);
                signatures.last_mut().unwrap().description = parse_description(&ch.body_html);
            }
            "Описание:" => description = parse_description(&ch.body_html),
            "Пример:" => example = non_empty(parse_example(&ch.body_html)),
            "См. также:" => related = parse_related_objects(&ch.body_html),
            "Примечание:" => note = non_empty(parse_note(&ch.body_html)),
            "Доступность:" | "Использование в версии:" | "Использование в интерфейсе:" =>
            {
                // Известные главы, ничего не дающие правилам.
            }
            _ => {
                // Эталон на неизвестный блок бросает UnknownPageBlockType:
                // не теряем сигнал о дрейфе схемы страниц.
                if !title.is_empty() {
                    tracing::warn!(chapter = title, "неизвестная глава страницы метода");
                }
            }
        }
    }

    MethodInfo {
        name_ru,
        name_en,
        description,
        signatures,
        return_value,
        example,
        note,
        related_objects: related,
    }
}

fn ensure_signature(signatures: &mut Vec<MethodSignatureInfo>) {
    if signatures.is_empty() {
        signatures.push(MethodSignatureInfo {
            name: "Основная".to_string(),
            syntax: String::new(),
            parameters: Vec::new(),
            description: String::new(),
        });
    }
}
