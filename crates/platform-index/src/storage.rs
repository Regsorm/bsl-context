//! `PlatformIndex` — central storage с тремя коллекциями.
//!
//! Иерархия (правильная для 1С): системное перечисление это разновидность типа,
//! а не отдельная категория. Поэтому `types` — единый словарь, в котором
//! и обычные типы, и перечисления (последние с непустым `enum_values`).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use tracing::warn;

use crate::entities::{Method, Property, Type};

/// Storage платформенного контекста (read-only после загрузки).
#[derive(Debug, Default, Clone)]
pub struct PlatformIndex {
    pub global_methods: Vec<Method>,
    pub global_properties: Vec<Property>,
    /// Ключ — `name_ru` в нижнем регистре. Тип-перечисление и обычный тип лежат вместе.
    pub types: HashMap<String, Type>,
    /// `name_en` в нижнем регистре → ключ в `types`. Платформа принимает оба
    /// написания (`Новый Массив` и `Новый Array`), и без этой карты английское
    /// имя давало находку «тип не найден» на законном коде. Отдельная карта, а
    /// не обход при промахе: `find_type` зовётся на каждое обращение, а типов
    /// больше двух тысяч.
    ///
    /// `pub(crate)` — карта сохраняется в дисковый кэш как есть: восстановить её
    /// из `types` после десериализации можно только приблизительно (при
    /// столкновении английских имён побеждает последний вставленный тип, а
    /// порядок вставки кэш не хранит).
    pub(crate) types_en: HashMap<String, String>,
    /// Ленивый кэш имён методов всех типов (см. `all_type_method_names`).
    /// Обход тысяч типов стоит единицы миллисекунд (release ~6 мс) — на каждый
    /// вызов `validate_module` это заметно, а индекс после загрузки неизменен.
    type_method_names: OnceLock<HashSet<String>>,
}

/// Содержимое индекса совпадает. `type_method_names` — производное от `types`
/// (ленивый кэш имён методов), поэтому в сравнении не участвует: индекс из
/// кэша обязан быть равен собранному из hbk по содержимому, но не по состоянию
/// вторичных карт, которые пересчитываются.
impl PartialEq for PlatformIndex {
    fn eq(&self, other: &Self) -> bool {
        self.global_methods == other.global_methods
            && self.global_properties == other.global_properties
            && self.types == other.types
            && self.types_en == other.types_en
    }
}

impl PlatformIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Сколько типов, у которых заполнен `enum_values` (системные перечисления).
    pub fn enum_types_count(&self) -> usize {
        self.types.values().filter(|t| t.is_enum()).count()
    }

    /// Точный поиск типа по имени (регистронезависимо).
    ///
    /// Сверяются ОБА имени — русское и английское, как в `find_global_method`:
    /// платформа принимает и `Массив`, и `Array`.
    pub fn find_type(&self, name: &str) -> Option<&Type> {
        let key = name.to_lowercase();
        if key.is_empty() {
            return None;
        }
        self.types
            .get(&key)
            .or_else(|| self.types_en.get(&key).and_then(|ru| self.types.get(ru)))
    }

    /// Точный поиск глобального метода по имени (регистронезависимо).
    ///
    /// Сверяются ОБА имени — русское и английское: платформа принимает и
    /// `Сообщить(...)`, и `Message(...)`. Раньше искали только по `name_ru`,
    /// из-за чего английский вызов не находился и уходил в fuzzy, где
    /// находил сам себя в `name_en` с нулевым расстоянием.
    pub fn find_global_method(&self, name: &str) -> Option<&Method> {
        let key = name.to_lowercase();
        if key.is_empty() {
            return None;
        }
        self.global_methods.iter().find(|m| {
            (!m.name_ru.is_empty() && m.name_ru.to_lowercase() == key)
                || (!m.name_en.is_empty() && m.name_en.to_lowercase() == key)
        })
    }

    /// Точный поиск глобального свойства по имени (регистронезависимо).
    ///
    /// Оба имени, как и у метода: `Справочники` и `Catalogs` — одно и то же
    /// свойство глобального контекста.
    pub fn find_global_property(&self, name: &str) -> Option<&Property> {
        let key = name.to_lowercase();
        if key.is_empty() {
            return None;
        }
        self.global_properties.iter().find(|p| {
            (!p.name_ru.is_empty() && p.name_ru.to_lowercase() == key)
                || (!p.name_en.is_empty() && p.name_en.to_lowercase() == key)
        })
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

    /// Вставка типа в storage. Ключ — `name_ru` в нижнем регистре.
    ///
    /// Два разных корня TOC могут дать один `name_ru` (например «Интерфейс
    /// (обычный)» и «Интерфейс (управляемый)»): молча затирать более полную
    /// запись более бедной нельзя — оставляем запись с большим числом членов,
    /// о конфликте пишем в журнал. Устаревший английский алиас перезаписанного
    /// типа удаляется, чтобы `find_type` по нему не возвращал чужой тип.
    pub fn insert_type(&mut self, ty: Type) {
        let key = ty.name_ru.to_lowercase();
        if let Some(old) = self.types.get(&key) {
            let score = |t: &Type| {
                t.methods.len() + t.properties.len() + t.constructors.len() + t.enum_values.len()
            };
            if old != &ty && score(old) > score(&ty) {
                warn!(
                    type_name = %ty.name_ru,
                    kept = score(old),
                    dropped = score(&ty),
                    "тип с тем же именем уже есть в индексе — более полная запись сохранена"
                );
                return;
            }
            if old.name_en.to_lowercase() != ty.name_en.to_lowercase() && !old.name_en.is_empty() {
                self.types_en.remove(&old.name_en.to_lowercase());
            }
        }
        if !ty.name_en.is_empty() {
            self.types_en.insert(ty.name_en.to_lowercase(), key.clone());
        }
        self.types.insert(key, ty);
        // Индекс после загрузки не меняется, но публичный мутатор обязан
        // сбрасывать производный кэш имён.
        self.type_method_names = OnceLock::new();
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
            note: None,
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
