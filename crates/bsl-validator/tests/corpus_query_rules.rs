//! Корпусный замер правил оптимальности запросов на реальной УТ.
//!
//! Модульные тесты проверяют правила на запросах, которые я написал сам, — то
//! есть ровно на тех, под которые правило и задумано. Масштаб ложных
//! срабатываний виден только на корпусе: у `config_objects` первый замер дал
//! 40398 находок при 15 зелёных модульных тестах.
//!
//! Метаданные конфигурации этим правилам не нужны, поэтому источник имён не
//! подключается, а индекс платформы берётся пустой: чужие виды находок
//! отфильтрованы по `kind`.
//!
//! ```pwsh
//! cargo test -p bsl-validator --test corpus_query_rules --release -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use bsl_validator::{validate_module_with_profile, ExprErrorKind, Profile};
use platform_index::PlatformIndex;

const CORPUS: &str = r"C:\RepoUT-test";

/// Пороги. Не «сколько допустимо ошибиться», а «выше этого — точно что-то
/// сломалось»: цифры выставлены по факту первого чистого замера.
const MAX_TEMP_TABLE: usize = 4000;
const MAX_OR_IN_JOIN: usize = 1500;
const MAX_SUBQUERY_JOIN: usize = 2000;

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

fn is_query_rule(kind: ExprErrorKind) -> bool {
    matches!(
        kind,
        ExprErrorKind::TempTableWithoutIndex
            | ExprErrorKind::OrInJoinCondition
            | ExprErrorKind::JoinWithSubquery
    )
}

#[test]
#[ignore = "требует выгрузку УТ в C:\\RepoUT-test"]
fn query_rules_on_real_ut_corpus() {
    let root = Path::new(CORPUS);
    assert!(root.is_dir(), "корпус не найден: {CORPUS}");

    let mut files = Vec::new();
    collect_bsl(root, &mut files);

    let index = PlatformIndex::new();
    let mut by_kind: HashMap<String, usize> = HashMap::new();
    let mut modules_hit = 0usize;
    // По одному примеру на вид — с ним разбираются, настоящая находка или нет.
    let mut samples: HashMap<String, String> = HashMap::new();
    // Модули с наибольшим числом находок: если правило сорвалось, оно обычно
    // срывается лавиной в одном месте.
    let mut per_module: Vec<(usize, String)> = Vec::new();

    for path in &files {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };

        let result = validate_module_with_profile(&index, &text, None, None, 1, Profile::Full);
        let found: Vec<_> = result
            .errors
            .into_iter()
            .filter(|e| is_query_rule(e.kind))
            .collect();

        if found.is_empty() {
            continue;
        }
        modules_hit += 1;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        per_module.push((found.len(), rel.clone()));

        for error in found {
            let key = format!("{:?}", error.kind);
            *by_kind.entry(key.clone()).or_default() += 1;
            samples
                .entry(key)
                .or_insert_with(|| format!("{rel}:{}:{} — {}", error.line, error.col, error.message));
        }
    }

    let total: usize = by_kind.values().sum();
    println!("── Правила запросов на корпусе УТ ──");
    println!("модулей просмотрено: {}", files.len());
    println!("модулей с находками: {modules_hit}");
    println!("находок всего:       {total}\n");

    let mut kinds: Vec<_> = by_kind.iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(a.1));
    for (kind, count) in &kinds {
        println!("{count:>6}  {kind}");
        if let Some(sample) = samples.get(*kind) {
            println!("        {sample}");
        }
    }

    per_module.sort_by(|a, b| b.0.cmp(&a.0));
    println!("\n── Модули с наибольшим числом находок ──");
    for (count, module) in per_module.iter().take(10) {
        println!("{count:>5}  {module}");
    }

    let temp_table = *by_kind.get("TempTableWithoutIndex").unwrap_or(&0);
    let or_in_join = *by_kind.get("OrInJoinCondition").unwrap_or(&0);
    let subquery = *by_kind.get("JoinWithSubquery").unwrap_or(&0);

    assert!(
        temp_table <= MAX_TEMP_TABLE,
        "временных таблиц без индекса: {temp_table} (порог {MAX_TEMP_TABLE})"
    );
    assert!(
        or_in_join <= MAX_OR_IN_JOIN,
        "ИЛИ в условии соединения: {or_in_join} (порог {MAX_OR_IN_JOIN})"
    );
    assert!(
        subquery <= MAX_SUBQUERY_JOIN,
        "соединений с подзапросом: {subquery} (порог {MAX_SUBQUERY_JOIN})"
    );
}
