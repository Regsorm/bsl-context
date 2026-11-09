//! Маппинг `*Info` → доменные сущности.
//!
//! Переносятся имена, описания, `note`, типы, параметры и синтаксис;
//! `example`/`related_objects` и описание возвращаемого значения в домен
//! пока не попадают (см. `entities`).
//!
//! Отношения к апстриму (`upstream/.../persistent/storage/Mapper.kt`, вне
//! репозитория):
//! - `MethodInfo → Method` сохраняет полный `signatures` (апстрим теряет: `signature = emptyList()`).
//! - `ObjectInfo → Type` переносит реальные `constructors` (апстрим всегда `emptyList()`).
//! - `EnumInfo → Type` переносит `enum_values` (у апстрима этого пути нет — `EnumInfo` теряется).

use hbk_parser::{
    AvailabilityInfo, ConstructorInfo, EnumInfo, EnumValueInfo, MethodInfo, MethodParameterInfo,
    MethodSignatureInfo, ObjectInfo, PropertyInfo,
};

use crate::entities::{
    Availability, AvailabilityMark, Constructor, EnumValue, Method, Parameter, Property, Signature,
    Type,
};

pub fn method_from(info: &MethodInfo) -> Method {
    Method {
        name_ru: info.name_ru.clone(),
        name_en: info.name_en.clone(),
        description: info.description.clone(),
        note: info.note.clone(),
        return_type: info
            .return_value
            .as_ref()
            .map(|v| v.type_name.clone())
            .unwrap_or_default(),
        signatures: info.signatures.iter().map(signature_from).collect(),
        availability: info.availability.as_ref().map(availability_from),
    }
}

pub fn property_from(info: &PropertyInfo) -> Property {
    Property {
        name_ru: info.name_ru.clone(),
        name_en: info.name_en.clone(),
        description: info.description.clone(),
        note: info.note.clone(),
        type_name: info.type_name.clone(),
        readonly: info.readonly,
        availability: info.availability.as_ref().map(availability_from),
    }
}

pub fn signature_from(info: &MethodSignatureInfo) -> Signature {
    Signature {
        name: info.name.clone(),
        syntax: info.syntax.clone(),
        description: info.description.clone(),
        parameters: info.parameters.iter().map(parameter_from).collect(),
    }
}

pub fn parameter_from(info: &MethodParameterInfo) -> Parameter {
    Parameter {
        name: info.name.clone(),
        type_name: info.type_name.clone(),
        required: !info.is_optional,
        description: info.description.clone(),
    }
}

pub fn constructor_from(info: &ConstructorInfo) -> Constructor {
    Constructor {
        name: info.name.clone(),
        name_en: info.name_en.clone(),
        syntax: info.syntax.clone(),
        description: info.description.clone(),
        note: info.note.clone(),
        parameters: info.parameters.iter().map(parameter_from).collect(),
    }
}

pub fn enum_value_from(info: &EnumValueInfo) -> EnumValue {
    EnumValue {
        name_ru: info.name_ru.clone(),
        name_en: info.name_en.clone(),
        description: info.description.clone(),
        note: info.note.clone(),
    }
}

/// Обычный тип (объект): методы/свойства/конструкторы заполнены, `enum_values` пуст.
pub fn type_from_object(info: &ObjectInfo) -> Type {
    Type {
        name_ru: info.name_ru.clone(),
        name_en: info.name_en.clone(),
        description: info.description.clone(),
        note: info.note.clone(),
        methods: info.methods.iter().map(method_from).collect(),
        properties: info.properties.iter().map(property_from).collect(),
        constructors: info.constructors.iter().map(constructor_from).collect(),
        enum_values: Vec::new(),
        availability: info.availability.as_ref().map(availability_from),
    }
}

/// Тип-перечисление: `enum_values` заполнен, остальные коллекции пусты.
///
/// Главы «Доступность:» у страниц типов-перечислений не проверяли — данных о
/// доступности нет (см. §10.4 плана).
pub fn type_from_enum(info: &EnumInfo) -> Type {
    Type {
        name_ru: info.name_ru.clone(),
        name_en: info.name_en.clone(),
        description: info.description.clone(),
        note: info.note.clone(),
        methods: Vec::new(),
        properties: Vec::new(),
        constructors: Vec::new(),
        enum_values: info.values.iter().map(enum_value_from).collect(),
        availability: None,
    }
}

/// Глава «Доступность:» → домен.
///
/// Неполный список (`has_unknown`: в главе есть текст, который меткой не
/// является) даёт [`AvailabilityMark::Unknown`]: правило проверки по такому
/// набору обязано молчать, а не считать «доступно везде».
fn availability_from(info: &AvailabilityInfo) -> Availability {
    let mut marks: Vec<AvailabilityMark> = info
        .labels
        .iter()
        .map(|label| AvailabilityMark::from_label(label))
        .collect();
    if info.has_unknown {
        marks.push(AvailabilityMark::Unknown(
            info.note.clone().unwrap_or_default(),
        ));
    }
    Availability::new(marks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method_info(availability: Option<AvailabilityInfo>) -> MethodInfo {
        MethodInfo {
            name_ru: "Найти".to_string(),
            name_en: "Find".to_string(),
            description: String::new(),
            signatures: Vec::new(),
            return_value: None,
            example: None,
            note: None,
            related_objects: Vec::new(),
            availability,
        }
    }

    #[test]
    fn method_from_keeps_availability() {
        // Глава «Доступность:» обязана доехать до домена: правило проверяет
        // именно `Method::availability`.
        let info = method_info(Some(AvailabilityInfo {
            labels: vec!["Сервер".to_string(), "толстый клиент".to_string()],
            note: None,
            has_unknown: false,
        }));
        let method = method_from(&info);

        let availability = method
            .availability
            .expect("доступность обязана перенестись");
        assert!(availability.server_side());
        assert!(availability.ordinary_client_side());
        assert!(!availability.managed_client_side());
        assert!(!availability.has_unknown());
        assert_eq!(availability.labels(), "сервер, толстый клиент");
    }

    #[test]
    fn unknown_tail_is_not_lost_in_mapping() {
        // Неполный список превращается в `Unknown`: молчать обязано правило,
        // а не маппер.
        let info = method_info(Some(AvailabilityInfo {
            labels: vec!["Сервер".to_string()],
            note: Some("неведомая метка".to_string()),
            has_unknown: true,
        }));
        let availability = availability_from(
            info.availability
                .as_ref()
                .expect("доступность в тестовой модели"),
        );

        assert!(availability.has_unknown());
        assert!(availability.server_side());
        assert_eq!(availability.labels(), "сервер, неведомая метка");
    }

    #[test]
    fn page_without_chapter_has_no_availability() {
        assert!(method_from(&method_info(None)).availability.is_none());
    }
}
