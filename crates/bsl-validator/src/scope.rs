//! Phase 8 MVP — локальный type inference в пределах одной процедуры.
//!
//! Собирает scope `Map<имя_переменной_lower, ТипX>` из трёх источников:
//!
//! 1. `Х = Новый ТипX` или `Х = Новый ТипX(args)` → переменная Х имеет тип `ТипX`.
//! 2. `Х = ТипX.ЗначениеY` (где `ТипX` есть в `PlatformIndex.types` как enum)
//!    → переменная Х имеет тип `ТипX` (не значение перечисления, а сам тип).
//! 3. `// @type ТипX` на строке непосредственно перед присваиванием
//!    `Х = <выражение>` (или в той же строке) → переменная Х получает тип `ТипX`.
//!    Аннотация переопределяет автоматический вывод.
//! 4. **(Уровень 2.5, только `level >= 3`)** return-type tracking: `Х = obj.Метод()`
//!    или цепочка `Х = Запрос.Выполнить().Выбрать()` — тип `Х` выводится из
//!    возвращаемого типа метода (`Method.return_type`) или типа свойства
//!    (`Property.type_name`), с итеративным резолвом по звеньям цепочки.
//!    Также `Х = ГлобальныйМетод()` → return-type глобального метода.
//!    Приоритет ниже аннотации `// @type` (ручная подсказка побеждает).
//!
//! Не покрывает (это Уровень 3 / не обязательная цель):
//! - Inter-procedural type inference (вывод типа параметра процедуры по местам вызова).
//! - Типизированные коллекции (`Массив` чего, `Соответствие` ключ-значение).
//! - Реквизиты справочников/документов через метаданные конфигурации — это
//!   под-фаза B Уровня 2.5, реализуется в server-слое (не здесь).
//!
//! Сегментация на процедуры — простой regex по `Процедура`/`Функция` ...
//! `КонецПроцедуры`/`КонецФункции`. В BSL вложенных процедур нет — поэтому
//! линейного сканирования достаточно.

use std::collections::HashMap;

use regex::Regex;
use std::sync::OnceLock;

use platform_index::PlatformIndex;

/// Один scope — набор `имя_переменной_lower → ТипX` в пределах одной процедуры
/// (или модуля, если процедур нет).
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// Включающий байтовый диапазон `[start..end)`.
    pub byte_start: usize,
    pub byte_end: usize,
    pub vars: HashMap<String, String>,
}

impl Scope {
    pub fn contains(&self, byte_idx: usize) -> bool {
        byte_idx >= self.byte_start && byte_idx < self.byte_end
    }
}

/// Контейнер: scope-ы по процедурам в порядке появления в исходнике.
#[derive(Debug, Clone, Default)]
pub struct ScopeMap {
    pub scopes: Vec<Scope>,
}

impl ScopeMap {
    /// Найти scope, охватывающий данный байтовый offset.
    pub fn lookup(&self, byte_idx: usize) -> Option<&Scope> {
        self.scopes.iter().find(|s| s.contains(byte_idx))
    }

    /// Получить тип переменной по имени, регистронезависимо.
    pub fn type_of_var(&self, byte_idx: usize, var_name: &str) -> Option<&String> {
        let scope = self.lookup(byte_idx)?;
        scope.vars.get(&var_name.to_lowercase())
    }
}

fn proc_block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // (?is) — case-insensitive + dot matches newline.
        Regex::new(
            r"(?is)(?P<head>(?:Процедура|Функция)\s+\w+\s*\([^)]*\))(?P<body>.*?)(?P<tail>КонецПроцедуры|КонецФункции)",
        )
        .unwrap()
    })
}

fn assign_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Имя на левой стороне присваивания. Ловим "Идентификатор = ..." с возможным
        // префиксом из пробелов в начале строки. Lookbehind в regex crate нет,
        // поэтому используем (?m:^) и проверяем границы вручную.
        Regex::new(r"(?m:^)\s*(?P<lhs>[A-Za-zА-Яа-яЁё_][A-Za-zА-Яа-яЁё_0-9]*)\s*=\s*(?P<rhs>[^;\n]*)")
            .unwrap()
    })
}

fn new_rhs_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^\s*(?:Новый|New)\s+(?P<ty>[A-Za-zА-Яа-яЁё_][A-Za-zА-Яа-яЁё_0-9]*)").unwrap()
    })
}

fn enum_rhs_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^\s*(?P<ty>[A-Za-zА-Яа-яЁё_][A-Za-zА-Яа-яЁё_0-9]*)\.(?P<member>[A-Za-zА-Яа-яЁё_][A-Za-zА-Яа-яЁё_0-9]*)\s*$",
        )
        .unwrap()
    })
}

fn type_annot_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `// @type ТипX` или `// @type: ТипX`
        Regex::new(r"//\s*@type:?\s+(?P<ty>[A-Za-zА-Яа-яЁё_][A-Za-zА-Яа-яЁё_0-9]*)").unwrap()
    })
}

/// Извлечь scope из исходника. На вход — уже очищенный от строк/комментариев
/// текст (но аннотации `// @type` извлекаются ДО очистки и подаются отдельно).
pub fn extract_scope_map(
    index: &PlatformIndex,
    cleaned: &str,
    annotations: &HashMap<usize, String>,
    level: u8,
) -> ScopeMap {
    let mut scopes = Vec::new();
    let blocks: Vec<(usize, usize)> = proc_block_re()
        .find_iter(cleaned)
        .map(|m| (m.start(), m.end()))
        .collect();

    if blocks.is_empty() {
        // Глобальный scope на весь файл.
        let scope = build_scope(index, cleaned, 0, cleaned.len(), annotations, level);
        scopes.push(scope);
    } else {
        for (start, end) in blocks {
            let body = &cleaned[start..end];
            let scope = build_scope(index, body, start, end, annotations, level);
            scopes.push(scope);
        }
    }

    ScopeMap { scopes }
}

/// Извлечь все аннотации `// @type ТипX` из ИСХОДНОГО (не очищенного) текста.
/// Возвращает `byte_offset_следующей_строки → ТипX`. Аннотация применяется к
/// первому присваиванию на строке annotation_line+1 или дальше (до пустой строки).
pub fn extract_type_annotations(src: &str) -> HashMap<usize, String> {
    let mut out = HashMap::new();
    for cap in type_annot_re().captures_iter(src) {
        let ty = cap.name("ty").unwrap().as_str().to_string();
        let m = cap.get(0).unwrap();
        // Найти конец строки, где аннотация
        let end_of_line = src[m.end()..]
            .find('\n')
            .map(|i| m.end() + i + 1)
            .unwrap_or(src.len());
        out.insert(end_of_line, ty);
    }
    out
}

fn build_scope(
    index: &PlatformIndex,
    body: &str,
    byte_start: usize,
    byte_end: usize,
    annotations: &HashMap<usize, String>,
    level: u8,
) -> Scope {
    let mut vars: HashMap<String, String> = HashMap::new();

    for cap in assign_re().captures_iter(body) {
        let lhs_match = cap.name("lhs").unwrap();
        let rhs_match = cap.name("rhs").unwrap();
        let lhs = lhs_match.as_str().to_string();
        let rhs = rhs_match.as_str().trim();

        let abs_start = byte_start + lhs_match.start();

        // 1. Проверить аннотацию: ищем последнюю аннотацию, чей end_of_line <= abs_start
        // и абсолютная разница не больше ~200 байт (примерно 4 строки).
        let mut typ: Option<String> = None;
        for (&annot_end, annot_ty) in annotations {
            if annot_end <= abs_start && abs_start.saturating_sub(annot_end) <= 200 {
                // Используем последнюю
                if typ.is_none() {
                    typ = Some(annot_ty.clone());
                }
            }
        }

        // 2. Если аннотации нет — пробуем извлечь из RHS.
        if typ.is_none() {
            if let Some(c) = new_rhs_re().captures(rhs) {
                let ty = c.name("ty").unwrap().as_str();
                if index.find_type(ty).is_some() {
                    typ = Some(ty.to_string());
                }
            }
        }
        if typ.is_none() {
            if let Some(c) = enum_rhs_re().captures(rhs) {
                let ty = c.name("ty").unwrap().as_str();
                if let Some(t) = index.find_type(ty) {
                    if t.is_enum() {
                        typ = Some(ty.to_string());
                    }
                }
            }
        }

        // 4. return-type tracking (Уровень 2.5, только level>=3). Резолвим
        // только когда RHS — чистая цепочка вызовов/обращений к членам:
        // либо длиной >=2 звена (`obj.Метод()`), либо одиночный вызов
        // глобального метода (`ГлобальныйМетод()`). Опирается на vars,
        // уже собранные предыдущими присваиваниями (однопроходный порядок).
        if typ.is_none() && level >= 3 {
            if let Some(segs) = parse_chain(rhs) {
                if segs.len() >= 2 || (segs.len() == 1 && segs[0].is_call) {
                    typ = resolve_chain_type(index, &vars, &segs);
                }
            }
        }

        if let Some(t) = typ {
            vars.insert(lhs.to_lowercase(), t);
        }
    }

    Scope {
        byte_start,
        byte_end,
        vars,
    }
}

/// Одно звено цепочки обращений: `Имя` или `Имя(...)`.
#[derive(Debug, Clone)]
struct ChainSeg {
    name: String,
    is_call: bool,
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Разобрать RHS в цепочку звеньев `head.member1(...).member2...`.
///
/// Возвращает `None`, если RHS — не чистая цепочка (есть бинарный оператор,
/// литерал, незакрытые скобки, или вообще не начинается с идентификатора).
/// Аргументы вызовов пропускаются целиком (баланс скобок), их содержимое
/// не анализируется (в `cleaned` строки уже замаскированы пробелами).
fn parse_chain(rhs: &str) -> Option<Vec<ChainSeg>> {
    let chars: Vec<char> = rhs.trim().chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut segs: Vec<ChainSeg> = Vec::new();

    loop {
        // Имя звена. Идентификатор BSL начинается с буквы или `_`, не с цифры.
        let start = i;
        if i >= n || !(chars[i].is_alphabetic() || chars[i] == '_') {
            return None;
        }
        while i < n && is_ident_char(chars[i]) {
            i += 1;
        }
        let name: String = chars[start..i].iter().collect();

        // Пропустить пробелы перед возможной скобкой вызова.
        while i < n && chars[i] == ' ' {
            i += 1;
        }

        let mut is_call = false;
        if i < n && chars[i] == '(' {
            is_call = true;
            let mut depth = 0i32;
            while i < n {
                match chars[i] {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            if depth != 0 {
                return None; // несбалансированные скобки
            }
        }

        segs.push(ChainSeg { name, is_call });

        // Пропустить пробелы.
        while i < n && chars[i] == ' ' {
            i += 1;
        }

        if i < n && chars[i] == '.' {
            // Следующее звено.
            i += 1;
            while i < n && chars[i] == ' ' {
                i += 1;
            }
            continue;
        }
        break;
    }

    // RHS должна быть исчерпана цепочкой целиком — иначе это сложное выражение
    // (`a.b() + c`, `a.b() = Истина` и т.п.), тип которого выводить ненадёжно.
    if i != n {
        return None;
    }
    if segs.is_empty() {
        None
    } else {
        Some(segs)
    }
}

/// Из (возможно составного) описания типа выбрать первый реально существующий
/// в индексе компонент. В hbk возвращаемый тип нередко составной — например
/// `Запрос.Выполнить()` даёт `"РезультатЗапроса, Неопределено"` — и каждый
/// компонент может быть обёрнут в backtick'и (`to_markdown` сохраняет их из
/// `<code>`-тегов hbk: `` `РезультатЗапроса`, `Неопределено` ``). Поэтому
/// каждый компонент чистим от не-идентификаторных символов по краям (backtick,
/// кавычки, пробелы). Берём первый известный тип, пропуская служебные
/// `Неопределено`/`Произвольный`. `None` — если ни один компонент не является
/// известным типом (резолв цепочки дальше не идёт, ошибка не порождается).
fn primary_type(index: &PlatformIndex, raw: &str) -> Option<String> {
    for part in raw.split([',', ';', '|']) {
        let p = part.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        if p.is_empty()
            || p.eq_ignore_ascii_case("Произвольный")
            || p.eq_ignore_ascii_case("Неопределено")
        {
            continue;
        }
        if index.find_type(p).is_some() {
            return Some(p.to_string());
        }
    }
    None
}

/// Вычислить тип значения цепочки `head.member1(...).member2...`.
///
/// `head` резолвится: из `vars` (локальная переменная с выведенным типом),
/// либо как вызов глобального метода (`ГлобальныйМетод()` → return_type),
/// либо как голое имя платформенного типа. Дальше по каждому звену: метод →
/// `return_type`, свойство → `type_name`. Любой неизвестный шаг (член не найден,
/// тип пустой/«Произвольный») → `None` (тип не выводим, ошибку не порождаем).
fn resolve_chain_type(
    index: &PlatformIndex,
    vars: &HashMap<String, String>,
    segs: &[ChainSeg],
) -> Option<String> {
    let head = &segs[0];
    let mut cur = if let Some(t) = vars.get(&head.name.to_lowercase()) {
        t.clone()
    } else if head.is_call {
        let m = index.find_global_method(&head.name)?;
        primary_type(index, &m.return_type)?
    } else if index.find_type(&head.name).is_some() {
        head.name.clone()
    } else {
        return None;
    };

    for seg in &segs[1..] {
        let ty = index.find_type(&cur)?;
        let key = seg.name.to_lowercase();
        let next = if let Some(m) = ty
            .methods
            .iter()
            .find(|m| m.name_ru.to_lowercase() == key || m.name_en.to_lowercase() == key)
        {
            m.return_type.clone()
        } else if let Some(p) = ty
            .properties
            .iter()
            .find(|p| p.name_ru.to_lowercase() == key || p.name_en.to_lowercase() == key)
        {
            p.type_name.clone()
        } else {
            return None; // член не найден — тип не выводим
        };
        cur = primary_type(index, &next)?;
    }

    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_index::{Method, Property, Type};

    fn assert_var(map: &ScopeMap, byte_idx: usize, var: &str, expected_type: &str) {
        let t = map
            .type_of_var(byte_idx, var)
            .unwrap_or_else(|| panic!("var '{var}' not found in scope"));
        assert_eq!(t, expected_type);
    }

    fn method(name: &str, return_type: &str) -> Method {
        Method {
            name_ru: name.to_string(),
            name_en: String::new(),
            description: String::new(),
            return_type: return_type.to_string(),
            signatures: Vec::new(),
        }
    }

    fn property(name: &str, type_name: &str) -> Property {
        Property {
            name_ru: name.to_string(),
            name_en: String::new(),
            description: String::new(),
            type_name: type_name.to_string(),
            readonly: false,
        }
    }

    fn ty(name: &str, methods: Vec<Method>, properties: Vec<Property>) -> Type {
        Type {
            name_ru: name.to_string(),
            name_en: String::new(),
            description: String::new(),
            methods,
            properties,
            constructors: Vec::new(),
            enum_values: Vec::new(),
        }
    }

    /// Мини-индекс: Запрос.Выполнить()→РезультатЗапроса, .Выбрать()→ВыборкаИзРезультатаЗапроса,
    /// у выборки есть метод Следующий()→Булево и свойство Текст→Строка у Запроса.
    fn mock_index() -> PlatformIndex {
        let mut idx = PlatformIndex::new();
        idx.insert_type(ty(
            "Запрос",
            // Составной return_type как в реальном hbk — primary_type должен
            // выбрать первый известный компонент (РезультатЗапроса).
            vec![method("Выполнить", "РезультатЗапроса, Неопределено")],
            vec![property("Текст", "Строка")],
        ));
        idx.insert_type(ty(
            "РезультатЗапроса",
            vec![method("Выбрать", "ВыборкаИзРезультатаЗапроса")],
            vec![],
        ));
        idx.insert_type(ty(
            "ВыборкаИзРезультатаЗапроса",
            vec![method("Следующий", "Булево")],
            vec![],
        ));
        idx.insert_type(ty("Строка", vec![], vec![]));
        idx.insert_type(ty("Булево", vec![], vec![]));
        idx.global_methods
            .push(method("ПолучитьОбщийМакет", "ТабличныйДокумент"));
        idx.insert_type(ty("ТабличныйДокумент", vec![], vec![]));
        idx
    }

    #[test]
    fn extract_annotations_finds_type_directive() {
        let src = "// @type ТаблицаЗначений\nХ = СоздатьТЗ();\n";
        let annot = extract_type_annotations(src);
        assert_eq!(annot.values().next(), Some(&"ТаблицаЗначений".to_string()));
    }

    #[test]
    fn parse_chain_basic() {
        let segs = parse_chain("Запрос.Выполнить()").unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].name, "Запрос");
        assert!(!segs[0].is_call);
        assert_eq!(segs[1].name, "Выполнить");
        assert!(segs[1].is_call);
    }

    #[test]
    fn parse_chain_long() {
        let segs = parse_chain("Запрос.Выполнить().Выбрать()").unwrap();
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[2].name, "Выбрать");
    }

    #[test]
    fn parse_chain_rejects_complex_expr() {
        // Бинарное выражение — не чистая цепочка.
        assert!(parse_chain("Х.Метод() + 1").is_none());
        // "Новый Тип" — не цепочка (остаток после идентификатора).
        assert!(parse_chain("Новый Запрос").is_none());
        // Литерал.
        assert!(parse_chain("123").is_none());
    }

    #[test]
    fn parse_chain_skips_call_args() {
        // Точки и скобки внутри аргументов не ломают разбор.
        let segs = parse_chain("Спр.НайтиПоКоду(А.Б(\"x\"))").unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[1].name, "НайтиПоКоду");
        assert!(segs[1].is_call);
    }

    #[test]
    fn resolve_chain_method_return_type() {
        let idx = mock_index();
        let vars = HashMap::new();
        let segs = parse_chain("Запрос.Выполнить()").unwrap();
        assert_eq!(
            resolve_chain_type(&idx, &vars, &segs),
            Some("РезультатЗапроса".to_string())
        );
    }

    #[test]
    fn resolve_chain_multi_level() {
        let idx = mock_index();
        let vars = HashMap::new();
        let segs = parse_chain("Запрос.Выполнить().Выбрать()").unwrap();
        assert_eq!(
            resolve_chain_type(&idx, &vars, &segs),
            Some("ВыборкаИзРезультатаЗапроса".to_string())
        );
    }

    #[test]
    fn resolve_chain_via_var_and_property() {
        let idx = mock_index();
        let mut vars = HashMap::new();
        vars.insert("рез".to_string(), "РезультатЗапроса".to_string());
        // голова — переменная с выведенным типом
        let segs = parse_chain("Рез.Выбрать()").unwrap();
        assert_eq!(
            resolve_chain_type(&idx, &vars, &segs),
            Some("ВыборкаИзРезультатаЗапроса".to_string())
        );
        // свойство → type_name
        let segs2 = parse_chain("Запрос.Текст").unwrap();
        assert_eq!(
            resolve_chain_type(&idx, &vars, &segs2),
            Some("Строка".to_string())
        );
    }

    #[test]
    fn primary_type_picks_first_known_component() {
        let idx = mock_index();
        assert_eq!(
            primary_type(&idx, "РезультатЗапроса, Неопределено"),
            Some("РезультатЗапроса".to_string())
        );
        // служебные/неизвестные компоненты пропускаются
        assert_eq!(primary_type(&idx, "Неопределено"), None);
        assert_eq!(primary_type(&idx, "НетТакогоТипа"), None);
        // первый известный после неизвестного
        assert_eq!(
            primary_type(&idx, "Произвольный, Строка"),
            Some("Строка".to_string())
        );
        // backtick'и из hbk (`to_markdown` оборачивает <code>) должны очищаться
        assert_eq!(
            primary_type(&idx, "`РезультатЗапроса`, `Неопределено`"),
            Some("РезультатЗапроса".to_string())
        );
    }

    #[test]
    fn resolve_chain_global_method() {
        let idx = mock_index();
        let vars = HashMap::new();
        let segs = parse_chain("ПолучитьОбщийМакет()").unwrap();
        assert_eq!(
            resolve_chain_type(&idx, &vars, &segs),
            Some("ТабличныйДокумент".to_string())
        );
    }

    #[test]
    fn resolve_chain_unknown_member_returns_none() {
        let idx = mock_index();
        let vars = HashMap::new();
        let segs = parse_chain("Запрос.НетТакогоМетода()").unwrap();
        assert_eq!(resolve_chain_type(&idx, &vars, &segs), None);
    }

    #[test]
    fn build_scope_infers_chain_at_level3() {
        let idx = mock_index();
        let src = "Запрос = Новый Запрос;\nРез = Запрос.Выполнить();\nВыб = Рез.Выбрать();\n";
        let annotations = HashMap::new();
        // level=3 — цепочки выводятся
        let map3 = extract_scope_map(&idx, src, &annotations, 3);
        assert_var(&map3, src.len() - 1, "запрос", "Запрос");
        assert_var(&map3, src.len() - 1, "рез", "РезультатЗапроса");
        assert_var(&map3, src.len() - 1, "выб", "ВыборкаИзРезультатаЗапроса");
    }

    #[test]
    fn build_scope_level2_no_returntype() {
        let idx = mock_index();
        let src = "Запрос = Новый Запрос;\nРез = Запрос.Выполнить();\n";
        let annotations = HashMap::new();
        // level=2 — return-type НЕ выводится (регрессия не должна появиться)
        let map2 = extract_scope_map(&idx, src, &annotations, 2);
        assert_var(&map2, src.len() - 1, "запрос", "Запрос"); // из Новый — есть
        assert!(map2.type_of_var(src.len() - 1, "рез").is_none()); // из вызова — нет на level=2
    }
}
