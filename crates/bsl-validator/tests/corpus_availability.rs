//! Корпусный замер правила `unavailable_in_context` на реальной конфигурации
//! (этап 3 плана `TZ-dostupnost-metodov-kontekst.md`, раздел 7.4).
//!
//! Модульные тесты проверяют правило только на коде, под который оно задумано;
//! ложные срабатывания видны исключительно на корпусе. Ожидание здесь иное, чем
//! у правил объявлений: конфигурация компилируется, а платформа не компилирует
//! вызов недоступного в данном контексте метода. Значит настоящих находок в
//! выгрузке быть почти не должно, а каждая находка — либо ложное срабатывание,
//! либо дефект самого модуля (обычно модуля расширения или обхода через `#Если`).
//!
//! Пороги не «сколько допустимо ошибиться», а «выше этого правило сорвалось»:
//! они ставятся по факту первого чистого замера и ужимаются до факта + 10 %.
//!
//! ```pwsh
//! $env:BSL_CONTEXT_CORPUS_PATH = "C:\Repo1C"
//! $env:BSL_CONTEXT_PLATFORM_PATH = "C:\Program Files\1cv8\8.3.27.1786"
//! $env:TEMP = "C:\MCP-Servers\Bsl-context\.tmp"
//! $env:TMP = $env:TEMP
//! cargo test -p bsl-validator --test corpus_availability --release -- --ignored --nocapture
//! ```
//!
//! Шардинг (`BSL_CONTEXT_CORPUS_SHARD="i/n"`) и дамп (`BSL_CONTEXT_CORPUS_DUMP`)
//! — как в `corpus_type_name_noise.rs`: список обходится в детерминированном
//! порядке, объединение шардов равно полному прогону.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use bsl_validator::{validate_module_with_profile, Confidence, ExprErrorKind, Profile};
use platform_index::load_from_hbk;

const CORPUS_ENV: &str = "BSL_CONTEXT_CORPUS_PATH";

/// Сколько примеров печатать в stdout; в дамп попадают ВСЕ находки.
const SAMPLE_LIMIT: usize = 40;

/// Потолки числа находок на корпусе. Замер 2026-11-09 на выгрузке УТ
/// (14 943 модуля, `level=3`) после правки правила: high = 4, medium = 1.
/// 4 — синхронные методы `HTTPСоединение` в `&НаКлиенте` расширений
/// (по разбору — истинные: для клиента в платформе есть `*Асинх`-варианты).
/// 1 medium — реквизит формы с именем `ТаблицаЗначений`: корпусный тест не
/// читает `Form.xml`, поэтому имя-тень реквизита не отсекается; в
/// `validate_module` с `path` реквизиты формы известны, и находки нет.
/// Пороги — факт + 10 %, как требует раздел 7.4 плана: они сторожат срыв
/// правила, а не качество.
const MAX_HIGH: usize = 5;
const MAX_MEDIUM: usize = 1;

fn collect_bsl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_bsl(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("bsl") {
            out.push(path);
        }
    }
}

fn hbk_path() -> Option<PathBuf> {
    let root = std::env::var("BSL_CONTEXT_PLATFORM_PATH")
        .ok()
        .map(PathBuf::from)?;
    let candidates = [
        root.join("shcntx_ru.hbk"),
        root.join("bin").join("shcntx_ru.hbk"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// Вид модуля по пути выгрузки — та же раскладка, что у
/// `bsl_validator::module_context::module_kind`, но с отдельным счётчиком для
/// «всего остального» (общие модули, модули приложения и команд).
fn module_class(path: &str) -> &'static str {
    let low = path.replace('\\', "/").to_lowercase();
    if low.ends_with("/ext/objectmodule.bsl") {
        "ObjectModule"
    } else if low.ends_with("/ext/managermodule.bsl") {
        "ManagerModule"
    } else if low.ends_with("/ext/recordsetmodule.bsl") {
        "RecordSetModule"
    } else if low.ends_with("/ext/form/module.bsl") {
        "FormModule"
    } else {
        "Other"
    }
}

/// Вид находки по тексту сообщения: глобальный метод, метод типа, конструктор.
fn finding_class(message: &str) -> &'static str {
    if message.starts_with("Тип `") {
        "type_constructor"
    } else if message.contains(" типа `") {
        "type_method"
    } else {
        "global_method"
    }
}

fn confidence_name(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    }
}

#[test]
#[ignore = "требует выгрузку конфигурации; путь — в BSL_CONTEXT_CORPUS_PATH"]
fn availability_rule_on_real_corpus() {
    let Ok(corpus) = std::env::var(CORPUS_ENV) else {
        eprintln!("skip: не задан {CORPUS_ENV}");
        return;
    };
    let Some(hbk) = hbk_path() else {
        eprintln!("skip: hbk не найден (задайте BSL_CONTEXT_PLATFORM_PATH)");
        return;
    };
    let root = PathBuf::from(&corpus);
    assert!(root.is_dir(), "корпус не найден: {corpus}");
    let root = root.as_path();

    let index = load_from_hbk(&hbk).expect("PlatformIndex");
    let mut files = Vec::new();
    collect_bsl(root, &mut files);
    // Ограничение объёма — для быстрых сравнений «до/после» на части корпуса.
    if let Ok(limit) = std::env::var("BSL_CONTEXT_CORPUS_LIMIT") {
        if let Ok(n) = limit.parse::<usize>() {
            files.truncate(n);
        }
    }
    // Шардинг для параллельного замера: `BSL_CONTEXT_CORPUS_SHARD="i/n"` оставляет
    // каждый n-й модуль. Границы «i/n» не пересекаются, объединение равно полному
    // прогону, а время падает почти линейно.
    let shard = std::env::var("BSL_CONTEXT_CORPUS_SHARD").ok();
    if let Some((index, total)) = shard.as_deref().and_then(|s| s.split_once('/')) {
        if let (Ok(index), Ok(total)) =
            (index.trim().parse::<usize>(), total.trim().parse::<usize>())
        {
            if total > 1 && index < total {
                files = files
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| i % total == index)
                    .map(|(_, path)| path)
                    .collect();
            }
        }
    }

    let dump_path = std::env::var("BSL_CONTEXT_CORPUS_DUMP").ok();
    let mut dump: Vec<String> = Vec::new();
    let mut by_confidence: HashMap<&'static str, usize> = HashMap::new();
    let mut by_finding_class: HashMap<String, usize> = HashMap::new();
    let mut by_module_class: HashMap<String, usize> = HashMap::new();
    let mut by_module: HashMap<String, usize> = HashMap::new();
    let mut samples: Vec<String> = Vec::new();
    let mut modules = 0usize;

    println!("модулей найдено: {}", files.len());
    for (idx, path) in files.iter().enumerate() {
        if idx % 500 == 0 {
            println!("обработано {idx}/{}", files.len());
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        modules += 1;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        let result =
            validate_module_with_profile(&index, &text, Some(&rel), None, 3, Profile::Full);
        for err in result
            .errors
            .iter()
            .filter(|e| e.kind == ExprErrorKind::UnavailableInContext)
        {
            let confidence = confidence_name(err.confidence);
            *by_confidence.entry(confidence).or_default() += 1;
            let class = finding_class(&err.message);
            *by_finding_class.entry(class.to_string()).or_default() += 1;
            let module_class = module_class(&rel);
            *by_module_class.entry(module_class.to_string()).or_default() += 1;
            *by_module.entry(rel.clone()).or_default() += 1;
            dump.push(format!(
                "{rel}\t{}\t{}\t{confidence}\t{module_class}\t{class}\t{}",
                err.line, err.col, err.message
            ));
            if samples.len() < SAMPLE_LIMIT {
                samples.push(format!(
                    "{rel}:{} [{confidence}/{module_class}/{class}] {}",
                    err.line, err.message
                ));
            }
        }
    }

    if let Some(path) = dump_path {
        let mut sorted = dump;
        sorted.sort();
        if let Err(e) = fs::write(&path, sorted.join("\n")) {
            eprintln!("не удалось записать дамп {path}: {e}");
        } else {
            println!("дамп находок: {path} ({} строк)", sorted.len());
        }
    }

    let high = *by_confidence.get("high").unwrap_or(&0);
    let medium = *by_confidence.get("medium").unwrap_or(&0);
    let low = *by_confidence.get("low").unwrap_or(&0);
    let total = high + medium + low;

    println!("\n=== ЗАМЕР unavailable_in_context на {modules} модулях (level=3) ===");
    println!("ИТОГО: {total} (high: {high}, medium: {medium}, low: {low})");

    let mut classes: Vec<_> = by_finding_class.iter().collect();
    classes.sort_by(|a, b| b.1.cmp(a.1));
    println!("по видам находки:");
    for (class, count) in classes {
        println!("   {class}: {count}");
    }

    let mut module_classes: Vec<_> = by_module_class.iter().collect();
    module_classes.sort_by(|a, b| b.1.cmp(a.1));
    println!("по видам модулей:");
    for (class, count) in module_classes {
        println!("   {class}: {count}");
    }

    let mut top_modules: Vec<_> = by_module.iter().collect();
    top_modules.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    println!("топ-10 модулей по числу находок:");
    for (path, count) in top_modules.iter().take(10) {
        println!("   {count} — {path}");
    }

    println!("\n=== примеры (первые {SAMPLE_LIMIT}) ===");
    for sample in &samples {
        println!("   {sample}");
    }

    assert!(
        high <= MAX_HIGH,
        "unavailable_in_context high: {high} > {MAX_HIGH} — правило сорвалось"
    );
    assert!(
        medium <= MAX_MEDIUM,
        "unavailable_in_context medium: {medium} > {MAX_MEDIUM} — правило сорвалось"
    );
}
