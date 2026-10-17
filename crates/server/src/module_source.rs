//! Определение модуля по пути к файлу (issue #13).
//!
//! До этого `validate_module` принимал только текст: агент, знающий путь к модулю
//! (типовой менеджер документа — 120 КБ), вынужден был целиком прогонять его через
//! свой контекст, чтобы проверить одну правку. Параметр `path` даёт серверу
//! прочитать файл самому. Взамен — жёсткое ограничение: файл обязан лежать ВНУТРИ
//! корня выгрузки конфигурации (поле `root` источника имён), иначе инструмент
//! читал бы произвольные файлы машины.
//!
//! Тут же решается вторая половина issue: строка-путь, переданная в `source`,
//! разбиралась как BSL и давала `valid: true` без единой проверки — агент ошибался
//! молча. Такая строка распознаётся ([`looks_like_path`]) и превращается во внятный
//! отказ.
//!
//! Отпечаток прочитанного — размер и время изменения, а не хеш: этого достаточно,
//! чтобы вызывающий увидел, какая версия файла проверена (так же устроена проверка
//! годности кэша платформенного индекса), и не требует новой зависимости.

use std::path::{Path, PathBuf};

/// Потолок размера файла модуля. Модули типовой конфигурации — сотни килобайт;
/// 16 МиБ отсекают случайное чтение постороннего файла при ошибке в пути.
pub const MAX_MODULE_BYTES: u64 = 16 * 1024 * 1024;

/// Прочитанный модуль: текст без BOM, путь внутри выгрузки и его отпечаток.
#[derive(Debug)]
pub struct ModuleFile {
    /// Текст модуля (UTF-8, BOM снят).
    pub text: String,
    /// Полный путь к прочитанному файлу.
    pub path: PathBuf,
    /// Путь относительно корня выгрузки, с прямыми слэшами — он же `module_path`
    /// для валидатора (по нему распознаётся модуль формы и объектный контекст).
    pub module_path: String,
    /// Размер файла в байтах.
    pub bytes: u64,
    /// Время изменения, RFC3339 UTC; `None` — файловая система не отдала его.
    pub modified: Option<String>,
}

/// Строка похожа на путь к файлу, а не на текст BSL?
///
/// Признаки (все обязательны, чтобы не отказывать законному коду):
/// - одна строка, без переводов строки;
/// - либо оканчивается на `.bsl`, либо начинается как абсолютный путь Windows
///   (`C:\`, `\\сервер\`) — обе формы в BSL невозможны;
/// - есть разделитель каталогов;
/// - нет признаков кода: `;`, кавычек, `=`, комментария `//`.
///
/// Разделителя мало: `Структура.bsl` — законное обращение к свойству, а `А/Б` —
/// деление, поэтому в одиночку они путём не считаются. Многострочный текст путём
/// не считается тоже: в модуле путь встречается в комментарии или строке.
pub fn looks_like_path(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.contains('\n') || t.contains('\r') {
        return false;
    }
    let lower = t.to_lowercase();
    let absolute_windows = lower.starts_with("\\\\")
        || (lower.len() > 2
            && lower.as_bytes()[1] == b':'
            && matches!(lower.as_bytes()[2], b'\\' | b'/')
            && lower.as_bytes()[0].is_ascii_alphabetic());
    if !lower.ends_with(".bsl") && !absolute_windows {
        return false;
    }
    if !t.contains('\\') && !t.contains('/') {
        return false;
    }
    !t.contains(';')
        && !t.contains('"')
        && !t.contains('\'')
        && !t.contains('=')
        && !t.contains("//")
}

/// Текст отказа, когда в `source` пришёл путь, а не модуль.
pub fn path_in_source_message(text: &str) -> String {
    let shown: String = text.trim().chars().take(120).collect();
    format!(
        "В source передан ПУТЬ к файлу, а не текст модуля: \"{shown}\". Так проверять нельзя — \
         строка-путь разбирается как BSL и даёт valid: true без единой проверки. Передайте текст \
         модуля в source либо путь в параметре path (файл должен лежать внутри корня выгрузки \
         конфигурации — поле root источника имён)."
    )
}

/// Прочитать модуль по пути внутри корня выгрузки конфигурации.
///
/// Обёртка над [`read_module_in`] для одного корня: так вызывают тесты и код,
/// которому дополнительные корни не нужны.
pub fn read_module(root: &Path, raw: &str) -> Result<ModuleFile, String> {
    let roots = [root.to_path_buf()];
    read_module_in(&roots, raw)
}

/// Прочитать модуль по пути внутри ОДНОГО из корней выгрузки (issue #45).
///
/// `raw` — абсолютный путь или путь относительно корня. Корни перебираются по
/// порядку: сначала основной (`root`), затем дополнительные (`extra_roots`).
/// Файл обязан после разрешения символических ссылок и junction'ов остаться
/// внутри ТОГО корня, из которого он прочитан: иначе инструмент стал бы средством
/// чтения любых файлов машины. `module_path` считается от этого же корня, поэтому
/// раскладка выгрузки в нём сохраняется и эвристики вида модуля продолжают
/// работать.
pub fn read_module_in(roots: &[PathBuf], raw: &str) -> Result<ModuleFile, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("параметр path пуст".to_string());
    }
    if roots.is_empty() {
        return Err(
            "у источника имён не задан корень выгрузки (root): читать модуль \
                    по пути не из чего"
                .to_string(),
        );
    }

    let candidate = Path::new(raw);
    let mut not_found: Vec<String> = Vec::new();
    for root in roots {
        let root_canon = root
            .canonicalize()
            .map_err(|e| format!("корень выгрузки {} недоступен: {e}", root.display()))?;
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            root_canon.join(candidate)
        };
        let Ok(file) = joined.canonicalize() else {
            not_found.push(joined.display().to_string());
            continue;
        };
        if !file.starts_with(&root_canon) {
            // Файл есть, но вне ЭТОГО корня: возможно, он в дополнительном.
            continue;
        }
        return finish_module(&file, &root_canon);
    }

    // Ни один корень не подошёл: файл либо вне всех корней, либо его нет.
    if candidate.is_absolute() && candidate.exists() {
        return Err(match roots {
            [only] => format!(
                "путь выходит за корень выгрузки {}: {}",
                only.display(),
                candidate.display()
            ),
            _ => format!(
                "путь выходит за корни выгрузки ({}): {}",
                roots
                    .iter()
                    .map(|root| root.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                candidate.display()
            ),
        });
    }
    Err(format!("файл не найден: {}", not_found.join(" или ")))
}

/// Дочитать найденный файл: проверки вида и размера, текст без BOM, `module_path`
/// от корня, внутри которого файл лежит.
fn finish_module(file: &Path, root_canon: &Path) -> Result<ModuleFile, String> {
    let meta = std::fs::metadata(file).map_err(|e| {
        format!(
            "не удалось прочитать свойства файла {}: {e}",
            file.display()
        )
    })?;
    if !meta.is_file() {
        return Err(format!("это не файл: {}", file.display()));
    }
    let is_bsl = file
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase() == "bsl")
        .unwrap_or(false);
    if !is_bsl {
        return Err(format!(
            "проверять можно только модули .bsl, а не {}",
            file.display()
        ));
    }
    if meta.len() > MAX_MODULE_BYTES {
        return Err(format!(
            "файл больше {} МиБ: {}",
            MAX_MODULE_BYTES / (1024 * 1024),
            file.display()
        ));
    }

    let bytes = std::fs::read(file)
        .map_err(|e| format!("не удалось прочитать файл {}: {e}", file.display()))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("файл не читается как UTF-8: {}", file.display()))?;
    // BOM формата выгрузки 1С снимаем: он не часть текста модуля и мешает разбору.
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_string();

    let module_path = file
        .strip_prefix(root_canon)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/");

    Ok(ModuleFile {
        text,
        path: file.to_path_buf(),
        module_path,
        bytes: meta.len(),
        modified: meta.modified().ok().map(|t| {
            let dt: chrono::DateTime<chrono::Utc> = t.into();
            dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_path_in_source_is_detected() {
        // Путь из issue #13 и его варианты.
        for case in [
            r"b:\projects\x\src\cf\CommonModules\НетТакогоМодуля\Ext\Module.bsl",
            r"C:\Repo1C\base\CommonModules\Х\Ext\Module.bsl",
            r"\\server\share\Repo1C\base\CommonModules\Х\Ext\Module.bsl",
            "base/CommonModules/Х/Ext/Module.bsl",
            "  base/CommonModules/Х/Ext/Module.bsl  ",
        ] {
            assert!(
                looks_like_path(case),
                "должно распознаться как путь: {case}"
            );
        }
    }

    #[test]
    fn clean_one_line_snippets_are_not_paths() {
        // Законный однострочный код путём не считается.
        for case in [
            "Х = 1;",
            "А/Б",
            "Структура.bsl",                     // обращение к свойству
            "Возврат \"C:/Repo1C/Module.bsl\";", // путь внутри строкового литерала
            "// C:\\Repo1C\\Module.bsl",         // путь в комментарии
            "ЗагрузитьФайл(\"base/Module.bsl\");",
        ] {
            assert!(!looks_like_path(case), "не должно считаться путём: {case}");
        }
    }

    #[test]
    fn multiline_module_is_not_a_path_even_with_path_inside() {
        let src = "// см. C:\\Repo1C\\base\\Модуль.bsl\nПроцедура Т()\nКонецПроцедуры\n";
        assert!(!looks_like_path(src));
    }

    #[test]
    fn read_module_returns_text_without_bom_and_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let nested = root
            .join("base")
            .join("CommonModules")
            .join("Х")
            .join("Ext");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("Module.bsl");
        std::fs::write(&file, "\u{feff}Процедура Т()\nКонецПроцедуры\n").unwrap();

        let module = read_module(root, "base/CommonModules/Х/Ext/Module.bsl").unwrap();
        assert_eq!(module.text, "Процедура Т()\nКонецПроцедуры\n");
        assert_eq!(module.module_path, "base/CommonModules/Х/Ext/Module.bsl");
        assert!(module.bytes > 0);
        assert!(module.modified.is_some());
    }

    #[test]
    fn absolute_path_inside_root_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Module.bsl");
        std::fs::write(&file, "Процедура Т()\nКонецПроцедуры\n").unwrap();
        let module = read_module(dir.path(), &file.to_string_lossy()).unwrap();
        assert_eq!(module.module_path, "Module.bsl");
    }

    #[test]
    fn path_outside_root_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let alien = outside.path().join("Чужой.bsl");
        std::fs::write(&alien, "Процедура Т()\nКонецПроцедуры\n").unwrap();

        let err = read_module(root.path(), &alien.to_string_lossy()).unwrap_err();
        assert!(err.contains("выходит за корень"), "{err}");
        assert!(read_module(root.path(), "../outside.bsl").is_err());
    }

    #[test]
    fn missing_file_and_wrong_extension_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_module(dir.path(), "НетТакого.bsl").unwrap_err();
        assert!(err.contains("файл не найден"), "{err}");

        let txt = dir.path().join("Заметки.txt");
        std::fs::write(&txt, "не модуль").unwrap();
        let err = read_module(dir.path(), "Заметки.txt").unwrap_err();
        assert!(err.contains("только модули .bsl"), "{err}");

        let dir_as_file = dir.path().join("Папка.bsl");
        std::fs::create_dir_all(&dir_as_file).unwrap();
        let err = read_module(dir.path(), "Папка.bsl").unwrap_err();
        assert!(err.contains("это не файл"), "{err}");
    }

    #[test]
    fn missing_root_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_module(&dir.path().join("нет-такого-корня"), "Module.bsl").unwrap_err();
        assert!(err.contains("корень выгрузки"), "{err}");
    }

    /// Issue #45: модуль внешней обработки лежит ВНЕ корня выгрузки конфигурации.
    #[test]
    fn extra_root_is_searched_after_the_main_one() {
        let main = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let nested = extra
            .path()
            .join("Form")
            .join("ВнешняяОбработка")
            .join("Ext")
            .join("Form");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("Module.bsl"), "Процедура Т()\nКонецПроцедуры\n").unwrap();

        let roots = vec![main.path().to_path_buf(), extra.path().to_path_buf()];
        let module = read_module_in(&roots, "Form/ВнешняяОбработка/Ext/Form/Module.bsl").unwrap();
        // `module_path` считается от того корня, где файл нашёлся: раскладка
        // выгрузки в нём сохраняется, и эвристики вида модуля работают.
        assert_eq!(
            module.module_path,
            "Form/ВнешняяОбработка/Ext/Form/Module.bsl"
        );
        assert!(module.text.contains("Процедура Т"));

        // Основной корень ищется первым: файл с тем же относительным путём
        // берётся из него, а не из дополнительного.
        let main_file = main
            .path()
            .join("Form")
            .join("ВнешняяОбработка")
            .join("Ext")
            .join("Form");
        std::fs::create_dir_all(&main_file).unwrap();
        std::fs::write(main_file.join("Module.bsl"), "// из основного корня\n").unwrap();
        let module = read_module_in(&roots, "Form/ВнешняяОбработка/Ext/Form/Module.bsl").unwrap();
        assert!(module.text.contains("основного корня"));

        // Абсолютный путь вне ВСЕХ корней по-прежнему отвергается.
        let alien = tempfile::tempdir().unwrap();
        let alien_file = alien.path().join("Чужой.bsl");
        std::fs::write(&alien_file, "Процедура Т()\nКонецПроцедуры\n").unwrap();
        let err = read_module_in(&roots, &alien_file.to_string_lossy()).unwrap_err();
        assert!(err.contains("выходит за корни"), "{err}");

        // Без единого корня читать нечего — понятный отказ, а не паника.
        let err = read_module_in(&[], "Module.bsl").unwrap_err();
        assert!(err.contains("не задан корень"), "{err}");
    }
}
