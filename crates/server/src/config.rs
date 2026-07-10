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
    pub allowed_hosts: Vec<String>,

    /// Внешний источник имён методов конфигурации (см. крейт `symbol-source`).
    /// Нужен, чтобы `validate_module` не считал опиской вызовы процедур глобальных
    /// общих модулей и методов модуля объекта-владельца внешней обработки.
    pub symbol_source: SymbolSourceConfig,

    /// Белый список инструментов. Пустой (по умолчанию) — доступны все.
    pub tools: ToolsConfig,
}

/// Конфигурация внешнего источника имён (крейт `symbol-source`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SymbolSourceConfig {
    /// "none" (по умолчанию) | "lite" | "code_index_db" | "code_index_mcp"
    pub kind: String,
    /// Абсолютный путь к базе: файл lite-индекса либо `<repo>/.code-index/index.db`.
    pub db_path: Option<PathBuf>,
    /// Корень выгрузки конфигурации. Нужен ТОЛЬКО инструменту `rebuild_symbol_index`
    /// при `kind = "lite"`: из него собирается облегчённый индекс. Валидация имён
    /// файловую систему не трогает — читает только базу/сервис.
    pub root: Option<PathBuf>,
    /// URL MCP-сервера code-index, например http://127.0.0.1:8011/mcp
    pub url: Option<String>,
    /// Алиас репозитория для MCP-источника, например "ut-test".
    pub repo: Option<String>,
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
            timeout_ms: 5000,
        }
    }
}

impl SymbolSourceConfig {
    /// Проверка обязательных полей по `kind`. Понятная ошибка на загрузке
    /// конфига вместо тихого падения источника при первом обращении.
    fn validate(&self) -> anyhow::Result<()> {
        match self.kind.as_str() {
            "none" => Ok(()),
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
                if self.repo.is_none() {
                    anyhow::bail!(
                        "symbol_source.kind = \"code_index_mcp\" требует symbol_source.repo"
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
            symbol_source: SymbolSourceConfig::default(),
            tools: ToolsConfig::default(),
        }
    }
}

impl Config {
    /// Загрузить конфиг из файла, либо вернуть дефолт.
    pub fn load_or_default(path: Option<&Path>) -> anyhow::Result<Self> {
        let Some(path) = path else { return Ok(Self::default()) };
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read config {}: {}", path.display(), e))?;
        let mut cfg: Config = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parse config {}: {}", path.display(), e))?;
        // Кламп уровня в безопасный диапазон, чтобы конфиг с опечаткой
        // (`level = 5`) не валил сервер и не приводил к скрытым ошибкам.
        cfg.default_validation_level = cfg.default_validation_level.clamp(1, 3);
        cfg.symbol_source.validate()?;
        Ok(cfg)
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
        let cfg: Config = toml::from_str("[symbol_source]\nkind = \"lite\"\ndb_path = \"a.db\"\nroot = \"C:/RepoUT\"\n").unwrap();
        assert_eq!(cfg.symbol_source.root.as_deref(), Some(std::path::Path::new("C:/RepoUT")));
    }
}
