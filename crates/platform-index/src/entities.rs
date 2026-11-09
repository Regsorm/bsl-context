//! Доменные сущности business-слоя.
//!
//! Это УЗКАЯ проекция `hbk_parser::*Info`: переносятся имена, описания, `note`,
//! типы, параметры, синтаксис и доступность по контексту выполнения
//! (`availability`). `example`/`related_objects` и описание возвращаемого
//! значения пока остаются в промежуточных Info — при расширении домена
//! обязателен подъём `cache::FORMAT_VERSION`.

use serde::{Deserialize, Serialize};

/// Контекст выполнения 1С, в котором доступен метод/свойство/тип.
///
/// Метки приходят из главы «Доступность:» справки (см. `hbk_parser::AvailabilityInfo`).
/// Названия — как у BSL Language Server, чтобы не заводить свой словарь.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AvailabilityMark {
    /// `Сервер` — серверный код (модули объекта, менеджера, набора записей).
    Server,
    /// `внешнее соединение` — вызов из другой программы: COM-соединение и
    /// внешнее соединение.
    ExternalConnection,
    /// `мобильное приложение (сервер)`.
    MobileAppServer,
    /// `мобильный автономный сервер`.
    MobileStandaloneServer,
    /// `Тонкий клиент` — клиент управляемого приложения.
    ThinClient,
    /// `веб-клиент`.
    WebClient,
    /// `мобильный клиент`.
    MobileClient,
    /// `мобильное приложение (клиент)`.
    MobileAppClient,
    /// `толстый клиент` — клиент ОБЫЧНОГО приложения. Отдельный контекст, а не
    /// «любой клиент»: рядом с ним в списке стоят тонкий и веб-клиент, значит
    /// он аддитивен, и права на вызов из управляемой формы не даёт.
    ThickClient,
    /// `интеграция` — смысл метки не установлен (см. §10.3 плана), поэтому
    /// [`Availability::has_unknown`] считает набор с ней неполным.
    Integration,
    /// Метка, которой нет в словаре индекса (справка версии новее кода).
    /// Потребитель обязан молчать, а не делать вывод из неполного списка.
    Unknown(String),
}

impl AvailabilityMark {
    /// Метка справки → метка домена. Незнакомое написание даёт [`Self::Unknown`].
    ///
    /// Сверка без регистра и без различия `ё`/`е`: справка пишет первую метку
    /// списка с заглавной, остальные — строчными.
    pub fn from_label(label: &str) -> Self {
        match label.trim().to_lowercase().replace('ё', "е").as_str() {
            "сервер" => Self::Server,
            "внешнее соединение" => Self::ExternalConnection,
            "мобильное приложение (сервер)" => Self::MobileAppServer,
            "мобильный автономный сервер" => Self::MobileStandaloneServer,
            "тонкий клиент" => Self::ThinClient,
            "веб-клиент" => Self::WebClient,
            "мобильный клиент" => Self::MobileClient,
            "мобильное приложение (клиент)" => Self::MobileAppClient,
            "толстый клиент" => Self::ThickClient,
            "интеграция" => Self::Integration,
            _ => Self::Unknown(label.trim().to_string()),
        }
    }

    /// Метка в написании для сообщения пользователю.
    pub fn label(&self) -> &str {
        match self {
            Self::Server => "сервер",
            Self::ExternalConnection => "внешнее соединение",
            Self::MobileAppServer => "мобильное приложение (сервер)",
            Self::MobileStandaloneServer => "мобильный автономный сервер",
            Self::ThinClient => "тонкий клиент",
            Self::WebClient => "веб-клиент",
            Self::MobileClient => "мобильный клиент",
            Self::MobileAppClient => "мобильное приложение (клиент)",
            Self::ThickClient => "толстый клиент",
            Self::Integration => "интеграция",
            Self::Unknown(label) => label,
        }
    }
}

/// Доступность сущности по контексту выполнения: набор меток главы
/// «Доступность:».
///
/// Группы контекстов выводятся независимо: смешанные наборы вида «тонкий
/// клиент, веб-клиент, сервер, толстый клиент, внешнее соединение» — норма, и
/// один флаг на «клиент/сервер» их не описывает.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Availability {
    pub marks: Vec<AvailabilityMark>,
}

impl Availability {
    pub fn new(marks: Vec<AvailabilityMark>) -> Self {
        Self { marks }
    }

    /// Есть ли метка, дающая право на серверный вызов.
    pub fn server_side(&self) -> bool {
        self.marks.iter().any(|m| {
            matches!(
                m,
                AvailabilityMark::Server
                    | AvailabilityMark::ExternalConnection
                    | AvailabilityMark::MobileAppServer
                    | AvailabilityMark::MobileStandaloneServer
            )
        })
    }

    /// Есть ли метка клиента УПРАВЛЯЕМОГО приложения.
    pub fn managed_client_side(&self) -> bool {
        self.marks.iter().any(|m| {
            matches!(
                m,
                AvailabilityMark::ThinClient
                    | AvailabilityMark::WebClient
                    | AvailabilityMark::MobileClient
                    | AvailabilityMark::MobileAppClient
            )
        })
    }

    /// Есть ли метка клиента ОБЫЧНОГО приложения («толстый клиент»).
    pub fn ordinary_client_side(&self) -> bool {
        self.marks.contains(&AvailabilityMark::ThickClient)
    }

    /// Есть ли метка, смысла которой индекс не знает: незнакомое написание
    /// (`Unknown`) или `интеграция`. По такому набору решать нельзя —
    /// проверка обязана молчать, а не считать «доступно везде».
    pub fn has_unknown(&self) -> bool {
        self.marks.iter().any(|m| {
            matches!(
                m,
                AvailabilityMark::Unknown(_) | AvailabilityMark::Integration
            )
        })
    }

    /// Метки для сообщения пользователю — через запятую, в написании домена.
    pub fn labels(&self) -> String {
        self.marks
            .iter()
            .map(AvailabilityMark::label)
            .filter(|label| !label.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Метод платформы (глобальный или член типа).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Method {
    pub name_ru: String,
    pub name_en: String,
    pub description: String,
    /// «Примечание:» страницы справки, если оно есть.
    pub note: Option<String>,
    pub return_type: String,
    /// Список перегрузок. У апстрима всегда `emptyList()` — это исправляется здесь.
    pub signatures: Vec<Signature>,
    /// Глава «Доступность:» страницы метода; `None` — страница её не содержит.
    pub availability: Option<Availability>,
}

/// Перегрузка метода или конструктора.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signature {
    pub name: String,
    /// Авторитетный текст вызова из справки (`Найти(<Значение>, …)`).
    pub syntax: String,
    pub description: String,
    pub parameters: Vec<Parameter>,
}

/// Параметр метода/конструктора.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    pub type_name: String,
    pub required: bool,
    pub description: String,
}

/// Свойство (глобальное или член типа).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Property {
    pub name_ru: String,
    pub name_en: String,
    pub description: String,
    /// «Примечание:» страницы справки, если оно есть.
    pub note: Option<String>,
    pub type_name: String,
    pub readonly: bool,
    /// Глава «Доступность:» страницы свойства; `None` — страница её не содержит.
    pub availability: Option<Availability>,
}

/// Конструктор объекта (`Новый ТипX(...)`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Constructor {
    pub name: String,
    /// Английская форма имени конструктора, если она есть на странице.
    pub name_en: String,
    /// Текст синтаксиса со страницы (`Новый Тип(<Параметр>)`), если он есть.
    pub syntax: String,
    pub description: String,
    /// «Примечание:» страницы справки, если оно есть.
    pub note: Option<String>,
    pub parameters: Vec<Parameter>,
}

/// Значение системного перечисления.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnumValue {
    pub name_ru: String,
    pub name_en: String,
    pub description: String,
    /// «Примечание:» страницы значения (на 8.3.27 таких страниц 52 — раньше
    /// терялось на пути парсер → домен).
    pub note: Option<String>,
}

/// Тип платформы. Системное перечисление — это разновидность `Type` с непустым
/// `enum_values` и пустыми `methods/properties/constructors`. Обычный тип —
/// наоборот.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Type {
    pub name_ru: String,
    pub name_en: String,
    pub description: String,
    /// «Примечание:» страницы справки, если оно есть.
    pub note: Option<String>,
    pub methods: Vec<Method>,
    pub properties: Vec<Property>,
    pub constructors: Vec<Constructor>,
    /// Непустой ТОЛЬКО для типов-перечислений.
    pub enum_values: Vec<EnumValue>,
    /// Глава «Доступность:» страницы типа; `None` — страницы её не содержит.
    /// Источник доступности конструктора: у страницы конструктора своей главы нет.
    pub availability: Option<Availability>,
}

impl Type {
    /// `true`, если у типа есть значения системного перечисления.
    pub fn is_enum(&self) -> bool {
        !self.enum_values.is_empty()
    }

    /// Открытая коллекция: значения добавляет конфигурация (`ЦветаСтиля`,
    /// `БиблиотекаКартинок`). Признак — псевдо-значение вида `<Имя картинки>`
    /// в списке значений справки. Проверка по фиксированному списку для
    /// такого типа неполна.
    pub fn is_open_enum(&self) -> bool {
        self.enum_values.iter().any(|v| v.name_ru.starts_with('<'))
    }

    pub fn has_methods(&self) -> bool {
        !self.methods.is_empty()
    }

    pub fn has_properties(&self) -> bool {
        !self.properties.is_empty()
    }

    pub fn has_constructors(&self) -> bool {
        !self.constructors.is_empty()
    }
}

/// Универсальная ссылка на сущность для результатов поиска.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Definition {
    Method(Method),
    Property(Property),
    Type(Type),
}

/// Член ТИПА, которым индекс дополняют поверх справки (issue #40, дополнение
/// автора): часть устаревших методов платформа исполняет, хотя страницы в
/// справке уже нет.
///
/// Отдельный вид, а не [`Definition`]: тот описывает найденную сущность и
/// включает тип целиком, а здесь важно ровно одно — чем дополнить конкретный тип.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeMember {
    Method(Method),
    Property(Property),
}

impl TypeMember {
    /// Имя члена — по нему ищется одноимённый член в типе.
    pub fn name_ru(&self) -> &str {
        match self {
            TypeMember::Method(m) => &m.name_ru,
            TypeMember::Property(p) => &p.name_ru,
        }
    }
}

impl Definition {
    pub fn name_ru(&self) -> &str {
        match self {
            Definition::Method(m) => &m.name_ru,
            Definition::Property(p) => &p.name_ru,
            Definition::Type(t) => &t.name_ru,
        }
    }

    pub fn name_en(&self) -> &str {
        match self {
            Definition::Method(m) => &m.name_en,
            Definition::Property(p) => &p.name_en,
            Definition::Type(t) => &t.name_en,
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Definition::Method(m) => &m.description,
            Definition::Property(p) => &p.description,
            Definition::Type(t) => &t.description,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            Definition::Method(_) => "Method",
            Definition::Property(_) => "Property",
            Definition::Type(_) => "Type",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn availability(labels: &[&str]) -> Availability {
        Availability::new(
            labels
                .iter()
                .map(|label| AvailabilityMark::from_label(label))
                .collect(),
        )
    }

    #[test]
    fn side_groups_are_independent() {
        // Смешанный набор — норма справки, поэтому «клиент» и «сервер» выводятся
        // независимо, а не из одного флага.
        let mixed = availability(&["Тонкий клиент", "веб-клиент", "сервер", "толстый клиент"]);
        assert!(mixed.server_side());
        assert!(mixed.managed_client_side());
        assert!(mixed.ordinary_client_side());
        assert!(!mixed.has_unknown());

        let client_only = availability(&["Тонкий клиент", "веб-клиент", "толстый клиент"]);
        assert!(!client_only.server_side());
        assert!(client_only.managed_client_side());

        let server_only = availability(&[
            "Сервер",
            "внешнее соединение",
            "мобильный автономный сервер",
        ]);
        assert!(server_only.server_side());
        assert!(!server_only.managed_client_side());
        assert!(!server_only.ordinary_client_side());
        assert_eq!(
            server_only.labels(),
            "сервер, внешнее соединение, мобильный автономный сервер"
        );
    }

    #[test]
    fn integration_mark_makes_the_list_incomplete() {
        // Смысл «интеграции» не установлен (§10.3 плана): решать по такому
        // набору нельзя, иначе метод с единственной этой меткой дал бы находку.
        let integration_only = availability(&["интеграция"]);
        assert!(integration_only.has_unknown());
        assert!(!integration_only.server_side());
        assert!(!integration_only.managed_client_side());
    }

    #[test]
    fn unknown_label_is_kept_verbatim() {
        let unknown = Availability::new(vec![AvailabilityMark::from_label("новая метка")]);
        assert!(unknown.has_unknown());
        assert_eq!(unknown.labels(), "новая метка");
    }
}
