//! BSL-валидатор: проверки кода и платформенного контекста.
//!
//! Уровни: точечные `validate_enum`/`validate_method_call`; выражение
//! (`validate_expression`) с разбором дерева; целый модуль (`validate_module*`)
//! — объявления, директивы, теней контекста, правила запросов и выражения.
//! Разбор BSL вынесен в крейт `bsl-parse`.

pub mod blocks;
pub mod check;
pub mod config_objects;
pub mod context_names;
pub mod declarations;
pub mod directives;
pub mod enum_values;
pub mod expression;
pub mod homoglyphs;
pub(crate) mod locals;
pub mod module;
pub mod module_context;
pub mod query_rules;
pub mod scope;
pub mod symbols;

/// Единый слой разбора вынесен в отдельный крейт: им пользуется и индексатор кода.
pub use bsl_parse::{module_declarations, module_declarations_split, normalize_for_parser};
pub mod ast {
    //! Совместимость: разбор переехал в крейт `bsl-parse`.
    pub use bsl_parse::{collect_facts, normalize_for_parser};
}
pub use check::{
    validate_enum, validate_method_call, EnumValidation, MethodCallValidation, SignatureBrief,
    SimilarValue,
};
pub use context_names::{is_form_module, FORM_TYPE};
pub use expression::{
    validate_expression, validate_expression_at_level, validate_expression_with_profile,
    Confidence, ExprError, ExprErrorKind, ExpressionValidation, Profile,
};
pub use module::{
    validate_module, validate_module_at_level, validate_module_degraded,
    validate_module_with_profile, validate_module_with_symbols,
    validate_module_with_symbols_and_form_kind,
};
pub use scope::{extract_scope_map, extract_type_annotations, Scope, ScopeMap, VarBinding};
pub use symbols::{ObjectField, ObjectSchema, SymbolSource};
