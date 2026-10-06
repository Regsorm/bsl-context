//! HTTP-роутер: /health (для healthcheck-обёртки супервизора) и /mcp
//! (всегда Streamable HTTP; без индекса справочные инструменты отвечают отказом).

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};
use rmcp::transport::streamable_http_server::{
    session::never::NeverSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use serde::Serialize;
use tower_http::limit::RequestBodyLimitLayer;

use crate::config::Config;
use crate::mcp_server::BslContextServer;

/// Предел размера тела запроса MCP-кадра. rmcp читает тело без границы
/// (`body.collect()`), поэтому без явного лимита один клиент выедал бы память.
///
/// Тело — это JSON-кадр целиком, а кириллица в нём может прийти
/// `\uXXXX`-экранированной: шесть байт на символ вместо двух (так делает,
/// например, `json.dumps` в Python по умолчанию). Предел берётся с четырёхкратным
/// запасом от [`crate::mcp_server::MAX_SOURCE_BYTES`]: иначе модуль, проходящий
/// проверку размера исходника, до неё бы не доехал — сервер обрывал бы соединение
/// вместо внятного отказа (наблюдалось на модуле 8,4 МиБ).
const MAX_REQUEST_BYTES: usize = crate::mcp_server::MAX_SOURCE_BYTES * 4;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Краткая статистика индекса для /health (заполнена, если индекс загружен).
    pub index_stats: Option<IndexStats>,
    /// Сервер хранит состояние индекса и актуальную карту источников имён:
    /// /health берёт снимок карты на каждый запрос, включая работу без индекса.
    server: BslContextServer,
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
    /// Причина недоступности платформенного индекса, если он не загружен.
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable_reason: Option<String>,
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

/// Собрать роутер: /health и /mcp через Streamable HTTP при любом состоянии индекса.
pub fn router(config: Config, server: BslContextServer) -> Router {
    // Список разрешённых Host для /mcp (защита rmcp от DNS-rebinding). Клонируем
    // до перемещения config в AppState; тот же список проверяем middleware'ом
    // на ВСЕХ маршрутах (/health rmcp не прикрывает).
    //
    // Пустой список rmcp трактует как «разрешить ЛЮБОЙ Host» — подстраховываемся
    // и здесь, не полагаясь только на загрузчик конфига: `router` публичный.
    let mut allowed_hosts = config.allowed_hosts.clone();
    if allowed_hosts.is_empty() {
        allowed_hosts = Config::default().allowed_hosts;
    }
    let allowed_for_guard = Arc::new(allowed_hosts.clone());
    let index_stats = server.index_loaded().then(|| IndexStats {
        global_methods: server.index.global_methods.len(),
        global_properties: server.index.global_properties.len(),
        types: server.index.types.len(),
        enum_types: server.index.enum_types_count(),
    });

    let state = AppState {
        config: Arc::new(config),
        started_at: chrono::Utc::now(),
        index_stats,
        server: server.clone(),
    };

    // Stateless Streamable HTTP — устраняет 404 Session not found при
    // рестарте сервера (см. карточку #1184 для mcp-cache-ci v0.3.0).
    let session_manager = Arc::new(NeverSessionManager::default());
    let service_factory = move || Ok(server.clone());
    let http_config = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(allowed_hosts);
    let http_service = StreamableHttpService::new(service_factory, session_manager, http_config);
    Router::new()
        .route("/health", get(health))
        .nest_service("/mcp", http_service)
        // Host проверяем на всех маршрутах: rmcp валидирует только /mcp, а
        // /health отдаёт локальные пути и легко читается через DNS-rebinding.
        .layer(middleware::from_fn_with_state(
            allowed_for_guard,
            host_guard,
        ))
        .layer(RequestBodyLimitLayer::new(MAX_REQUEST_BYTES))
        .with_state(state)
}

/// Проверка заголовка `Host` по списку `allowed_hosts` для всех маршрутов.
///
/// Отсутствующий Host пропускаем (HTTP/1.0 и не-браузерные клиенты); браузерный
/// DNS-rebinding всегда шлёт Host, и именно его мы отклоняем.
async fn host_guard(State(allowed): State<Arc<Vec<String>>>, req: Request, next: Next) -> Response {
    if !host_header_allowed(req.headers(), &allowed) {
        return (StatusCode::FORBIDDEN, "Host header is not allowed").into_response();
    }
    next.run(req).await
}

/// Пропускать ли запрос по заголовку `Host`.
///
/// Отсутствующий заголовок пропускаем (HTTP/1.0 и не-браузерные клиенты).
/// Присутствующий, но нечитаемый (не-ASCII) — отклоняем: иначе allowlist
/// обходится одним битым заголовком (аудит PR, fail-open).
fn host_header_allowed(headers: &axum::http::HeaderMap, allowed: &[String]) -> bool {
    match headers.get(axum::http::header::HOST) {
        None => true,
        Some(value) => value
            .to_str()
            .is_ok_and(|host| host_is_allowed(host, allowed)),
    }
}

/// Разрешён ли `Host`-заголовок. Запись без порта разрешает любой порт хоста;
/// поддержаны имена хостов, IPv4 и bracketed IPv6 (`[::1]:8007`).
///
/// Фильтр стоит на ВСЕХ маршрутах, включая `/health`: healthcheck по внешнему
/// адресу тоже требует записи в `allowed_hosts` (см. README, «Сетевой деплой»).
fn host_is_allowed(host_header: &str, allowed: &[String]) -> bool {
    let header_lc = host_header.to_ascii_lowercase();
    let bare = authority_host(&header_lc);
    allowed.iter().any(|entry| {
        let entry = entry.to_ascii_lowercase();
        entry == header_lc || entry == bare
    })
}

/// Хост из `Host`-заголовка без порта.
///
/// Порт — только цифры после последнего `:`. Прежний `split(':').next()` отрезал
/// всё после ПЕРВОГО двоеточия, и `127.0.0.1:8007.evil.com` выглядел как
/// разрешённый loopback `127.0.0.1`.
fn authority_host(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        // Bracketed IPv6: `[::1]:8007` или `[::1]`.
        return rest.split(']').next().unwrap_or("");
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => authority,
    }
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let now = chrono::Utc::now();
    let uptime = (now - state.started_at).num_seconds();
    let mut symbol_sources = std::collections::BTreeMap::new();
    // Снимок карты берём и сразу отпускаем std-блокировку: ниже в цикле `await`,
    // а std::sync-блокировка не должна переживать его.
    let sources = state.server.sources_snapshot();
    for (name, slot) in sources.iter() {
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
        index_loaded: state.server.index_loaded(),
        unavailable_reason: state.server.unavailable_reason().map(str::to_string),
        index_stats: state.index_stats.clone(),
        default_validation_level: state.config.default_validation_level,
        symbol_sources,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header::HOST, HeaderMap, HeaderValue};

    fn headers_with_host(value: &[u8]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_bytes(value).unwrap());
        headers
    }

    #[test]
    fn host_guard_absent_host_is_allowed() {
        assert!(host_header_allowed(
            &HeaderMap::new(),
            &["localhost".into()]
        ));
    }

    #[test]
    fn host_guard_allowed_host_passes() {
        let allowed = vec!["127.0.0.1:8007".to_string(), "localhost".to_string()];
        assert!(host_header_allowed(
            &headers_with_host(b"localhost"),
            &allowed
        ));
        assert!(host_header_allowed(
            &headers_with_host(b"127.0.0.1:8007"),
            &allowed
        ));
    }

    #[test]
    fn host_guard_foreign_host_is_rejected() {
        let allowed = vec!["localhost".to_string()];
        assert!(!host_header_allowed(
            &headers_with_host(b"evil.example"),
            &allowed
        ));
    }

    #[test]
    fn host_guard_non_ascii_host_is_rejected() {
        let allowed = vec!["localhost".to_string()];
        // 0x80 — допустимый байт HeaderValue, но не видимый ASCII: `to_str`
        // вернёт Err, и запрос обязан быть отклонён, а не пропущен.
        assert!(!host_header_allowed(
            &headers_with_host(b"\x80host"),
            &allowed
        ));
    }
}
