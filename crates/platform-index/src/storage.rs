//! `PlatformIndex` — central storage с тремя коллекциями.
//!
//! Иерархия (правильная для 1С): системное перечисление это разновидность типа,
//! а не отдельная категория. Поэтому `types` — единый словарь, в котором
//! и обычные типы, и перечисления (последние с непустым `enum_values`).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::entities::{Method, Property, Type};

/// Storage платформенного контекста (read-only после загрузки).
#[derive(Debug, Default, Clone)]
pub struct PlatformIndex {
    pub global_methods: Vec<Method>,
    pub global_properties: Vec<Property>,
    /// Ключ — `name_ru` в нижнем регистре. Тип-перечисление и обычный тип лежат вместе.
    pub types: HashMap<String, Type>,
    /// Ленивый кэш имён методов всех типов (см. `all_type_method_names`).
    /// Обход 2414 типов стоит десятки миллисекунд — на каждый вызов
    /// `validate_module` это заметно, а индекс после загрузки неизменен.
    type_method_names: OnceLock<HashSet<String>>,
}

impl PlatformIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Сколько типов, у которых заполнен `enum_values` (системные перечисления).
    pub fn enum_types_count(&self) -> usize {
        self.types.values().filter(|t| t.is_enum()).count()
    }

    /// Точный поиск типа по русскому имени (регистронезависимо).
    pub fn find_type(&self, name_ru: &str) -> Option<&Type> {
        self.types.get(&name_ru.to_lowercase())
    }

    /// Точный поиск глобального метода по имени (регистронезависимо).
    ///
    /// Сверяются ОБА имени — русское и английское: платформа принимает и
    /// `Сообщить(...)`, и `Message(...)`. Раньше искали только по `name_ru`,
    /// из-за чего английский вызов не находился и уходил в fuzzy, где
    /// находил сам себя в `name_en` с нулевым расстоянием.
    pub fn find_global_method(&self, name: &str) -> Option<&Method> {
        let key = name.to_lowercase();
        self.global_methods.iter().find(|m| {
            m.name_ru.to_lowercase() == key
                || (!m.name_en.is_empty() && m.name_en.to_lowercase() == key)
        })
    }

    /// Точный поиск глобального свойства по русскому имени (регистронезависимо).
    pub fn find_global_property(&self, name_ru: &str) -> Option<&Property> {
        let key = name_ru.to_lowercase();
        self.global_properties
            .iter()
            .find(|p| p.name_ru.to_lowercase() == key)
    }

    /// Имена (lowercase, русские и английские) ВСЕХ методов ВСЕХ типов платформы.
    ///
    /// Нужны строгой проверке модуля: внутри собственного модуля объекта или
    /// формы её методы зовутся без префикса — `Закрыть()`, `ЭтоНовый()`,
    /// `РеквизитФормыВЗначение(...)`. Это не глобальные методы, поэтому
    /// `find_global_method` их не видит, но опиской они не являются. Какой
    /// именно тип соответствует модулю, известно только из метаданных
    /// конфигурации, которых у платформенного индекса нет, — поэтому берём
    /// объединение по всем типам.
    /// Считается один раз при первом обращении и кэшируется: индекс после
    /// загрузки не меняется, а обход всех типов на каждый вызов валидатора
    /// съедал заметное время.
    pub fn all_type_method_names(&self) -> &HashSet<String> {
        self.type_method_names.get_or_init(|| {
            let mut names = HashSet::new();
            for ty in self.types.values() {
                for m in &ty.methods {
                    names.insert(m.name_ru.to_lowercase());
                    if !m.name_en.is_empty() {
                        names.insert(m.name_en.to_lowercase());
                    }
                }
            }
            names
        })
    }

    /// Вставка типа в storage. Перезаписывает по ключу `name_ru.lowercase()`.
    pub fn insert_type(&mut self, ty: Type) {
        let key = ty.name_ru.to_lowercase();
        self.types.insert(key, ty);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::Method;

    fn method(name_ru: &str, name_en: &str) -> Method {
        Method {
            name_ru: name_ru.into(),
            name_en: name_en.into(),
            description: String::new(),
            return_type: String::new(),
            signatures: Vec::new(),
        }
    }

    #[test]
    fn find_global_method_by_russian_name() {
        let mut index = PlatformIndex::new();
        index.global_methods.push(method("Сообщить", "Message"));
        assert!(index.find_global_method("сообщить").is_some());
    }

    #[test]
    fn find_global_method_by_english_name() {
        // Регресс: раньше сверялся только name_ru, английский синоним не находился.
        let mut index = PlatformIndex::new();
        index.global_methods.push(method("Сообщить", "Message"));
        assert!(index.find_global_method("Message").is_some());
        assert!(index.find_global_method("message").is_some());
    }

    #[test]
    fn find_global_method_empty_name_en_does_not_match_empty_query() {
        let mut index = PlatformIndex::new();
        index.global_methods.push(method("Прочее", ""));
        assert!(index.find_global_method("").is_none());
    }
}
