//! Faithful port of the Babel plugin's `transformCondition`
//! (`shared/utils.ts`). Shared between the dom and universal generates so the
//! two can't drift; each generate supplies its memo import bookkeeping through
//! [`ConditionBuilder`].

use oxc_allocator::{Allocator, CloneIn};
use oxc_ast::ast::{Expression, FormalParameterKind};
use oxc_span::{GetSpan, Span};
use oxc_syntax::operator::{BinaryOperator, LogicalOperator, UnaryOperator};

use crate::shared::ast::{arrow_return_expression, expression_to_argument, variable_statement};
use crate::shared::ast_builder::AstBuilder;
use crate::shared::classify::Classify;

pub(crate) trait ConditionBuilder<'a> {
    fn condition_allocator(&self) -> &'a Allocator;
    /// Whether `memoWrapper` is configured; when false the hoisted memo
    /// declaration falls back to a plain thunk, mirroring Babel.
    fn memo_wrapper_enabled(&self) -> bool;
    /// Marks the memo helper as used and returns its local identifier.
    fn register_memo(&mut self) -> String;
    /// The built-in DOM wrapper's owner helper, when available.
    fn register_prop_condition_owner(&mut self) -> Option<String> {
        None
    }
    /// Fresh identifier for a hoisted memoized condition (Babel's `_c$` uid).
    fn next_condition_id(&mut self) -> String;
    /// The shared classification authority (Babel's `isDynamic` probes on
    /// tests and branches consult the same marker/namespace rules as child
    /// holes).
    fn classify(&self) -> Classify<'_>;
}

/// Result of `transform_condition`: Babel returns either a
/// `[varDeclaration, arrowFunction]` statement pair (non-inline with a
/// dynamic test) or a plain arrow function.
pub(crate) enum TransformedCondition<'a> {
    Hoisted {
        memo_statement: oxc_ast::ast::Statement<'a>,
        getter: Expression<'a>,
    },
    Arrow(Expression<'a>),
}

impl<'a> TransformedCondition<'a> {
    /// Collapses to a single expression: the hoisted form becomes the
    /// `(() => { var _c$ = memo(...); return () => ...; })()` IIFE that the
    /// dom generate inserts, the plain form stays an arrow.
    pub(crate) fn into_expression(self, allocator: &'a Allocator, span: Span) -> Expression<'a> {
        match self {
            TransformedCondition::Hoisted {
                memo_statement,
                getter,
            } => {
                let ast = AstBuilder::new(allocator);
                let mut statements = ast.vec();
                statements.push(memo_statement);
                statements.push(ast.statement_return(span, Some(getter)));
                let iife = crate::shared::ast::arrow_iife(allocator, span, statements);
                ast.expression_call(span, iife, None, ast.vec(), false)
            }
            TransformedCondition::Arrow(arrow) => arrow,
        }
    }
}

/// The `Conditional`/`Logical` shape `transformCondition` handles.
pub(crate) fn is_condition_shape(value: &Expression<'_>) -> bool {
    matches!(
        value,
        Expression::ConditionalExpression(_) | Expression::LogicalExpression(_)
    )
}

/// Babel's zero-arg-call unwrap in the shared `transformNode`: `{foo()}`
/// inserts the callee itself as the thunk when the callee is neither a call
/// nor a member expression.
pub(crate) fn zero_arg_call_thunk<'a>(
    value: &Expression<'a>,
    allocator: &'a Allocator,
) -> Option<Expression<'a>> {
    let Expression::CallExpression(call) = value else {
        return None;
    };
    if !call.arguments.is_empty() {
        return None;
    }
    if matches!(
        call.callee,
        Expression::CallExpression(_)
            | Expression::StaticMemberExpression(_)
            | Expression::ComputedMemberExpression(_)
            | Expression::PrivateFieldExpression(_)
    ) {
        return None;
    }
    Some(call.callee.clone_in(allocator))
}

/// Statically guaranteed to evaluate to a boolean: the memoized value IS the
/// expression's value (`false` is the only falsy boolean), so no `!!` coercion
/// is needed and the `&&` wrap keeps the logical form instead of the
/// value-preserving ternary. Mirrors the Babel plugin's `isBooleanExpression`.
fn is_boolean_expression(value: &Expression<'_>) -> bool {
    match value {
        Expression::BinaryExpression(binary) => matches!(
            binary.operator,
            BinaryOperator::Equality
                | BinaryOperator::Inequality
                | BinaryOperator::StrictEquality
                | BinaryOperator::StrictInequality
                | BinaryOperator::LessThan
                | BinaryOperator::LessEqualThan
                | BinaryOperator::GreaterThan
                | BinaryOperator::GreaterEqualThan
                | BinaryOperator::Instanceof
                | BinaryOperator::In
        ),
        Expression::UnaryExpression(unary) => unary.operator == UnaryOperator::LogicalNot,
        _ => false,
    }
}

fn booleanize<'a>(allocator: &'a Allocator, span: Span, value: Expression<'a>) -> Expression<'a> {
    if is_boolean_expression(&value) {
        return value;
    }
    let ast = AstBuilder::new(allocator);
    let inner = ast.expression_unary(span, UnaryOperator::LogicalNot, value);
    ast.expression_unary(span, UnaryOperator::LogicalNot, inner)
}

/// The memoized-condition handle: inline mode calls the memo expression
/// directly (`memo(() => cond)()`), hoisted mode calls a generated id that the
/// caller declares (`_c$()`).
struct ConditionHoist<'a> {
    condition: Expression<'a>,
    id: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConditionMode {
    Hoisted,
    Inline,
    Prop,
}

pub(crate) fn transform_condition<'a, C: ConditionBuilder<'a>>(
    ctx: &mut C,
    span: Span,
    value: Expression<'a>,
    inline: bool,
) -> TransformedCondition<'a> {
    let allocator = ctx.condition_allocator();
    let mode = if inline {
        ConditionMode::Inline
    } else {
        ConditionMode::Hoisted
    };
    let (expr, hoist) = transform_condition_value(ctx, span, value, mode);
    if let Some(hoist) = hoist {
        debug_assert!(!inline, "inline conditions never hoist");
        let memo_init = memo_expression(ctx, span, hoist.condition);
        let memo_statement = variable_statement(
            allocator,
            span,
            oxc_ast::ast::VariableDeclarationKind::Var,
            &hoist.id,
            memo_init,
        );
        return TransformedCondition::Hoisted {
            memo_statement,
            getter: arrow_return_expression(allocator, span, expr),
        };
    }
    TransformedCondition::Arrow(arrow_return_expression(allocator, span, expr))
}

/// `transformCondition(path, true).body` — the transformed expression itself,
/// with memos collapsed inline.
pub(crate) fn transform_condition_inline<'a, C: ConditionBuilder<'a>>(
    ctx: &mut C,
    span: Span,
    value: Expression<'a>,
) -> Expression<'a> {
    transform_condition_value(ctx, span, value, ConditionMode::Inline).0
}

pub(crate) fn transform_prop_condition<'a, C: ConditionBuilder<'a>>(
    ctx: &mut C,
    span: Span,
    value: Expression<'a>,
) -> Expression<'a> {
    transform_condition_value(ctx, span, value, ConditionMode::Prop).0
}

/// `memo(thunk)` — or the thunk unchanged when `memoWrapper` is disabled.
pub(crate) fn memo_wrap_thunk<'a, C: ConditionBuilder<'a>>(
    ctx: &mut C,
    span: Span,
    thunk: Expression<'a>,
) -> Expression<'a> {
    if !ctx.memo_wrapper_enabled() {
        return thunk;
    }
    let allocator = ctx.condition_allocator();
    let memo_local = ctx.register_memo();
    let ast = AstBuilder::new(allocator);
    ast.expression_call(
        span,
        ast.expression_identifier(span, ast.ident(&memo_local)),
        None,
        ast.vec1(expression_to_argument(thunk)),
        false,
    )
}

/// `memo(() => cond)`, or a plain `() => cond` thunk when `memoWrapper` is
/// disabled.
fn memo_expression<'a, C: ConditionBuilder<'a>>(
    ctx: &mut C,
    span: Span,
    condition: Expression<'a>,
) -> Expression<'a> {
    let thunk = arrow_return_expression(ctx.condition_allocator(), span, condition);
    memo_wrap_thunk(ctx, span, thunk)
}

fn call_expression_no_args<'a>(
    allocator: &'a Allocator,
    span: Span,
    callee: Expression<'a>,
) -> Expression<'a> {
    let ast = AstBuilder::new(allocator);
    ast.expression_call(span, callee, None, ast.vec(), false)
}

/// Builds the `id()` (hoisted) or `memo(() => cond)()` (inline) test call and
/// the hoist record for the caller.
fn condition_test_call<'a, C: ConditionBuilder<'a>>(
    ctx: &mut C,
    span: Span,
    condition: Expression<'a>,
    mode: ConditionMode,
) -> (Expression<'a>, Option<ConditionHoist<'a>>) {
    let allocator = ctx.condition_allocator();
    if mode != ConditionMode::Hoisted {
        let owner = match mode {
            ConditionMode::Prop => ctx.register_prop_condition_owner(),
            _ => None,
        };
        let memo = if let Some(owner) = owner {
            prop_condition_read(ctx, span, condition, &owner)
        } else {
            memo_expression(ctx, span, condition)
        };
        (call_expression_no_args(allocator, span, memo), None)
    } else {
        let id = ctx.next_condition_id();
        let ast = AstBuilder::new(allocator);
        let call = call_expression_no_args(
            allocator,
            span,
            ast.expression_identifier(span, ast.ident(&id)),
        );
        (call, Some(ConditionHoist { condition, id }))
    }
}

fn prop_condition_read<'a, C: ConditionBuilder<'a>>(
    ctx: &mut C,
    span: Span,
    condition: Expression<'a>,
    owner: &str,
) -> Expression<'a> {
    // An ownerless prop read evaluates against the shown value (F-1).
    let allocator = ctx.condition_allocator();
    let ast = AstBuilder::new(allocator);
    let read = ast.ident("_read$");
    let owned = call_expression_no_args(
        allocator,
        span,
        ast.expression_identifier(span, ast.ident(owner)),
    );
    let memoized = memo_wrap_thunk(ctx, span, ast.expression_identifier(span, read));
    let choice =
        ast.expression_conditional(span, owned, memoized, ast.expression_identifier(span, read));
    let param = ast.formal_parameter(
        span,
        ast.vec(),
        ast.binding_pattern_binding_identifier(span, read),
        None,
        None,
        false,
        None,
        false,
        false,
    );
    let params = ast.formal_parameters(
        span,
        FormalParameterKind::ArrowFormalParameters,
        ast.vec1(param),
        None,
    );
    let body = ast.function_body(
        span,
        ast.vec(),
        ast.vec1(ast.statement_expression(span, choice)),
    );
    let choose = ast.expression_arrow_function(span, true, false, None, params, None, body);
    let thunk = arrow_return_expression(allocator, span, condition);
    ast.expression_call(
        span,
        choose,
        None,
        ast.vec1(expression_to_argument(thunk)),
        false,
    )
}

fn transform_condition_value<'a, C: ConditionBuilder<'a>>(
    ctx: &mut C,
    span: Span,
    value: Expression<'a>,
    mode: ConditionMode,
) -> (Expression<'a>, Option<ConditionHoist<'a>>) {
    // Babel's `transformCondition` registers the memo import at entry, before
    // knowing whether the expression actually transforms — so a
    // conditional-shaped child with static branches still pulls the import.
    if ctx.memo_wrapper_enabled() {
        ctx.register_memo();
    }
    let allocator = ctx.condition_allocator();
    // Babel probes the branches and test with the same `isDynamic` as child
    // holes — marker comments and namespace-import members short-circuit
    // here too. A branch's leading trivia starts after the preceding
    // `?`/`:` token's operand (`test.span.end` / `consequent.span.end`).
    match value {
        Expression::ConditionalExpression(conditional)
            if ctx.classify().is_dynamic(
                Some(conditional.test.span().end),
                &conditional.consequent,
                true,
            ) || ctx.classify().is_dynamic(
                Some(conditional.consequent.span().end),
                &conditional.alternate,
                true,
            ) =>
        {
            if !ctx.classify().is_dynamic(None, &conditional.test, false) {
                return (Expression::ConditionalExpression(conditional), None);
            }
            let condition = booleanize(allocator, span, conditional.test.clone_in(allocator));
            let (test_call, hoist) = condition_test_call(ctx, span, condition, mode);
            // Nested conditionals/logicals in the branches collapse their own
            // memos inline, exactly like Babel's recursive
            // `transformCondition(..., true).body`.
            let branch_mode = if mode == ConditionMode::Prop {
                ConditionMode::Prop
            } else {
                ConditionMode::Inline
            };
            let consequent = inline_branch(
                ctx,
                span,
                conditional.consequent.clone_in(allocator),
                branch_mode,
            );
            let alternate = inline_branch(
                ctx,
                span,
                conditional.alternate.clone_in(allocator),
                branch_mode,
            );
            let ast = AstBuilder::new(allocator);
            (
                ast.expression_conditional(span, test_call, consequent, alternate),
                hoist,
            )
        }
        Expression::ConditionalExpression(conditional) => {
            (Expression::ConditionalExpression(conditional), None)
        }
        Expression::LogicalExpression(logical) => {
            transform_logical_chain(ctx, span, Expression::LogicalExpression(logical), mode)
        }
        other => (other, None),
    }
}

fn inline_branch<'a, C: ConditionBuilder<'a>>(
    ctx: &mut C,
    span: Span,
    branch: Expression<'a>,
    mode: ConditionMode,
) -> Expression<'a> {
    if is_condition_shape(&branch) {
        transform_condition_value(ctx, span, branch, mode).0
    } else {
        branch
    }
}

/// Babel walks the left spine of a logical chain (`a && b() || c`,
/// `(a && b()) ?? c ?? d`) until it finds the `&&` and memoizes that node in
/// place, leaving the outer chain intact.
fn transform_logical_chain<'a, C: ConditionBuilder<'a>>(
    ctx: &mut C,
    span: Span,
    value: Expression<'a>,
    mode: ConditionMode,
) -> (Expression<'a>, Option<ConditionHoist<'a>>) {
    let allocator = ctx.condition_allocator();
    let Expression::LogicalExpression(logical) = value else {
        return (value, None);
    };
    if logical.operator != LogicalOperator::And {
        if matches!(logical.left, Expression::LogicalExpression(_)) {
            let (new_left, hoist) =
                transform_logical_chain(ctx, span, logical.left.clone_in(allocator), mode);
            let ast = AstBuilder::new(allocator);
            return (
                ast.expression_logical(
                    span,
                    new_left,
                    logical.operator,
                    logical.right.clone_in(allocator),
                ),
                hoist,
            );
        }
        return (Expression::LogicalExpression(logical), None);
    }
    if !ctx
        .classify()
        .is_dynamic(Some(logical.left.span().end), &logical.right, true)
        || !ctx.classify().is_dynamic(None, &logical.left, false)
    {
        return (Expression::LogicalExpression(logical), None);
    }
    // `left && right` is exactly `left ? right : left`. Branch on the
    // memoized truthiness (so truthy-value churn never re-creates the right
    // side) but return the raw left in the alternate so the expression keeps
    // JS value semantics — `0`/`""`/`undefined` flow through instead of
    // collapsing to `false`, matching the untransformed ssr output (#532).
    // Statically boolean lefts skip the ternary: the memo's value is the
    // expression's value, so the logical form is already exact and the left
    // never evaluates twice.
    let bool_left = is_boolean_expression(&logical.left);
    let condition = booleanize(allocator, span, logical.left.clone_in(allocator));
    let (test_call, hoist) = condition_test_call(ctx, span, condition, mode);
    let ast = AstBuilder::new(allocator);
    let replaced = if bool_left {
        ast.expression_logical(
            span,
            test_call,
            LogicalOperator::And,
            logical.right.clone_in(allocator),
        )
    } else {
        ast.expression_conditional(
            span,
            test_call,
            logical.right.clone_in(allocator),
            logical.left.clone_in(allocator),
        )
    };
    (replaced, hoist)
}
