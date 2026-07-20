//! BSL-валидатор.
//!
//! Phase 5 — точечные проверки `validateEnum` и `validateMethodCall` без парсера.
//! Phase 6 (отдельный модуль `expression`) — `validateExpression` через tree-sitter.

pub mod check;
pub mod config_objects;
pub mod context_names;
pub mod directives;
pub mod expression;
pub mod module;
pub mod query_rules;
pub mod scope;
pub mod symbols;

/// Единый слой разбора вынесен в отдельный крейт: им пользуется и индексатор кода.
pub use bsl_parse::{module_declarations, module_declarations_split, normalize_for_parser};
pub mod ast {
    //! Совместимость: разбор переехал в крейт `bsl-parse`.
    pub use bsl_parse::normalize_for_parser;
}
pub use check::{
    validate_enum, validate_method_call, EnumValidation, MethodCallValidation, SimilarValue,
    SignatureBrief,
};
pub use context_names::{is_form_module, FORM_TYPE};
pub use expression::{
    validate_expression, validate_expression_at_level, validate_expression_with_profile,
    Confidence, ExprError, ExprErrorKind, ExpressionValidation, Profile,
};
pub use module::{
    validate_module, validate_module_at_level, validate_module_with_profile,
    validate_module_with_symbols,
};
pub use scope::{extract_scope_map, extract_type_annotations, Scope, ScopeMap};
pub use symbols::{ObjectField, ObjectSchema, SymbolSource};
