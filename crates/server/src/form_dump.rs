//! Имена элементов формы из `Form.xml` выгрузки (вторая половина issue #43).
//!
//! Ветка `Элементы.<Имя>` не проверялась вовсе: в модуле управляемой формы
//! `Элементы.НетТакогоЭлемента123.Видимость = Ложь` проходило молча, хотя состав
//! элементов лежит рядом с модулем — в `Ext/Form.xml`.
//!
//! Формат logform: имя элемента — АТРИБУТ `name` тега (`<Table name="Список" id="1">`,
//! `<UsualGroup name="Группа" id="2">`), а не текст `<Name>`. Элементы лежат в
//! `ChildItems` (вложенность групп — сколько угодно уровней), реквизиты формы — в
//! `Attributes`, команды — в `Commands`. Собираем имена из этих секций.
//!
//! Сбор намеренно ПЕРМИССИВНЫЙ: лишнее имя лишь выключает проверку, а пропущенное
//! дало бы ложную находку на законном коде. Поэтому имена берутся на любой глубине
//! внутри секции, а если секций не нашлось вовсе (неожиданный формат) — из всего
//! документа.
//!
//! Зависимостей не добавляем: разбор — один проход по тексту, а XML выгрузки
//! машиночитаемый и простой (CDATA и сущности в именах не встречаются).

use bsl_validator::form_elements::FormElements;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Потолок размера `Form.xml`: формы типовой конфигурации — сотни килобайт,
/// 8 МиБ отсекают случайное чтение постороннего файла при ошибке в пути.
pub const MAX_FORM_XML_BYTES: u64 = 8 * 1024 * 1024;

/// Секции logform, `name` внутри которых — имя, доступное из модуля формы.
const NAME_SECTIONS: [&str; 4] = ["ChildItems", "Attributes", "Commands", "Parameters"];

/// Имена формы и типы элементов из секций ChildItems.
fn parse_form_elements(xml: &str) -> FormElements {
    let mut form = FormElements::default();
    let mut sections: Vec<String> = Vec::new();
    let mut rest = xml;

    while let Some(start) = rest.find('<') {
        let after = &rest[start + 1..];
        if let Some((content, delimiter)) = after
            .strip_prefix("!--")
            .map(|text| (text, "-->"))
            .or_else(|| after.strip_prefix("![CDATA[").map(|text| (text, "]]>")))
        {
            let Some(end) = content.find(delimiter) else {
                break;
            };
            rest = &content[end + delimiter.len()..];
            continue;
        }
        let Some(end) = after.find('>') else {
            break;
        };
        let tag = &after[..end];
        rest = &after[end + 1..];

        // Закрывающий тег: `</ChildItems>`. Комментарии (`<!--`), объявления
        // (`<?xml`) и CDATA (`<![CDATA[`) закрывающими не считаются.
        if let Some(closed) = tag.strip_prefix('/') {
            let name = tag_name(closed);
            if !name.is_empty() && sections.last().is_some_and(|open| open == name) {
                sections.pop();
            }
            continue;
        }
        if tag.starts_with('!') || tag.starts_with('?') {
            continue;
        }

        let name = tag_name(tag);
        let self_closing = tag.ends_with('/');
        if NAME_SECTIONS.iter().any(|s| s.eq_ignore_ascii_case(name)) && !self_closing {
            sections.push(name.to_string());
            continue;
        }
        // Имя собираем и у самозакрытых тегов (`<ExtendedTooltip name="…"/>`).
        if !sections.is_empty() {
            if let Some(value) = attribute(tag, "name") {
                if !value.is_empty() {
                    form.names.insert(value.to_string());
                    if sections
                        .last()
                        .is_some_and(|section| section.eq_ignore_ascii_case("ChildItems"))
                    {
                        form.record_type(value, element_type(name));
                    }
                }
            }
        }
    }

    if form.names.is_empty() {
        form.names = all_name_attributes(xml);
    }
    form
}

fn element_type(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "Button" => "КнопкаФормы",
        "InputField"
        | "CheckBoxField"
        | "LabelField"
        | "PictureField"
        | "RadioButtonField"
        | "TextDocumentField"
        | "SpreadSheetDocumentField"
        | "SpreadsheetDocumentField"
        | "PDFDocumentField"
        | "FormattedDocumentField"
        | "HTMLDocumentField"
        | "CalendarField"
        | "TrackBarField"
        | "ProgressBarField"
        | "ChartField"
        | "GanttChartField"
        | "DendrogramField"
        | "GeographicalSchemaField"
        | "GraphicalSchemaField"
        | "PeriodField"
        | "PlannerField" => "ПолеФормы",
        "Table" => "ТаблицаФормы",
        "UsualGroup" | "Pages" | "Page" => "ГруппаФормы",
        "LabelDecoration" | "PictureDecoration" => "ДекорацияФормы",
        _ => return None,
    })
}

#[cfg(test)]
fn element_names(xml: &str) -> HashSet<String> {
    parse_form_elements(xml).names
}

/// Имя тега: до пробела, слэша или конца.
fn tag_name(tag: &str) -> &str {
    let end = tag
        .find(|c: char| c.is_whitespace() || c == '/')
        .unwrap_or(tag.len());
    &tag[..end]
}

/// Значение атрибута `key="…"` в тексте тега (одинарные кавычки тоже принимаются).
///
/// Ключ ищется как отдельное слово: `typename="…"` атрибутом `name` НЕ считается
/// (иначе чужое значение попало бы в набор имён и выключило проверку).
fn attribute<'a>(tag: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(position) = rest.find(key) {
        let before_ok = position == 0
            || rest[..position]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace());
        let after = &rest[position + key.len()..];
        let after_trimmed = after.trim_start();
        if before_ok && after_trimmed.starts_with('=') {
            let value = after_trimmed[1..].trim_start();
            let quote = value.chars().next()?;
            if quote == '"' || quote == '\'' {
                let value = &value[1..];
                let end = value.find(quote)?;
                return Some(&value[..end]);
            }
        }
        rest = after;
    }
    None
}

/// Все `name="…"` документа — запасной, самый пермиссивный путь.
fn all_name_attributes(xml: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut rest = xml;
    while let Some(position) = rest.find("name=") {
        let value = &rest[position + "name=".len()..];
        let value = value.trim_start();
        let Some(quote) = value.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            rest = value;
            continue;
        };
        let value = &value[1..];
        match value.find(quote) {
            Some(end) => {
                if end > 0 {
                    names.insert(value[..end].to_string());
                }
                rest = &value[end + 1..];
            }
            None => break,
        }
    }
    names
}

/// Прочитать `Form.xml` рядом с модулем формы и вернуть имена из него.
///
/// `module_path` — путь модуля внутри выгрузки
/// (`Catalogs/X/Forms/Ф/Ext/Form/Module.bsl`). `None` — модуль не формы, файла
/// нет, он слишком велик или не читается: проверка элементов формы молчит
/// (лучше не проверить, чем выдать ложную находку).
pub fn form_elements_from_dump(
    roots: &[PathBuf],
    module_path: Option<&str>,
) -> Option<FormElements> {
    let module_path = module_path?.replace('\\', "/");
    // `…/Ext/Form/Module.bsl` → `…/Ext/` — рядом лежит `Form.xml`.
    let dir = module_path.strip_suffix("Form/Module.bsl")?;
    for root in roots {
        let path = root.join(dir).join("Form.xml");
        if let Some(names) = read_form_xml(&path) {
            return Some(names);
        }
    }
    None
}

/// Прочитать и разобрать один `Form.xml`.
fn read_form_xml(path: &Path) -> Option<FormElements> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FORM_XML_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let form = parse_form_elements(&text);
    (!form.names.is_empty()).then_some(form)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Урезанный, но настоящий по структуре `Form.xml` управляемой формы.
    const FORM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Form xmlns="http://v8.1c.ru/8.3/xcf/logform">
	<Attributes>
		<Attribute name="Список" id="1">
			<Type>
				<v8:Type>cfg:DynamicList</v8:Type>
			</Type>
			<MainAttribute>true</MainAttribute>
			<Settings xsi:type="DynamicList">
				<QueryText>ВЫБРАТЬ
	Выручка.Период КАК Период
ИЗ
	РегистрНакопления.Выручка КАК Выручка</QueryText>
			</Settings>
		</Attribute>
	</Attributes>
	<ChildItems>
		<UsualGroup name="ГруппаНастроек" id="288">
			<Title>
				<v8:item>
					<v8:lang>ru</v8:lang>
					<v8:content>Группа настроек</v8:content>
				</v8:item>
			</Title>
			<ChildItems>
				<CheckBoxField name="ТолькоПросроченные" id="289">
					<DataPath>ТолькоПросроченные</DataPath>
				</CheckBoxField>
				<ExtendedTooltip name="ГруппаНастроекРасширеннаяПодсказка" id="290"/>
			</ChildItems>
		</UsualGroup>
		<Table name="Список" id="1">
			<ContextMenu name="СписокКонтекстноеМеню" id="2"/>
			<Columns>
				<LabelField name="Период" id="3"/>
			</Columns>
		</Table>
	</ChildItems>
	<Commands>
		<Command name="Обновить" id="1"/>
	</Commands>
</Form>"#;

    #[test]
    fn names_come_from_child_items_attributes_and_commands() {
        let names = element_names(FORM);
        // Элементы, включая вложенные в группу.
        assert!(names.contains("Список"), "{names:?}");
        assert!(names.contains("ГруппаНастроек"), "{names:?}");
        assert!(names.contains("ТолькоПросроченные"), "{names:?}");
        assert!(
            names.contains("ГруппаНастроекРасширеннаяПодсказка"),
            "{names:?}"
        );
        // Реквизит формы и команда.
        assert!(names.contains("Обновить"), "{names:?}");
    }

    #[test]
    fn text_inside_sections_is_not_a_name() {
        let names = element_names(FORM);
        // Текст запроса, пути данных и подписи именами не становятся: у них нет
        // атрибута `name`, а `name=` в тексте (если бы встретился) не собран —
        // мы смотрим только секции.
        assert!(!names.contains("Выручка"), "{names:?}");
        assert!(!names.contains("РегистрНакопления"), "{names:?}");
        assert!(!names.contains("Группа настроек"), "{names:?}");
        // Колонка таблицы — тоже имя (доступна как `Элементы.Список.Колонки…`).
        assert!(names.contains("Период"), "{names:?}");
    }

    #[test]
    fn xml_without_known_sections_falls_back_to_all_names() {
        let names = element_names(r#"<Form><Item name="Один"/><Item name='Два'/></Form>"#);
        assert!(names.contains("Один"), "{names:?}");
        assert!(names.contains("Два"), "{names:?}");
    }

    #[test]
    fn attribute_lookup_does_not_match_similar_keys() {
        let names = element_names(r#"<ChildItems><Item typename="Нет" name="Да"/></ChildItems>"#);
        assert!(names.contains("Да"), "{names:?}");
        assert!(!names.contains("Нет"), "{names:?}");
    }

    #[test]
    fn types_come_from_static_child_items_only() {
        let form = parse_form_elements(
            r#"<Form>
            <Attributes><Button name="Реквизит"/></Attributes>
            <ChildItems>
                <Button name="Кнопка"/>
                <InputField name="Поле"/>
                <SpreadSheetDocumentField name="ТабличноеПоле"/>
                <Button name="Дубликат"/>
                <InputField name="дубликат"/>
                <Button name="Неоднозначный"/>
                <FutureField name="Неоднозначный"/>
                <FutureField name="Неизвестный"/>
                <!-- <Button name="Комментарий"/> -->
                <![CDATA[<Button name="Текст"/>]]>
            </ChildItems>
        </Form>"#,
        );
        assert_eq!(form.element_type("кНоПкА"), Some("КнопкаФормы"));
        assert_eq!(form.element_type("поле"), Some("ПолеФормы"));
        assert_eq!(form.element_type("ТабличноеПоле"), Some("ПолеФормы"));
        for name in [
            "Реквизит",
            "Дубликат",
            "Неоднозначный",
            "Неизвестный",
            "Комментарий",
            "Текст",
        ] {
            assert_eq!(form.element_type(name), None, "{name}");
        }
        assert!(form.names.contains("Неизвестный"));
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let roots = vec![dir.path().to_path_buf()];
        assert!(
            form_elements_from_dump(&roots, Some("Catalogs/Х/Forms/Ф/Ext/Form/Module.bsl"))
                .is_none()
        );
        // Не модуль формы — тоже молчание.
        assert!(form_elements_from_dump(&roots, Some("CommonModules/Х/Ext/Module.bsl")).is_none());
        assert!(form_elements_from_dump(&roots, None).is_none());
    }

    #[test]
    fn form_xml_next_to_module_is_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir
            .path()
            .join("Catalogs")
            .join("Х")
            .join("Forms")
            .join("Ф")
            .join("Ext");
        std::fs::create_dir_all(&nested).expect("каталоги");
        std::fs::write(nested.join("Form.xml"), FORM).expect("Form.xml");
        let roots = vec![dir.path().to_path_buf()];
        let form = form_elements_from_dump(&roots, Some("Catalogs/Х/Forms/Ф/Ext/Form/Module.bsl"))
            .expect("имена должны собраться");
        assert!(form.names.contains("Список"), "{:?}", form.names);
        assert_eq!(form.element_type("список"), Some("ТаблицаФормы"));
    }
}
