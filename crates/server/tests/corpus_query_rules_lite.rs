//! Корпусный замер правил оптимальности запросов С МЕТАДАННЫМИ конфигурации.
//!
//! Правила про физическую таблицу регистра, отбор виртуальной таблицы и
//! неиндексированное поле опираются на состав объекта, поэтому меряются только
//! с поднятым источником. Без него они молчат — это проверяется модульно.
//!
//! Индекс должен быть схемы 3 (в нём есть `object_fields`):
//!
//! ```pwsh
//! cargo run -p lite-index --release --bin bsl-lite-index -- build --root C:\RepoUT-test --db C:\Temp\ut_lite_v3.db
//! $env:BSL_CONTEXT_PLATFORM_PATH = 'C:\Program Files\1cv8\8.3.27.1786'
//! cargo test -p bsl-context-server --test corpus_query_rules_lite --release -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use bsl_validator::{validate_module_with_symbols, ExprErrorKind, Profile};
use platform_index::PlatformIndex;
use symbol_source::LiteSource;

const CORPUS: &str = r"C:\RepoUT-test";
const LITE_DB: &str = r"C:\Temp\ut_lite_v3.db";

/// Пороги: не «допустимая ошибка», а «выше этого — правило сорвалось».
/// Замер 2026-07-20 на УТ: 34 / 495 / 380. Порог — примерно вдвое выше факта,
/// чтобы обычные правки конфигурации его не пробивали, а сорванный гейт — сразу.
///
/// Для сравнения, чего стоят гейты: без учёта того, что индекс нужен
/// ПРИСОЕДИНЯЕМОЙ таблице, соединений набиралось 16360; без гейта по служебным
/// полям и реквизитам регистра физических чтений было 1624.
const MAX_PHYSICAL_REGISTER: usize = 100;
const MAX_VIRTUAL_WITHOUT_FILTER: usize = 1000;
const MAX_UNINDEXED_JOIN: usize = 800;

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
            | ExprErrorKind::PhysicalRegisterTable
            | ExprErrorKind::VirtualTableWithoutFilter
            | ExprErrorKind::JoinOnUnindexedField
    )
}

#[test]
#[ignore = "требует выгрузку УТ и lite-индекс схемы 3"]
fn query_rules_with_metadata_on_real_ut_corpus() {
    let root = Path::new(CORPUS);
    assert!(root.is_dir(), "корпус не найден: {CORPUS}");
    let db = Path::new(LITE_DB);
    assert!(db.is_file(), "нет lite-индекса {LITE_DB} — соберите его схемой 3");

    let source = LiteSource::open(db).expect("lite-индекс не открылся");
    let index = PlatformIndex::new();

    let mut files = Vec::new();
    collect_bsl(root, &mut files);

    let mut by_kind: HashMap<String, usize> = HashMap::new();
    let mut samples: HashMap<String, String> = HashMap::new();
    let mut modules_hit = 0usize;
    // Пофайловый эталон: по нему сверяется, что живой MCP-слой отдаёт ровно то
    // же, что и прямой вызов библиотеки. Формат — построчный JSON, чтобы читать
    // его без зависимостей.
    let mut baseline = String::new();

    for path in &files {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let result = validate_module_with_symbols(
            &index,
            &text,
            1,
            Profile::Full,
            None,
            None,
            Some(&source),
        );
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
        let mut per_file: HashMap<String, usize> = HashMap::new();
        for error in found {
            let key = format!("{:?}", error.kind);
            *by_kind.entry(key.clone()).or_default() += 1;
            *per_file.entry(key.clone()).or_default() += 1;
            samples.entry(key).or_insert_with(|| {
                format!("{rel}:{}:{} — {}", error.line, error.col, error.message)
            });
        }

        let counts: Vec<String> = {
            let mut pairs: Vec<_> = per_file.iter().collect();
            pairs.sort();
            pairs
                .into_iter()
                .map(|(kind, count)| format!("\"{kind}\":{count}"))
                .collect()
        };
        baseline.push_str(&format!(
            "{{\"file\":{},\"kinds\":{{{}}}}}\n",
            serde_json::to_string(&rel).unwrap_or_else(|_| "\"?\"".to_string()),
            counts.join(",")
        ));
    }

    let baseline_path = r"C:\Temp\qr_baseline.jsonl";
    if let Err(e) = fs::write(baseline_path, &baseline) {
        println!("не удалось записать эталон {baseline_path}: {e}");
    } else {
        println!("эталон записан: {baseline_path}");
    }

    let total: usize = by_kind.values().sum();
    println!("── Правила запросов С МЕТАДАННЫМИ на корпусе УТ ──");
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

    let count = |kind: &str| *by_kind.get(kind).unwrap_or(&0);
    assert!(
        count("PhysicalRegisterTable") <= MAX_PHYSICAL_REGISTER,
        "физических таблиц регистров: {} (порог {MAX_PHYSICAL_REGISTER})",
        count("PhysicalRegisterTable")
    );
    assert!(
        count("VirtualTableWithoutFilter") <= MAX_VIRTUAL_WITHOUT_FILTER,
        "виртуальных таблиц без отбора: {} (порог {MAX_VIRTUAL_WITHOUT_FILTER})",
        count("VirtualTableWithoutFilter")
    );
    assert!(
        count("JoinOnUnindexedField") <= MAX_UNINDEXED_JOIN,
        "соединений по неиндексированному полю: {} (порог {MAX_UNINDEXED_JOIN})",
        count("JoinOnUnindexedField")
    );
}
