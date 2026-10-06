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
            // Пустой db_path — это опечатка/недописанный конфиг, а не «индекса
            // ещё нет»: иначе источник молча отключается с last_error=null.
            let path = cfg
                .db_path
                .as_deref()
                .filter(|p| !p.as_os_str().is_empty())
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
                    // Путь — в журнал; клиенту (last_error уходит в MCP-ответ)
                    // локальные пути не раскрываем.
                    tracing::warn!(error = %e, path = %path.display(), "lite-индекс не открылся");
                    "не удалось открыть lite-индекс (подробности в журнале сервера)".to_string()
                })
        }
        "code_index_db" => {
            let path = cfg
                .db_path
                .as_deref()
                .filter(|p| !p.as_os_str().is_empty())
                .ok_or_else(|| {
                    "symbol_source.kind = \"code_index_db\", но db_path не задан".to_string()
                })?;
            symbol_source::CodeIndexDbSource::open_with_refresh(
                path,
                std::time::Duration::from_millis(cfg.refresh_ms),
            )
            .map(|src| Some(Arc::new(src) as Arc<dyn SymbolSource>))
            .map_err(|e| {
                tracing::warn!(error = %e, path = %path.display(), "база code-index не открылась");
                "не удалось открыть базу code-index (подробности в журнале сервера)".to_string()
            })
        }
        "code_index_mcp" => {
            let url = cfg
                .url
                .clone()
                .filter(|u| !u.trim().is_empty())
                .ok_or_else(|| {
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
                    // URL маскируется: userinfo/query с паролями и токенами не
                    // должны попадать в last_error, который уходит MCP-клиенту.
                    format!(
                        "не удалось подключить MCP-источник code-index {} (repo={repo}): {e}",
                        symbol_source::redact_url(&url)
                    )
                })
        }
        other => Err(format!(
            "неизвестный symbol_source.kind = \"{other}\" — источник имён не создан"
        )),
    }
}
