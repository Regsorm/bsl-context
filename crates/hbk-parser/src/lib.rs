//! Парсеры html-страниц `shcntx_ru.hbk`.
//!
//! Pipeline (Phase 2): hbk-страница (UTF-8 html) → структурированный `*Info` (`MethodInfo`,
//! `PropertyInfo`, `ObjectInfo`, `EnumInfo`, `EnumValueInfo`, `ConstructorInfo`).
//!
//! Архитектура отличается от апстрима: апстрим использует SAX-обработчики
//! поверх Ksoup (см. `BlockHandler.kt`), мы используем DOM-подход через
//! `scraper` — режем страницу на главы по маркерам
//! `<p|div class="V8SH_chapter">`/`<hr>`, потом каждую главу обрабатываем
//! функцией под её тип.
//!
//! Все `parse_*`-функции не возвращают `Result`: на пустом/битом html они
//! отдают структуру по умолчанию (fail-open), а решение «молчать» принимает
//! вызывающий код.
//!
//! ```
//! let info = hbk_parser::parse_method_page("<body><hr></body>");
//! assert!(info.name_ru.is_empty());
//! ```

mod blocks;
mod constructor_page;
mod enum_page;
mod enum_value;
mod html;
mod method_page;
mod models;
mod object_page;
mod property_page;

pub use constructor_page::parse_constructor_page;
pub use enum_page::parse_enum_page;
pub use enum_value::parse_enum_value_page;
pub use method_page::parse_method_page;
pub use object_page::parse_object_page;
pub use property_page::parse_property_page;

pub use models::{
    ConstructorInfo, EnumInfo, EnumValueInfo, MethodInfo, MethodParameterInfo, MethodSignatureInfo,
    ObjectInfo, PropertyInfo, RelatedObject, ValueInfo,
};
