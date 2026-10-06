//! TOML-конфиг сервера. Минимальная схема под Phase 0; в Phase 1+ добавятся
//! поля для кеша индекса и других опций.

use std::path::{Path, PathBuf};

use bsl_validator::Profile;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Адрес для bind. По умолчанию loopback — наружу не торчим.
    pub host: String,

    /// Порт MCP-сервера. 8007 свободен после декомиссии bsl-platform-context (карточка #252).
    pub port: u16,

    /// Каталог установки 1С с файлом shcntx_ru.hbk внутри.
    /// На корпоративных машинах часто стоит несколько версий платформы
    /// (`C:\Program Files\1cv8\8.3.25.1257`, `\8.3.27.1786`, ...). Сервер
    /// автоматически НЕ выбирает — пользователь обязан явно указать каталог
    /// нужной версии, иначе на загрузке индекса будет понятная ошибка.
    pub platform_path: Option<PathBuf>,

    /// Каталог логов (service.YYYY-MM-DD.log + stdout/stderr — последние пишет run.bat).
    pub log_dir: PathBuf,

    /// Фильтр tracing — `info`, `debug`, или полный EnvFilter-выражение.
    pub log_level: String,

    /// Дефолтный уровень для `validate_module`, если клиент не передал параметр.
    ///
    /// `1` — статический анализ ссылок с явным именем типа в исходнике (низкий шум,
    /// безопасный дефолт). `2` — дополнительно локальный type inference в пределах
    /// процедуры (Phase 8 MVP — `Новый ТипX`, `ТипY.ЗначениеZ`, `// @type ТипX`).
    /// `3` — дополнительно return-type tracking (Уровень 2.5): тип переменной из
    /// возвращаемого типа метода/свойства, цепочки `Запрос.Выполнить().Выбрать()`,
    /// и реквизиты справочников/документов из метаданных конфигурации (при заданном
    /// `base`). Чем выше уровень — тем больше находок и потенциальных false-positive.
    ///
    /// Значение клампится в `[1..=3]` на чтении.
    pub default_validation_level: u8,

    /// Дефолтный профиль потребителя для `validate_module`, если клиент не
    /// передал параметр `profile` (карточка-decision #1230).
    ///
    /// `full` (дефолт) — все находки, `level` из параметра/конфига; рассчитан на
    /// сильную модель, которая сама отбросит сомнительные. `strict` — только
    /// high-confidence находки и форсированный `level=1`; для слабых моделей
    /// (LibreChat/DeepSeek), чтобы ложное срабатывание не приводило к зацикливанию.
    pub default_profile: Profile,

    /// Разрешённые значения заголовка `Host` для входящих запросов к `/mcp`
    /// (защита rmcp от DNS-rebinding). По умолчанию — только loopback.
    ///
    /// При сетевом деплое (`host = "0.0.0.0"`) сюда нужно добавить адрес, по
    /// которому клиенты обращаются к серверу (например, IP/имя хоста сервера),
    /// иначе rmcp вернёт `403 Forbidden: Host header is not allowed`. Запись без
    /// порта разрешает любой порт этого хоста.
    ///
    /// Origin-заголовок сервер не проверяет (у rmcp `allowed_origins` пуст):
    /// защита от браузерных cross-origin запросов держится на Host — браузерный
    /// DNS-rebinding шлёт чужой Host и получает 403.
    pub allowed_hosts: Vec<String>,

    /// Путь к файлу кэша собранного платформенного индекса.
    ///
    /// Разбор `shcntx_ru.hbk` занимает секунды на каждом старте — это холодный
    /// старт сервера. Готовый индекс сохраняется на диск и при следующем
    /// запуске читается без повторного разбора (см. `platform-index::cache`).
    ///
    /// - поле не задано — кэш включён, файл `<log_dir>/platform-index.cache`;
    /// - пустая строка `""` — кэш выключен, индекс каждый раз собирается из hbk;
    /// - иной путь — свой файл кэша (каталог создаётся при записи).
    ///
    /// Годность кэша проверяется отпечатком hbk и версией формата, поэтому
    /// смена версии платформы или сервера пересобирает его автоматически.
    pub platform_cache_path: Option<PathBuf>,

    /// Внешний источник имён методов конфигурации (см. крейт `symbol-source`).
    /// Нужен, чтобы `validate_module` не считал опиской вызовы процедур глобальных
    /// общих модулей и методов модуля объекта-владельца внешней обработки.
    pub symbol_source: SymbolSourceConfig,

    /// Несколько именованных источников имён — по одному на конфигурацию
    /// (`[[symbol_sources]]` в config.toml). Взаимоисключающе с одиночной
    /// секцией `[symbol_source]`: указаны обе — ошибка на старте.
    pub symbol_sources: Vec<SymbolSourceConfig>,

    /// Белый список инструментов. Пустой (по умолчанию) — доступны все.
    pub tools: ToolsConfig,
}

/// Конфигурация внешнего источника имён (крейт `symbol-source`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SymbolSourceConfig {
    /// "none" (по умолчанию) | "lite" | "code_index_db" | "code_index_mcp"
    pub kind: String,
    /// Абсолютный путь к базе: файл lite-индекса либо `<repo>/.code-index/index.db`.
    pub db_path: Option<PathBuf>,
    /// Корень выгрузки конфигурации. Нужен инструменту `rebuild_symbol_index`
    /// при `kind = "lite"` (из него собирается облегчённый индекс) и инструменту
    /// `validate_module` с параметром `path`: модуль читается только внутри этого
    /// корня. Проверка по тексту (`source`) файловую систему не трогает.
    pub root: Option<PathBuf>,
    /// URL MCP-сервера code-index, например http://127.0.0.1:8011/mcp
    pub url: Option<String>,
    /// Алиас конфигурации: это значение параметра `repo` у `validate_module` и
    /// `rebuild_symbol_index`. Обязателен в секциях `[[symbol_sources]]`. У
    /// одиночной секции `[symbol_source]` без него берётся алиас `default`.
    ///
    /// Для `kind = "code_index_mcp"` это же имя по умолчанию подставляется в запросы
    /// к code-index — совпадение алиасов норма, а не совпадение имён разных сущностей.
    pub repo: Option<String>,
    /// Имя репозитория в code-index, если оно отличается от алиаса конфигурации
    /// (`repo`). Только для `kind = "code_index_mcp"`. Не задано — берётся `repo`.
    pub code_index_repo: Option<String>,
    /// Как часто источник проверяет, не изменилась ли его база, мс. `0` — не
    /// проверять (снимок до переподключения).
    ///
    /// Нужно сценарию «добавил объект в выгрузку → пишу код, который к нему
    /// обращается → проверяю»: без проверки источник отвечает снимком на момент
    /// подключения, и только что созданный объект выглядит несуществующим
    /// (`unknown_metadata_object`) до ручного `reconnect_symbol_source`
    /// (issue #35). Соединение при этом живое и здоровое, само оно не
    /// переподключается.
    pub refresh_ms: u64,
    /// Таймаут HTTP, мс.
    pub timeout_ms: u64,
}

impl Default for SymbolSourceConfig {
    fn default() -> Self {
        Self {
            kind: "none".to_string(),
            db_path: None,
            root: None,
            url: None,
            repo: None,
            code_index_repo: None,
            refresh_ms: symbol_source::DEFAULT_DB_REFRESH_MS,
            timeout_ms: 5000,
        }
    }
}

impl SymbolSourceConfig {
    /// Имя репозитория, которое подставляется в запросы к code-index: явное
    /// `code_index_repo`, иначе алиас конфигурации.
    pub fn code_index_repo_effective(&self) -> Option<&str> {
        self.code_index_repo.as_deref().or(self.repo.as_deref())
    }

    /// Разрешить относительные пути источника от каталога конфига.
    fn resolve_paths(&mut self, base: &Path) {
        self.db_path = self.db_path.take().map(|p| resolve_relative(base, p));
        self.root = self.root.take().map(|p| resolve_relative(base, p));
    }

    /// Проверка обязательных полей по `kind`. Понятная ошибка на загрузке
    /// конфига вместо тихого падения источника при первом обращении.
    fn validate(&self) -> anyhow::Result<()> {
        match self.kind.as_str() {
            "none" => {
                // Секция есть, но источник не выбран: перечисленные поля молча
                // игнорировались бы — предупреждаем (в списке это ошибка).
                if self.db_path.is_some()
                    || self.root.is_some()
                    || self.url.is_some()
                    || self.code_index_repo.is_some()
                {
                    tracing::warn!(
                        "symbol_source.kind = \"none\": поля db_path/root/url/code_index_repo \
                         заданы, но не используются — укажите kind источника"
                    );
                }
                Ok(())
            }
            "lite" | "code_index_db" => {
                if self.db_path.is_none() {
                    anyhow::bail!(
                        "symbol_source.kind = \"{}\" требует symbol_source.db_path",
                        self.kind
                    );
                }
                Ok(())
            }
            "code_index_mcp" => {
                if self.url.is_none() {
                    anyhow::bail!(
                        "symbol_source.kind = \"code_index_mcp\" требует symbol_source.url"
                    );
                }
                if self.code_index_repo_effective().is_none() {
                    anyhow::bail!(
                        "symbol_source.kind = \"code_index_mcp\" требует repo (алиас конфигурации) \
                         либо code_index_repo"
                    );
                }
                if self.timeout_ms == 0 {
                    anyhow::bail!(
                        "symbol_source.timeout_ms = 0 недопустим: каждый запрос к code-index \
                         немедленно упирался бы в таймаут"
                    );
                }
                Ok(())
            }
            other => anyhow::bail!(
                "symbol_source.kind = \"{other}\" неизвестен. Допустимые значения: \
                 none, lite, code_index_db, code_index_mcp"
            ),
        }
    }
}

/// Относительный путь — от каталога конфига; пустой (им выключается кэш) и
/// абсолютный остаются как есть.
fn resolve_relative(base: &Path, path: PathBuf) -> PathBuf {
    if path.as_os_str().is_empty() || path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

/// Алиас, который получает одиночная секция `[symbol_source]` без явного `repo`.
pub const DEFAULT_SOURCE_NAME: &str = "default";

/// Белый список MCP-инструментов (`[tools]` в config.toml).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ToolsConfig {
    /// Имена разрешённых инструментов. Пустой список — фильтр выключен,
    /// доступны все. Пример: `enabled = ["validate_module"]` — сервер отдаёт
    /// и выполняет только валидацию модуля.
    pub enabled: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8007,
            platform_path: None,
            log_dir: PathBuf::from(r"C:\bsl-context-rs\logs"),
            log_level: "info".to_string(),
            default_validation_level: 1,
            default_profile: Profile::Full,
            allowed_hosts: vec![
                "localhost".to_string(),
                "127.0.0.1".to_string(),
                "::1".to_string(),
            ],
            platform_cache_path: None,
            symbol_source: SymbolSourceConfig::default(),
            symbol_sources: Vec::new(),
            tools: ToolsConfig::default(),
        }
    }
}

impl Config {
    /// Действующий путь кэша платформенного индекса: явный из конфига, иначе
    /// `<log_dir>/platform-index.cache`. `None` — кэш выключен пустой строкой.
    pub fn platform_cache_path_effective(&self) -> Option<PathBuf> {
        match &self.platform_cache_path {
            Some(p) if p.as_os_str().is_empty() => None,
            Some(p) => Some(p.clone()),
            None => Some(
                self.log_dir
                    .join(platform_index::cache::DEFAULT_CACHE_FILE_NAME),
            ),
        }
    }

    /// Загрузить конфиг из файла, либо вернуть дефолт.
    pub fn load_or_default(path: Option<&Path>) -> anyhow::Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let raw = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!(
                "read config {}: {} (файл должен быть в UTF-8: UTF-16/ANSI не поддерживаются)",
                path.display(),
                e
            )
        })?;
        let mut cfg: Config = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parse config {}: {}", path.display(), e))?;
        // Кламп уровня в безопасный диапазон, чтобы конфиг с опечаткой
        // (`level = 5`) не валил сервер и не приводил к скрытым ошибкам.
        cfg.default_validation_level = cfg.default_validation_level.clamp(1, 3);
        if cfg.port == 0 {
            anyhow::bail!("port = 0 недопустим: укажите порт 1..=65535");
        }
        // Пустой allowed_hosts rmcp трактует как «разрешить ЛЮБОЙ Host» —
        // защита от DNS-rebinding молча выключалась бы. Пустой список —
        // почти всегда недописанный конфиг: возвращаем loopback-дефолт.
        if cfg.allowed_hosts.is_empty() {
            tracing::warn!(
                "allowed_hosts пуст — rmcp принял бы любой Host; подставлен loopback-дефолт. \
                 Для сетевого деплоя перечислите адреса клиентов явно"
            );
            cfg.allowed_hosts = Self::default().allowed_hosts;
        }
        // Относительные пути — от каталога конфига, а не от CWD процесса
        // (у службы это System32: логи, кэш и база уезжали бы туда).
        let base = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        cfg.resolve_paths(&base);
        cfg.resolved_symbol_sources()?;
        Ok(cfg)
    }

    /// Разрешить относительные пути настроек от каталога конфига.
    fn resolve_paths(&mut self, base: &Path) {
        self.log_dir = resolve_relative(base, std::mem::take(&mut self.log_dir));
        self.platform_path = self.platform_path.take().map(|p| resolve_relative(base, p));
        self.platform_cache_path = self
            .platform_cache_path
            .take()
            .map(|p| resolve_relative(base, p));
        self.symbol_source.resolve_paths(base);
        for source in &mut self.symbol_sources {
            source.resolve_paths(base);
        }
    }

    /// Именованные источники имён: либо одна секция `[symbol_source]`, либо
    /// список `[[symbol_sources]]`. Возвращает пары (алиас, конфиг) — алиас и
    /// есть значение параметра `repo` у инструментов.
    pub fn resolved_symbol_sources(&self) -> anyhow::Result<Vec<(String, SymbolSourceConfig)>> {
        let single_present = self.symbol_source.kind != "none"
            || self.symbol_source.db_path.is_some()
            || self.symbol_source.root.is_some()
            || self.symbol_source.url.is_some()
            || self.symbol_source.repo.is_some()
            || self.symbol_source.code_index_repo.is_some();
        if !self.symbol_sources.is_empty() && single_present {
            anyhow::bail!(
                "укажите либо [symbol_source] (одна конфигурация), либо [[symbol_sources]] \
                 (несколько) — но не обе секции сразу"
            );
        }
        if !self.symbol_sources.is_empty() {
            let mut seen = std::collections::BTreeSet::new();
            let mut result = Vec::with_capacity(self.symbol_sources.len());
            for entry in &self.symbol_sources {
                let name = match entry.repo.as_deref() {
                    Some(n) if !n.is_empty() => n,
                    _ => {
                        anyhow::bail!("каждая секция [[symbol_sources]] требует непустое поле repo")
                    }
                };
                if !seen.insert(name.to_string()) {
                    anyhow::bail!("повторяющийся repo в [[symbol_sources]]: \"{name}\"");
                }
                // kind = "none" в списке — явно отключённый источник (алиас
                // остаётся виден в symbol_sources_status). Недописанным конфигом
                // считаем только запись с полями источника: их молча
                // игнорировать нельзя.
                if entry.kind == "none" {
                    if entry.db_path.is_some()
                        || entry.root.is_some()
                        || entry.url.is_some()
                        || entry.code_index_repo.is_some()
                    {
                        anyhow::bail!(
                            "секция [[symbol_sources]] repo = \"{name}\": kind = \"none\", \
                             но заданы поля источника — укажите kind или уберите поля"
                        );
                    }
                    tracing::warn!(
                        source = %name,
                        "источник [[symbol_sources]] отключён (kind = \"none\")"
                    );
                }
                entry.validate()?;
                result.push((name.to_string(), entry.clone()));
            }
            return Ok(result);
        }
        if self.symbol_source.kind == "none" {
            // Одиночная секция с полями источника — тот же недописанный
            // конфиг, что и в списке `[[symbol_sources]]`: молча игнорировать
            // нельзя (аудит PR: предупреждение `validate()` сюда не доходило).
            if self.symbol_source.db_path.is_some()
                || self.symbol_source.root.is_some()
                || self.symbol_source.url.is_some()
                || self.symbol_source.code_index_repo.is_some()
            {
                anyhow::bail!(
                    "[symbol_source]: kind = \"none\", но заданы поля источника — \
                     укажите kind или уберите поля"
                );
            }
            return Ok(Vec::new());
        }
        self.symbol_source.validate()?;
        let name = self
            .symbol_source
            .repo
            .clone()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| DEFAULT_SOURCE_NAME.to_string());
        Ok(vec![(name, self.symbol_source.clone())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_whitelist_parsed_from_toml() {
        let cfg: Config = toml::from_str("[tools]\nenabled = [\"validate_module\"]\n").unwrap();
        assert_eq!(cfg.tools.enabled, vec!["validate_module".to_string()]);
    }

    #[test]
    fn tools_section_absent_means_empty_whitelist() {
        let cfg: Config = toml::from_str("port = 8007\n").unwrap();
        assert!(cfg.tools.enabled.is_empty());
    }

    #[test]
    fn symbol_source_root_parsed() {
        let cfg: Config = toml::from_str(
            "[symbol_source]\nkind = \"lite\"\ndb_path = \"a.db\"\nroot = \"C:/Repo1C\"\n",
        )
        .unwrap();
        assert_eq!(
            cfg.symbol_source.root.as_deref(),
            Some(std::path::Path::new("C:/Repo1C"))
        );
    }

    #[test]
    fn symbol_sources_list_parsed_with_names() {
        let cfg: Config = toml::from_str(
            "[[symbol_sources]]\n\
             repo = \"ut\"\n\
             kind = \"lite\"\n\
             db_path = \"ut.db\"\n\
             \n\
             [[symbol_sources]]\n\
             repo = \"bp\"\n\
             kind = \"code_index_mcp\"\n\
             url = \"http://127.0.0.1:8011/mcp\"\n",
        )
        .unwrap();
        let resolved = cfg.resolved_symbol_sources().unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].0, "ut");
        assert_eq!(resolved[1].0, "bp");
    }

    #[test]
    fn legacy_single_section_gets_default_name() {
        let cfg: Config =
            toml::from_str("[symbol_source]\nkind = \"lite\"\ndb_path = \"a.db\"\n").unwrap();
        let resolved = cfg.resolved_symbol_sources().unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, DEFAULT_SOURCE_NAME);
    }

    #[test]
    fn both_sections_rejected() {
        let cfg: Config = toml::from_str(
            "[symbol_source]\n\
             kind = \"lite\"\n\
             db_path = \"a.db\"\n\
             \n\
             [[symbol_sources]]\n\
             repo = \"ut\"\n\
             kind = \"lite\"\n\
             db_path = \"ut.db\"\n",
        )
        .unwrap();
        assert!(cfg.resolved_symbol_sources().is_err());
    }

    #[test]
    fn duplicate_source_names_rejected() {
        let cfg: Config = toml::from_str(
            "[[symbol_sources]]\n\
             repo = \"ut\"\n\
             kind = \"lite\"\n\
             db_path = \"ut1.db\"\n\
             \n\
             [[symbol_sources]]\n\
             repo = \"ut\"\n\
             kind = \"lite\"\n\
             db_path = \"ut2.db\"\n",
        )
        .unwrap();
        assert!(cfg.resolved_symbol_sources().is_err());
    }

    #[test]
    fn nameless_entry_in_list_rejected() {
        let cfg: Config = toml::from_str(
            "[[symbol_sources]]\n\
             kind = \"lite\"\n\
             db_path = \"ut.db\"\n",
        )
        .unwrap();
        assert!(cfg.resolved_symbol_sources().is_err());
    }

    #[test]
    fn code_index_repo_defaults_to_alias() {
        let cfg = SymbolSourceConfig {
            kind: "code_index_mcp".to_string(),
            url: Some("http://127.0.0.1:8011/mcp".to_string()),
            repo: Some("zup".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.code_index_repo_effective(), Some("zup"));

        let cfg = SymbolSourceConfig {
            code_index_repo: Some("zup-prod".to_string()),
            ..cfg
        };
        assert_eq!(cfg.code_index_repo_effective(), Some("zup-prod"));
    }

    #[test]
    fn cache_path_defaults_into_log_dir() {
        let cfg = Config::default();
        assert_eq!(
            cfg.platform_cache_path_effective(),
            Some(
                cfg.log_dir
                    .join(platform_index::cache::DEFAULT_CACHE_FILE_NAME)
            )
        );
    }

    #[test]
    fn cache_path_empty_disables_cache() {
        let cfg: Config = toml::from_str("platform_cache_path = \"\"\n").unwrap();
        assert_eq!(cfg.platform_cache_path_effective(), None);
    }

    #[test]
    fn cache_path_explicit_wins() {
        let cfg: Config = toml::from_str("platform_cache_path = 'C:/tmp/pc.cache'\n").unwrap();
        assert_eq!(
            cfg.platform_cache_path_effective(),
            Some(PathBuf::from("C:/tmp/pc.cache"))
        );
    }

    #[test]
    fn empty_allowed_hosts_restored_to_loopback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "allowed_hosts = []\n").unwrap();
        let cfg = Config::load_or_default(Some(&path)).unwrap();
        assert_eq!(cfg.allowed_hosts, Config::default().allowed_hosts);
    }

    #[test]
    fn relative_paths_resolve_against_config_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "log_dir = \"logs\"\nplatform_path = \"1cv8\"\n").unwrap();
        let cfg = Config::load_or_default(Some(&path)).unwrap();
        assert_eq!(cfg.log_dir, dir.path().join("logs"));
        assert_eq!(
            cfg.platform_path.as_deref(),
            Some(dir.path().join("1cv8").as_path())
        );
    }

    #[test]
    fn port_zero_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "port = 0\n").unwrap();
        assert!(Config::load_or_default(Some(&path)).is_err());
    }

    #[test]
    fn list_entry_with_kind_none_is_disabled_not_error() {
        // Явно отключённый источник: алиас остаётся видимым для
        // symbol_sources_status, ошибки нет.
        let cfg: Config =
            toml::from_str("[[symbol_sources]]\nrepo = \"ut\"\nkind = \"none\"\n").unwrap();
        let resolved = cfg.resolved_symbol_sources().unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, "ut");
    }

    #[test]
    fn list_entry_with_kind_none_and_fields_is_error() {
        // kind = "none" с полями источника — недописанный конфиг.
        let cfg: Config = toml::from_str(
            "[[symbol_sources]]\nrepo = \"ut\"\nkind = \"none\"\ndb_path = \"ut.db\"\n",
        )
        .unwrap();
        assert!(cfg.resolved_symbol_sources().is_err());
    }

    #[test]
    fn single_section_kind_none_with_fields_is_error() {
        // Та же недописанная секция, но одиночная: раньше поля молча
        // игнорировались, а источник считался отключённым (аудит PR).
        let cfg: Config =
            toml::from_str("[symbol_source]\nkind = \"none\"\ndb_path = \"ut.db\"\n").unwrap();
        let err = cfg.resolved_symbol_sources().unwrap_err().to_string();
        assert!(err.contains("kind = \"none\""), "{err}");
    }

    #[test]
    fn single_section_kind_none_without_fields_is_disabled() {
        let cfg: Config = toml::from_str("[symbol_source]\nkind = \"none\"\n").unwrap();
        assert!(cfg.resolved_symbol_sources().unwrap().is_empty());
    }

    #[test]
    fn empty_repo_in_single_section_gets_default_name() {
        let cfg: Config =
            toml::from_str("[symbol_source]\nkind = \"lite\"\ndb_path = \"a.db\"\nrepo = \"\"\n")
                .unwrap();
        let resolved = cfg.resolved_symbol_sources().unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, DEFAULT_SOURCE_NAME);
    }

    #[test]
    fn code_index_mcp_zero_timeout_rejected() {
        let cfg: Config = toml::from_str(
            "[symbol_source]\nkind = \"code_index_mcp\"\nurl = \"http://x/mcp\"\n\
             repo = \"r\"\ntimeout_ms = 0\n",
        )
        .unwrap();
        assert!(cfg.resolved_symbol_sources().is_err());
    }
}
