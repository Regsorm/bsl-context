//! Регресс проверки объектов конфигурации на РЕАЛЬНОМ корпусе.
//!
//! Код рабочей конфигурации заведомо корректен — он в проде. Значит любая
//! находка `UnknownCommonModule`/`UnknownMetadataObject` на нём есть либо
//! настоящий дефект (такие встречаются: `глЗначениеПеременной` из УТ 10.3
//! пережил переход на УТ 11), либо ложное срабатывание. Разделить их может
//! только человек, поэтому тест ничего не утверждает про «ноль находок» — он
//! печатает КАЖДУЮ находку с путём и строкой, чтобы её можно было проверить
//! глазами, и падает при превышении порога.
//!
//! Порог намеренно грубый: проверка сверяет имена со списком объектов реальной
//! конфигурации, поэтому массовая находка означает не «в УТ много ошибок», а
//! дефект гейтов молчания — ровно то, на чём проект уже обжигался (наивная
//! проверка имён контекста дала 412 находок при единицах настоящих).
//!
//! `#[ignore]`: нужен корпус и справка платформы, в обычном прогоне пропускается.
//!
//! ```pwsh
//! $env:BSL_CONTEXT_PLATFORM_PATH = 'C:\Program Files\1cv8\8.3.27.1786'
//! cargo test -p bsl-context-server --test corpus_config_objects --release -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bsl_validator::{validate_module_with_symbols, ExprErrorKind, Profile};
use platform_index::load_from_hbk;
use symbol_source::LiteSource;

const CORPUS: &str = r"C:\RepoUT-test";
const LITE_DB: &str = r"C:\Temp\ut_lite_v2.db";

/// Потолок общего числа находок. Замер 2026-07-16: 219 на 14905 модулях, и это
/// НЕ ложные срабатывания — проверено по индексу конфигурации: `Справочники.
/// СтатусыДокументов`, `Документы.Поступление`, `Перечисления.ТипыНалогов` и
/// подобные в УТ действительно отсутствуют. Почти все — в модулях обмена
/// EnterpriseData/EDI и внешних обработках, написанных под семейство
/// конфигураций: в УТ этот код мёртв, но обращения в нём реальны.
///
/// Потолок сторожит не «ноль находок», а ВЗРЫВ: до починки гейтов замер давал
/// 40398 (переменные циклов и методы менеджеров).
const MAX_FINDINGS: usize = 250;

/// Отдельный, жёсткий потолок для находок по общим модулям. Это самое
/// FP-опасное правило: головой обращения может оказаться что угодно — реквизит
/// объекта, переменная цикла, экспортная переменная модуля приложения. Замер:
/// 4 находки, все настоящие (`Б_ОбщиеПроцедурыИФункцииСервер` и ещё два модуля
/// зовутся из кода, но в конфигурации их нет). Рост здесь = сломанный гейт.
const MAX_COMMON_MODULE_FINDINGS: usize = 10;

fn hbk_path() -> Option<PathBuf> {
    let root = std::env::var("BSL_CONTEXT_PLATFORM_PATH")
        .ok()
        .map(PathBuf::from)?;
    let candidates = [root.join("shcntx_ru.hbk"), root.join("bin").join("shcntx_ru.hbk")];
    candidates.into_iter().find(|p| p.exists())
}

#[test]
#[ignore]
fn config_objects_on_real_ut_corpus() {
    let Some(hbk) = hbk_path() else {
        eprintln!("skip: BSL_CONTEXT_PLATFORM_PATH не задан");
        return;
    };
    let corpus = Path::new(CORPUS);
    if !corpus.exists() {
        eprintln!("skip: корпуса {CORPUS} нет");
        return;
    }
    if !Path::new(LITE_DB).exists() {
        eprintln!("skip: базы {LITE_DB} нет — соберите bsl-lite-index build");
        return;
    }

    let index = load_from_hbk(&hbk).expect("не удалось прочитать справку платформы");
    let source = LiteSource::open(Path::new(LITE_DB)).expect("не удалось открыть lite-индекс");

    let files: Vec<PathBuf> = walkdir::WalkDir::new(corpus)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("bsl"))
        })
        .map(|e| e.path().to_path_buf())
        .collect();
    assert!(!files.is_empty(), "корпус пуст");

    let mut findings: Vec<String> = Vec::new();
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut all_kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut checked = 0usize;

    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        // Путь модуля обязателен: именно по нему различаются общий модуль
        // (проверка работает) и модуль объекта (проверка молчит).
        let rel = file
            .strip_prefix(corpus)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        checked += 1;

        // Состав реквизитов форм в корпусном прогоне неизвестен — как и у
        // потребителя, который его не передал. Модули форм при этом молчат
        // целиком: это и есть проверяемое поведение.
        let result = validate_module_with_symbols(
            &index,
            &text,
            3,
            Profile::Full,
            Some(&rel),
            None,
            Some(&source),
        );

        for err in &result.errors {
            // Считаем ВСЕ виды: правки гейтов задевают и соседние проверки
            // (например, `is_form_module` общий с `ShadowedContextName`), а
            // заметить это можно только сравнив полный расклад до и после.
            *all_kinds.entry(format!("{:?}", err.kind)).or_default() += 1;

            if !matches!(
                err.kind,
                ExprErrorKind::UnknownCommonModule | ExprErrorKind::UnknownMetadataObject
            ) {
                continue;
            }
            *by_kind.entry(format!("{:?}", err.kind)).or_default() += 1;
            findings.push(format!("{rel}:{} {:?} — {}", err.line, err.kind, err.message));
        }
    }

    println!("\n=== Регресс объектов конфигурации на {CORPUS} ===");
    println!("Проверено модулей: {checked}");
    println!("Находок всего: {}", findings.len());
    for (kind, count) in &by_kind {
        println!("  {kind}: {count}");
    }
    println!("\n--- ВСЕ виды находок валидатора (контроль побочных изменений) ---");
    for (kind, count) in &all_kinds {
        println!("  {kind}: {count}");
    }
    println!("\n--- Каждая находка (проверить глазами: настоящий дефект или ложная) ---");
    for finding in &findings {
        println!("{finding}");
    }

    let common_module_findings = by_kind
        .get("UnknownCommonModule")
        .copied()
        .unwrap_or_default();
    assert!(
        common_module_findings <= MAX_COMMON_MODULE_FINDINGS,
        "находок по общим модулям {common_module_findings} при пороге \
         {MAX_COMMON_MODULE_FINDINGS} — сломан гейт молчания, смотрите список выше"
    );
    assert!(
        findings.len() <= MAX_FINDINGS,
        "находок {} при пороге {MAX_FINDINGS} — гейты молчания сломаны, смотрите список выше",
        findings.len()
    );
}
