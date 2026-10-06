//! Промежуточные структуры результатов парсинга hbk-страниц.
//!
//! Порт `upstream/.../infrastructure/hbk/models/Model.kt`. Все структуры
//! двуязычные (русское + английское имя), описание/примеры/связанные объекты
//! опциональны.
//!
//! Эти Info-типы — **только промежуточные** результаты парсера. В business-слое
//! `platform-index` они мапятся в `Type / Method / Property / EnumValue /
//! Constructor`; какие поля доходят до индекса, смотри в `mapper`.

use serde::Serialize;

/// Глобальный/локальный метод платформы.
#[derive(Debug, Clone, Serialize)]
pub struct MethodInfo {
    pub name_ru: String,
    pub name_en: String,
    pub description: String,
    /// Список перегрузок. У большинства методов одна перегрузка с именем "Основная".
    pub signatures: Vec<MethodSignatureInfo>,
    pub return_value: Option<ValueInfo>,
    pub example: Option<String>,
    pub note: Option<String>,
    pub related_objects: Vec<RelatedObject>,
}

/// Одна перегрузка метода (синтаксис + параметры + описание).
#[derive(Debug, Clone, Serialize)]
pub struct MethodSignatureInfo {
    pub name: String,
    pub syntax: String,
    pub parameters: Vec<MethodParameterInfo>,
    pub description: String,
}

/// Параметр метода/конструктора.
#[derive(Debug, Clone, Serialize)]
pub struct MethodParameterInfo {
    pub name: String,
    pub type_name: String,
    pub is_optional: bool,
    pub description: String,
}

/// Глобальное/типовое свойство (поле объекта или элемент глобального контекста).
#[derive(Debug, Clone, Serialize)]
pub struct PropertyInfo {
    pub name_ru: String,
    pub name_en: String,
    pub description: String,
    pub type_name: String,
    pub readonly: bool,
    pub note: Option<String>,
    pub related_objects: Vec<RelatedObject>,
}

/// Тип платформы (объект, не системное перечисление).
///
/// Поля `properties / methods / constructors` опциональны: апстрим заполняет
/// их через PagesVisitor отдельным проходом по дочерним страницам TOC. Если
/// у типа нет соответствующей подсекции — поле пустое (не None — Vec пуст).
#[derive(Debug, Clone, Serialize)]
pub struct ObjectInfo {
    pub name_ru: String,
    pub name_en: String,
    pub description: String,
    pub example: Option<String>,
    pub note: Option<String>,
    pub related_objects: Vec<RelatedObject>,
    pub properties: Vec<PropertyInfo>,
    pub methods: Vec<MethodInfo>,
    pub constructors: Vec<ConstructorInfo>,
}

/// Конструктор объекта (`Новый ТипX(...)`).
#[derive(Debug, Clone, Serialize)]
pub struct ConstructorInfo {
    pub name: String,
    /// Английская форма имени конструктора (пустая, если её нет на странице).
    pub name_en: String,
    pub syntax: String,
    pub parameters: Vec<MethodParameterInfo>,
    pub description: String,
    pub example: Option<String>,
    pub note: Option<String>,
    pub related_objects: Vec<RelatedObject>,
}

/// Системное перечисление платформы (тип-перечисление со значениями).
///
/// Хранится отдельно от `ObjectInfo` ровно потому, что у него нет
/// `methods/properties/constructors` (платформенные перечисления — это
/// лёгкие enum'ы), но есть `values`. В platform-index оно мапится в `Type` с
/// заполненным `enum_values` и пустыми остальными полями.
#[derive(Debug, Clone, Serialize)]
pub struct EnumInfo {
    pub name_ru: String,
    pub name_en: String,
    pub description: String,
    pub example: Option<String>,
    /// «Примечание:» главы-родителя (в текущих данных платформы встречается
    /// редко, но модель обязана вмещать его).
    pub note: Option<String>,
    pub related_objects: Vec<RelatedObject>,
    /// Заполняется PagesVisitor-аналогом из дочерних страниц TOC в `/properties/`.
    /// Сам `EnumPageParser` его не заполняет.
    pub values: Vec<EnumValueInfo>,
}

/// Одно значение системного перечисления (`ТипРазмещенияТекстаТабличногоДокумента.Авто`).
#[derive(Debug, Clone, Serialize)]
pub struct EnumValueInfo {
    pub name_ru: String,
    pub name_en: String,
    pub description: String,
    /// «Примечание:» страницы значения (52 страницы в 8.3.27 — раньше терялось).
    pub note: Option<String>,
    pub related_objects: Vec<RelatedObject>,
}

/// Ссылка «См. также» на другую страницу синтакс-помощника.
#[derive(Debug, Clone, Serialize)]
pub struct RelatedObject {
    pub name: String,
    pub href: String,
}

/// Тип + описание (для возвращаемого значения метода или для описания свойства).
#[derive(Debug, Clone, Serialize)]
pub struct ValueInfo {
    pub type_name: String,
    pub description: String,
}
