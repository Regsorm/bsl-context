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

use bsl_parse::{IfBranches, NameSite};

/// Один scope — привязки имён в пределах одной процедуры (или модуля, если
/// процедур нет).
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// Включающий байтовый диапазон `[start..end)`.
    pub byte_start: usize,
    pub byte_end: usize,
    /// Привязки имён в порядке появления в тексте. Именно ПОСЛЕДОВАТЕЛЬНОСТЬ, а не
    /// карта «имя → тип»: тип переменной берётся из ближайшего присваивания ВЫШЕ
    /// точки использования, а не из присваивания в другом месте процедуры
    /// (issue #15, класс 1: имя получало тип из ветки или из строки ниже, к
    /// которой в этой точке отношения нет).
    pub bindings: Vec<VarBinding>,
    /// Ветви условных операторов этого scope — для объединения типов после
    /// `КонецЕсли` (issue #15, класс 1).
    pub if_branches: Vec<IfBranches>,
}

/// Привязка имени к типу в конкретном месте текста.
#[derive(Debug, Clone)]
pub struct VarBinding {
    /// Имя переменной в нижнем регистре (в BSL регистр не различается).
    pub name: String,
    /// Байт КОНЦА оператора присваивания.
    ///
    /// Не начало имени: тип переменной становится известен только ПОСЛЕ
    /// присваивания. С началом имени точка внутри СВОЕЙ ЖЕ правой части
    /// (`Х = Х.Метод()`) находила собственную привязку, и получался круг:
    /// `Выгрузить()` типизировал `Результат`, а `Результат` типизировал
    /// `Выгрузить()` (issue #29, второй случай).
    pub byte: usize,
    /// Альтернативы типа: у составного типа их несколько (issue #15, класс 4).
    /// ПУСТОЙ список означает «тип не выведен» — вызывающий код молчит.
    pub types: Vec<String>,
}

impl Scope {
    pub fn contains(&self, byte_idx: usize) -> bool {
        byte_idx >= self.byte_start && byte_idx < self.byte_end
    }

    /// Типы переменной на момент `byte`: ближайшая привязка не позже этой точки.
    ///
    /// Если ближайшая привязка сделана ВНУТРИ ветви условного оператора, а
    /// спрашиваем мы уже ПОСЛЕ него, тип мог прийти из любой ветви — возвращаем
    /// объединение альтернатив (issue #15, класс 1). Внутри самой ветви объединения
    /// нет: там действует только своя ветка.
    pub fn type_of_var(&self, byte: usize, var_name: &str) -> Option<Vec<String>> {
        let lower = var_name.to_lowercase();
        let nearest = self
            .bindings
            .iter()
            .rev()
            .find(|b| b.name == lower && b.byte <= byte)?;
        // Ближайшая привязка без выведенного типа означает «тип неизвестен» — и это
        // именно `None`, а не пустой список: иначе вызывающий код вернулся бы к типу
        // ПРЕДЫДУЩЕГО присваивания, которого в этой точке уже нет (issue #29).
        if nearest.types.is_empty() {
            return None;
        }
        let mut types = nearest.types.clone();

        // Самая вложенная ветвь, накрывающая привязку и уже закрытая к этой точке.
        let block = self
            .if_branches
            .iter()
            .filter(|b| b.span.1 <= byte)
            .filter(|b| {
                b.branches
                    .iter()
                    .any(|(s, e)| *s <= nearest.byte && nearest.byte < *e)
            })
            .min_by_key(|b| b.span.1.saturating_sub(b.span.0));
        if let Some(block) = block {
            for binding in self
                .bindings
                .iter()
                .filter(|b| b.name == lower && b.byte <= byte)
            {
                let in_block = block
                    .branches
                    .iter()
                    .any(|(s, e)| *s <= binding.byte && binding.byte < *e);
                if !in_block {
                    continue;
                }
                for t in &binding.types {
                    if !types.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                        types.push(t.clone());
                    }
                }
            }
        }
        Some(types)
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

    /// Типы переменной по имени на момент `byte_idx`, регистронезависимо.
    ///
    /// Возвращает альтернативы: у составного типа их несколько, и проверка члена
    /// считает член найденным, если он есть хотя бы у одной.
    pub fn type_of_var(&self, byte_idx: usize, var_name: &str) -> Option<Vec<String>> {
        self.lookup(byte_idx)?.type_of_var(byte_idx, var_name)
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
        //
        // Класс буквы — `\p{L}`, а не `А-Яа-яЁё`: платформа принимает в именах
        // ЛЮБЫЕ буквы Unicode, а диапазон русского алфавита рвал украинские и
        // казахские имена на куски (issue #20: `Прав(Закінчення, 1)` выглядело
        // как три аргумента).
        Regex::new(r"(?m:^)\s*(?P<lhs>[\p{L}_][\p{L}\p{N}_]*)\s*=\s*(?P<rhs>[^;\n]*)").unwrap()
    })
}

fn new_rhs_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^\s*(?:Новый|New)\s+(?P<ty>[\p{L}_][\p{L}\p{N}_]*)").unwrap()
    })
}

fn enum_rhs_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\s*(?P<ty>[\p{L}_][\p{L}\p{N}_]*)\.(?P<member>[\p{L}_][\p{L}\p{N}_]*)\s*$")
            .unwrap()
    })
}

fn type_annot_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `// @type ТипX` или `// @type: ТипX`
        Regex::new(r"//\s*@type:?\s+(?P<ty>[\p{L}_][\p{L}\p{N}_]*)").unwrap()
    })
}

/// Извлечь scope из исходника. На вход — уже очищенный от строк/комментариев
/// текст (но аннотации `// @type` извлекаются ДО очистки и подаются отдельно).
pub fn extract_scope_map(
    index: &PlatformIndex,
    cleaned: &str,
    annotations: &HashMap<usize, String>,
    level: u8,
    if_branches: &[IfBranches],
    loop_var_sites: &[NameSite],
) -> ScopeMap {
    let mut scopes = Vec::new();
    let blocks: Vec<(usize, usize)> = proc_block_re()
        .find_iter(cleaned)
        .map(|m| (m.start(), m.end()))
        .collect();

    if blocks.is_empty() {
        // Глобальный scope на весь файл.
        let scope = build_scope(
            index,
            cleaned,
            0,
            cleaned.len(),
            annotations,
            level,
            if_branches,
            loop_var_sites,
        );
        scopes.push(scope);
    } else {
        for (start, end) in blocks {
            let body = &cleaned[start..end];
            let scope = build_scope(
                index,
                body,
                start,
                end,
                annotations,
                level,
                if_branches,
                loop_var_sites,
            );
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

#[allow(clippy::too_many_arguments)]
fn build_scope(
    index: &PlatformIndex,
    body: &str,
    byte_start: usize,
    byte_end: usize,
    annotations: &HashMap<usize, String>,
    level: u8,
    if_branches: &[IfBranches],
    loop_var_sites: &[NameSite],
) -> Scope {
    let mut bindings: Vec<VarBinding> = Vec::new();
    // Ветви, целиком лежащие внутри этого scope: объединение типов после
    // `КонецЕсли` считается только по своим ветвям (issue #15, класс 1).
    let own_branches: Vec<IfBranches> = if_branches
        .iter()
        .filter(|b| byte_start <= b.span.0 && b.span.1 <= byte_end)
        .cloned()
        .collect();

    // Абсолютные смещения ВСЕХ присваиваний тела: по ним видно, израсходована
    // ли аннотация. Без этого `// @type` цеплялась к каждому присваиванию в
    // окне 200 байт и перебивала честный вывод по `Новый ТипX`.
    let assign_starts: Vec<usize> = assign_re()
        .captures_iter(body)
        .map(|cap| byte_start + cap.name("lhs").unwrap().start())
        .collect();

    for cap in assign_re().captures_iter(body) {
        let lhs_match = cap.name("lhs").unwrap();
        let rhs_match = cap.name("rhs").unwrap();
        let lhs = lhs_match.as_str().to_string();
        let rhs = rhs_match.as_str().trim();

        let abs_start = byte_start + lhs_match.start();

        // 1. Проверить аннотацию: берём БЛИЖАЙШУЮ к присваиванию (наибольший
        // end_of_line) в пределах ~200 байт (примерно 4 строки) — и только ту,
        // между которой и этим присваиванием нет другого присваивания.
        //
        // Оба условия существенны. Без выбора по максимуму порядок обхода
        // `HashMap` произволен, и из двух подходящих аннотаций бралась
        // случайная — набор находок менялся от запуска к запуску. Без проверки
        // «первое присваивание после аннотации» она цеплялась ко всем
        // последующим в окне и перебивала вывод по `Новый ТипX`.
        let mut typ: Option<Vec<String>> = annotations
            .iter()
            .filter(|(&annot_end, _)| {
                annot_end <= abs_start && abs_start.saturating_sub(annot_end) <= 200
            })
            .filter(|(&annot_end, _)| {
                !assign_starts
                    .iter()
                    .any(|&other| other >= annot_end && other < abs_start)
            })
            .max_by_key(|(&annot_end, _)| annot_end)
            .map(|(_, annot_ty)| vec![annot_ty.clone()]);

        // 2. Если аннотации нет — пробуем извлечь из RHS.
        if typ.is_none() {
            if let Some(c) = new_rhs_re().captures(rhs) {
                let ty = c.name("ty").unwrap().as_str();
                if index.find_type(ty).is_some() {
                    typ = Some(vec![ty.to_string()]);
                }
            }
        }
        if typ.is_none() {
            if let Some(c) = enum_rhs_re().captures(rhs) {
                let ty = c.name("ty").unwrap().as_str();
                if let Some(t) = index.find_type(ty) {
                    if t.is_enum() {
                        typ = Some(vec![ty.to_string()]);
                    }
                }
            }
        }

        // 4. return-type tracking (Уровень 2.5, только level>=3). Резолвим
        // только когда RHS — чистая цепочка вызовов/обращений к членам:
        // либо длиной >=2 звена (`obj.Метод()`), либо одиночный вызов
        // глобального метода (`ГлобальныйМетод()`). Опирается на привязки,
        // собранные предыдущими присваиваниями (однопроходный порядок по тексту).
        // Тип может выйти составным — храним все альтернативы (issue #15).
        if typ.is_none() && level >= 3 {
            if let Some(segs) = parse_chain(rhs) {
                if segs.len() >= 2 || (segs.len() == 1 && segs[0].is_call) {
                    typ = resolve_chain_types(index, &bindings, abs_start, &segs);
                }
            }
        }

        // Привязка создаётся ВСЕГДА, даже когда тип не выведен (пустой список
        // альтернатив). Иначе присваивание с невыводимым типом не оставляет следа,
        // и `type_of_var` возвращает тип ПРЕДЫДУЩЕГО присваивания — проверка члена
        // считает его актуальным. Так выглядел первый случай issue #29:
        // `Выборка = Новый Массив; Выборка = Справочники.Номенклатура.Выбрать();`
        // давало «У типа 'Массив' нет члена 'Следующий'». Пустой список вызывающий
        // код читает как «тип неизвестен» (в `check_type_dot_members`:
        // `unwrap_or_default()` и затем `if candidates.is_empty() { continue; }`).
        //
        // Байт привязки — КОНЕЦ оператора: см. пояснение у `VarBinding::byte`.
        let statement_end = byte_start + cap.get(0).map_or(lhs_match.end(), |m| m.end());
        bindings.push(VarBinding {
            name: lhs.to_lowercase(),
            byte: statement_end,
            types: typ.unwrap_or_default(),
        });
    }

    // Переменные циклов: заголовок `Для Каждого Х Из С Цикл` переопределяет имя
    // с НАЧАЛА тела цикла (issue #36, класс 2). Привязка с ПУСТЫМ списком типов —
    // «тип неизвестен»: прежний тип из присваивания ВЫШЕ цикла к телу уже не
    // относится (иначе `Х = Новый Массив; … Для Каждого Х Из С Цикл Х.Значение`
    // даёт находку про `Массив`). Тип элемента коллекции выводится не всегда,
    // поэтому молчание здесь лучше находки про чужой тип — тот же выбор, что в #29.
    for site in loop_var_sites {
        if byte_start <= site.byte && site.byte < byte_end {
            bindings.push(VarBinding {
                name: site.name.clone(),
                byte: site.byte,
                types: Vec::new(),
            });
        }
    }
    // `type_of_var` берёт БЛИЖАЙШУЮ привязку через `.rev().find(..)` — вектор
    // обязан быть упорядочен по `byte`.
    bindings.sort_by_key(|b| b.byte);

    Scope {
        byte_start,
        byte_end,
        bindings,
        if_branches: own_branches,
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

/// Все известные типы из (возможно составного) описания типа, в порядке справки.
///
/// В hbk возвращаемый тип или тип свойства нередко составной — например
/// `Запрос.Выполнить()` даёт `` `РезультатЗапроса`, `Неопределено` ``, а
/// `ПараметрыВыполненияКоманды.Источник` — `` `ФормаКлиентскогоПриложения`,
/// `ОкноКлиентскогоПриложения` ``. Каждый компонент может быть обёрнут в
/// backtick'и (`to_markdown` сохраняет их из `<code>`-тегов hbk), поэтому края
/// чистим от не-идентификаторных символов. Служебные `Неопределено` и
/// `Произвольный` пропускаем, дубликаты схлопываем.
///
/// Возвращаем ВСЕ компоненты, а не первый: проверка члена обязана учесть каждую
/// альтернативу (issue #15), иначе `Источник` проверялся бы по одному из двух
/// типов и давал ложную находку.
pub(crate) fn types_of(index: &PlatformIndex, raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split([',', ';', '|']) {
        let p = part.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        if p.is_empty()
            || p.eq_ignore_ascii_case("Произвольный")
            || p.eq_ignore_ascii_case("Неопределено")
        {
            continue;
        }
        if index.find_type(p).is_some() && !out.iter().any(|t| t.eq_ignore_ascii_case(p)) {
            out.push(p.to_string());
        }
    }
    out
}

/// Первый известный тип из описания (там, где альтернативы не нужны: например,
/// тип свойства контекста в `context_names`).
pub(crate) fn primary_type(index: &PlatformIndex, raw: &str) -> Option<String> {
    types_of(index, raw).into_iter().next()
}

/// Вычислить типы значения цепочки `head.member1(...).member2...`.
///
/// `head` резолвится: из `vars` (локальная переменная с выведенным типом),
/// либо как вызов глобального метода (`ГлобальныйМетод()` → return_type),
/// либо как голое имя платформенного типа. Дальше по каждому звену: метод →
/// `return_type`, свойство → `type_name`; если звено составное, продолжаем ВСЕ
/// альтернативы (issue #15). Альтернатива, у которой члена нет, из результата
/// выпадает; если выпали все — тип не выводим (`None`), и находок не будет.
fn resolve_chain_types(
    index: &PlatformIndex,
    bindings: &[VarBinding],
    at: usize,
    segs: &[ChainSeg],
) -> Option<Vec<String>> {
    let head = &segs[0];
    // Тип головы — ближайшая привязка ВЫШЕ этой строки (позиционность, issue #15,
    // класс 1), а не последняя в процедуре.
    let head_lower = head.name.to_lowercase();
    let bound = bindings
        .iter()
        .rev()
        .find(|b| b.name == head_lower && b.byte <= at)
        .map(|b| b.types.clone());
    let mut cur = if let Some(t) = bound {
        t
    } else if head.is_call {
        let m = index.find_global_method(&head.name)?;
        types_of(index, &m.return_type)
    } else if index.find_type(&head.name).is_some() {
        vec![head.name.clone()]
    } else {
        return None;
    };
    if cur.is_empty() {
        return None;
    }

    for seg in &segs[1..] {
        let mut next: Vec<String> = Vec::new();
        for type_name in &cur {
            let Some(ty) = index.find_type(type_name) else {
                continue;
            };
            let raw = if let Some(m) = ty.methods.iter().find(|m| {
                crate::homoglyphs::same_after_fold(&m.name_ru, &seg.name)
                    || crate::homoglyphs::same_after_fold(&m.name_en, &seg.name)
            }) {
                m.return_type.clone()
            } else if let Some(p) = ty.properties.iter().find(|p| {
                crate::homoglyphs::same_after_fold(&p.name_ru, &seg.name)
                    || crate::homoglyphs::same_after_fold(&p.name_en, &seg.name)
            }) {
                p.type_name.clone()
            } else {
                // У этой альтернативы члена нет — ветка не продолжается.
                continue;
            };
            for t in types_of(index, &raw) {
                if !next.iter().any(|n| n.eq_ignore_ascii_case(&t)) {
                    next.push(t);
                }
            }
        }
        if next.is_empty() {
            return None;
        }
        cur = next;
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
        assert_eq!(t, vec![expected_type.to_string()]);
    }

    fn method(name: &str, return_type: &str) -> Method {
        Method {
            name_ru: name.to_string(),
            name_en: String::new(),
            description: String::new(),
            return_type: return_type.to_string(),
            signatures: Vec::new(),
            note: None,
        }
    }

    fn property(name: &str, type_name: &str) -> Property {
        Property {
            name_ru: name.to_string(),
            name_en: String::new(),
            description: String::new(),
            type_name: type_name.to_string(),
            readonly: false,
            note: None,
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
            note: None,
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

    /// Привязки для теста: имя → тип, байты по порядку (1, 2, 3 …).
    fn bindings_of(pairs: &[(&str, &str)]) -> Vec<VarBinding> {
        pairs
            .iter()
            .enumerate()
            .map(|(i, (name, ty))| VarBinding {
                name: name.to_lowercase(),
                byte: i + 1,
                types: vec![ty.to_string()],
            })
            .collect()
    }

    #[test]
    fn resolve_chain_method_return_type() {
        let idx = mock_index();
        let segs = parse_chain("Запрос.Выполнить()").unwrap();
        assert_eq!(
            resolve_chain_types(&idx, &[], 100, &segs),
            Some(vec!["РезультатЗапроса".to_string()])
        );
    }

    #[test]
    fn resolve_chain_multi_level() {
        let idx = mock_index();
        let segs = parse_chain("Запрос.Выполнить().Выбрать()").unwrap();
        assert_eq!(
            resolve_chain_types(&idx, &[], 100, &segs),
            Some(vec!["ВыборкаИзРезультатаЗапроса".to_string()])
        );
    }

    #[test]
    fn resolve_chain_via_var_and_property() {
        let idx = mock_index();
        // Голова — переменная с выведенным типом (привязка выше точки).
        let vars = bindings_of(&[("рез", "РезультатЗапроса")]);
        let segs = parse_chain("Рез.Выбрать()").unwrap();
        assert_eq!(
            resolve_chain_types(&idx, &vars, 100, &segs),
            Some(vec!["ВыборкаИзРезультатаЗапроса".to_string()])
        );
        // Свойство → type_name.
        let segs2 = parse_chain("Запрос.Текст").unwrap();
        assert_eq!(
            resolve_chain_types(&idx, &vars, 100, &segs2),
            Some(vec!["Строка".to_string()])
        );
    }

    /// Issue #15, класс 1: тип берётся из ближайшего присваивания ВЫШЕ точки, а
    /// не из последнего в процедуре.
    #[test]
    fn chain_resolution_is_positional() {
        let idx = mock_index();
        // `рез` сначала был числами (тип неизвестен), потом получил РезультатЗапроса.
        let vars = vec![
            VarBinding {
                name: "рез".to_string(),
                byte: 10,
                types: vec!["Строка".to_string()],
            },
            VarBinding {
                name: "рез".to_string(),
                byte: 50,
                types: vec!["РезультатЗапроса".to_string()],
            },
        ];
        let segs = parse_chain("Рез.Выбрать()").unwrap();
        // Между привязками (после 10, до 50) типа РезультатЗапроса ещё нет.
        assert_eq!(resolve_chain_types(&idx, &vars, 30, &segs), None);
        // После второй привязки — уже есть.
        assert_eq!(
            resolve_chain_types(&idx, &vars, 60, &segs),
            Some(vec!["ВыборкаИзРезультатаЗапроса".to_string()])
        );
    }

    /// Issue #15, класс 4: составной тип свойства не схлопывается в одну
    /// альтернативу — иначе `Источник` проверялся бы по одному из двух типов.
    #[test]
    fn composite_property_keeps_all_alternatives() {
        let mut idx = mock_index();
        idx.insert_type(ty(
            "ФормаКлиентскогоПриложения",
            vec![],
            vec![property("ИмяФормы", "Строка")],
        ));
        idx.insert_type(ty("ОкноКлиентскогоПриложения", vec![], vec![]));
        idx.insert_type(ty(
            "ПараметрыВыполненияКоманды",
            vec![],
            vec![property(
                "Источник",
                "`ОкноКлиентскогоПриложения`, `ФормаКлиентскогоПриложения`",
            )],
        ));

        let segs = parse_chain("ПараметрыВыполненияКоманды.Источник").unwrap();
        assert_eq!(
            resolve_chain_types(&idx, &[], 100, &segs),
            Some(vec![
                "ОкноКлиентскогоПриложения".to_string(),
                "ФормаКлиентскогоПриложения".to_string()
            ]),
            "обе альтернативы обязаны дойти до проверки членов"
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
        // types_of отдаёт ВСЕ известные компоненты, а не первый
        assert_eq!(
            types_of(&idx, "`РезультатЗапроса`, `Неопределено`, `Строка`"),
            vec!["РезультатЗапроса".to_string(), "Строка".to_string()]
        );
    }

    #[test]
    fn resolve_chain_global_method() {
        let idx = mock_index();
        let segs = parse_chain("ПолучитьОбщийМакет()").unwrap();
        assert_eq!(
            resolve_chain_types(&idx, &[], 100, &segs),
            Some(vec!["ТабличныйДокумент".to_string()])
        );
    }

    #[test]
    fn resolve_chain_unknown_member_returns_none() {
        let idx = mock_index();
        let segs = parse_chain("Запрос.НетТакогоМетода()").unwrap();
        assert_eq!(resolve_chain_types(&idx, &[], 100, &segs), None);
    }

    #[test]
    fn build_scope_infers_chain_at_level3() {
        let idx = mock_index();
        let src = "Запрос = Новый Запрос;\nРез = Запрос.Выполнить();\nВыб = Рез.Выбрать();\n";
        let annotations = HashMap::new();
        // level=3 — цепочки выводятся
        let map3 = extract_scope_map(&idx, src, &annotations, 3, &[], &[]);
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
        let map2 = extract_scope_map(&idx, src, &annotations, 2, &[], &[]);
        assert_var(&map2, src.len() - 1, "запрос", "Запрос"); // из Новый — есть
        assert!(map2.type_of_var(src.len() - 1, "рез").is_none()); // из вызова — нет на level=2
    }

    /// issue #29, первый случай: присваивание с НЕВЫВОДИМЫМ типом обязано сбрасывать
    /// прежний тип, а не оставлять его — иначе член проверяется по устаревшему типу.
    #[test]
    fn unknown_type_assignment_resets_previous_type() {
        let idx = mock_index();
        let annotations = HashMap::new();
        let src = "Выборка = Новый Запрос;\nВыборка = СоздатьЧтоТоНеизвестное();\nВыборка.Текст = \"х\";\n";
        let map = extract_scope_map(&idx, src, &annotations, 3, &[], &[]);
        assert!(
            map.type_of_var(src.len() - 1, "выборка").is_none(),
            "после присваивания с невыводимым типом тип должен быть неизвестен, а не прежний"
        );

        // Контроль: без второго присваивания прежний тип на месте.
        let src2 = "Выборка = Новый Запрос;\nВыборка.Текст = \"х\";\n";
        let map2 = extract_scope_map(&idx, src2, &annotations, 3, &[], &[]);
        assert_var(&map2, src2.len() - 1, "выборка", "Запрос");
    }

    /// issue #29, второй случай: в правой части присваивания переменная видит ПРЕЖНИЙ
    /// тип, а не собственное присваивание (`Х = Х.Метод()` — иначе получается круг).
    #[test]
    fn assignment_does_not_type_its_own_right_side() {
        let idx = mock_index();
        let annotations = HashMap::new();
        let src = "Запрос = Новый Запрос;\nРез = Запрос.Выполнить();\nРез = Рез.Выбрать();\n";
        let map = extract_scope_map(&idx, src, &annotations, 3, &[], &[]);

        // Точка внутри правой части второго присваивания — тип ещё прежний.
        let dot_inside = src.find("Рез.Выбрать").unwrap() + "Рез".len();
        assert_var(&map, dot_inside, "рез", "РезультатЗапроса");

        // После оператора — уже новый тип.
        assert_var(&map, src.len() - 1, "рез", "ВыборкаИзРезультатаЗапроса");
    }

    /// Issue #36, класс 2: переменная цикла переопределяет имя с НАЧАЛА тела
    /// цикла в позиционном выводе типов (`ScopeMap`), а не только в
    /// `constructed_type` (`locals`). Без этого `Х = Новый Массив; … Для Каждого
    /// Х Из С Цикл Х.Значение` брало тип из присваивания ВЫШЕ цикла.
    #[test]
    fn loop_variable_resets_scope_map_type() {
        let mut idx = mock_index();
        idx.insert_type(ty("Массив", vec![], vec![]));
        idx.insert_type(ty("Соответствие", vec![], vec![]));

        let src = "Процедура Т()\nХ = Новый Массив;\nС = Новый Соответствие;\n\
                   Для Каждого Х Из С Цикл\nЗ = Х.Значение;\nКонецЦикла;\nКонецПроцедуры\n";
        let facts = bsl_parse::collect_facts(src);
        let annotations = HashMap::new();
        let map = extract_scope_map(
            &idx,
            src,
            &annotations,
            3,
            &facts.if_branches,
            &facts.loop_var_sites,
        );

        // Внутри тела цикла ближайшая привязка — переменная цикла с пустым
        // списком типов: тип неизвестен, находки про прежний `Массив` быть не должно.
        let inside = src.find("Х.Значение").expect("подстрока есть");
        assert_eq!(map.type_of_var(inside, "Х"), None);

        // Контроль: ВЫШЕ цикла прежний тип продолжает действовать.
        let before = src.find("С = Новый Соответствие").expect("подстрока есть");
        assert_eq!(
            map.type_of_var(before, "Х"),
            Some(vec!["Массив".to_string()])
        );
    }
}
