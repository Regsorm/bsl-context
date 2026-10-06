//! Маппинг `*Info` → доменные сущности.
//!
//! Проекция узкая: переносятся имена, описания, типы, параметры и синтаксис;
//! `note`/`example`/`related_objects` и описание возвращаемого значения в домен
//! пока не попадают (см. `entities`).
//!
//! Отношения к апстриму (`upstream/.../persistent/storage/Mapper.kt`, вне
//! репозитория):
//! - `MethodInfo → Method` сохраняет полный `signatures` (апстрим теряет: `signature = emptyList()`).
//! - `ObjectInfo → Type` переносит реальные `constructors` (апстрим всегда `emptyList()`).
//! - `EnumInfo → Type` переносит `enum_values` (у апстрима этого пути нет — `EnumInfo` теряется).

use hbk_parser::{
    ConstructorInfo, EnumInfo, EnumValueInfo, MethodInfo, MethodParameterInfo, MethodSignatureInfo,
    ObjectInfo, PropertyInfo,
};

use crate::entities::{Constructor, EnumValue, Method, Parameter, Property, Signature, Type};

pub fn method_from(info: &MethodInfo) -> Method {
    Method {
        name_ru: info.name_ru.clone(),
        name_en: info.name_en.clone(),
        description: info.description.clone(),
        return_type: info
            .return_value
            .as_ref()
            .map(|v| v.type_name.clone())
            .unwrap_or_default(),
        signatures: info.signatures.iter().map(signature_from).collect(),
    }
}

pub fn property_from(info: &PropertyInfo) -> Property {
    Property {
        name_ru: info.name_ru.clone(),
        name_en: info.name_en.clone(),
        description: info.description.clone(),
        type_name: info.type_name.clone(),
        readonly: info.readonly,
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
        syntax: info.syntax.clone(),
        description: info.description.clone(),
        parameters: info.parameters.iter().map(parameter_from).collect(),
    }
}

pub fn enum_value_from(info: &EnumValueInfo) -> EnumValue {
    EnumValue {
        name_ru: info.name_ru.clone(),
        name_en: info.name_en.clone(),
        description: info.description.clone(),
    }
}

/// Обычный тип (объект): методы/свойства/конструкторы заполнены, `enum_values` пуст.
pub fn type_from_object(info: &ObjectInfo) -> Type {
    Type {
        name_ru: info.name_ru.clone(),
        name_en: info.name_en.clone(),
        description: info.description.clone(),
        methods: info.methods.iter().map(method_from).collect(),
        properties: info.properties.iter().map(property_from).collect(),
        constructors: info.constructors.iter().map(constructor_from).collect(),
        enum_values: Vec::new(),
    }
}

/// Тип-перечисление: `enum_values` заполнен, остальные коллекции пусты.
pub fn type_from_enum(info: &EnumInfo) -> Type {
    Type {
        name_ru: info.name_ru.clone(),
        name_en: info.name_en.clone(),
        description: info.description.clone(),
        methods: Vec::new(),
        properties: Vec::new(),
        constructors: Vec::new(),
        enum_values: info.values.iter().map(enum_value_from).collect(),
    }
}
