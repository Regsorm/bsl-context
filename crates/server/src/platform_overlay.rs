//! Дополнения к платформенному контексту из файла (issue #40, дополнение автора).
//!
//! Часть устаревших методов и членов типов платформа исполняет, хотя страницы в
//! справке уже нет: `УстановитьЗаголовокПриложения`, `ПолучитьЗаголовокПриложения`
//! и ещё два десятка имён. Раньше единственным способом их учесть была ручная
//! правка дискового кэша (`platform-index.cache`) — она требовала знать
//! внутренний формат и молча пропадала при обновлении сервера (кэш пересобирается
//! по версии сборки). Теперь те же записи описываются файлом TOML и накладываются
//! на индекс ПОСЛЕ загрузки — из кэша или из hbk, — поэтому переживают и
//! пересборку кэша, и смену версии сервера.
//!
//! ```toml
//! [[global_methods]]
//! name = "УстановитьЗаголовокПриложения"
//! description = "Устаревший метод, оставлен для совместимости"
//!
//! [[global_methods.signatures]]
//! parameters = ["Заголовок"]   # имена параметров по порядку
//! required = 1                 # сколько первых обязательны (по умолчанию — все)
//!
//! [[type_members]]
//! type_name = "ДокументОбъект.<Имя документа>"   # тип как в справке
//! kind = "method"                                 # method | property
//! name = "ПолучитьФормуМодально"
//! ```
//!
//! Подключается секцией `[platform_overlay] file = "..."`. Ошибка в файле —
//! отказ старта: работать без заявленных дополнений молча нельзя (ровно на это
//! и жаловался автор issue).
//!
//! Переменное число аргументов выразить нельзя: у вариативных имён сигнатуру
//! лучше не описывать вовсе — тогда арность не проверяется (см. README).

use std::path::Path;

use anyhow::{Context, Result};
use platform_index::{Method, Parameter, PlatformIndex, Property, Signature, TypeMember};
use serde::Deserialize;

/// Дополнения, прочитанные из файла.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Overlay {
    /// Методы глобального контекста.
    pub global_methods: Vec<GlobalMethodEntry>,
    /// Члены типов: методы и свойства.
    pub type_members: Vec<TypeMemberEntry>,
}

/// Метод глобального контекста.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalMethodEntry {
    /// Русское имя — по нему метод и находится в коде.
    pub name: String,
    /// Английское имя, если платформа принимает и его.
    #[serde(default)]
    pub name_en: String,
    #[serde(default)]
    pub description: String,
    /// «Примечание:» страницы — у дополнения его обычно нет.
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub return_type: String,
    /// Перегрузки. Пусто — арность вызова не проверяется.
    #[serde(default)]
    pub signatures: Vec<SignatureEntry>,
}

/// Член типа.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeMemberEntry {
    /// Имя типа как в справке (`ДокументОбъект.<Имя документа>`, `Массив`, …).
    pub type_name: String,
    /// `method` или `property`.
    pub kind: MemberKind,
    pub name: String,
    #[serde(default)]
    pub name_en: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub note: Option<String>,
    /// Для свойства — имя типа значения.
    #[serde(default)]
    pub type_name_of_value: String,
    /// Для метода — тип возвращаемого значения.
    #[serde(default)]
    pub return_type: String,
    /// Для метода: перегрузки (пусто — арность не проверяется).
    #[serde(default)]
    pub signatures: Vec<SignatureEntry>,
}

/// Вид члена типа в файле дополнений.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemberKind {
    Method,
    Property,
}

/// Перегрузка метода.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureEntry {
    /// Имена параметров по порядку.
    #[serde(default)]
    pub parameters: Vec<String>,
    /// Сколько первых параметров обязательны. По умолчанию — все.
    #[serde(default)]
    pub required: Option<usize>,
    #[serde(default)]
    pub description: String,
}

/// Что получилось при наложении дополнений — для журнала и тестов.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayReport {
    pub methods_added: usize,
    pub methods_replaced: usize,
    pub members_added: usize,
    pub members_replaced: usize,
    /// Записи, которые применить не удалось (тип не найден, пустое имя).
    pub problems: Vec<String>,
}

impl OverlayReport {
    pub fn is_empty(&self) -> bool {
        self.methods_added == 0
            && self.methods_replaced == 0
            && self.members_added == 0
            && self.members_replaced == 0
    }

    /// Строка для журнала: что добавлено и что не применилось.
    pub fn summary(&self) -> String {
        let mut text = format!(
            "дополнения к справке: методов добавлено {}, заменено {}; членов типов \
             добавлено {}, заменено {}",
            self.methods_added, self.methods_replaced, self.members_added, self.members_replaced
        );
        if !self.problems.is_empty() {
            text.push_str(&format!("; не применилось: {}", self.problems.join("; ")));
        }
        text
    }
}

/// Прочитать файл дополнений. Ошибки чтения и разбора — с путём в контексте.
pub fn load(path: &Path) -> Result<Overlay> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("чтение файла дополнений {}", path.display()))?;
    let overlay: Overlay = toml::from_str(&text)
        .with_context(|| format!("разбор файла дополнений {}", path.display()))?;
    Ok(overlay)
}

/// Наложить дополнения на индекс. Индекс меняется на месте, отчёт возвращается
/// вызывающему: он пишет его в журнал, а тесты проверяют.
pub fn apply(index: &mut PlatformIndex, overlay: &Overlay) -> OverlayReport {
    let mut report = OverlayReport::default();

    for entry in &overlay.global_methods {
        if entry.name.trim().is_empty() {
            report
                .problems
                .push("метод глобального контекста с пустым именем".to_string());
            continue;
        }
        let replaced = index.add_global_method(Method {
            name_ru: entry.name.clone(),
            name_en: entry.name_en.clone(),
            description: entry.description.clone(),
            note: entry.note.clone(),
            return_type: entry.return_type.clone(),
            signatures: entry.signatures.iter().map(signature).collect(),
        });
        if replaced {
            report.methods_replaced += 1;
        } else {
            report.methods_added += 1;
        }
    }

    for entry in &overlay.type_members {
        if entry.name.trim().is_empty() {
            report
                .problems
                .push(format!("член типа '{}' с пустым именем", entry.type_name));
            continue;
        }
        let member = match entry.kind {
            MemberKind::Method => TypeMember::Method(Method {
                name_ru: entry.name.clone(),
                name_en: entry.name_en.clone(),
                description: entry.description.clone(),
                note: entry.note.clone(),
                return_type: entry.return_type.clone(),
                signatures: entry.signatures.iter().map(signature).collect(),
            }),
            MemberKind::Property => TypeMember::Property(Property {
                name_ru: entry.name.clone(),
                name_en: entry.name_en.clone(),
                description: entry.description.clone(),
                note: entry.note.clone(),
                type_name: entry.type_name_of_value.clone(),
                readonly: false,
            }),
        };
        match index.add_type_member(&entry.type_name, member) {
            Some(true) => report.members_replaced += 1,
            Some(false) => report.members_added += 1,
            None => report.problems.push(format!(
                "тип '{}' не найден в справке — член '{}' не добавлен",
                entry.type_name, entry.name
            )),
        }
    }

    report
}

/// Перегрузка из файла: параметры по порядку, первые `required` — обязательные.
fn signature(entry: &SignatureEntry) -> Signature {
    let required = entry.required.unwrap_or(entry.parameters.len());
    Signature {
        name: String::new(),
        syntax: String::new(),
        description: entry.description.clone(),
        parameters: entry
            .parameters
            .iter()
            .enumerate()
            .map(|(position, name)| Parameter {
                name: name.clone(),
                type_name: String::new(),
                required: position < required,
                description: String::new(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_index::Type;

    fn index_with_document_object() -> PlatformIndex {
        let mut index = PlatformIndex::new();
        index.insert_type(Type {
            name_ru: "ДокументОбъект.<Имя документа>".to_string(),
            name_en: "DocumentObject.<Document name>".to_string(),
            description: String::new(),
            note: None,
            methods: Vec::new(),
            properties: Vec::new(),
            constructors: Vec::new(),
            enum_values: Vec::new(),
        });
        index
    }

    fn overlay_from(text: &str) -> Overlay {
        toml::from_str(text).expect("файл дополнений должен разбираться")
    }

    #[test]
    fn global_method_is_added_with_signature() {
        let mut index = index_with_document_object();
        let overlay = overlay_from(
            r#"
            [[global_methods]]
            name = "УстановитьЗаголовокПриложения"
            description = "Устаревший метод"

            [[global_methods.signatures]]
            parameters = ["Заголовок"]
            "#,
        );
        let report = apply(&mut index, &overlay);
        assert_eq!(report.methods_added, 1, "{report:?}");
        assert!(report.problems.is_empty(), "{report:?}");

        let method = index
            .find_global_method("УстановитьЗаголовокПриложения")
            .expect("метод должен появиться в индексе");
        assert_eq!(method.signatures.len(), 1);
        assert_eq!(method.signatures[0].parameters.len(), 1);
        assert!(method.signatures[0].parameters[0].required);
    }

    #[test]
    fn signature_required_marks_only_the_first_ones() {
        let mut index = PlatformIndex::new();
        let overlay = overlay_from(
            r#"
            [[global_methods]]
            name = "Проба"
            [[global_methods.signatures]]
            parameters = ["А", "Б", "В"]
            required = 2
            "#,
        );
        apply(&mut index, &overlay);
        let method = index.find_global_method("Проба").expect("метод");
        let required: Vec<bool> = method.signatures[0]
            .parameters
            .iter()
            .map(|p| p.required)
            .collect();
        assert_eq!(required, [true, true, false]);
    }

    #[test]
    fn type_member_is_added_to_existing_type() {
        let mut index = index_with_document_object();
        let overlay = overlay_from(
            r#"
            [[type_members]]
            type_name = "ДокументОбъект.<Имя документа>"
            kind = "method"
            name = "ПолучитьФормуМодально"

            [[type_members]]
            type_name = "ДокументОбъект.<Имя документа>"
            kind = "property"
            name = "УстаревшееСвойство"
            "#,
        );
        let report = apply(&mut index, &overlay);
        assert_eq!(report.members_added, 2, "{report:?}");
        let ty = index
            .find_type("ДокументОбъект.<Имя документа>")
            .expect("тип");
        assert!(ty
            .methods
            .iter()
            .any(|m| m.name_ru == "ПолучитьФормуМодально"));
        assert!(ty
            .properties
            .iter()
            .any(|p| p.name_ru == "УстаревшееСвойство"));
    }

    #[test]
    fn unknown_type_is_a_problem_not_a_panic() {
        let mut index = index_with_document_object();
        let overlay = overlay_from(
            r#"
            [[type_members]]
            type_name = "ТипКоторогоНет.<Имя>"
            kind = "method"
            name = "Чтото"
            "#,
        );
        let report = apply(&mut index, &overlay);
        assert_eq!(report.members_added, 0);
        assert_eq!(report.problems.len(), 1, "{report:?}");
        assert!(report.problems[0].contains("не найден"), "{report:?}");
        // Тип-призрак дополнением не создаётся.
        assert!(index.find_type("ТипКоторогоНет.<Имя>").is_none());
    }

    #[test]
    fn existing_method_is_replaced_not_duplicated() {
        let mut index = PlatformIndex::new();
        index.add_global_method(Method {
            name_ru: "Проба".to_string(),
            name_en: String::new(),
            description: "из справки".to_string(),
            note: None,
            return_type: String::new(),
            signatures: Vec::new(),
        });
        let overlay = overlay_from(
            r#"
            [[global_methods]]
            name = "Проба"
            description = "из дополнений"
            "#,
        );
        let report = apply(&mut index, &overlay);
        assert_eq!(report.methods_replaced, 1, "{report:?}");
        assert_eq!(report.methods_added, 0, "{report:?}");
        assert_eq!(
            index
                .find_global_method("Проба")
                .expect("метод")
                .description,
            "из дополнений"
        );
    }

    #[test]
    fn unknown_field_is_a_parse_error() {
        let err = toml::from_str::<Overlay>(
            r#"
            [[global_methods]]
            name = "Проба"
            variadic = true
            "#,
        )
        .expect_err("лишнее поле должно быть ошибкой, а не молчанием");
        assert!(err.to_string().contains("variadic"), "{err}");
    }
}
