//! Business-слой `bsl-context-rs`: storage платформенного контекста + поиск + Markdown-форматтер.
//!
//! Иерархия (правильная для 1С): системное перечисление это разновидность типа,
//! а не отдельная категория storage. Три коллекции в [`PlatformIndex`]:
//! `global_methods`, `global_properties`, `types` (HashMap по `name_ru`).
//!
//! Главное отличие от апстрима — `signatures` методов (включая текст синтаксиса),
//! `constructors` типов и `enum_values` системных перечислений заполняются.
//! У апстрима (путь `upstream/.../persistent/storage/Mapper.kt`, вне
//! репозитория) они теряются.

pub mod cache;
pub mod entities;
pub mod format;
pub mod loader;
pub mod mapper;
pub mod search;
pub mod storage;
pub mod visitor;

pub use cache::{load_cached, LoadSource, DEFAULT_CACHE_FILE_NAME};
pub use entities::{
    Constructor, Definition, EnumValue, Method, Parameter, Property, Signature, Type,
};
pub use loader::{build_index, load_from_hbk};
pub use search::SearchEngine;
pub use storage::PlatformIndex;
