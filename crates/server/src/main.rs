//! bsl-context-rs — MCP-сервер контекста платформы 1С.
//!
//! Phase 0 (bootstrap) — HTTP-сервер с /health и заглушкой /mcp, без логики.
//! Дальнейшие фазы добавляют hbk-парсер, индекс, MCP-tools.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tracing::{error, info};

use bsl_context_server::{config, http, mcp_server, pid_lock};
use bsl_validator::SymbolSource;

#[derive(Parser, Debug)]
#[command(name = "bsl-context-rs", version, about = "MCP-сервер контекста платформы 1С")]
struct Cli {
    /// Путь к config.toml. Если не указан — используются дефолты.
    #[arg(short = 'c', long = "config", value_name = "PATH")]
    config: Option<PathBuf>,
}

/// Создать внешний источник имён по конфигу (`symbol_source.kind`). Ошибка
/// создания НЕ валит сервер — предупреждение в лог, `validate_module`
/// работает без источника (как раньше).
fn build_symbol_source(cfg: &config::SymbolSourceConfig) -> Option<Arc<dyn SymbolSource>> {
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
            let repo = cfg.repo.clone()?;
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = config::Config::load_or_default(cli.config.as_deref())?;
    init_tracing(&cfg);

    let platform_path_display: String = cfg
        .platform_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<not set>".to_string());

    info!(
        version = env!("CARGO_PKG_VERSION"),
        port = cfg.port,
        platform_path = %platform_path_display,
        log_dir = %cfg.log_dir.display(),
        "bsl-context-rs starting"
    );

    // Singleton-защита (см. ~/.claude/rules/service-build-checklist.md, пункт 7).
    // Берётся ДО загрузки индекса, чтобы второй экземпляр не тратил 5 секунд cold-start
    // и не конкурировал за RAM. Lock автоматически снимается через Drop при выходе.
    let _pid_lock = match pid_lock::PidLock::acquire(&cfg.log_dir) {
        Ok(lock) => lock,
        Err(e) => {
            error!(error = %e, "не удалось захватить PID-lock");
            // stderr полезен, потому что супервизор фиксирует stderr-вывод в stderr.log
            eprintln!("ERROR: {e}");
            return Err(e);
        }
    };

    if cfg.platform_path.is_none() {
        tracing::warn!(
            "platform_path не задан в конфиге. /health стартует, но MCP-инструменты \
             будут отвечать 503: индекс не загружен. На многоплатформенных машинах \
             укажите каталог нужной версии 1С явно — например \
             'C:\\Program Files\\1cv8\\8.3.27.1786'. Автодетектора нет специально."
        );
    }

    // Внешний источник имён (см. крейт `symbol-source`) — строится один раз,
    // независимо от индекса платформы; ошибка создания не мешает старту.
    let symbol_source = build_symbol_source(&cfg.symbol_source);

    // Загрузка индекса (Phase 4): если platform_path задан — eager build перед стартом
    // HTTP. Парсинг hbk на 8.3.27 занимает ~5–7 сек, поэтому делаем синхронно через
    // spawn_blocking, чтобы не блокировать tokio worker.
    let mcp = if let Some(platform_path) = cfg.platform_path.clone() {
        let hbk_candidates = [
            platform_path.join("shcntx_ru.hbk"),
            platform_path.join("bin").join("shcntx_ru.hbk"),
        ];
        match hbk_candidates.into_iter().find(|p| p.exists()) {
            Some(hbk) => {
                info!(?hbk, "загрузка платформенного индекса");
                let index = tokio::task::spawn_blocking(move || {
                    platform_index::load_from_hbk(&hbk)
                })
                .await
                .map_err(|e| anyhow::anyhow!("задача загрузки индекса упала: {e}"))??;
                info!(
                    types = index.types.len(),
                    enum_types = index.enum_types_count(),
                    global_methods = index.global_methods.len(),
                    global_properties = index.global_properties.len(),
                    "PlatformIndex загружен"
                );
                Some(
                    mcp_server::BslContextServer::with_defaults(
                        index,
                        cfg.default_validation_level,
                        cfg.default_profile,
                    )
                    .with_symbol_source(symbol_source.clone())
                    .with_symbol_source_config(cfg.symbol_source.clone())
                    .apply_tools_whitelist(&cfg.tools.enabled),
                )
            }
            None => {
                tracing::warn!(
                    %platform_path_display,
                    "не найден shcntx_ru.hbk в platform_path и его подкаталоге bin/. MCP-инструменты будут отдавать 503."
                );
                None
            }
        }
    } else {
        None
    };

    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port).parse()?;
    let app = http::router(cfg.clone(), mcp);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "listening");

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        error!(error = %e, "server stopped with error");
        return Err(e.into());
    }
    info!("graceful shutdown complete");
    Ok(())
}

/// Инициализация tracing: stdout + ежедневная ротация в log_dir.
fn init_tracing(cfg: &config::Config) {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    // Каталог логов гарантированно существует — run.bat создаёт его до запуска
    // бинарника, но на всякий случай проверим и создадим программно.
    if let Err(e) = std::fs::create_dir_all(&cfg.log_dir) {
        eprintln!(
            "warning: cannot create log dir {}: {}",
            cfg.log_dir.display(),
            e
        );
    }

    let file_appender = tracing_appender::rolling::daily(&cfg.log_dir, "service.log");
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&cfg.log_level));

    let subscriber = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().with_writer(std::io::stdout))
        .with(fmt::layer().with_writer(file_appender).with_ansi(false));

    if subscriber.try_init().is_err() {
        eprintln!("warning: tracing already initialized");
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!(error = %e, "failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut s) = signal(SignalKind::terminate()) {
            s.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("shutdown signal received");
}
