//! Создание внешних источников имён конфигурации по конфигу.
//!
//! Вынесено из `main.rs`: источник нужно создавать не только на старте, но и
//! на ходу — сетевой источник (`code_index_mcp`) роняет свой флаг `healthy`
//! навсегда при первой же ошибке транспорта, и без пересоздания валидация по
//! этой конфигурации оставалась бы отключённой до перезапуска процесса.

use std::sync::Arc;

use bsl_validator::SymbolSource;

use crate::config::SymbolSourceConfig;

/// Создать внешний источник имён по конфигу (`symbol_source.kind`).
///
/// - `Ok(Some(_))` — источник готов.
/// - `Ok(None)` — источника штатно нет: `kind = "none"` либо lite-индекс ещё
///   не собран (его собирает `rebuild_symbol_index`).
/// - `Err(текст)` — подключить не удалось. Текст возвращается вызывающему, а
///   не только пишется в журнал: он нужен инструменту `symbol_sources_status`,
///   иначе причину отказа можно узнать лишь чтением логов сервера.
///
/// Ошибка создания НЕ валит сервер: вызывающий пишет предупреждение, а
/// `validate_module` переходит на проверку против одного платформенного
/// контекста (см. `validate_module_degraded`).
pub fn build_symbol_source(
    cfg: &SymbolSourceConfig,
) -> Result<Option<Arc<dyn SymbolSource>>, String> {
    match cfg.kind.as_str() {
        "none" => Ok(None),
        "lite" => {
            let path = cfg
                .db_path
                .as_deref()
                .ok_or_else(|| "symbol_source.kind = \"lite\", но db_path не задан".to_string())?;
            if !path.exists() {
                tracing::warn!(
                    path = %path.display(),
                    "lite-индекса ещё нет — источник не подключён; вызовите инструмент rebuild_symbol_index"
                );
                return Ok(None);
            }
            symbol_source::LiteSource::open(path)
                .map(|src| Some(Arc::new(src) as Arc<dyn SymbolSource>))
                .map_err(|e| {
                    format!(
                        "не удалось открыть lite-индекс {}: {e}",
                        path.display()
                    )
                })
        }
        "code_index_db" => {
            let path = cfg.db_path.as_deref().ok_or_else(|| {
                "symbol_source.kind = \"code_index_db\", но db_path не задан".to_string()
            })?;
            symbol_source::CodeIndexDbSource::open(path)
                .map(|src| Some(Arc::new(src) as Arc<dyn SymbolSource>))
                .map_err(|e| {
                    format!(
                        "не удалось открыть базу code-index {}: {e}",
                        path.display()
                    )
                })
        }
        "code_index_mcp" => {
            let url = cfg.url.clone().ok_or_else(|| {
                "symbol_source.kind = \"code_index_mcp\", но url не задан".to_string()
            })?;
            let repo = cfg
                .code_index_repo_effective()
                .ok_or_else(|| {
                    "symbol_source.kind = \"code_index_mcp\", но алиас репозитория не задан"
                        .to_string()
                })?
                .to_string();
            symbol_source::CodeIndexMcpSource::new(url.clone(), repo.clone(), cfg.timeout_ms)
                .map(|src| Some(Arc::new(src) as Arc<dyn SymbolSource>))
                .map_err(|e| {
                    format!("не удалось подключить MCP-источник code-index {url} (repo={repo}): {e}")
                })
        }
        other => Err(format!(
            "неизвестный symbol_source.kind = \"{other}\" — источник имён не создан"
        )),
    }
}
