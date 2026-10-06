//! Модели TOC: дерево страниц с двуязычными названиями.

/// Двуязычная строка (английская + русская версия; порядок полей — `en`, `ru`).
///
/// Платформа 1С хранит названия типов и методов параллельно на двух языках;
/// основной носитель пары — [`Page::title`], остальные сущности TOC служебные.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DoubleLanguageString {
    pub en: String,
    pub ru: String,
}

impl DoubleLanguageString {
    pub fn new(en: impl Into<String>, ru: impl Into<String>) -> Self {
        Self {
            en: en.into(),
            ru: ru.into(),
        }
    }
}

/// Страница документации в TOC. Дерево формируется через `children`; глубина
/// ограничена парсером (см. `MAX_TOC_DEPTH` в `toc`), чтобы враждебный TOC не
/// переполнял стек рекурсивным `Drop`/`Clone`.
#[derive(Debug, Clone)]
pub struct Page {
    pub title: DoubleLanguageString,
    pub html_path: String,
    pub children: Vec<Page>,
}

impl Page {
    pub fn new(title: DoubleLanguageString, html_path: impl Into<String>) -> Self {
        Self {
            title,
            html_path: html_path.into(),
            children: Vec::new(),
        }
    }
}

/// Чанк TOC (промежуточный слой парсера, до построения дерева Page).
#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: i32,
    pub parent_id: i32,
    pub child_count: i32,
    pub child_ids: Vec<i32>,
    pub properties: PropertiesContainer,
}

#[derive(Debug, Clone)]
pub struct PropertiesContainer {
    pub number1: i32,
    pub number2: i32,
    pub name_container: NameContainer,
    pub html_path: String,
}

#[derive(Debug, Clone)]
pub struct NameContainer {
    pub number1: i32,
    pub number2: i32,
    pub name_objects: Vec<NameObject>,
}

#[derive(Debug, Clone)]
pub struct NameObject {
    pub language_code: String,
    pub name: String,
}

/// Готовое оглавление: список страниц-корней (дочерние элементы виртуального
/// корня TOC). Каждая страница — поддерево.
#[derive(Debug, Clone)]
pub struct Toc {
    pub pages: Vec<Page>,
}
