//! Чтение бинарного контейнера `shcntx_ru.hbk`:
//! - таблица entities + извлечение тел;
//! - распаковка `PackBlock` (TOC) и парсинг текстового формата;
//! - открытие `FileStorage` (zip с html-страницами).
//!
//! Публичный API: [`HbkContent::read`] — основной вход.
//!
//! Порт логики из апстрима `alkoleft/mcp-bsl-platform-context` (Kotlin).
//! Эталон — апстримный каталог `upstream/src/main/kotlin/.../infrastructure/hbk/`
//! (вне репозитория, см. `.gitignore`).

pub mod container;
pub mod content;
pub mod error;
pub mod models;
pub mod toc;

pub use container::HbkContainer;
pub use content::HbkContent;
pub use error::{HbkError, Result};
pub use models::{
    Chunk, DoubleLanguageString, NameContainer, NameObject, Page, PropertiesContainer, Toc,
};
pub use toc::parse_toc;
