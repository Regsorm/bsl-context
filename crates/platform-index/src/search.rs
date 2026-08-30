//! Поисковые индексы по `PlatformIndex`.
//!
//! Порт `HashIndex` + `StartWithIndex` + упрощённый word-order fuzzy.
//! Эталон — `upstream/.../infrastructure/search/`.

use std::collections::{BTreeMap, HashMap};

use crate::entities::{Definition, Method, Property, Type};
use crate::storage::PlatformIndex;

/// Точечный индекс по нижнему регистру имени → элемент.
#[derive(Debug, Default, Clone)]
pub struct HashIndex<T: Clone> {
    inner: HashMap<String, T>,
}

impl<T: Clone> HashIndex<T> {
    /// `keys` отдаёт ОБА имени элемента — русское и английское. Индексируются
    /// оба: платформа принимает `Сообщить` и `Message` наравне, и справочные
    /// инструменты (`info`, `get_member`) обязаны отвечать одинаково на любое
    /// из них. Русское имя приоритетно: английский омоним его не перетирает.
    pub fn from_iter_with<I, K>(items: I, keys: K) -> Self
    where
        I: IntoIterator<Item = T>,
        K: Fn(&T) -> (&str, &str),
    {
        let mut inner = HashMap::new();
        let mut aliases: Vec<(String, T)> = Vec::new();
        for it in items {
            let (ru, en) = keys(&it);
            let (ru, en) = (ru.to_lowercase(), en.to_lowercase());
            if !en.is_empty() && en != ru {
                aliases.push((en, it.clone()));
            }
            inner.insert(ru, it);
        }
        for (en, it) in aliases {
            inner.entry(en).or_insert(it);
        }
        Self { inner }
    }

    pub fn get(&self, name: &str) -> Option<&T> {
        self.inner.get(&name.to_lowercase())
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// Префиксный индекс на отсортированной мапе.
#[derive(Debug, Default, Clone)]
pub struct StartWithIndex<T: Clone> {
    inner: BTreeMap<String, T>,
}

impl<T: Clone> StartWithIndex<T> {
    /// Оба имени, как и в [`HashIndex::from_iter_with`]: набор `Мас` и `Arr`
    /// должны одинаково приводить к типу `Массив`.
    pub fn from_iter_with<I, K>(items: I, keys: K) -> Self
    where
        I: IntoIterator<Item = T>,
        K: Fn(&T) -> (&str, &str),
    {
        let mut inner = BTreeMap::new();
        let mut aliases: Vec<(String, T)> = Vec::new();
        for it in items {
            let (ru, en) = keys(&it);
            let (ru, en) = (ru.to_lowercase(), en.to_lowercase());
            if !en.is_empty() && en != ru {
                aliases.push((en, it.clone()));
            }
            inner.insert(ru, it);
        }
        for (en, it) in aliases {
            inner.entry(en).or_insert(it);
        }
        Self { inner }
    }

    pub fn starts_with(&self, prefix: &str) -> Vec<&T> {
        let p = prefix.to_lowercase();
        self.inner
            .range(p.clone()..)
            .take_while(|(k, _)| k.starts_with(&p))
            .map(|(_, v)| v)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// Поисковый движок над `PlatformIndex`.
#[derive(Debug, Clone)]
pub struct SearchEngine {
    methods_hash: HashIndex<Method>,
    properties_hash: HashIndex<Property>,
    types_hash: HashIndex<Type>,
    methods_prefix: StartWithIndex<Method>,
    properties_prefix: StartWithIndex<Property>,
    types_prefix: StartWithIndex<Type>,
}

impl SearchEngine {
    pub fn from_index(index: &PlatformIndex) -> Self {
        let methods_hash = HashIndex::from_iter_with(index.global_methods.iter().cloned(), |m| {
            (m.name_ru.as_str(), m.name_en.as_str())
        });
        let properties_hash =
            HashIndex::from_iter_with(index.global_properties.iter().cloned(), |p| {
                (p.name_ru.as_str(), p.name_en.as_str())
            });
        let types_hash = HashIndex::from_iter_with(index.types.values().cloned(), |t| {
            (t.name_ru.as_str(), t.name_en.as_str())
        });
        let methods_prefix =
            StartWithIndex::from_iter_with(index.global_methods.iter().cloned(), |m| {
                (m.name_ru.as_str(), m.name_en.as_str())
            });
        let properties_prefix =
            StartWithIndex::from_iter_with(index.global_properties.iter().cloned(), |p| {
                (p.name_ru.as_str(), p.name_en.as_str())
            });
        let types_prefix = StartWithIndex::from_iter_with(index.types.values().cloned(), |t| {
            (t.name_ru.as_str(), t.name_en.as_str())
        });
        Self {
            methods_hash,
            properties_hash,
            types_hash,
            methods_prefix,
            properties_prefix,
            types_prefix,
        }
    }

    /// Точный поиск по любому виду имени.
    pub fn find_method(&self, name: &str) -> Option<&Method> {
        self.methods_hash.get(name)
    }

    pub fn find_property(&self, name: &str) -> Option<&Property> {
        self.properties_hash.get(name)
    }

    pub fn find_type(&self, name: &str) -> Option<&Type> {
        self.types_hash.get(name)
    }

    /// Найти член (метод/свойство) у типа по точному имени — русскому либо
    /// английскому.
    pub fn find_type_member(&self, ty: &Type, name: &str) -> Option<Definition> {
        let key = name.to_lowercase();
        let matches = |ru: &str, en: &str| {
            ru.to_lowercase() == key || (!en.is_empty() && en.to_lowercase() == key)
        };
        if let Some(m) = ty.methods.iter().find(|m| matches(&m.name_ru, &m.name_en)) {
            return Some(Definition::Method(m.clone()));
        }
        if let Some(p) = ty
            .properties
            .iter()
            .find(|p| matches(&p.name_ru, &p.name_en))
        {
            return Some(Definition::Property(p.clone()));
        }
        None
    }

    /// Универсальный поиск: префиксное совпадение + word-order fallback. Никаких
    /// весовых тонкостей — выдаём в порядке: типы, методы, свойства, без сортировки.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Definition> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }

        let mut out = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // 1. Префиксы.
        for t in self.types_prefix.starts_with(&q) {
            push_unique(&mut out, &mut seen, Definition::Type(t.clone()));
        }
        for m in self.methods_prefix.starts_with(&q) {
            push_unique(&mut out, &mut seen, Definition::Method(m.clone()));
        }
        for p in self.properties_prefix.starts_with(&q) {
            push_unique(&mut out, &mut seen, Definition::Property(p.clone()));
        }

        // 2. Word-order fuzzy — все слова query идут по порядку в name_ru/name_en.
        let words: Vec<&str> = q.split_whitespace().collect();
        if words.len() > 1 {
            for ty in self.types_hash.inner.values() {
                if word_order_match(&ty.name_ru, &words) || word_order_match(&ty.name_en, &words) {
                    push_unique(&mut out, &mut seen, Definition::Type(ty.clone()));
                }
            }
            for m in self.methods_hash.inner.values() {
                if word_order_match(&m.name_ru, &words) || word_order_match(&m.name_en, &words) {
                    push_unique(&mut out, &mut seen, Definition::Method(m.clone()));
                }
            }
            for p in self.properties_hash.inner.values() {
                if word_order_match(&p.name_ru, &words) || word_order_match(&p.name_en, &words) {
                    push_unique(&mut out, &mut seen, Definition::Property(p.clone()));
                }
            }
        }

        // 3. Substring — последний резерв (если префикс ничего не дал).
        if out.is_empty() {
            for ty in self.types_hash.inner.values() {
                if ty.name_ru.to_lowercase().contains(&q)
                    || ty.name_en.to_lowercase().contains(&q)
                {
                    push_unique(&mut out, &mut seen, Definition::Type(ty.clone()));
                }
            }
            for m in self.methods_hash.inner.values() {
                if m.name_ru.to_lowercase().contains(&q)
                    || m.name_en.to_lowercase().contains(&q)
                {
                    push_unique(&mut out, &mut seen, Definition::Method(m.clone()));
                }
            }
            for p in self.properties_hash.inner.values() {
                if p.name_ru.to_lowercase().contains(&q)
                    || p.name_en.to_lowercase().contains(&q)
                {
                    push_unique(&mut out, &mut seen, Definition::Property(p.clone()));
                }
            }
        }

        out.truncate(limit.max(1).min(50));
        out
    }
}

fn push_unique(
    out: &mut Vec<Definition>,
    seen: &mut std::collections::HashSet<String>,
    def: Definition,
) {
    let key = format!("{}:{}", def.kind_label(), def.name_ru().to_lowercase());
    if seen.insert(key) {
        out.push(def);
    }
}

fn word_order_match(haystack: &str, words: &[&str]) -> bool {
    let h = haystack.to_lowercase();
    let mut cursor = 0usize;
    for w in words {
        if let Some(pos) = h[cursor..].find(w) {
            cursor += pos + w.len();
        } else {
            return false;
        }
    }
    true
}
