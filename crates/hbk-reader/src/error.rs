//! Ошибки при чтении hbk-контейнера.

use std::io;
use std::path::PathBuf;

/// Ошибки чтения и разбора hbk.
///
/// `#[non_exhaustive]`: новые варианты добавляются без breaking-change,
/// внешние матчи обязаны иметь ветку `_`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HbkError {
    #[error("hbk-файл не существует: {0}")]
    NotFound(PathBuf),

    #[error("ошибка ввода-вывода: {0}")]
    Io(#[from] io::Error),

    #[error("неожиданный формат hbk-контейнера: {0}")]
    BadFormat(String),

    #[error("ошибка распаковки PackBlock (zlib/zip): {0}")]
    Inflate(String),

    #[error("ошибка zip-архива FileStorage: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("entity '{0}' не найдена в hbk-контейнере")]
    EntityNotFound(String),

    #[error("файл '{0}' не найден в архиве FileStorage")]
    HtmlEntryNotFound(String),

    #[error("ошибка чтения страницы '{path}': {source}")]
    EntryRead {
        path: String,
        #[source]
        source: io::Error,
    },

    #[error("страница '{path}': превышен лимит размера {limit} байт")]
    PageTooLarge { path: String, limit: u64 },

    #[error("ошибка парсинга TOC: {0}")]
    TocParse(String),
}

pub type Result<T> = std::result::Result<T, HbkError>;
