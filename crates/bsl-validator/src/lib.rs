//! BSL-валидатор.
//!
//! Phase 5 — точечные проверки `validateEnum` и `validateMethodCall` без парсера.
//! Phase 6 (отдельный модуль `expression`) — `validateExpression` через tree-sitter.

pub mod check;
pub mod directives;
pub mod expression;
pub mod module;
pub mod scope;

pub use check::{
    validate_enum, validate_method_call, EnumValidation, MethodCallValidation, SimilarValue,
    SignatureBrief,
};
pub use expression::{
    validate_expression, validate_expression_at_level, validate_expression_with_profile,
    Confidence, ExprError, ExprErrorKind, ExpressionValidation, Profile,
};
pub use module::{
    validate_module, validate_module_at_level, validate_module_with_profile,
};
pub use scope::{extract_scope_map, extract_type_annotations, Scope, ScopeMap};
