//! Markdown-форматтер для ответов MCP-tools.
//!
//! Порт `MarkdownFormatterService.kt` (вне репозитория). Отличия от старого
//! вывода: типы рендерятся code-span'ами (в т.ч. каждый компонент составного
//! типа), в блоке ```bsl``` печатается авторитетный синтаксис со страницы
//! (если он есть), пустые значения не дают пустых code-span'ов.

use std::fmt::Write;

use crate::entities::{Constructor, Definition, EnumValue, Method, Property, Signature, Type};

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
        let _ = writeln!(out, "### {}", d.name_ru());
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
    let _ = writeln!(out, "# {}\n", t.name_ru);

    if !t.description.is_empty() {
        let _ = writeln!(out, "{}\n", t.description);
    }

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
    let _ = writeln!(out, "### {}\n", m.name_ru);
    if !m.description.is_empty() {
        let _ = writeln!(out, "{}\n", m.description);
    }
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
    let _ = writeln!(out, "### {}\n", p.name_ru);
    if !p.description.is_empty() {
        let _ = writeln!(out, "{}\n", p.description);
    }
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
        let _ = writeln!(out, "## Конструктор: {} ({})", c.name, desc);
        out.push_str(&format_signature_block(
            &c.parameters,
            &c.syntax,
            &format!("Новый {type_name}"),
        ));
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
    if m.signatures.is_empty() {
        format!(
            "- {}(){} - {}\n",
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
                "- {}({}){} - {}",
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
    if type_name.is_empty() {
        format!("- {} - {}\n", p.name_ru, inline_line(&p.description))
    } else {
        format!(
            "- {}: {} - {}\n",
            p.name_ru,
            type_name,
            inline_line(&p.description)
        )
    }
}

fn format_constructor_summary(c: &Constructor) -> String {
    format!("- {} - {}\n", c.name, c.description)
}

fn format_enum_value_summary(v: &EnumValue) -> String {
    let en = if v.name_en.is_empty() {
        String::new()
    } else {
        format!(" (`{}`)", v.name_en)
    };
    if v.description.is_empty() {
        format!("- {}{}\n", v.name_ru, en)
    } else {
        format!("- {}{} - {}\n", v.name_ru, en, inline_line(&v.description))
    }
}
