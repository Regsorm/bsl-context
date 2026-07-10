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

high-confidence findings have a false-positive rate near zero; low-confidence ones
depend on the accuracy of type inference and the completeness of the `hbk`.

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

The validator sees the text of a SINGLE module plus the platform context. It
does not know the configuration's metadata, so it cannot verify that
application procedures declared in other modules exist. The `undeclared_method`
finding is suppressed when the name may come from outside:

- an ordinary (non-managed) form module calls its owner's object module export
  methods without a prefix;
- an extension module calls procedures of the module it extends (such a module
  is recognized by the `&Перед`, `&После`, `&Вместо`, `&ИзменениеИКонтроль`
  directives, and strict checking is disabled for it);
- a global common module (`Глобальный = Истина`) is called without a prefix
  from any module.

The first and third cases cannot be recognized from the module text alone —
false positives for `undeclared_method` are possible there. The `strict`
profile will not filter them out (the finding has `high` confidence); when
working with such modules it's better not to rely on this finding.

### Profiles

The `profile` parameter (or `default_profile` in the config):

- **`full`** (default) — all findings, `level` as passed. For a strong model that
  discards questionable findings itself.
- **`strict`** — only high-confidence findings and a forced `level=1`. For weaker
  models, so a false positive does not cause a feedback loop.

## Architecture

A Cargo workspace of five crates:

| Crate | Purpose |
|-------|---------|
| `hbk-reader` | Reads the binary `shcntx_ru.hbk` container |
| `hbk-parser` | Parses help HTML pages (types, methods, enumerations) |
| `platform-index` | Platform index: loading, storage, search |
| `bsl-validator` | BSL expression validator (tree-sitter) |
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
