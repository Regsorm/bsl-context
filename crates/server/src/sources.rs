//! Создание внешних источников имён конфигурации по конфигу.
//!
//! Вынесено из `main.rs`: источник нужно создавать не только на старте, но и
//! на ходу — сетевой источник (`code_index_mcp`) роняет свой флаг `healthy`
//! навсегда при первой же ошибке транспорта, и без пересоздания валидация по
//! этой конфигурации оставалась бы отключённой до перезапуска процесса.

use std::sync::Arc;

use bsl_validator::SymbolSource;
use tracing::error;

use crate::config::SymbolSourceConfig;

/// Создать внешний источник имён по конфигу (`symbol_source.kind`). Ошибка
/// создания НЕ валит сервер — предупреждение в лог, `validate_module`
/// работает без источника (как раньше).
pub fn build_symbol_source(cfg: &SymbolSourceConfig) -> Option<Arc<dyn SymbolSource>> {
    match cfg.kind.as_str() {
        "none" => None,
        "lite" => {
            let path = cfg.db_path.as_deref()?;
            if !path.exists() {
                tracing::warn!(
                    path = %path.display(),
                    "lite-индекса ещё нет — источник не подключён; вызовите инструмент rebuild_symbol_index"
                );
                return None;
            }
            match symbol_source::LiteSource::open(path) {
                Ok(src) => Some(Arc::new(src) as Arc<dyn SymbolSource>),
                Err(e) => {
                    error!(error = %e, path = %path.display(), "не удалось открыть lite-index источник имён");
                    None
                }
            }
        }
        "code_index_db" => {
            let path = cfg.db_path.as_deref()?;
            match symbol_source::CodeIndexDbSource::open(path) {
                Ok(src) => Some(Arc::new(src) as Arc<dyn SymbolSource>),
                Err(e) => {
                    error!(error = %e, path = %path.display(), "не удалось открыть code-index источник имён");
                    None
                }
            }
        }
        "code_index_mcp" => {
            let url = cfg.url.clone()?;
            let repo = cfg.code_index_repo_effective()?.to_string();
            match symbol_source::CodeIndexMcpSource::new(url.clone(), repo.clone(), cfg.timeout_ms)
            {
                Ok(src) => Some(Arc::new(src) as Arc<dyn SymbolSource>),
                Err(e) => {
                    error!(error = %e, %url, %repo, "не удалось подключить MCP-источник имён code-index");
                    None
                }
            }
        }
        other => {
            error!(kind = other, "неизвестный symbol_source.kind — источник имён не создан");
            None
        }
    }
}
