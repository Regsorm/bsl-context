//! Сквозные проверки потокового транспорта (`--transport stdio`).
//!
//! Два уровня:
//! - рукопожатие и диспетч в процессе, на парном канале в памяти: быстрый
//!   контракт `initialize` → `tools/list` → `tools/call` без внешнего процесса;
//! - настоящий процесс с `--transport stdio`: проверяется то, что видит
//!   MCP-клиент, — в стандартном выводе только кадры протокола (журнал не
//!   подмешивается), журнал идёт в поток ошибок, после закрытия ввода процесс
//!   завершается успешно.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use bsl_context_server::mcp_server::BslContextServer;
use platform_index::PlatformIndex;
use rmcp::ServiceExt;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Отправить один кадр (строка JSON).
async fn send(writer: &mut (impl AsyncWriteExt + Unpin), value: &Value) {
    let mut line = value.to_string();
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .await
        .expect("запись кадра");
    writer.flush().await.expect("сброс кадра");
}

/// Прочитать один кадр. Каждая прочитанная строка обязана быть JSON — так
/// проверяется, что в стандартный вывод не подмешивается журнал. Таймаут
/// превращает зависание сервера в понятный отказ вместо бесконечного ожидания.
///
/// 120 секунд, а не 30: старт процесса с реальным hbk в debug-сборке занимает
/// больше 30 секунд при параллельной нагрузке nextest (флейк, поймано аудитом),
/// а первый кадр приходит только после рукопожатия.
async fn recv(reader: &mut (impl AsyncBufReadExt + Unpin)) -> Value {
    let mut line = String::new();
    let read = tokio::time::timeout(Duration::from_secs(120), reader.read_line(&mut line))
        .await
        .expect("кадр не пришёл за 120 секунд")
        .expect("чтение кадра");
    assert!(read > 0, "поток закрылся вместо кадра");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("кадр не является JSON ({e}): {line:?}"))
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn initialized() -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
}

fn initialize_params() -> Value {
    json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "bsl-stdio-test", "version": "1"}
    })
}

/// Текст первого блока содержимого ответа инструмента и признак ошибки.
fn tool_result(response: &Value) -> (String, bool) {
    let result = response
        .get("result")
        .unwrap_or_else(|| panic!("нет result: {response}"));
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("нет текста результата: {response}"))
        .to_string();
    let is_error = result["isError"].as_bool().unwrap_or(false);
    (text, is_error)
}

#[tokio::test]
async fn in_process_session_over_duplex() {
    let (server_side, client_side) = tokio::io::duplex(1 << 20);
    let server = BslContextServer::new(PlatformIndex::new());
    let server_task = tokio::spawn(async move {
        let service = server
            .serve(server_side)
            .await
            .expect("рукопожатие в процессе");
        service.waiting().await.expect("завершение сервера")
    });

    let (read_half, mut writer) = tokio::io::split(client_side);
    let mut reader = BufReader::new(read_half);

    send(&mut writer, &request(1, "initialize", initialize_params())).await;
    let init = recv(&mut reader).await;
    assert_eq!(
        init["result"]["serverInfo"]["name"],
        json!("bsl-context-rs")
    );

    send(&mut writer, &initialized()).await;
    send(&mut writer, &request(2, "tools/list", json!({}))).await;
    let listed = recv(&mut reader).await;
    assert_eq!(
        listed["result"]["tools"]
            .as_array()
            .expect("список инструментов")
            .len(),
        14,
        "ожидался полный набор инструментов"
    );

    // Пустой индекс — это контекст «ничего не найдено», а не отказ сервера.
    send(
        &mut writer,
        &request(
            3,
            "tools/call",
            json!({"name": "info", "arguments": {"name": "ЪНетТакогоЭлемента"}}),
        ),
    )
    .await;
    let called = recv(&mut reader).await;
    let (text, is_error) = tool_result(&called);
    assert!(!is_error, "пустой индекс — не ошибка сервера: {text}");
    assert!(text.contains("Не найдено"), "текст ответа: {text}");

    // Закрытие канала клиентом завершает сервер штатно: снимаем обе половины,
    // иначе парный канал в памяти остаётся открытым и сервер ждёт ввод.
    drop(writer);
    drop(reader);
    tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("сервер завершился по закрытию канала")
        .expect("задача сервера");
}

#[tokio::test(flavor = "multi_thread")]
async fn process_stdio_keeps_stdout_clean_and_reports_missing_index() {
    let dir = tempfile::tempdir().expect("временный каталог");
    let config_path = dir.path().join("config.toml");
    // platform_path не задан: сервер обязан подняться и честно отвечать отказом
    // на инструменты справки. log_dir — временный, чтобы не трогать служебный.
    std::fs::write(
        &config_path,
        format!("log_dir = {:?}\n", dir.path().join("logs")),
    )
    .expect("запись конфига");

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_bsl-context-rs"))
        .arg("--transport")
        .arg("stdio")
        .arg("--config")
        .arg(&config_path)
        // Уровень журнала должен задавать конфиг, а не унаследованное
        // окружение: иначе проверка «журнал в stderr» зависит от RUST_LOG.
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("запуск bsl-context-rs");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");
    let mut reader = BufReader::new(stdout);
    // Поток ошибок вычитываем параллельно: переполненный пайп остановил бы
    // процесс на записи журнала, и ожидание стандартного вывода зависло бы.
    let stderr_task = tokio::spawn(async move {
        let mut all = String::new();
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            all.push_str(&line);
            all.push('\n');
        }
        all
    });

    send(&mut stdin, &request(1, "initialize", initialize_params())).await;
    let init = recv(&mut reader).await;
    assert_eq!(
        init["result"]["serverInfo"]["name"],
        json!("bsl-context-rs")
    );
    let instructions = init["result"]["instructions"].as_str().unwrap_or_default();
    assert!(
        instructions.contains("НЕ загружен"),
        "описание сервера обязано называть причину: {instructions}"
    );

    send(&mut stdin, &initialized()).await;
    send(&mut stdin, &request(2, "tools/list", json!({}))).await;
    let listed = recv(&mut reader).await;
    assert_eq!(
        listed["result"]["tools"]
            .as_array()
            .expect("список инструментов")
            .len(),
        14,
        "список инструментов доступен и без индекса"
    );

    // Справочный инструмент: результат с признаком ошибки и внятной причиной.
    send(
        &mut stdin,
        &request(
            3,
            "tools/call",
            json!({"name": "search", "arguments": {"query": "Массив"}}),
        ),
    )
    .await;
    let called = recv(&mut reader).await;
    let (text, is_error) = tool_result(&called);
    assert!(
        is_error,
        "без индекса справка обязана отвечать ошибкой: {text}"
    );
    assert!(text.contains("не загружен"), "причина в ответе: {text}");

    // Инструмент обслуживания работает и без индекса.
    send(
        &mut stdin,
        &request(
            4,
            "tools/call",
            json!({"name": "reload_config", "arguments": {}}),
        ),
    )
    .await;
    let reload = recv(&mut reader).await;
    let (text, is_error) = tool_result(&reload);
    assert!(
        !is_error,
        "reload_config обязан работать без индекса: {text}"
    );
    assert!(text.contains("\"ok\":true"), "ответ reload_config: {text}");

    // EOF стандартного ввода — штатное завершение клиентского сеанса.
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("процесс завершился по закрытию ввода")
        .expect("статус процесса");
    assert!(status.success(), "код возврата: {status:?}");

    let stderr = stderr_task.await.expect("задача чтения журнала");
    assert!(
        stderr.contains("bsl-context-rs starting"),
        "журнал обязан быть в потоке ошибок: {stderr}"
    );
}

/// Белый список и источники имён действуют и в режиме без индекса: клиент видит
/// ровно разрешённые инструменты, а служебные отвечают по фактической карте
/// источников, а не по пустой заглушке.
#[tokio::test(flavor = "multi_thread")]
async fn process_stdio_unavailable_respects_whitelist_and_sources() {
    let dir = tempfile::tempdir().expect("временный каталог");
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            "log_dir = {:?}\n\n[tools]\nenabled = [\"search\", \"symbol_sources_status\"]\n\n\
             [[symbol_sources]]\nrepo = \"demo\"\nkind = \"none\"\n",
            dir.path().join("logs")
        ),
    )
    .expect("запись конфига");

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_bsl-context-rs"))
        .arg("--transport")
        .arg("stdio")
        .arg("--config")
        .arg(&config_path)
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("запуск bsl-context-rs");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");
    let mut reader = BufReader::new(stdout);
    let stderr_task = tokio::spawn(async move {
        let mut all = String::new();
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            all.push_str(&line);
            all.push('\n');
        }
        all
    });

    send(&mut stdin, &request(1, "initialize", initialize_params())).await;
    let _ = recv(&mut reader).await;

    send(&mut stdin, &initialized()).await;
    send(&mut stdin, &request(2, "tools/list", json!({}))).await;
    let listed = recv(&mut reader).await;
    let mut actual: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("список инструментов")
        .iter()
        .map(|tool| tool["name"].as_str().expect("имя инструмента"))
        .collect();
    actual.sort_unstable();
    assert_eq!(
        actual,
        ["search", "symbol_sources_status"],
        "белый список обязан действовать и без индекса"
    );

    // Разрешённый справочный инструмент отвечает причиной недоступности.
    send(
        &mut stdin,
        &request(
            3,
            "tools/call",
            json!({"name": "search", "arguments": {"query": "Массив"}}),
        ),
    )
    .await;
    let (text, is_error) = tool_result(&recv(&mut reader).await);
    assert!(is_error, "без индекса справка отвечает ошибкой: {text}");

    // Источник из конфига обязан быть виден сразу, без перечитки.
    send(
        &mut stdin,
        &request(
            4,
            "tools/call",
            json!({"name": "symbol_sources_status", "arguments": {}}),
        ),
    )
    .await;
    let (text, is_error) = tool_result(&recv(&mut reader).await);
    assert!(!is_error, "состояние источников: {text}");
    assert!(
        text.contains("\"repo\":\"demo\""),
        "источник из конфига обязан быть в карте: {text}"
    );

    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("процесс завершился по закрытию ввода")
        .expect("статус процесса");
    assert!(status.success(), "код возврата: {status:?}");
    let _ = stderr_task.await;
}

/// Сквозная проверка на живой справке платформы — как остальные интеграционные
/// тесты, под переменной окружения. Если файла платформы нет, тест пропускается
/// с подсказкой, а не падает: CI его не знает.
#[tokio::test(flavor = "multi_thread")]
async fn process_stdio_serves_real_hbk_when_available() {
    let Some(platform_path) = std::env::var("BSL_CONTEXT_PLATFORM_PATH")
        .ok()
        .map(PathBuf::from)
    else {
        eprintln!("skip: BSL_CONTEXT_PLATFORM_PATH не задан");
        return;
    };
    let hbk_candidates = [
        platform_path.join("shcntx_ru.hbk"),
        platform_path.join("bin").join("shcntx_ru.hbk"),
    ];
    if !hbk_candidates.iter().any(|path| path.exists()) {
        eprintln!(
            "skip: shcntx_ru.hbk не найден в {}",
            platform_path.display()
        );
        return;
    }

    let dir = tempfile::tempdir().expect("временный каталог");
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!("log_dir = {:?}\n", dir.path().join("logs")),
    )
    .expect("запись конфига");

    // Путь к платформе передаётся опцией, а не файлом: проверяется и она.
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_bsl-context-rs"))
        .arg("--transport")
        .arg("stdio")
        .arg("--platform-path")
        .arg(&platform_path)
        .arg("--config")
        .arg(&config_path)
        // Уровень журнала — из конфига, а не из унаследованного окружения.
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("запуск bsl-context-rs");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");
    let mut reader = BufReader::new(stdout);
    let stderr_task = tokio::spawn(async move {
        let mut all = String::new();
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            all.push_str(&line);
            all.push('\n');
        }
        all
    });

    // Сборка индекса из hbk занимает секунды — рукопожатие ждём с запасом
    // (таймаут внутри `recv`).
    send(&mut stdin, &request(1, "initialize", initialize_params())).await;
    let init = recv(&mut reader).await;
    assert_eq!(
        init["result"]["serverInfo"]["name"],
        json!("bsl-context-rs")
    );
    let instructions = init["result"]["instructions"].as_str().unwrap_or_default();
    assert!(
        !instructions.contains("НЕ загружен"),
        "индекс обязан загрузиться: {instructions}"
    );

    send(&mut stdin, &initialized()).await;
    send(
        &mut stdin,
        &request(
            2,
            "tools/call",
            json!({"name": "search", "arguments": {"query": "Массив"}}),
        ),
    )
    .await;
    let called = recv(&mut reader).await;
    let (text, is_error) = tool_result(&called);
    assert!(!is_error, "поиск по живой справке: {text}");
    assert!(text.contains("Массив"), "текст ответа: {text}");

    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("процесс завершился по закрытию ввода")
        .expect("статус процесса");
    assert!(status.success(), "код возврата: {status:?}");
    let _ = stderr_task.await;
}
