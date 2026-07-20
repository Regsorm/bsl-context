# bsl-context

[Русский](README_RU.md) | **English**

<a href="https://infostart.ru/1c/articles/2698363/" title="Published on Infostart">
  <img src="https://infostart.ru/bitrix/templates/sandbox_empty/assets/tpl/abo/img/logo.svg" alt="Infostart" height="32">
</a>

Published on Infostart: [bsl-context — проверка ИИ-кода 1С на соответствие API платформы](https://infostart.ru/1c/articles/2698363/)

An MCP server providing **1C:Enterprise 8.3** platform context: types, methods,
properties, constructors, system enumeration values — plus static validation of
BSL expressions against a real platform index.

The data source is the platform's syntax assistant (`shcntx_ru.hbk`), parsed by a
custom reader (running 1C is not required).

## Why

Language models and linters handle BSL syntax well but are "blind" to referential
correctness against the platform: whether a system enumeration value exists,
whether a platform type has a given method, whether a global function's argument
count fits its overloads. `bsl-context` covers exactly that layer — it checks code
against the actual API of a specific platform version.

## Features

**Reference tools** — search and details for platform types, methods, properties,
constructors, and enumeration values.

**Module/fragment validation** (`validate_module`) — accepts either a whole
module or an arbitrary fragment; via `tree-sitter-bsl` the server
extracts `Процедура`/`Функция` declarations from the submitted text and does
not treat their calls as typos of platform methods (for a fragment this set is
simply empty); it also validates compiler/extension directive names. Returns
findings with line, column, kind, and confidence:

| Finding kind | confidence | Meaning |
|--------------|-----------|---------|
| `unknown_enum_value` | high | System enumeration value does not exist |
| `wrong_argument_count` | high | Global function argument count outside its overloads |
| `unknown_type_member` | low | Platform type has no such method/property |
| `unknown_new_type` | low | `Новый TypeX` constructor unknown to the platform |
| `unknown_global_method` | high / low | Unknown global call similar to a platform method (fuzzy: strong match → high, weak → low) |
| `undeclared_method` | high | Call is not declared in the submitted module and unknown to the platform (whole-module check only; suppressed in extension modules) |
| `unknown_directive` | high / low | Directive name (`&НаСервере`, `&Перед`, …) not in the whitelist |
| `shadowed_context_name` | high | Variable name is taken by a read-only context property: the assignment fails at runtime. The form-member rule needs `module_path` |
| `unknown_common_module` | high | `ModuleName.Method(...)` — no common module with that name exists in the configuration. Requires an external name source |
| `unknown_metadata_object` | high | `Справочники.Name`, `Документы.Name`, … — no object with that name exists in the collection. Requires an external name source |
| `temp_table_without_index` | high | A temporary table takes part in a join but has no `ИНДЕКСИРОВАТЬ ПО` |
| `or_in_join_condition` | high | `ИЛИ` splits a join condition, so no index can be used |
| `join_with_subquery` | low | Join with a subquery instead of an indexed temporary table |
| `physical_register_table` | low | Reading a balance register's movement table instead of its virtual table. Requires an external name source |
| `virtual_table_without_filter` | low | A virtual table is called without a filter on its dimensions. Requires an external name source |
| `join_on_unindexed_field` | low | The joined table has neither a standard index nor «Индексировать» on the join field. Requires an external name source |

high-confidence findings have a false-positive rate near zero; low-confidence ones
depend on the accuracy of type inference and the completeness of the `hbk`.

### Query optimality

The last six findings come from parsing the 1C query language inside string
literals. A query whose text is built by concatenation with variables, or one the
parser could not read, is not analysed at all — an incomplete parse must not
produce findings. On the UT configuration (14905 modules) 97.3% of the 23260
queries found in the code are parsed.

### Validation levels

Analysis depth is set via the `level` parameter (or `default_validation_level` in
the config), clamped to `[1..=3]`:

- **1** — references with an explicit type name in the source (`Новый TypeX`,
  `TypeY.ValueZ`, global function argument counts). Low noise, safe default.
- **2** — additionally, local type inference within a procedure:
  `X = Новый TypeX`, `X = TypeY.ValueZ`, the `// @type TypeX` annotation.
- **3** — additionally, return-type tracking: a variable's type from the return
  type of a method/property, including chains like `Query.Execute().Select()`.

The higher the level, the more findings — and the more potential false positives.

### What the validator cannot know

The validator sees the text of a SINGLE module plus the platform context. About
the configuration it knows exactly what the external name source tells it (see
"External configuration name source"): it does know the set of objects (hence
`unknown_common_module`, `unknown_metadata_object` and the query optimality
rules). What no source knows is the VISIBILITY RULES for application procedures:
whether a procedure is exported, whether its module is reachable from here,
whether the context matches (server/client).

So an `undeclared_method` finding has three different fates when the name comes
from outside the module:

| Case | What the name source does |
|------|---------------------------|
| An export method of a GLOBAL common module (`Глобальный = Истина`) — called without a prefix from anywhere | the finding is **dropped entirely** |
| An export method of the owner object module of an external data processor — visible to an ordinary form module without a prefix (requires `module_path`) | the finding is **dropped entirely** |
| The name is declared somewhere in the configuration, but whether it is visible from here is unknown | the finding **stays with `low` confidence**, i.e. it is hidden in the `strict` profile |

Extension modules are a case of their own: they call procedures of the module
they extend, are recognized by the `&Перед`, `&После`, `&Вместо`,
`&ИзменениеИКонтроль` directives, and strict checking is disabled for them
entirely — no name source involved.

Without a name source none of the three cases is recognized: findings keep
`high` confidence and the `strict` profile will not filter them out. How much
this changes the picture — see the measurement in "External configuration name
source".

Likewise, the validator does not know the form's set of attributes. An attribute
shadows a context name (UT has forms with attributes named `Метаданные`,
`БезопасноеХранилище`), so inside a form module the `shadowed_context_name`
finding for global-context names is off by default. Pass the attribute list via
`form_attributes` and it works there too: attribute names are excluded, the rest
are checked.

### Profiles

The `profile` parameter (or `default_profile` in the config):

- **`full`** (default) — all findings, `level` as passed. For a strong model that
  discards questionable findings itself.
- **`strict`** — only high-confidence findings and a forced `level=1`. For weaker
  models, so a false positive does not cause a feedback loop.

## Architecture

A Cargo workspace of nine crates:

| Crate | Purpose |
|-------|---------|
| `hbk-reader` | Reads the binary `shcntx_ru.hbk` container |
| `hbk-parser` | Parses help HTML pages (types, methods, enumerations) |
| `platform-index` | Platform index: loading, storage, search |
| `bsl-parse` | BSL parsing on top of tree-sitter: procedures, calls, query texts |
| `sdbl-parse` | Parser for the 1C query language (SDBL), recursive descent |
| `bsl-validator` | BSL expression validator and query optimality rules |
| `lite-index` | Built-in lightweight index of configuration names (SQLite) |
| `symbol-source` | Three configuration name sources: `lite`, `code_index_db`, `code_index_mcp` |
| `server` | HTTP MCP server (axum + rmcp), config, PID lock |

## Requirements

- Rust (edition 2021), built with `cargo build --release`.
- The `shcntx_ru.hbk` file from an installed 1C:Enterprise platform
  (`C:\Program Files\1cv8\<version>\bin\shcntx_ru.hbk`). Not included in the repo.

## Build

```bash
cargo build --release
```

The binary is `target/release/bsl-context-rs` (`.exe` on Windows).

## Configuration

Copy [`configs/config.toml.example`](configs/config.toml.example) to
`configs/config.toml` and adjust it for your machine. Key fields:

```toml
host = "127.0.0.1"          # bind, loopback by default
port = 8007                 # MCP server port
platform_path = 'C:\Program Files\1cv8\8.3.27.1786'   # platform version directory
default_validation_level = 1
```

### Choosing the platform version when several are installed

If multiple platform versions are installed side by side, the server does **not**
pick a version automatically — the path is set explicitly via `platform_path`.
Inside that directory it looks for `shcntx_ru.hbk` at two paths:
`<platform_path>/shcntx_ru.hbk` and `<platform_path>/bin/shcntx_ru.hbk`.

This is deliberate: method signatures and the set of system enumerations differ
between platform versions, so code must be validated against the version it is
written for. If `platform_path` is unset, the server starts and `/health`
responds, but the MCP tools return `503` with a hint to set the path.

### Network deployment

By default the server listens on loopback. With `host = "0.0.0.0"` you must add
the external address to `allowed_hosts` (rmcp's DNS-rebinding protection),
otherwise networked requests get `403 Forbidden: Host header is not allowed`:

```toml
allowed_hosts = ["localhost", "127.0.0.1", "::1", "<server-ip>"]
```

### External source of configuration names

The validator receives the text of a SINGLE module, so a call to a procedure declared
in another file of the configuration looks like a typo to it. A symbol source fixes
that: it answers three questions — "does the configuration declare such a method",
"is it an exported method of a GLOBAL common module", and "which exported methods does
the owner object module of an external data processor have".

Measured on the UT configuration (14905 modules): without a source — 1420 high-confidence
`undeclared_method` findings; with one — 44, of which exactly one is real.

Three ways to connect it, same result:

```toml
# 1. Own lightweight index. The server stays self-contained.
[symbol_source]
kind = "lite"
root = 'C:\RepoUT'                          # configuration dump directory
db_path = 'C:\tools\bsl-context\ut_lite.db' # database file; the directory is created for you
```

The database is built by the `rebuild_symbol_index` tool — call it after the first start
and after every change to the configuration. It takes no parameters: the paths come from
the config. While it runs, validation keeps using the previous database; if the build
fails, the previous database stays intact. The same can be done from the command line:
`bsl-lite-index build --root <dump> --db <file.db>` (the directory must already exist).

```toml
# 2. Reading the code-index database directly (same machine only).
[symbol_source]
kind = "code_index_db"
db_path = 'C:\RepoUT\.code-index\index.db'
```

```toml
# 3. Through a running code-index service — any machine, by address and port.
[symbol_source]
kind = "code_index_mcp"
url = "http://127.0.0.1:8011/mcp"
repo = "ut"
timeout_ms = 5000
```

With no section (or `kind = "none"`) the validator behaves as before, knowing nothing
about the configuration.

How they differ. `lite` needs nothing external and answers from memory, but its database
must be rebuilt after the configuration changes. The `code-index` sources read from that
index instead — if a file watcher keeps it current, the names are always fresh. The
"global common module" flag is taken from the module's XML, which `code-index` stores verbatim.

They also differ in speed. Measured on a 261,548-function index: a direct SQLite lookup
takes 13 µs (142 ns once the names are held in memory), a networked `code-index` request
13.4 ms. The validator checks hundreds of calls per module, so prefer `lite` or
`code_index_db`; use `code_index_mcp` when the index lives on another machine.

### Several configurations on one server

One server can serve several configurations at once, each with its own access method.
Instead of the single `[symbol_source]` section, list them:

```toml
[[symbol_sources]]
repo = "ut"                    # configuration alias — this is the tools' `repo` argument
kind = "lite"
root = 'C:\RepoUT'
db_path = 'C:\tools\bsl-context\ut_lite.db'

[[symbol_sources]]
repo = "bp"
kind = "code_index_db"
db_path = 'C:\RepoBP\.code-index\index.db'

[[symbol_sources]]
repo = "zup"
kind = "code_index_mcp"
url = "http://10.0.0.5:8011/mcp"
code_index_repo = "zup-prod"   # optional: only when code-index names that repository differently
```

`validate_module` and `rebuild_symbol_index` then take a `repo` argument — that alias.
**It is required whenever at least one configuration is configured**, even a single one:
a call must be unambiguous. An unknown alias is refused with the list of available ones.
`[symbol_source]` and `[[symbol_sources]]` are mutually exclusive.

With no configuration set up at all the argument is not needed: code is checked against
the platform reference only.

### What happens when a symbol source is unavailable

A source that answers "no such method" to everything is worse than no source at all: the
validator turns that into a high-confidence `undeclared_method` finding on every single
procedure call. Hence:

- **On connect**, a `code_index_mcp` source asks `code-index` for the stats of its own
  repository. If `code-index` does not know it, the source is not created and the log gets
  an explicit error listing the available repositories.
- **A configuration is declared but its source failed to come up** — `validate_module` for
  that alias refuses and explains why (for `lite`: "index not built, call
  `rebuild_symbol_index`") instead of emitting findings that are certainly false.
- **A source dies mid-flight** (network, `code-index` down) — it is marked unhealthy, the
  empty answer is not cached, and validation for that configuration refuses until recovery.

### Tool whitelist

If you only need part of the server's surface, list the tools you want in the
`[tools]` section. For example, module validation plus two lookup helpers:

```toml
[tools]
enabled = ["validate_module", "get_constructors", "get_enum_values"]
```

A missing section or an empty list means all eleven tools are available, as before.
Hidden tools are absent from `tools/list` and are rejected on a direct call.
An unknown name does not break startup: it produces a warning in the log and the
tool simply never appears.

## Running

```bash
bsl-context-rs --config /path/to/config.toml
```

Healthcheck — `GET http://127.0.0.1:8007/health` (no MCP handshake required).

## MCP tools

Transport — Streamable HTTP at `http://127.0.0.1:8007/mcp` (stateless).

| Tool | Purpose |
|------|---------|
| `search` | Fuzzy search across types, global methods, properties |
| `info` | Details by exact name |
| `get_member` | A specific method/property of a type |
| `get_members` | All members of a type (methods + properties + enum values) |
| `get_constructors` | A type's constructors with signatures |
| `get_enum_values` | Values of a system enumeration |
| `validate_enum` | Validate an enumeration value |
| `validate_method_call` | Validate a global function's argument count |
| `validate_module` | Validate BSL code (whole module or fragment) against the platform |
| `rebuild_symbol_index` | Rebuild the own name index (`kind = "lite"`); paths come from the config |
| `reserved_names` | Context-occupied names from the platform help: `global_readonly`/`form_readonly` (assignment fails at runtime), `global_writable`/`form_writable` (no variable is created — the session or the form is silently changed) |

## Connecting an MCP client

```json
{
  "mcpServers": {
    "bsl-context": {
      "type": "http",
      "url": "http://127.0.0.1:8007/mcp"
    }
  }
}
```

## Changelog

See [CHANGELOG.md](CHANGELOG.md) (in Russian).

## License

[MIT](LICENSE).
