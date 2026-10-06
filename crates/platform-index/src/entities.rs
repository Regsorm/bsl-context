//! Доменные сущности business-слоя.
//!
//! Это УЗКАЯ проекция `hbk_parser::*Info`: переносятся имена, описания, типы,
//! параметры и синтаксис. `note`/`example`/`related_objects` и описание
//! возвращаемого значения пока остаются в промежуточных Info и в домен не
//! попадают — при расширении домена обязателен подъём `cache::FORMAT_VERSION`.

use serde::{Deserialize, Serialize};

/// Метод платформы (глобальный или член типа).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Method {
    pub name_ru: String,
    pub name_en: String,
    pub description: String,
    pub return_type: String,
    /// Список перегрузок. У апстрима всегда `emptyList()` — это исправляется здесь.
    pub signatures: Vec<Signature>,
}

/// Перегрузка метода или конструктора.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signature {
    pub name: String,
    /// Авторитетный текст вызова из справки (`Найти(<Значение>, …)`).
    pub syntax: String,
    pub description: String,
    pub parameters: Vec<Parameter>,
}

/// Параметр метода/конструктора.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    pub type_name: String,
    pub required: bool,
    pub description: String,
}

/// Свойство (глобальное или член типа).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Property {
    pub name_ru: String,
    pub name_en: String,
    pub description: String,
    pub type_name: String,
    pub readonly: bool,
}

/// Конструктор объекта (`Новый ТипX(...)`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Constructor {
    pub name: String,
    /// Текст синтаксиса со страницы (`Новый Тип(<Параметр>)`), если он есть.
    pub syntax: String,
    pub description: String,
    pub parameters: Vec<Parameter>,
}

/// Значение системного перечисления.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnumValue {
    pub name_ru: String,
    pub name_en: String,
    pub description: String,
}

/// Тип платформы. Системное перечисление — это разновидность `Type` с непустым
/// `enum_values` и пустыми `methods/properties/constructors`. Обычный тип —
/// наоборот.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Type {
    pub name_ru: String,
    pub name_en: String,
    pub description: String,
    pub methods: Vec<Method>,
    pub properties: Vec<Property>,
    pub constructors: Vec<Constructor>,
    /// Непустой ТОЛЬКО для типов-перечислений.
    pub enum_values: Vec<EnumValue>,
}

impl Type {
    /// `true`, если у типа есть значения системного перечисления.
    pub fn is_enum(&self) -> bool {
        !self.enum_values.is_empty()
    }

    /// Открытая коллекция: значения добавляет конфигурация (`ЦветаСтиля`,
    /// `БиблиотекаКартинок`). Признак — псевдо-значение вида `<Имя картинки>`
    /// в списке значений справки. Проверка по фиксированному списку для
    /// такого типа неполна.
    pub fn is_open_enum(&self) -> bool {
        self.enum_values.iter().any(|v| v.name_ru.starts_with('<'))
    }

    pub fn has_methods(&self) -> bool {
        !self.methods.is_empty()
    }

    pub fn has_properties(&self) -> bool {
        !self.properties.is_empty()
    }

    pub fn has_constructors(&self) -> bool {
        !self.constructors.is_empty()
    }
}

/// Универсальная ссылка на сущность для результатов поиска.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Definition {
    Method(Method),
    Property(Property),
    Type(Type),
}

impl Definition {
    pub fn name_ru(&self) -> &str {
        match self {
            Definition::Method(m) => &m.name_ru,
            Definition::Property(p) => &p.name_ru,
            Definition::Type(t) => &t.name_ru,
        }
    }

    pub fn name_en(&self) -> &str {
        match self {
            Definition::Method(m) => &m.name_en,
            Definition::Property(p) => &p.name_en,
            Definition::Type(t) => &t.name_en,
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Definition::Method(m) => &m.description,
            Definition::Property(p) => &p.description,
            Definition::Type(t) => &t.description,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            Definition::Method(_) => "Method",
            Definition::Property(_) => "Property",
            Definition::Type(_) => "Type",
        }
    }
}
