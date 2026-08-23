//! Корпусный замер правил по объявлениям и структуре модуля на реальной
//! конфигурации.
//!
//! Модульные тесты проверяют правило ровно на том коде, под который оно
//! задумано, — ложные срабатывания там не видны в принципе. Масштаб виден
//! только на корпусе: у `config_objects` первый замер дал 40398 находок при 15
//! зелёных модульных тестах.
//!
//! Ожидание для ЭТИХ правил особое: типовая конфигурация компилируется, значит
//! настоящих находок в ней быть не должно вовсе. Любая находка здесь — либо
//! ложное срабатывание, либо реальный дефект расширения. Поэтому пороги низкие
//! и заданы «сколько угодно, но не лавина».
//!
//! Индекс платформы берётся пустой: тогда правило «имя совпало с глобальной
//! функцией» молчит, и замер показывает чистый вклад разбора текста. Полный
//! индекс проверяется отдельно, в `real_declarations`.
//!
//! ```pwsh
//! $env:BSL_CONTEXT_CORPUS_PATH = "C:\Repo1C"
//! cargo test -p bsl-validator --test corpus_declarations --release -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use bsl_validator::{validate_module_with_profile, ExprErrorKind, Profile};
use platform_index::PlatformIndex;

/// Каталог с выгрузкой конфигурации 1С. Путь у каждого свой, поэтому берётся
/// из окружения; без него тест пропускается.
const CORPUS_ENV: &str = "BSL_CONTEXT_CORPUS_PATH";

/// Пороги. Не «сколько допустимо ошибиться», а «выше этого — точно что-то
/// сломалось»: выставлены по факту чистого замера 23.08.2026, где на 14905
/// модулях осталась ровно ОДНА находка — настоящая (дубль
/// `ОбработкаПолученияПолейПредставления` в модуле менеджера «Номенклатура»).
///
/// Путь к этому замеру стоит помнить, потому что каждый промежуточный вариант
/// проходил модульные тесты и валился на корпусе:
/// 853 (`Асинх Процедура` не считалась заголовком) → 197 (BOM в начале файла)
/// → 12 (защищённые модули в одну строку) → 1317 (пословный разбор принял
/// свойство `Обработчик.Процедура` за заголовок) → 1 (неразрывный пробел в
/// отступе) → 0.
const MAX_RESERVED: usize = 2;
const MAX_DUPLICATE: usize = 5;
const MAX_UNBALANCED: usize = 5;

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

fn is_declaration_rule(kind: ExprErrorKind) -> bool {
    matches!(
        kind,
        ExprErrorKind::ReservedProcedureName
            | ExprErrorKind::DuplicateDeclaration
            | ExprErrorKind::UnbalancedModuleBlock
    )
}

#[test]
#[ignore = "требует выгрузку конфигурации; путь — в BSL_CONTEXT_CORPUS_PATH"]
fn declaration_rules_on_real_corpus() {
    let Ok(corpus) = std::env::var(CORPUS_ENV) else {
        eprintln!("skip: не задан {CORPUS_ENV}");
        return;
    };
    let root = PathBuf::from(&corpus);
    assert!(root.is_dir(), "корпус не найден: {corpus}");
    let root = root.as_path();

    let mut files = Vec::new();
    collect_bsl(root, &mut files);

    let index = PlatformIndex::new();
    let mut by_kind: HashMap<String, usize> = HashMap::new();
    let mut samples: HashMap<String, Vec<String>> = HashMap::new();

    println!("модулей найдено: {}", files.len());

    for (idx, path) in files.iter().enumerate() {
        // Прогон идёт минутами: без счётчика снаружи не отличить работу от зависания.
        if idx % 500 == 0 {
            println!("обработано {idx}/{}", files.len());
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let result = validate_module_with_profile(&index, &text, Some(&rel), None, 1, Profile::Full);
        for err in result.errors.iter().filter(|e| is_declaration_rule(e.kind)) {
            let kind = format!("{:?}", err.kind);
            *by_kind.entry(kind.clone()).or_default() += 1;
            let bucket = samples.entry(kind).or_default();
            if bucket.len() < 5 {
                bucket.push(format!("{rel}:{} — {}", err.line, err.message));
            }
        }
    }

    println!("модулей в корпусе: {}", files.len());
    for (kind, count) in &by_kind {
        println!("\n{kind}: {count}");
        for sample in samples.get(kind).into_iter().flatten() {
            println!("   {sample}");
        }
    }

    let reserved = *by_kind.get("ReservedProcedureName").unwrap_or(&0);
    let duplicate = *by_kind.get("DuplicateDeclaration").unwrap_or(&0);
    let unbalanced = *by_kind.get("UnbalancedModuleBlock").unwrap_or(&0);

    assert!(
        reserved <= MAX_RESERVED,
        "ReservedProcedureName: {reserved} > {MAX_RESERVED} — правило сорвалось"
    );
    assert!(
        duplicate <= MAX_DUPLICATE,
        "DuplicateDeclaration: {duplicate} > {MAX_DUPLICATE} — правило сорвалось"
    );
    assert!(
        unbalanced <= MAX_UNBALANCED,
        "UnbalancedModuleBlock: {unbalanced} > {MAX_UNBALANCED} — правило сорвалось"
    );
}
