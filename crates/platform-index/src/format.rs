//! Markdown-форматтер для ответов MCP-tools.
//!
//! Порт `MarkdownFormatterService.kt` (вне репозитория). Отличия от старого
//! вывода: типы рендерятся code-span'ами (в т.ч. каждый компонент составного
//! типа), в блоке ```bsl``` печатается авторитетный синтаксис со страницы
//! (если он есть), пустые значения не дают пустых code-span'ов.

use std::fmt::Write;

use crate::entities::{
    Availability, Constructor, Definition, EnumValue, Method, Property, Signature, Type,
};

/// Тип (в т.ч. составной `A,B`) → markdown-code-span по каждому компоненту.
fn render_type(type_name: &str) -> String {
    type_name
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| format!("`{p}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Однострочить текст для list-item/заголовка: переводы строк ломают разметку.
fn inline_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

/// ` (`En`)` для заголовков и элементов списка, если английское имя есть.
fn english_name(name_en: &str) -> String {
    if name_en.is_empty() {
        String::new()
    } else {
        format!(" (`{name_en}`)")
    }
}

/// Абзац «**Примечание:** …» для подробного вывода; пустого примечания нет —
/// пустая строка.
fn note_block(note: Option<&str>) -> String {
    match note.map(str::trim).filter(|n| !n.is_empty()) {
        Some(note) => format!("**Примечание:** {}\n\n", inline_line(note)),
        None => String::new(),
    }
}

/// Абзац «**Доступность:** …» — контексты выполнения из справки.
///
/// Пустого абзаца нет: у конструкторов и значений перечислений главы
/// «Доступность:» не бывает, а у части страниц свойств она не заполнена.
fn availability_block(availability: Option<&Availability>) -> String {
    match availability.map(Availability::labels) {
        Some(labels) if !labels.is_empty() => format!("**Доступность:** {labels}\n\n"),
        _ => String::new(),
    }
}

pub fn format_query_header(query: &str) -> String {
    format!("# Результаты поиска: '{}'\n\n", inline_line(query))
}

pub fn format_search_results(results: &[Definition]) -> String {
    if results.is_empty() {
        return "❌ **Не найдено:** Ничего не найдено для запроса\n".to_string();
    }
    if results.len() == 1 {
        return format_member(&results[0]);
    }

    let mut out = String::new();
    let _ = writeln!(out, "## Найдено {} элементов\n", results.len());
    for d in results {
        let desc = if d.description().is_empty() {
            "Нет описания"
        } else {
            d.description()
        };
        let _ = writeln!(out, "### {}{}", d.name_ru(), english_name(d.name_en()));
        let _ = writeln!(out, "**Тип элемента:** {}", d.kind_label());
        let _ = writeln!(out, "**Описание:** {desc}\n");
    }
    out
}

pub fn format_member(def: &Definition) -> String {
    match def {
        Definition::Type(t) => format_type(t),
        Definition::Method(m) => format_method(m),
        Definition::Property(p) => format_property(p),
    }
}

pub fn format_type(t: &Type) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {}{}\n", t.name_ru, english_name(&t.name_en));

    if !t.description.is_empty() {
        let _ = writeln!(out, "{}\n", t.description);
    }
    out.push_str(&note_block(t.note.as_deref()));
    out.push_str(&availability_block(t.availability.as_ref()));

    if t.has_methods() {
        out.push_str("## Методы\n\n");
        for m in &t.methods {
            out.push_str(&format_method_summary(m));
        }
    }

    if t.has_properties() {
        out.push_str("\n## Свойства\n\n");
        for p in &t.properties {
            out.push_str(&format_property_summary(p));
        }
    }

    if t.has_constructors() {
        out.push_str("\n## Конструкторы\n\n");
        for c in &t.constructors {
            out.push_str(&format_constructor_summary(c));
        }
    }

    if t.is_enum() {
        out.push_str("\n## Значения\n\n");
        for v in &t.enum_values {
            out.push_str(&format_enum_value_summary(v));
        }
    }

    out
}

pub fn format_method(m: &Method) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "### {}{}\n", m.name_ru, english_name(&m.name_en));
    if !m.description.is_empty() {
        let _ = writeln!(out, "{}\n", m.description);
    }
    out.push_str(&note_block(m.note.as_deref()));
    out.push_str(&availability_block(m.availability.as_ref()));
    out.push_str(&format_signatures(&m.signatures, &m.name_ru));
    if !m.return_type.is_empty() {
        let _ = writeln!(
            out,
            "**Возвращаемый тип:** {}\n",
            render_type(&m.return_type)
        );
    }
    out
}

pub fn format_property(p: &Property) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "### {}{}\n", p.name_ru, english_name(&p.name_en));
    if !p.description.is_empty() {
        let _ = writeln!(out, "{}\n", p.description);
    }
    out.push_str(&note_block(p.note.as_deref()));
    out.push_str(&availability_block(p.availability.as_ref()));
    let _ = writeln!(out, "**Тип:** {}", render_type(&p.type_name));
    let _ = writeln!(
        out,
        "**Только для чтения:** {}\n",
        if p.readonly { "Да" } else { "Нет" }
    );
    out
}

pub fn format_constructors(constructors: &[Constructor], type_name: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Конструкторы объекта {type_name}");
    if constructors.is_empty() {
        out.push_str("❌ **Не найдено:** у типа нет конструкторов\n");
        return out;
    }
    for c in constructors {
        let desc = inline_line(&c.description);
        let _ = writeln!(
            out,
            "## Конструктор: {}{} ({})",
            c.name,
            english_name(&c.name_en),
            desc
        );
        out.push_str(&format_signature_block(
            &c.parameters,
            &c.syntax,
            &format!("Новый {type_name}"),
        ));
        out.push_str(&note_block(c.note.as_deref()));
    }
    out
}

pub fn format_enum_values(values: &[EnumValue], type_name: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Значения системного перечисления {type_name}\n");
    if values.is_empty() {
        out.push_str(
            "❌ **Не найдено:** у типа нет значений (он не является системным перечислением)\n",
        );
        return out;
    }
    for v in values {
        out.push_str(&format_enum_value_summary(v));
    }
    out
}

fn format_signatures(signatures: &[Signature], method_name: &str) -> String {
    let mut out = String::new();
    for s in signatures {
        let _ = writeln!(
            out,
            "## Сигнатура: {} ({})",
            s.name,
            inline_line(&s.description)
        );
        out.push_str(&format_signature_block(
            &s.parameters,
            &s.syntax,
            method_name,
        ));
    }
    out
}

fn format_signature_block(
    parameters: &[crate::entities::Parameter],
    syntax: &str,
    call_name: &str,
) -> String {
    let mut out = String::new();
    let inline = parameters
        .iter()
        .map(|p| {
            format!(
                "{}{}: {}",
                p.name,
                if p.required { "" } else { "?" },
                p.type_name
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let call = if syntax.is_empty() {
        format!("{call_name}({inline})")
    } else {
        // Авторитетный синтаксис со страницы — как есть; fence не должен
        // закрыться раньше времени, поэтому тройные бэктики обезвреживаем.
        syntax.replace("```", "'''")
    };
    out.push_str("```bsl\n");
    let _ = writeln!(out, "{call}");
    out.push_str("```\n\n");

    if !parameters.is_empty() {
        out.push_str("### Параметры\n");
        for p in parameters {
            let mut line = format!("- **{}**", p.name);
            if !p.type_name.is_empty() {
                line.push_str(&format!(" *({})*", p.type_name));
            }
            if p.required {
                line.push_str(" (обязательный)");
            }
            if !p.description.is_empty() {
                line.push_str(" - ");
                line.push_str(&inline_line(&p.description));
            }
            line.push('\n');
            out.push_str(&line);
        }
        out.push('\n');
    }
    out
}

fn format_method_summary(m: &Method) -> String {
    let return_type = match render_type(&m.return_type) {
        t if t.is_empty() => String::new(),
        t => format!(": {t}"),
    };
    let en = english_name(&m.name_en);
    if m.signatures.is_empty() {
        format!(
            "- {}{en}(){} - {}\n",
            m.name_ru,
            return_type,
            inline_line(&m.description)
        )
    } else {
        let mut out = String::new();
        for s in &m.signatures {
            let inline = s
                .parameters
                .iter()
                .map(|p| {
                    format!(
                        "{}{}: {}",
                        p.name,
                        if p.required { "" } else { "?" },
                        p.type_name
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                out,
                "- {}{en}({}){} - {}",
                m.name_ru,
                inline,
                return_type,
                inline_line(&s.description)
            );
        }
        out
    }
}

fn format_property_summary(p: &Property) -> String {
    let type_name = render_type(&p.type_name);
    let en = english_name(&p.name_en);
    if type_name.is_empty() {
        format!("- {}{en} - {}\n", p.name_ru, inline_line(&p.description))
    } else {
        format!(
            "- {}{en}: {} - {}\n",
            p.name_ru,
            type_name,
            inline_line(&p.description)
        )
    }
}

fn format_constructor_summary(c: &Constructor) -> String {
    format!(
        "- {}{} - {}\n",
        c.name,
        english_name(&c.name_en),
        c.description
    )
}

fn format_enum_value_summary(v: &EnumValue) -> String {
    let en = english_name(&v.name_en);
    let base = if v.description.is_empty() {
        format!("- {}{}", v.name_ru, en)
    } else {
        format!("- {}{} - {}", v.name_ru, en, inline_line(&v.description))
    };
    // «Примечание:» значения (на 8.3.27 таких страниц 52) — в той же строке
    // списка: отдельным абзацем оно разрывало бы перечень значений.
    match v.note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(note) => format!("{base} — **Примечание:** {}\n", inline_line(note)),
        None => format!("{base}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{AvailabilityMark, Parameter};

    fn availability(labels: &[&str]) -> Availability {
        Availability::new(
            labels
                .iter()
                .map(|label| AvailabilityMark::from_label(label))
                .collect(),
        )
    }

    fn parameter() -> Parameter {
        Parameter {
            name: "Значение".to_string(),
            type_name: "Произвольный".to_string(),
            required: false,
            description: String::new(),
        }
    }

    fn method(note: Option<&str>) -> Method {
        Method {
            name_ru: "Добавить".to_string(),
            name_en: "Add".to_string(),
            description: "Добавляет элемент".to_string(),
            note: note.map(str::to_string),
            return_type: String::new(),
            signatures: vec![Signature {
                name: "Основная".to_string(),
                syntax: String::new(),
                description: String::new(),
                parameters: vec![parameter()],
            }],
            availability: None,
        }
    }

    fn property(note: Option<&str>) -> Property {
        Property {
            name_ru: "Количество".to_string(),
            name_en: "Count".to_string(),
            description: String::new(),
            note: note.map(str::to_string),
            type_name: "Число".to_string(),
            readonly: true,
            availability: None,
        }
    }

    fn constructor(note: Option<&str>) -> Constructor {
        Constructor {
            name: "Массив".to_string(),
            name_en: "Array".to_string(),
            syntax: "Новый Массив()".to_string(),
            description: "Пустой массив".to_string(),
            note: note.map(str::to_string),
            parameters: vec![parameter()],
        }
    }

    fn enum_value(note: Option<&str>) -> EnumValue {
        EnumValue {
            name_ru: "Красный".to_string(),
            name_en: "Red".to_string(),
            description: "Цвет".to_string(),
            note: note.map(str::to_string),
        }
    }

    fn ty(note: Option<&str>) -> Type {
        Type {
            name_ru: "Массив".to_string(),
            name_en: "Array".to_string(),
            description: String::new(),
            note: note.map(str::to_string),
            methods: vec![method(note)],
            properties: vec![property(note)],
            constructors: vec![constructor(note)],
            enum_values: vec![enum_value(note)],
            availability: None,
        }
    }

    #[test]
    fn english_names_appear_in_headers_and_lists() {
        let text = format_type(&ty(None));
        assert!(text.contains("# Массив (`Array`)"), "{text}");
        assert!(
            text.contains("- Добавить (`Add`)(Значение?: Произвольный)"),
            "{text}"
        );
        assert!(text.contains("- Количество (`Count`): `Число`"), "{text}");
        assert!(
            text.contains("- Массив (`Array`) - Пустой массив"),
            "{text}"
        );
        assert!(text.contains("- Красный (`Red`) - Цвет"), "{text}");

        let method_text = format_member(&Definition::Method(method(None)));
        assert!(
            method_text.contains("### Добавить (`Add`)"),
            "{method_text}"
        );
        let property_text = format_member(&Definition::Property(property(None)));
        assert!(
            property_text.contains("### Количество (`Count`)"),
            "{property_text}"
        );
        let ctor_text = format_constructors(&[constructor(None)], "Массив");
        assert!(
            ctor_text.contains("## Конструктор: Массив (`Array`)"),
            "{ctor_text}"
        );
    }

    #[test]
    fn search_list_shows_english_names() {
        let results = vec![Definition::Method(method(None)), Definition::Type(ty(None))];
        let text = format_search_results(&results);
        assert!(text.contains("### Добавить (`Add`)"), "{text}");
        assert!(text.contains("### Массив (`Array`)"), "{text}");
    }

    #[test]
    fn notes_appear_for_every_entity_kind() {
        // Обзор типа показывает примечание самого типа и значения перечисления;
        // методы/свойства/конструкторы раскрывают своё в подробном выводе.
        let overview = format_type(&ty(Some("Нюанс работы")));
        assert_eq!(
            overview.matches("**Примечание:** Нюанс работы").count(),
            2,
            "{overview}"
        );

        let method_text = format_member(&Definition::Method(method(Some("Нюанс метода"))));
        assert!(
            method_text.contains("**Примечание:** Нюанс метода"),
            "{method_text}"
        );
        let property_text = format_member(&Definition::Property(property(Some("Нюанс свойства"))));
        assert!(
            property_text.contains("**Примечание:** Нюанс свойства"),
            "{property_text}"
        );
        let ctor_text = format_constructors(&[constructor(Some("Нюанс конструктора"))], "Массив");
        assert!(
            ctor_text.contains("**Примечание:** Нюанс конструктора"),
            "{ctor_text}"
        );
        let enum_text = format_enum_values(&[enum_value(Some("Нюанс значения"))], "Массив");
        assert!(
            enum_text.contains("**Примечание:** Нюанс значения"),
            "{enum_text}"
        );
    }

    #[test]
    fn empty_note_and_english_name_add_nothing() {
        let mut t = ty(None);
        t.name_en = String::new();
        let text = format_type(&t);
        assert!(!text.contains("**Примечание:**"), "{text}");
        assert!(text.contains("# Массив\n"), "{text}");
    }

    #[test]
    fn availability_line_appears_for_every_entity_kind() {
        let mut m = method(None);
        m.availability = Some(availability(&["Тонкий клиент", "веб-клиент"]));
        let method_text = format_member(&Definition::Method(m));
        assert!(
            method_text.contains("**Доступность:** тонкий клиент, веб-клиент"),
            "{method_text}"
        );

        let mut p = property(None);
        p.availability = Some(availability(&["Сервер"]));
        let property_text = format_member(&Definition::Property(p));
        assert!(
            property_text.contains("**Доступность:** сервер"),
            "{property_text}"
        );

        let mut t = ty(None);
        t.availability = Some(availability(&["Сервер", "внешнее соединение"]));
        let type_text = format_type(&t);
        assert!(
            type_text.contains("**Доступность:** сервер, внешнее соединение"),
            "{type_text}"
        );

        // Без данных о доступности строки нет вовсе — не пустой заголовок.
        assert!(!format_type(&ty(None)).contains("**Доступность:**"));
    }
}
