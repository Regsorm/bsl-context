//! bsl-context-rs — MCP-сервер контекста платформы 1С.
//!
//! Phase 0 (bootstrap) — HTTP-сервер с /health и заглушкой /mcp, без логики.
//! Дальнейшие фазы добавляют hbk-парсер, индекс, MCP-tools.

use clap::{Parser, ValueEnum};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use tracing::{error, info};

use anyhow::Context as _;
use std::future::IntoFuture as _;

use bsl_context_server::sources::build_symbol_source;
use bsl_context_server::{config, http, mcp_server, pid_lock};

/// Предел ожидания graceful shutdown: после сигнала даём in-flight запросам
/// столько секунд, затем выходим принудительно (иначе долгий rebuild,
/// держащий source-лок, не давал бы остановиться).
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Транспорт MCP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Transport {
    /// Сетевая служба: Streamable HTTP на host:port (поведение по умолчанию).
    Http,
    /// Обмен по стандартным потокам ввода-вывода: процесс запускает сам
    /// MCP-клиент, вход и выход — кадры протокола, журнал идёт в поток ошибок.
    Stdio,
}

#[derive(Parser, Debug)]
#[command(
    name = "bsl-context-rs",
    version,
    about = "MCP-сервер контекста платформы 1С"
)]
struct Cli {
    /// Путь к config.toml. Если не указан — используются дефолты.
    #[arg(short = 'c', long = "config", value_name = "PATH")]
    config: Option<PathBuf>,

    /// Транспорт: http (по умолчанию) или stdio.
    #[arg(long = "transport", value_enum, default_value_t = Transport::Http)]
    transport: Transport,

    /// Каталог установки 1С — переопределяет platform_path из config.toml.
    /// Позволяет запустить stdio-режим без файла настройки.
    #[arg(long = "platform-path", value_name = "PATH")]
    platform_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut cfg = config::Config::load_or_default(cli.config.as_deref())?;
    // Опция командной строки приоритетнее файла: применяем до загрузки индекса и
    // сборки сетевого узла, чтобы /health и журнал показывали действующее
    // значение, а не файловое.
    if let Some(platform_path) = cli.platform_path.clone() {
        cfg.platform_path = Some(platform_path);
    }
    init_tracing(&cfg, cli.transport);

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
    //
    // Только для сетевого режима: у службы один bind и один общий холодный старт.
    // В stdio процессов ровно столько, сколько сеансов у клиента, — файл-замок
    // запретил бы второй сеанс, ничего не защищая.
    let pid_lock = if cli.transport == Transport::Http {
        match pid_lock::PidLock::acquire(&cfg.log_dir) {
            Ok(lock) => Some(lock),
            Err(e) => {
                error!(error = %e, "не удалось захватить PID-lock");
                // stderr полезен, потому что супервизор фиксирует stderr-вывод в stderr.log
                eprintln!("ERROR: {e}");
                return Err(e);
            }
        }
    } else {
        None
    };

    if cfg.platform_path.is_none() {
        tracing::warn!(
            "platform_path не задан в конфиге. Сервер стартует, но инструменты \
             справки будут отвечать отказом в обоих режимах, служебные работают: \
             индекс не загружен. На многоплатформенных машинах \
             укажите каталог нужной версии 1С явно — например \
             'C:\\Program Files\\1cv8\\8.3.27.1786'. Автодетектора нет специально."
        );
    }

    // Источники имён конфигураций (по одному на конфигурацию, у каждого свой способ
    // доступа). Ошибка создания конкретного источника не валит сервер: предупреждение
    // в лог, валидация по этой конфигурации пойдёт без знания её имён.
    //
    // Сборка идёт в spawn_blocking: code_index_mcp делает синхронный сетевой I/O
    // (initialize), и недоступный code-index иначе заблокировал бы tokio-воркер
    // на timeout_ms ещё до подъёма транспорта и /health.
    let resolved_sources = cfg.resolved_symbol_sources()?;
    let source_slots = tokio::task::spawn_blocking(move || {
        resolved_sources
            .into_iter()
            .map(|(name, sc)| {
                let built = build_symbol_source(&sc);
                if let Err(msg) = &built {
                    // Причину надо и в журнал, и в слот: инструмент
                    // symbol_sources_status отдаёт её вызывающему, не заставляя
                    // читать логи сервера.
                    error!(source = %name, error = %msg, "источник имён конфигурации не подключён");
                }
                (name, sc, built)
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| anyhow::anyhow!("задача сборки источников имён упала: {e}"))?;
    info!(
        sources = ?source_slots.iter().map(|(n, _, _)| n.as_str()).collect::<Vec<_>>(),
        "конфигурации, доступные параметру repo"
    );

    // Загрузка индекса (Phase 4): если platform_path задан — eager build перед стартом
    // транспорта. Сборка из hbk занимает секунды (замер на 8.3.27 после распараллеливания
    // разбора — ~1,3 с), поэтому делаем её синхронно через spawn_blocking, чтобы не
    // блокировать tokio worker. Готовый индекс пишется в дисковый кэш (см.
    // `platform_cache_path`): повторный старт читает кэш вместо разбора (~0,09 с).
    //
    // Индекс не собрался из-за отсутствия пути или файла — это не повод не
    // стартовать: в обоих режимах справочные инструменты отвечают отказом,
    // служебные работают (см. `unavailable`).
    let mut server = match load_platform_index(&cfg, &platform_path_display).await? {
        Ok(index) => mcp_server::BslContextServer::with_defaults(
            index,
            cfg.default_validation_level,
            cfg.default_profile,
        ),
        Err(reason) => {
            tracing::warn!(%reason, "справка платформы недоступна");
            mcp_server::BslContextServer::unavailable(
                reason,
                cfg.default_validation_level,
                cfg.default_profile,
            )
        }
    }
    .with_sources(source_slots)
    .apply_tools_whitelist(&cfg.tools.enabled)
    .with_cli_platform_path(cli.platform_path.is_some());
    // Путь нужен инструменту reload_config: без него перечитывать
    // config.toml нечего, и вызов честно отвечает отказом.
    if let Some(path) = cli.config.clone() {
        server = server.with_config_path(path);
    }

    match cli.transport {
        Transport::Http => {
            // Учитываем и имена хостов, и IPv6-литералы: формат "host:port"
            // парсился только для IPv4, и `localhost`/`::1` (оба есть в дефолтных
            // allowed_hosts) роняли старт «invalid socket address syntax».
            let addr = http_addr(&cfg)?;
            let app = http::router(cfg.clone(), server);

            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("не удалось забиндиться на {addr}"))?;
            info!(%addr, "listening");

            // Graceful shutdown с пределом по времени: in-flight запрос может
            // ждать source-лок, который держит долгий rebuild.
            let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
            let serve = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    shutdown_signal().await;
                    let _ = signal_tx.send(());
                })
                .into_future();
            tokio::pin!(serve);
            let grace = async {
                let _ = signal_rx.await;
                tokio::time::sleep(SHUTDOWN_GRACE).await;
            };
            let exit_code = tokio::select! {
                result = &mut serve => {
                    if let Err(e) = result {
                        error!(error = %e, "server stopped with error");
                        1
                    } else {
                        info!("graceful shutdown complete");
                        0
                    }
                }
                _ = grace => {
                    tracing::warn!(
                        "graceful shutdown не завершился за {} с — завершаю процесс",
                        SHUTDOWN_GRACE.as_secs()
                    );
                    0
                }
            };
            shutdown_now(pid_lock, exit_code)
        }
        Transport::Stdio => {
            serve_stdio(server).await?;
            shutdown_now(pid_lock, 0)
        }
    }
}

/// Завершить процесс, сняв PID-замок, минуя drop tokio-рантайма.
///
/// Обычный `return` из `main` не годится: `#[tokio::main]` затем роняет
/// рантайм, а его drop ждёт задачи `spawn_blocking` НЕОГРАНИЧЕННО — пересборка
/// индекса идёт минутами, и процесс висел бы после «graceful shutdown
/// complete». Всё это время PID-файл был бы уже снят (`PidLock::drop` случается
/// раньше drop'а рантайма), открывая окно второму экземпляру (аудит PR).
/// Освобождаем замок явно и выходим из процесса.
fn shutdown_now(lock: Option<pid_lock::PidLock>, code: i32) -> ! {
    drop(lock);
    std::process::exit(code)
}

/// Адрес HTTP-сервера из конфига: имя хоста или IP (включая IPv6).
///
/// Ошибка разрешения — фатальная: бессмысленно грузить индекс, если сервер
/// всё равно не сможет забиндиться.
fn http_addr(cfg: &config::Config) -> anyhow::Result<SocketAddr> {
    (cfg.host.as_str(), cfg.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("host='{}' не разрешился ни в один адрес", cfg.host))
}

async fn load_platform_index(
    cfg: &config::Config,
    platform_path_display: &str,
) -> anyhow::Result<Result<platform_index::PlatformIndex, String>> {
    if let Some(platform_path) = cfg.platform_path.clone() {
        let hbk_candidates = [
            platform_path.join("shcntx_ru.hbk"),
            platform_path.join("bin").join("shcntx_ru.hbk"),
        ];
        match hbk_candidates.into_iter().find(|p| p.exists()) {
            Some(hbk) => {
                info!(?hbk, "загрузка платформенного индекса");
                let cache_path = cfg.platform_cache_path_effective();
                let hbk_for_load = hbk.clone();
                let (loaded, hbk) = tokio::task::spawn_blocking(move || {
                    let result = match cache_path {
                        Some(cache_path) => platform_index::load_cached(&hbk_for_load, &cache_path),
                        None => platform_index::load_from_hbk(&hbk_for_load)
                            .map(|index| (index, platform_index::LoadSource::Hbk)),
                    };
                    (result, hbk_for_load)
                })
                .await
                .map_err(|e| anyhow::anyhow!("задача загрузки индекса упала: {e}"))?;
                let (index, loaded_from) = match loaded {
                    Ok(loaded) => loaded,
                    // Битый или частично скопированный hbk — проблема данных, а не
                    // повод не стартовать: политика сервера (см. ниже) — служебные
                    // инструменты и /health работают, справочные отвечают отказом.
                    Err(e) => {
                        let reason = format!("не удалось прочитать '{}': {e}", hbk.display());
                        tracing::warn!(
                            error = %e,
                            hbk = ?hbk,
                            "платформенный индекс не собран — сервер стартует без него"
                        );
                        return Ok(Err(reason));
                    }
                };
                info!(
                    loaded_from = ?loaded_from,
                    types = index.types.len(),
                    enum_types = index.enum_types_count(),
                    global_methods = index.global_methods.len(),
                    global_properties = index.global_properties.len(),
                    "PlatformIndex загружен"
                );
                Ok(Ok(index))
            }
            None => {
                let reason = format!(
                    "в каталоге '{platform_path_display}' и его подкаталоге bin/ не найден \
                     shcntx_ru.hbk — проверьте platform_path"
                );
                tracing::warn!(
                    %platform_path_display,
                    "не найден shcntx_ru.hbk в platform_path и его подкаталоге bin/. \
                     Инструменты справки будут отвечать отказом."
                );
                Ok(Err(reason))
            }
        }
    } else {
        Ok(Err(
            "platform_path не задан в config.toml (и не передан --platform-path): \
             укажите каталог установки 1С нужной версии, например \
             'C:\\Program Files\\1cv8\\8.3.27.2342'"
                .to_string(),
        ))
    }
}

/// Потоковый режим: кадры MCP через стандартные ввод и вывод.
///
/// Завершение штатное по EOF стандартного ввода (клиент закрыл процесс).
/// Закрытие ввода ещё до рукопожатия — тоже штатный случай: клиент передумал.
async fn serve_stdio(server: mcp_server::BslContextServer) -> anyhow::Result<()> {
    use rmcp::service::{QuitReason, ServerInitializeError};
    use rmcp::ServiceExt;

    let service = match server.serve(rmcp::transport::io::stdio()).await {
        Ok(service) => service,
        Err(ServerInitializeError::ConnectionClosed(_)) => {
            info!("stdio: входной поток закрыт до рукопожатия — завершение");
            return Ok(());
        }
        Err(e) => return Err(anyhow::anyhow!("stdio: рукопожатие не состоялось: {e}")),
    };

    match service.waiting().await {
        Ok(QuitReason::Closed | QuitReason::Cancelled) => {
            info!("stdio: входной поток закрыт — завершение");
            Ok(())
        }
        Ok(QuitReason::JoinError(e)) => Err(anyhow::anyhow!("stdio: служба упала: {e}")),
        // QuitReason помечен non_exhaustive: любое другое нормальное
        // завершение тоже успех.
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("stdio: служба упала: {e}")),
    }
}

/// Инициализация tracing: консоль + ежедневная ротация в log_dir.
///
/// В сетевом режиме консоль идёт в stdout (как было), в потоковом — в stderr:
/// stdout занят кадрами протокола, и одна строка журнала сломала бы сеанс.
/// Файловый слой — best-effort: недоступный каталог журналов (частый случай у
/// процесса, запущенного клиентом без прав на `C:\bsl-context-rs\logs`) не
/// должен ронять сервер до рукопожатия.
fn init_tracing(cfg: &config::Config, transport: Transport) {
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

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.log_level));

    // Цвета — только для сетевого режима: поток ошибок принимает пайп клиента,
    // а не терминал.
    let console = fmt::layer()
        .with_ansi(transport == Transport::Http)
        .with_writer(move || -> Box<dyn std::io::Write + Send> {
            match transport {
                Transport::Http => Box::new(std::io::stdout()),
                Transport::Stdio => Box::new(std::io::stderr()),
            }
        });

    let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("service")
        .filename_suffix("log")
        .build(&cfg.log_dir);

    let init_result = match file_appender {
        Ok(file_appender) => tracing_subscriber::registry()
            .with(env_filter)
            .with(console)
            .with(fmt::layer().with_writer(file_appender).with_ansi(false))
            .try_init(),
        Err(e) => {
            eprintln!(
                "warning: файловый журнал в {} недоступен ({e}) — вывод только в {}",
                cfg.log_dir.display(),
                match transport {
                    Transport::Http => "stdout",
                    Transport::Stdio => "stderr",
                }
            );
            tracing_subscriber::registry()
                .with(env_filter)
                .with(console)
                .try_init()
        }
    };

    if init_result.is_err() {
        eprintln!("warning: tracing already initialized");
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!(error = %e, "failed to install Ctrl+C handler");
            // Ошибка установки обработчика НЕ должна выглядеть как сигнал:
            // иначе select! немедленно запустит graceful shutdown на старте.
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                error!(error = %e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    // Windows: Ctrl+Break и закрытие консоли. Без своих обработчиков эти события
    // обрабатывает обработчик по умолчанию и просто убивает процесс
    // (`STATUS_CONTROL_C_EXIT` = 0xC000013A): graceful-путь недостижим вовсе, а
    // PID-файл остаётся — `TerminateProcess` минует `Drop`, и следующий старт
    // видит «живой» замок от мёртвого процесса. `Ctrl+C` ловится выше;
    // `Ctrl+Break` — стандартный способ послать сигнал отдельной группе процессов
    // (`GenerateConsoleCtrlEvent`), им же останавливают службу руками.
    #[cfg(windows)]
    let console_events = async {
        use tokio::signal::windows;
        let (mut brk, mut close) = match (windows::ctrl_break(), windows::ctrl_close()) {
            (Ok(brk), Ok(close)) => (brk, close),
            (brk, close) => {
                if let Err(e) = brk {
                    error!(error = %e, "failed to install Ctrl+Break handler");
                }
                if let Err(e) = close {
                    error!(error = %e, "failed to install console-close handler");
                }
                std::future::pending::<()>().await;
                return;
            }
        };
        tokio::select! {
            _ = brk.recv() => {},
            _ = close.recv() => {},
        }
    };

    #[cfg(not(windows))]
    let console_events = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
        _ = console_events => {},
    }

    info!("shutdown signal received");
}
