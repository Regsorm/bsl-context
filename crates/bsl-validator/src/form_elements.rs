//! Проверка имён элементов формы: `Элементы.<Имя>` (вторая половина issue #43).
//!
//! В модуле управляемой формы обращение к элементу идёт через `Элементы`:
//! `Элементы.Товары.Видимость = Ложь`, `Элементы.НетТакогоЭлемента123.Прочитать()`.
//! Имя элемента знает только сама форма — оно лежит в `Form.xml` выгрузки рядом с
//! модулем. Пока набор имён не передан, проверка молчит целиком: без него
//! выдуманное имя неотличимо от законного.
//!
//! Три случая молчания, кроме отсутствия набора:
//!
//! * **элемент создан в рантайме** — модуль зовёт `Элементы.Добавить(...)`, и
//!   дальше имя живёт только в коде: состав из XML про такие имена не знает;
//! * **голова связана локально** (`Элементы = …`) — это переменная, а не
//!   коллекция элементов формы;
//! * **обычная форма** (`ЭлементыФормы.<Имя>`): её состав лежит в двоичном
//!   `Form.bin`, а не в XML — данных нет, значит и находок нет.

use std::collections::{HashMap, HashSet};

use bsl_parse::{is_current_form, is_form_items, AstFacts, DotFact};
use platform_index::{PlatformIndex, Type};

use crate::locals::LocalNames;

use crate::expression::{fuzzy_confidence_for, lev, pos_at, Confidence, ExprError, ExprErrorKind};

/// Голова коллекции элементов УПРАВЛЯЕМОЙ формы.
const ELEMENTS_HEAD: &str = "Элементы";

/// Голова коллекции элементов обычной формы — данных о ней у нас нет.
const ORDINARY_ELEMENTS_HEAD: &str = "ЭлементыФормы";

/// Метод, которым элемент добавляется в рантайме.
const ADD_METHOD: &str = "Добавить";

/// Имена формы и типы её статических элементов из выгрузки.
#[derive(Default)]
pub struct FormElements {
    pub names: HashSet<String>,
    types: HashMap<String, Option<&'static str>>,
}

impl FormElements {
    pub fn record_type(&mut self, name: &str, ty: Option<&'static str>) {
        self.types
            .entry(name.to_lowercase())
            .and_modify(|previous| {
                if *previous != ty {
                    *previous = None;
                }
            })
            .or_insert(ty);
    }

    pub fn element_type(&self, name: &str) -> Option<&'static str> {
        self.types.get(&name.to_lowercase()).copied().flatten()
    }
}

/// Дополнительные члены для видов и источников данных элементов.
const FORM_EXTENSIONS: [(&str, &str); 4] = [
    ("ПолеФормы", "Расширение поля формы для "),
    ("ГруппаФормы", "Расширение группы формы для "),
    ("ТаблицаФормы", "Расширение таблицы формы для "),
    ("ДекорацияФормы", "Расширение декорации формы для "),
];

pub(crate) struct FormElementContext<'a> {
    elements: &'a FormElements,
    locals: &'a LocalNames<'a>,
    attributes: Option<&'a HashSet<String>>,
    extensions: HashMap<&'static str, Vec<&'a Type>>,
}

impl<'a> FormElementContext<'a> {
    pub(crate) fn new(
        index: &'a PlatformIndex,
        elements: &'a FormElements,
        locals: &'a LocalNames<'a>,
        attributes: Option<&'a HashSet<String>>,
        facts: &AstFacts,
    ) -> Option<Self> {
        fn changes_items(method: &str) -> bool {
            matches!(
                method.to_lowercase().as_str(),
                "добавить" | "вставить" | "удалить" | "add" | "insert" | "delete"
            )
        }
        if elements.types.is_empty()
            || facts.dots.iter().any(|dot| {
                dot.member_is_call && is_form_items(&dot.head) && changes_items(&dot.member)
            })
            || facts.manager_calls.iter().any(|call| {
                is_current_form(&call.collection)
                    && is_form_items(&call.object)
                    && changes_items(&call.method)
            })
        {
            return None;
        }
        let mut extensions: HashMap<_, Vec<_>> = FORM_EXTENSIONS
            .iter()
            .map(|(base, _)| (*base, Vec::new()))
            .collect();
        for ty in index.types.values() {
            if let Some((base, _)) = FORM_EXTENSIONS
                .iter()
                .find(|(_, prefix)| ty.name_ru.starts_with(prefix))
            {
                extensions.entry(*base).or_default().push(ty);
            }
        }
        Some(Self {
            elements,
            locals,
            attributes,
            extensions,
        })
    }

    pub(crate) fn member_may_be_defined_by_extension(&self, ty: &Type, member: &str) -> bool {
        self.extensions
            .get(ty.name_ru.as_str())
            .is_some_and(|extensions| {
                // Вид элемента или источник данных может быть изменён из BSL.
                // В справке без расширений состав членов этого типа неизвестен.
                extensions.is_empty()
                    || extensions
                        .iter()
                        .any(|ty| crate::expression::type_has_member(ty, member))
            })
    }

    fn type_at(&self, receiver: &str, byte: usize, name: &str) -> Option<&'static str> {
        if self.locals.is_local(byte, receiver)
            || !self.locals.has_form_context(byte)
            || self
                .attributes
                .is_some_and(|attrs| attrs.contains(&receiver.to_lowercase()))
        {
            return None;
        }
        self.elements.element_type(name)
    }

    pub(crate) fn rhs_type(&self, rhs: &str, byte: usize) -> Option<&'static str> {
        let (receiver, suffix) = rhs.split_once('.')?;
        let receiver = receiver.trim();
        let name = if is_form_items(receiver) {
            suffix
        } else if is_current_form(receiver) {
            let (items, name) = suffix.split_once('.')?;
            if !is_form_items(items.trim()) {
                return None;
            }
            name
        } else {
            return None;
        };
        // Поиск точного имени исключает вызовы, индексацию и составной RHS.
        self.type_at(receiver, byte, name.trim())
    }

    pub(crate) fn check_members(
        &self,
        index: &PlatformIndex,
        src: &str,
        facts: &AstFacts,
        errors: &mut Vec<ExprError>,
    ) {
        let dots: Vec<_> = facts
            .form_element_members
            .iter()
            .filter_map(|fact| {
                let ty = self.type_at(&fact.receiver, fact.receiver_byte, &fact.element)?;
                Some(DotFact {
                    head: ty.to_owned(),
                    head_byte: fact.receiver_byte,
                    member: fact.member.clone(),
                    member_byte: fact.member_byte,
                    member_is_call: fact.member_is_call,
                })
            })
            .collect();
        crate::expression::check_type_dot_members(
            index,
            src,
            &dots,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            Some(self),
            errors,
        );
    }
}

pub(crate) fn check_form_elements(
    src: &str,
    facts: &AstFacts,
    elements: Option<&HashSet<String>>,
    errors: &mut Vec<ExprError>,
) {
    let Some(elements) = elements else {
        return;
    };
    if elements.is_empty() {
        return;
    }
    // Рантайм-элементы: после `Элементы.Добавить(...)` состав из XML неполон.
    let dynamic_elements = facts.dots.iter().any(|dot| {
        dot.member_is_call
            && same_name(&dot.head, ELEMENTS_HEAD)
            && same_name(&dot.member, ADD_METHOD)
    });
    if dynamic_elements {
        return;
    }

    let bound = crate::config_objects::locally_bound_names(facts);
    let mut known: Vec<&str> = elements.iter().map(|name| name.as_str()).collect();
    known.sort_unstable();

    for dot in &facts.dots {
        if !same_name(&dot.head, ELEMENTS_HEAD) || same_name(&dot.head, ORDINARY_ELEMENTS_HEAD) {
            continue;
        }
        if bound.contains(&dot.head.to_lowercase()) {
            continue;
        }
        // `Элементы.Добавить(...)` и прочие методы самой коллекции — не имена.
        if dot.member_is_call {
            continue;
        }
        // Мусор восстановления дерева после ошибки разбора — не имя элемента.
        if !is_identifier_like(&dot.member) {
            continue;
        }
        let member_lower = dot.member.to_lowercase();
        if elements
            .iter()
            .any(|name| name.to_lowercase() == member_lower)
        {
            continue;
        }
        let suggestion = closest_element(&known, &dot.member);
        let (line, col) = pos_at(src, dot.member_byte);
        errors.push(ExprError::new_with_confidence(
            line,
            col,
            ExprErrorKind::UnknownFormElement,
            match &suggestion {
                Some(name) => format!(
                    "Элемента '{}' нет в форме. Возможно, вы имели в виду '{}'.",
                    dot.member, name
                ),
                None => format!("Элемента '{}' нет в форме.", dot.member),
            },
            // Состав приходит из файла выгрузки: он может отставать от правок
            // формы, поэтому уверенность низкая.
            Confidence::Low,
            suggestion,
            Vec::new(),
        ));
    }
}

/// Ближайшее имя элемента — только когда это правдоподобная опечатка.
fn closest_element(known: &[&str], member: &str) -> Option<String> {
    let member_lower = member.to_lowercase();
    let mut best: Option<(&str, usize)> = None;
    for name in known {
        let distance = lev(&member_lower, &name.to_lowercase());
        let better = match best {
            Some((best_name, best_distance)) => {
                // Оба имени — `&str`: сравнение не должно зависеть от тулчейна.
                distance < best_distance || (distance == best_distance && *name < best_name)
            }
            None => true,
        };
        if better {
            best = Some((name, distance));
        }
    }
    let (name, distance) = best?;
    fuzzy_confidence_for(member, name, distance).map(|_| name.to_string())
}

/// Сравнение имён без учёта регистра: `eq_ignore_ascii_case` кириллицу не сворачивает.
fn same_name(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

/// Похоже на идентификатор BSL: начинается с буквы или подчёркивания.
fn is_identifier_like(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
}
