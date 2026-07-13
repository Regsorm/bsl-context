//! HTTP-роутер: /health (для healthcheck-обёртки супервизора) и /mcp
//! (Streamable-HTTP MCP, если индекс загружен — иначе 503-заглушка).

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use rmcp::transport::streamable_http_server::{
    session::never::NeverSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use serde::Serialize;

use crate::config::Config;
use crate::mcp_server::{BslContextServer, SourceSlot};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Краткая статистика индекса для /health (заполнена, если индекс загружен).
    pub index_stats: Option<IndexStats>,
    /// Именованные источники имён конфигураций: describe() каждого читается на
    /// каждый /health, потому что `rebuild_symbol_index` подменяет источник на
    /// ходу. Пустая карта — сервера без источников или без индекса вообще.
    sources: Arc<std::collections::BTreeMap<String, SourceSlot>>,
}

#[derive(Clone, Serialize)]
pub struct IndexStats {
    pub global_methods: usize,
    pub global_properties: usize,
    pub types: usize,
    pub enum_types: usize,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    started_at: String,
    uptime_sec: i64,
    /// Путь к платформе из конфига. None — пользователь не указал.
    platform_path: Option<String>,
    /// `true`, если индекс платформы успешно загружен.
    index_loaded: bool,
    /// Статистика индекса (когда `index_loaded == true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    index_stats: Option<IndexStats>,
    /// Дефолтный уровень валидации (из `config.toml`).
    default_validation_level: u8,
    /// Алиас → `describe()` подключённого источника имён этой конфигурации,
    /// либо "не собран" (слот настроен, но lite-база ещё не построена).
    /// Пусто — конфигураций не настроено вовсе.
    symbol_sources: std::collections::BTreeMap<String, String>,
}

/// Собрать роутер: /health всегда + /mcp (рабочий или 503-заглушка).
pub fn router(config: Config, mcp: Option<BslContextServer>) -> Router {
    // Список разрешённых Host для /mcp (защита rmcp от DNS-rebinding). Клонируем
    // до перемещения config в AppState.
    let allowed_hosts = config.allowed_hosts.clone();
    let index_stats = mcp.as_ref().map(|s| IndexStats {
        global_methods: s.index.global_methods.len(),
        global_properties: s.index.global_properties.len(),
        types: s.index.types.len(),
        enum_types: s.index.enum_types_count(),
    });
    let sources = mcp.as_ref().map(|s| s.sources.clone()).unwrap_or_default();

    let state = AppState {
        config: Arc::new(config),
        started_at: chrono::Utc::now(),
        index_stats,
        sources,
    };

    let mut router = Router::new()
        .route("/health", get(health))
        .with_state(state);

    if let Some(server) = mcp {
        // Stateless Streamable HTTP — устраняет 404 Session not found при
        // рестарте сервера (см. карточку #1184 для mcp-cache-ci v0.3.0).
        let session_manager = Arc::new(NeverSessionManager::default());
        let service_factory = move || Ok(server.clone());
        let http_config = StreamableHttpServerConfig::default()
            .with_stateful_mode(false)
            .with_json_response(true)
            .with_allowed_hosts(allowed_hosts);
        let http_service = StreamableHttpService::new(
            service_factory,
            session_manager,
            http_config,
        );
        router = router.nest_service("/mcp", http_service);
    } else {
        router = router.route("/mcp", post(mcp_placeholder));
    }
    router
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let now = chrono::Utc::now();
    let uptime = (now - state.started_at).num_seconds();
    let mut symbol_sources = std::collections::BTreeMap::new();
    for (name, slot) in state.sources.iter() {
        let status = slot
            .source
            .read()
            .await
            .as_ref()
            .map(|s| s.describe())
            .unwrap_or_else(|| "не собран".to_string());
        symbol_sources.insert(name.clone(), status);
    }
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        started_at: state.started_at.to_rfc3339(),
        uptime_sec: uptime,
        platform_path: state
            .config
            .platform_path
            .as_ref()
            .map(|p| p.display().to_string()),
        index_loaded: state.index_stats.is_some(),
        index_stats: state.index_stats.clone(),
        default_validation_level: state.config.default_validation_level,
        symbol_sources,
    })
}

/// Заглушка MCP-эндпоинта: возвращается, когда `platform_path` не задан и
/// индекс не загружен. Это сигнал оператору: указать платформу в config.toml
/// и перезапустить сервис.
async fn mcp_placeholder() -> impl IntoResponse {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": "MCP недоступен: платформенный контекст не загружен.",
            "hint": "Укажите platform_path в config.toml (например, 'C:\\Program Files\\1cv8\\8.3.27.1786') и перезапустите сервис."
        })),
    )
}
