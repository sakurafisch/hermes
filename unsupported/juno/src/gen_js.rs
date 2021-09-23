/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::ast::*;
use sourcemap::{RawToken, SourceMap, SourceMapBuilder};

use std::{
    fmt,
    io::{self, BufWriter, Write},
};
use support::convert;

/// Whether to pretty-print the generated JS.
/// Does not do full formatting of the source, but does add indentation and
/// some extra spaces to make source more readable.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Pretty {
    No,
    Yes,
}

/// Generate JS for `root` and print it to `out`.
/// FIXME: This currently only returns an empty SourceMap.
pub fn generate<W: Write>(
    out: W,
    ctx: &Context,
    root: NodePtr,
    pretty: Pretty,
) -> io::Result<SourceMap> {
    GenJS::gen_root(out, ctx, root, pretty)
}

/// Associativity direction.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Assoc {
    /// Left to right associativity.
    Ltr,

    /// Right to left associativity.
    Rtl,
}

mod precedence {
    use crate::ast::{BinaryExpressionOperator, LogicalExpressionOperator};

    pub type Precedence = u32;

    pub const ALWAYS_PAREN: Precedence = 0;
    pub const SEQ: Precedence = 1;
    pub const ARROW: Precedence = 2;
    pub const YIELD: Precedence = 3;
    pub const ASSIGN: Precedence = 4;
    pub const COND: Precedence = 5;
    pub const BIN_START: Precedence = 6;
    pub const UNARY: Precedence = 26;
    pub const POST_UPDATE: Precedence = 27;
    pub const TAGGED_TEMPLATE: Precedence = 28;
    pub const NEW_NO_ARGS: Precedence = 29;
    pub const MEMBER: Precedence = 30;
    pub const PRIMARY: Precedence = 31;
    pub const TOP: Precedence = 32;

    pub const UNION_TYPE: Precedence = 1;
    pub const INTERSECTION_TYPE: Precedence = 2;

    pub fn get_binary_precedence(op: BinaryExpressionOperator) -> Precedence {
        use BinaryExpressionOperator::*;
        match op {
            Exp => 12,
            Mult => 11,
            Mod => 11,
            Div => 11,
            Plus => 10,
            Minus => 10,
            LShift => 9,
            RShift => 9,
            RShift3 => 9,
            Less => 8,
            Greater => 8,
            LessEquals => 8,
            GreaterEquals => 8,
            LooseEquals => 7,
            LooseNotEquals => 7,
            StrictEquals => 7,
            StrictNotEquals => 7,
            BitAnd => 6,
            BitXor => 5,
            BitOr => 4,
            In => 8 + BIN_START,
            Instanceof => 8 + BIN_START,
        }
    }

    pub fn get_logical_precedence(op: LogicalExpressionOperator) -> Precedence {
        use LogicalExpressionOperator::*;
        match op {
            And => 3,
            Or => 2,
            NullishCoalesce => 1,
        }
    }
}

/// Child position for the purpose of determining whether the child needs parens.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum ChildPos {
    Left,
    Anywhere,
    Right,
}

/// Whether parens are needed around something.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum NeedParens {
    /// No parentheses needed.
    No,
    /// Parentheses required.
    Yes,
    /// A space character is sufficient to distinguish.
    /// Used in unary operations, e.g.
    Space,
}

impl From<bool> for NeedParens {
    fn from(x: bool) -> NeedParens {
        if x { NeedParens::Yes } else { NeedParens::No }
    }
}

/// Whether to force a space when adding a space in JS generation.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum ForceSpace {
    No,
    Yes,
}

/// Whether to force the statements to be emitted inside a new block `{ }`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum ForceBlock {
    No,
    Yes,
}

/// Generator for output JS. Walks the AST to output real JS.
struct GenJS<W: Write> {
    /// Where to write the generated JS.
    out: BufWriter<W>,

    /// Whether to pretty print the output JS.
    pretty: Pretty,

    /// Size of the indentation step.
    /// May be configurable in the future.
    indent_step: usize,

    /// Current indentation level, used in pretty mode.
    indent: usize,

    /// Current position of the writer.
    position: SourceLoc,

    /// Raw token tracking the most recent node.
    cur_token: Option<RawToken>,

    /// Build a source map as we go along.
    sourcemap: SourceMapBuilder,

    /// Some(err) if an error has occurred when writing, else None.
    error: Option<io::Error>,
}

/// Print to the output stream if no errors have been seen so far.
/// `$gen_js` is a mutable reference to the GenJS struct.
/// `$arg` arguments follow the format pattern used by `format!`.
/// The output must be ASCII and contain no newlines.
macro_rules! out {
    ($gen_js:expr, $($arg:tt)*) => {{
        $gen_js.write_ascii(format_args!($($arg)*));
    }}
}

impl<W: Write> GenJS<W> {
    /// Generate JS for `root` and flush the output.
    /// If at any point, JS generation resulted in an error, return `Err(err)`,
    /// otherwise return `Ok(())`.
    fn gen_root(writer: W, ctx: &Context, root: NodePtr, pretty: Pretty) -> io::Result<SourceMap> {
        let mut gen_js = GenJS {
            out: BufWriter::new(writer),
            pretty,
            indent_step: 2,
            indent: 0,
            position: SourceLoc { line: 1, col: 1 },
            cur_token: None,
            // FIXME: Pass in file name here.
            sourcemap: SourceMapBuilder::new(None),
            error: None,
        };
        root.visit(ctx, &mut gen_js, None);
        gen_js.force_newline();
        gen_js.flush_cur_token();
        match gen_js.error {
            None => gen_js
                .out
                .flush()
                .and(Ok(gen_js.sourcemap.into_sourcemap())),
            Some(err) => Err(err),
        }
    }

    /// Write to the `out` writer if we haven't seen any errors.
    /// If we have seen any errors, do nothing.
    /// Used via the `out!` macro.
    /// The output must be ASCII and contain no newlines.
    fn write_ascii(&mut self, args: fmt::Arguments<'_>) {
        if self.error.is_none() {
            let buf = format!("{}", args);
            debug_assert!(buf.is_ascii(), "Output must be ASCII");
            debug_assert!(!buf.contains('\n'), "Output must have no newlines");
            if let Err(e) = self.out.write_all(buf.as_bytes()) {
                self.error = Some(e);
            }
            self.position.col += buf.len() as u32;
        }
    }

    /// Write a single unicode character to the `out` writer if we haven't seen any errors.
    /// Character must not be a newline.
    /// Use `dst` as a temporary buffer.
    /// If we have seen any errors, do nothing.
    fn write_char(&mut self, ch: char, dst: &mut [u8]) {
        debug_assert!(ch != '\n', "Output must not contain newlines");
        if self.error.is_none() {
            if let Err(e) = self.out.write_all(ch.encode_utf8(dst).as_bytes()) {
                self.error = Some(e);
            }
            self.position.col += 1;
        }
    }

    /// Write unicode to the `out` writer if we haven't seen any errors.
    /// If we have seen any errors, do nothing.
    /// The output must contain no newlines.
    fn write_utf8(&mut self, s: &str) {
        debug_assert!(
            !s.chars().any(|c| c == '\n'),
            "Output must not contain newlines"
        );
        if self.error.is_none() {
            if let Err(e) = self.out.write_all(s.as_bytes()) {
                self.error = Some(e);
            }
        }
        self.position.col += s.chars().count() as u32;
    }

    /// Generate the JS for each node kind.
    fn gen_node(&mut self, ctx: &Context, node: NodePtr, parent: Option<NodePtr>) {
        use NodeKind::*;

        match &node.get(ctx).kind {
            Empty => {}
            Metadata => {}

            Program { body } => {
                self.visit_stmt_list(ctx, body, node);
            }

            FunctionExpression {
                id,
                params,
                body,
                type_parameters,
                return_type,
                predicate,
                generator,
                is_async,
            }
            | FunctionDeclaration {
                id,
                params,
                body,
                type_parameters,
                return_type,
                predicate,
                generator,
                is_async,
            } => {
                if *is_async {
                    out!(self, "async ");
                }
                out!(self, "function");
                if *generator {
                    out!(self, "*");
                    if id.is_some() {
                        self.space(ForceSpace::No);
                    }
                } else if id.is_some() {
                    self.space(ForceSpace::Yes);
                }
                if let Some(id) = id {
                    id.visit(ctx, self, Some(node));
                }
                if let Some(type_parameters) = type_parameters {
                    type_parameters.visit(ctx, self, Some(node));
                }
                self.visit_func_params_body(ctx, params, *return_type, *predicate, *body, node);
            }

            ArrowFunctionExpression {
                id: _,
                params,
                body,
                type_parameters,
                return_type,
                predicate,
                expression,
                is_async,
            } => {
                let mut need_sep = false;
                if *is_async {
                    out!(self, "async");
                    need_sep = true;
                }
                if let Some(type_parameters) = type_parameters {
                    type_parameters.visit(ctx, self, Some(node));
                    need_sep = false;
                }
                // Single parameter without type info doesn't need parens.
                // But only in expression mode, otherwise it is ugly.
                if params.len() == 1
                    && type_parameters.is_none()
                    && return_type.is_none()
                    && predicate.is_none()
                    && (*expression || self.pretty == Pretty::No)
                {
                    if need_sep {
                        out!(self, " ");
                    }
                    params[0].visit(ctx, self, Some(node));
                } else {
                    out!(self, "(");
                    for (i, param) in params.iter().enumerate() {
                        if i > 0 {
                            self.comma();
                        }
                        param.visit(ctx, self, Some(node));
                    }
                    out!(self, ")");
                }
                if let Some(return_type) = return_type {
                    out!(self, ":");
                    self.space(ForceSpace::No);
                    return_type.visit(ctx, self, Some(node));
                }
                if let Some(predicate) = predicate {
                    self.space(ForceSpace::Yes);
                    predicate.visit(ctx, self, Some(node));
                }
                self.space(ForceSpace::No);
                out!(self, "=>");
                self.space(ForceSpace::No);
                match &body.get(ctx).kind {
                    BlockStatement { .. } => {
                        body.visit(ctx, self, Some(node));
                    }
                    _ => {
                        self.print_child(ctx, Some(*body), node, ChildPos::Right);
                    }
                }
            }

            WhileStatement { body, test } => {
                out!(self, "while");
                self.space(ForceSpace::No);
                out!(self, "(");
                test.visit(ctx, self, Some(node));
                out!(self, ")");
                self.visit_stmt_or_block(ctx, *body, ForceBlock::No, node);
            }
            DoWhileStatement { body, test } => {
                out!(self, "do");
                let block = self.visit_stmt_or_block(ctx, *body, ForceBlock::No, node);
                if block {
                    self.space(ForceSpace::No);
                } else {
                    out!(self, ";");
                    self.newline();
                }
                out!(self, "while");
                self.space(ForceSpace::No);
                out!(self, "(");
                test.visit(ctx, self, Some(node));
                out!(self, ")");
            }

            ForInStatement { left, right, body } => {
                out!(self, "for(");
                left.visit(ctx, self, Some(node));
                out!(self, " in ");
                right.visit(ctx, self, Some(node));
                out!(self, ")");
                self.visit_stmt_or_block(ctx, *body, ForceBlock::No, node);
            }
            ForOfStatement {
                left,
                right,
                body,
                is_await,
            } => {
                out!(self, "for{}(", if *is_await { " await" } else { "" });
                left.visit(ctx, self, Some(node));
                out!(self, " of ");
                right.visit(ctx, self, Some(node));
                out!(self, ")");
                self.visit_stmt_or_block(ctx, *body, ForceBlock::No, node);
            }
            ForStatement {
                init,
                test,
                update,
                body,
            } => {
                out!(self, "for(");
                self.print_child(ctx, *init, node, ChildPos::Left);
                out!(self, ";");
                if let Some(test) = test {
                    self.space(ForceSpace::No);
                    test.visit(ctx, self, Some(node));
                }
                out!(self, ";");
                if let Some(update) = update {
                    self.space(ForceSpace::No);
                    update.visit(ctx, self, Some(node));
                }
                out!(self, ")");
                self.visit_stmt_or_block(ctx, *body, ForceBlock::No, node);
            }

            DebuggerStatement => {
                out!(self, "debugger");
            }
            EmptyStatement => {}

            BlockStatement { body } => {
                if body.is_empty() {
                    out!(self, "{{}}");
                } else {
                    out!(self, "{{");
                    self.inc_indent();
                    self.newline();
                    self.visit_stmt_list(ctx, body, node);
                    self.dec_indent();
                    self.newline();
                    out!(self, "}}");
                }
            }

            BreakStatement { label } => {
                out!(self, "break");
                if let Some(label) = label {
                    self.space(ForceSpace::Yes);
                    label.visit(ctx, self, Some(node));
                }
            }
            ContinueStatement { label } => {
                out!(self, "continue");
                if let Some(label) = label {
                    self.space(ForceSpace::Yes);
                    label.visit(ctx, self, Some(node));
                }
            }

            ThrowStatement { argument } => {
                out!(self, "throw ");
                argument.visit(ctx, self, Some(node));
            }
            ReturnStatement { argument } => {
                out!(self, "return");
                if let Some(argument) = argument {
                    out!(self, " ");
                    argument.visit(ctx, self, Some(node));
                }
            }
            WithStatement { object, body } => {
                out!(self, "with");
                self.space(ForceSpace::No);
                out!(self, "(");
                object.visit(ctx, self, Some(node));
                out!(self, ")");
                self.visit_stmt_or_block(ctx, *body, ForceBlock::No, node);
            }

            SwitchStatement {
                discriminant,
                cases,
            } => {
                out!(self, "switch");
                self.space(ForceSpace::No);
                out!(self, "(");
                discriminant.visit(ctx, self, Some(node));
                out!(self, ")");
                self.space(ForceSpace::No);
                out!(self, "{{");
                self.newline();
                for case in cases {
                    case.visit(ctx, self, Some(node));
                    self.newline();
                }
                out!(self, "}}");
            }
            SwitchCase { test, consequent } => {
                match test {
                    Some(test) => {
                        out!(self, "case ");
                        test.visit(ctx, self, Some(node));
                    }
                    None => {
                        out!(self, "default");
                    }
                };
                out!(self, ":");
                if !consequent.is_empty() {
                    self.inc_indent();
                    self.newline();
                    self.visit_stmt_list(ctx, consequent, node);
                    self.dec_indent();
                }
            }

            LabeledStatement { label, body } => {
                label.visit(ctx, self, Some(node));
                out!(self, ":");
                self.newline();
                body.visit(ctx, self, Some(node));
            }

            ExpressionStatement {
                expression,
                directive: _,
            } => {
                self.print_child(ctx, Some(*expression), node, ChildPos::Anywhere);
            }

            TryStatement {
                block,
                handler,
                finalizer,
            } => {
                out!(self, "try");
                self.visit_stmt_or_block(ctx, *block, ForceBlock::Yes, node);
                if let Some(handler) = handler {
                    handler.visit(ctx, self, Some(node));
                }
                if let Some(finalizer) = finalizer {
                    out!(self, "finally");
                    self.space(ForceSpace::No);
                    self.visit_stmt_or_block(ctx, *finalizer, ForceBlock::Yes, node);
                }
            }

            IfStatement {
                test,
                consequent,
                alternate,
            } => {
                out!(self, "if");
                self.space(ForceSpace::No);
                out!(self, "(");
                test.visit(ctx, self, Some(node));
                out!(self, ")");
                let force_block =
                    if alternate.is_some() && consequent.get(ctx).kind.is_if_without_else() {
                        ForceBlock::Yes
                    } else {
                        ForceBlock::No
                    };
                let block = self.visit_stmt_or_block(ctx, *consequent, force_block, node);
                if let Some(alternate) = alternate {
                    if !block {
                        out!(self, ";");
                        self.newline();
                    } else {
                        self.space(ForceSpace::No);
                    }
                    out!(self, "else");
                    self.space(
                        if matches!(&alternate.get(ctx).kind, BlockStatement { .. }) {
                            ForceSpace::No
                        } else {
                            ForceSpace::Yes
                        },
                    );
                    self.visit_stmt_or_block(ctx, *alternate, ForceBlock::No, node);
                }
            }

            BooleanLiteral { value } => {
                out!(self, "{}", if *value { "true" } else { "false" });
            }
            NullLiteral => {
                out!(self, "null");
            }
            StringLiteral { value } => {
                out!(self, "\"");
                self.print_escaped_string_literal(value, '"');
                out!(self, "\"");
            }
            NumericLiteral { value } => {
                out!(self, "{}", convert::number_to_string(*value));
            }
            RegExpLiteral { pattern, flags } => {
                out!(self, "/");
                // Parser doesn't handle escapes when lexing RegExp,
                // so we don't need to do any manual escaping here.
                self.write_utf8(&pattern.str);
                out!(self, "/");
                self.write_utf8(&flags.str);
            }
            ThisExpression => {
                out!(self, "this");
            }
            Super => {
                out!(self, "super");
            }

            SequenceExpression { expressions } => {
                out!(self, "(");
                for (i, expr) in expressions.iter().enumerate() {
                    if i > 0 {
                        self.comma();
                    }
                    self.print_child(
                        ctx,
                        Some(*expr),
                        node,
                        if i == 1 {
                            ChildPos::Left
                        } else {
                            ChildPos::Right
                        },
                    );
                }
                out!(self, ")");
            }

            ObjectExpression { properties } => {
                self.visit_props(ctx, properties, node);
            }
            ArrayExpression {
                elements,
                trailing_comma,
            } => {
                out!(self, "[");
                for (i, elem) in elements.iter().enumerate() {
                    if i > 0 {
                        self.comma();
                    }
                    if let SpreadElement { .. } = &elem.get(ctx).kind {
                        elem.visit(ctx, self, Some(node));
                    } else {
                        self.print_comma_expression(ctx, *elem, node);
                    }
                }
                if *trailing_comma {
                    self.comma();
                }
                out!(self, "]");
            }

            SpreadElement { argument } => {
                out!(self, "...");
                argument.visit(ctx, self, Some(node));
            }

            NewExpression {
                callee,
                type_arguments,
                arguments,
            } => {
                out!(self, "new ");
                self.print_child(ctx, Some(*callee), node, ChildPos::Left);
                if let Some(type_arguments) = type_arguments {
                    type_arguments.visit(ctx, self, Some(node));
                }
                out!(self, "(");
                for (i, arg) in arguments.iter().enumerate() {
                    if i > 0 {
                        self.comma();
                    }
                    self.print_comma_expression(ctx, *arg, node);
                }
                out!(self, ")");
            }
            YieldExpression { argument, delegate } => {
                out!(self, "yield");
                if *delegate {
                    out!(self, "*");
                    self.space(ForceSpace::No);
                } else if argument.is_some() {
                    out!(self, " ");
                }
                if let Some(argument) = argument {
                    argument.visit(ctx, self, Some(node));
                }
            }
            AwaitExpression { argument } => {
                out!(self, "await ");
                argument.visit(ctx, self, Some(node));
            }

            ImportExpression { source, attributes } => {
                out!(self, "import(");
                source.visit(ctx, self, Some(node));
                if let Some(attributes) = attributes {
                    out!(self, ",");
                    self.space(ForceSpace::No);
                    attributes.visit(ctx, self, Some(node));
                }
                out!(self, ")");
            }

            CallExpression {
                callee,
                type_arguments,
                arguments,
            } => {
                self.print_child(ctx, Some(*callee), node, ChildPos::Left);
                if let Some(type_arguments) = type_arguments {
                    type_arguments.visit(ctx, self, Some(node));
                }
                out!(self, "(");
                for (i, arg) in arguments.iter().enumerate() {
                    if i > 0 {
                        self.comma();
                    }
                    self.print_comma_expression(ctx, *arg, node);
                }
                out!(self, ")");
            }
            OptionalCallExpression {
                callee,
                type_arguments,
                arguments,
                optional,
            } => {
                self.print_child(ctx, Some(*callee), node, ChildPos::Left);
                if let Some(type_arguments) = type_arguments {
                    type_arguments.visit(ctx, self, Some(node));
                }
                out!(self, "{}(", if *optional { "?." } else { "" });
                for (i, arg) in arguments.iter().enumerate() {
                    if i > 0 {
                        self.comma();
                    }
                    self.print_comma_expression(ctx, *arg, node);
                }
                out!(self, ")");
            }

            AssignmentExpression {
                operator,
                left,
                right,
            } => {
                self.print_child(ctx, Some(*left), node, ChildPos::Left);
                self.space(ForceSpace::No);
                out!(self, "{}", operator.as_str());
                self.space(ForceSpace::No);
                self.print_child(ctx, Some(*right), node, ChildPos::Right);
            }
            UnaryExpression {
                operator,
                argument,
                prefix,
            } => {
                let ident = operator.as_str().chars().next().unwrap().is_alphabetic();
                if *prefix {
                    out!(self, "{}", operator.as_str());
                    if ident {
                        out!(self, " ");
                    }
                    self.print_child(ctx, Some(*argument), node, ChildPos::Right);
                } else {
                    self.print_child(ctx, Some(*argument), node, ChildPos::Left);
                    if ident {
                        out!(self, " ");
                    }
                    out!(self, "{}", operator.as_str());
                }
            }
            UpdateExpression {
                operator,
                argument,
                prefix,
            } => {
                if *prefix {
                    out!(self, "{}", operator.as_str());
                    self.print_child(ctx, Some(*argument), node, ChildPos::Right);
                } else {
                    self.print_child(ctx, Some(*argument), node, ChildPos::Left);
                    out!(self, "{}", operator.as_str());
                }
            }
            MemberExpression {
                object,
                property,
                computed,
            } => {
                self.print_child(ctx, Some(*object), node, ChildPos::Left);
                if *computed {
                    out!(self, "[");
                } else {
                    out!(self, ".");
                }
                self.print_child(ctx, Some(*property), node, ChildPos::Right);
                if *computed {
                    out!(self, "]");
                }
            }
            OptionalMemberExpression {
                object,
                property,
                computed,
                optional,
            } => {
                self.print_child(ctx, Some(*object), node, ChildPos::Left);
                if *computed {
                    out!(self, "{}[", if *optional { "?." } else { "" });
                } else {
                    out!(self, "{}.", if *optional { "?" } else { "" });
                }
                self.print_child(ctx, Some(*property), node, ChildPos::Right);
                if *computed {
                    out!(self, "]");
                }
            }

            BinaryExpression {
                left,
                right,
                operator,
            } => {
                let ident = operator.as_str().chars().next().unwrap().is_alphabetic();
                self.print_child(ctx, Some(*left), node, ChildPos::Left);
                self.space(if ident {
                    ForceSpace::Yes
                } else {
                    ForceSpace::No
                });
                out!(self, "{}", operator.as_str());
                self.space(if ident {
                    ForceSpace::Yes
                } else {
                    ForceSpace::No
                });
                self.print_child(ctx, Some(*right), node, ChildPos::Right);
            }

            Directive { value } => {
                value.visit(ctx, self, Some(node));
            }
            DirectiveLiteral { .. } => {
                unimplemented!("No escaping for directive literals");
            }

            ConditionalExpression {
                test,
                consequent,
                alternate,
            } => {
                self.print_child(ctx, Some(*test), node, ChildPos::Left);
                self.space(ForceSpace::No);
                out!(self, "?");
                self.space(ForceSpace::No);
                self.print_child(ctx, Some(*consequent), node, ChildPos::Anywhere);
                self.space(ForceSpace::No);
                out!(self, ":");
                self.space(ForceSpace::No);
                self.print_child(ctx, Some(*alternate), node, ChildPos::Right);
            }

            Identifier {
                name,
                type_annotation,
                optional,
            } => {
                self.write_utf8(name.str.as_ref());
                if *optional {
                    out!(self, "?");
                }
                if let Some(type_annotation) = type_annotation {
                    out!(self, ":");
                    self.space(ForceSpace::No);
                    type_annotation.visit(ctx, self, Some(node));
                }
            }
            PrivateName { id } => {
                out!(self, "#");
                id.visit(ctx, self, Some(node));
            }
            MetaProperty { meta, property } => {
                meta.visit(ctx, self, Some(node));
                out!(self, ".");
                property.visit(ctx, self, Some(node));
            }

            CatchClause { param, body } => {
                self.space(ForceSpace::No);
                out!(self, "catch");
                if let Some(param) = param {
                    self.space(ForceSpace::No);
                    out!(self, "(");
                    param.visit(ctx, self, Some(node));
                    out!(self, ")");
                }
                self.visit_stmt_or_block(ctx, *body, ForceBlock::Yes, node);
            }

            VariableDeclaration { kind, declarations } => {
                out!(self, "{} ", kind.as_str());
                for (i, decl) in declarations.iter().enumerate() {
                    if i > 0 {
                        self.comma();
                    }
                    decl.visit(ctx, self, Some(node));
                }
            }
            VariableDeclarator { init, id } => {
                id.visit(ctx, self, Some(node));
                if let Some(init) = init {
                    out!(
                        self,
                        "{}",
                        match self.pretty {
                            Pretty::Yes => " = ",
                            Pretty::No => "=",
                        }
                    );
                    init.visit(ctx, self, Some(node));
                }
            }

            TemplateLiteral {
                quasis,
                expressions,
            } => {
                out!(self, "`");
                let mut it_expr = expressions.iter();
                for quasi in quasis {
                    if let TemplateElement {
                        raw,
                        tail: _,
                        cooked: _,
                    } = &quasi.get(ctx).kind
                    {
                        let mut buf = [0u8; 4];
                        for char in raw.str.chars() {
                            if char == '\n' {
                                self.force_newline_without_indent();
                                continue;
                            }
                            self.write_char(char, &mut buf);
                        }
                        if let Some(expr) = it_expr.next() {
                            out!(self, "${{");
                            expr.visit(ctx, self, Some(node));
                            out!(self, "}}");
                        }
                    }
                }
                out!(self, "`");
            }
            TaggedTemplateExpression { tag, quasi } => {
                self.print_child(ctx, Some(*tag), node, ChildPos::Left);
                self.print_child(ctx, Some(*quasi), node, ChildPos::Right);
            }
            TemplateElement { .. } => {
                unreachable!("TemplateElement is handled in TemplateLiteral case");
            }

            Property {
                key,
                value,
                kind,
                computed,
                method,
                shorthand,
            } => {
                let mut need_sep = false;
                if *kind != PropertyKind::Init {
                    out!(self, "{}", kind.as_str());
                    need_sep = true;
                } else if *method {
                    match &value.get(ctx).kind {
                        FunctionExpression {
                            generator,
                            is_async,
                            ..
                        } => {
                            if *is_async {
                                out!(self, "async");
                                need_sep = true;
                            }
                            if *generator {
                                out!(self, "*");
                                need_sep = false;
                                self.space(ForceSpace::No);
                            }
                        }
                        _ => unreachable!(),
                    };
                }
                if *computed {
                    if need_sep {
                        self.space(ForceSpace::No);
                    }
                    need_sep = false;
                    out!(self, "[");
                }
                if need_sep {
                    out!(self, " ");
                }
                key.visit(ctx, self, None);
                if *computed {
                    out!(self, "]");
                }
                if *shorthand {
                    return;
                }
                if *kind != PropertyKind::Init || *method {
                    match &value.get(ctx).kind {
                        FunctionExpression {
                            params,
                            body,
                            return_type,
                            predicate,
                            ..
                        } => {
                            self.visit_func_params_body(
                                ctx,
                                params,
                                *return_type,
                                *predicate,
                                *body,
                                *value,
                            );
                        }
                        _ => unreachable!(),
                    };
                } else {
                    out!(self, ":");
                    self.space(ForceSpace::No);
                    self.print_comma_expression(ctx, *value, node);
                }
            }

            LogicalExpression {
                left,
                right,
                operator,
            } => {
                self.print_child(ctx, Some(*left), node, ChildPos::Left);
                self.space(ForceSpace::No);
                out!(self, "{}", operator.as_str());
                self.space(ForceSpace::No);
                self.print_child(ctx, Some(*right), node, ChildPos::Right);
            }

            ClassExpression {
                id,
                type_parameters,
                super_class,
                super_type_parameters,
                implements,
                decorators,
                body,
            }
            | ClassDeclaration {
                id,
                type_parameters,
                super_class,
                super_type_parameters,
                implements,
                decorators,
                body,
            } => {
                for decorator in decorators {
                    decorator.visit(ctx, self, Some(node));
                    self.force_newline();
                }
                out!(self, "class");
                if let Some(id) = id {
                    self.space(ForceSpace::Yes);
                    id.visit(ctx, self, Some(node));
                }
                if let Some(type_parameters) = type_parameters {
                    type_parameters.visit(ctx, self, Some(node));
                }
                if let Some(super_class) = super_class {
                    out!(self, " extends ");
                    super_class.visit(ctx, self, Some(node));
                }
                if let Some(super_type_parameters) = super_type_parameters {
                    super_type_parameters.visit(ctx, self, Some(node));
                }
                if !implements.is_empty() {
                    out!(self, " implements ");
                    for (i, implement) in implements.iter().enumerate() {
                        if i > 0 {
                            self.comma();
                        }
                        implement.visit(ctx, self, Some(node));
                    }
                }

                self.space(ForceSpace::No);
                body.visit(ctx, self, Some(node));
            }

            ClassBody { body } => {
                if body.is_empty() {
                    out!(self, "{{}}");
                } else {
                    out!(self, "{{");
                    self.inc_indent();
                    self.newline();
                    for prop in body {
                        prop.visit(ctx, self, Some(node));
                        self.newline();
                    }
                    out!(self, "}}");
                    self.dec_indent();
                    self.newline();
                }
            }
            ClassProperty {
                key,
                value,
                computed,
                is_static,
                declare: _,
                optional: _,
                variance: _,
                type_annotation: _,
            } => {
                if *is_static {
                    out!(self, "static ");
                }
                if *computed {
                    out!(self, "[");
                }
                key.visit(ctx, self, Some(node));
                if *computed {
                    out!(self, "]");
                }
                self.space(ForceSpace::No);
                if let Some(value) = value {
                    out!(self, "=");
                    self.space(ForceSpace::No);
                    value.visit(ctx, self, Some(node));
                }
                out!(self, ";");
            }
            ClassPrivateProperty {
                key,
                value,
                is_static,
                declare: _,
                optional: _,
                variance: _,
                type_annotation: _,
            } => {
                if *is_static {
                    out!(self, "static ");
                }
                out!(self, "#");
                key.visit(ctx, self, Some(node));
                self.space(ForceSpace::No);
                if let Some(value) = value {
                    out!(self, "=");
                    self.space(ForceSpace::No);
                    value.visit(ctx, self, Some(node));
                }
                out!(self, ";");
            }
            MethodDefinition {
                key,
                value,
                kind,
                computed,
                is_static,
            } => {
                let (is_async, generator, params, body, return_type, predicate) =
                    match &value.get(ctx).kind {
                        FunctionExpression {
                            generator,
                            is_async,
                            params,
                            body,
                            return_type,
                            predicate,
                            ..
                        } => (*is_async, *generator, params, body, return_type, predicate),
                        _ => {
                            unreachable!("Invalid method value");
                        }
                    };
                if *is_static {
                    out!(self, "static ");
                }
                if is_async {
                    out!(self, "async ");
                }
                if generator {
                    out!(self, "*");
                }
                match *kind {
                    MethodDefinitionKind::Method => {}
                    MethodDefinitionKind::Constructor => {
                        // Will be handled by key output.
                    }
                    MethodDefinitionKind::Get => {
                        out!(self, "get ");
                    }
                    MethodDefinitionKind::Set => {
                        out!(self, "set ");
                    }
                };
                if *computed {
                    out!(self, "[");
                }
                key.visit(ctx, self, Some(node));
                if *computed {
                    out!(self, "]");
                }
                self.visit_func_params_body(ctx, params, *return_type, *predicate, *body, node);
            }

            ImportDeclaration {
                specifiers,
                source,
                attributes,
                import_kind,
            } => {
                out!(self, "import ");
                if *import_kind != ImportKind::Value {
                    out!(self, "{} ", import_kind.as_str());
                }
                let mut has_named_specs = false;
                for (i, spec) in specifiers.iter().enumerate() {
                    if i > 0 {
                        self.comma();
                    }
                    if let ImportSpecifier { .. } = &spec.get(ctx).kind {
                        if !has_named_specs {
                            has_named_specs = true;
                            out!(self, "{{");
                        }
                    }
                    spec.visit(ctx, self, Some(node));
                }
                if !specifiers.is_empty() {
                    if has_named_specs {
                        out!(self, "}}");
                        self.space(ForceSpace::No);
                    } else {
                        out!(self, " ");
                    }
                    out!(self, "from ");
                }
                source.visit(ctx, self, Some(node));
                if let Some(attributes) = attributes {
                    if !attributes.is_empty() {
                        out!(self, " assert {{");
                        for (i, attribute) in attributes.iter().enumerate() {
                            if i > 0 {
                                self.comma();
                            }
                            attribute.visit(ctx, self, Some(node));
                        }
                        out!(self, "}}");
                    }
                }
                self.newline();
            }
            ImportSpecifier {
                imported,
                local,
                import_kind,
            } => {
                if *import_kind != ImportKind::Value {
                    out!(self, "{} ", import_kind.as_str());
                }
                imported.visit(ctx, self, Some(node));
                out!(self, " as ");
                local.visit(ctx, self, Some(node));
            }
            ImportDefaultSpecifier { local } => {
                local.visit(ctx, self, Some(node));
            }
            ImportNamespaceSpecifier { local } => {
                out!(self, "* as ");
                local.visit(ctx, self, Some(node));
            }
            ImportAttribute { key, value } => {
                key.visit(ctx, self, Some(node));
                out!(self, ":");
                self.space(ForceSpace::No);
                value.visit(ctx, self, Some(node));
            }

            ExportNamedDeclaration {
                declaration,
                specifiers,
                source,
                export_kind,
            } => {
                out!(self, "export ");
                if *export_kind != ExportKind::Value {
                    out!(self, "{} ", export_kind.as_str());
                }
                if let Some(declaration) = declaration {
                    declaration.visit(ctx, self, Some(node));
                } else {
                    out!(self, "{{");
                    for (i, spec) in specifiers.iter().enumerate() {
                        if i > 0 {
                            self.comma();
                        }
                        spec.visit(ctx, self, Some(node));
                    }
                    out!(self, "}}");
                    if let Some(source) = source {
                        out!(self, " from ");
                        source.visit(ctx, self, Some(node));
                    }
                }
                self.newline();
            }
            ExportSpecifier { exported, local } => {
                local.visit(ctx, self, Some(node));
                out!(self, " as ");
                exported.visit(ctx, self, Some(node));
            }
            ExportNamespaceSpecifier { exported } => {
                out!(self, "* as ");
                exported.visit(ctx, self, Some(node));
            }
            ExportDefaultDeclaration { declaration } => {
                out!(self, "export default ");
                declaration.visit(ctx, self, Some(node));
                self.newline();
            }
            ExportAllDeclaration {
                source,
                export_kind,
            } => {
                out!(self, "export ");
                if *export_kind != ExportKind::Value {
                    out!(self, "{} ", export_kind.as_str());
                }
                out!(self, "* from ");
                source.visit(ctx, self, Some(node));
            }

            ObjectPattern {
                properties,
                type_annotation,
            } => {
                self.visit_props(ctx, properties, node);
                if let Some(type_annotation) = type_annotation {
                    out!(self, ":");
                    self.space(ForceSpace::No);
                    type_annotation.visit(ctx, self, Some(node));
                }
            }
            ArrayPattern {
                elements,
                type_annotation,
            } => {
                out!(self, "[");
                for (i, elem) in elements.iter().enumerate() {
                    if i > 0 {
                        self.comma();
                    }
                    elem.visit(ctx, self, Some(node));
                }
                out!(self, "]");
                if let Some(type_annotation) = type_annotation {
                    out!(self, ":");
                    self.space(ForceSpace::No);
                    type_annotation.visit(ctx, self, Some(node));
                }
            }
            RestElement { argument } => {
                out!(self, "...");
                argument.visit(ctx, self, Some(node));
            }
            AssignmentPattern { left, right } => {
                left.visit(ctx, self, Some(node));
                self.space(ForceSpace::No);
                out!(self, "=");
                self.space(ForceSpace::No);
                right.visit(ctx, self, Some(node));
            }

            JSXIdentifier { name } => {
                out!(self, "{}", name.str);
            }
            JSXMemberExpression { object, property } => {
                object.visit(ctx, self, Some(node));
                out!(self, ".");
                property.visit(ctx, self, Some(node));
            }
            JSXNamespacedName { namespace, name } => {
                namespace.visit(ctx, self, Some(node));
                out!(self, ":");
                name.visit(ctx, self, Some(node));
            }
            JSXEmptyExpression => {}
            JSXExpressionContainer { expression } => {
                out!(self, "{{");
                expression.visit(ctx, self, Some(node));
                out!(self, "}}");
            }
            JSXSpreadChild { expression } => {
                out!(self, "{{...");
                expression.visit(ctx, self, Some(node));
                out!(self, "}}");
            }
            JSXOpeningElement {
                name,
                attributes,
                self_closing,
            } => {
                out!(self, "<");
                name.visit(ctx, self, Some(node));
                for attr in attributes {
                    self.space(ForceSpace::Yes);
                    attr.visit(ctx, self, Some(node));
                }
                if *self_closing {
                    out!(self, " />");
                } else {
                    out!(self, ">");
                }
            }
            JSXClosingElement { name } => {
                out!(self, "</");
                name.visit(ctx, self, Some(node));
                out!(self, ">");
            }
            JSXAttribute { name, value } => {
                name.visit(ctx, self, Some(node));
                if let Some(value) = value {
                    out!(self, "=");
                    value.visit(ctx, self, Some(node));
                }
            }
            JSXSpreadAttribute { argument } => {
                out!(self, "{{...");
                argument.visit(ctx, self, Some(node));
                out!(self, "}}");
            }
            JSXText { value: _, .. } => {
                unimplemented!("JSXText");
                // FIXME: Ensure escaping here works properly.
                // out!(self, "{}", value.str);
            }
            JSXElement {
                opening_element,
                children,
                closing_element,
            } => {
                opening_element.visit(ctx, self, Some(node));
                if let Some(closing_element) = closing_element {
                    self.inc_indent();
                    self.newline();
                    for child in children {
                        child.visit(ctx, self, Some(node));
                        self.newline();
                    }
                    self.dec_indent();
                    closing_element.visit(ctx, self, Some(node));
                }
            }
            JSXFragment {
                opening_fragment,
                children,
                closing_fragment,
            } => {
                opening_fragment.visit(ctx, self, Some(node));
                self.inc_indent();
                self.newline();
                for child in children {
                    child.visit(ctx, self, Some(node));
                    self.newline();
                }
                self.dec_indent();
                closing_fragment.visit(ctx, self, Some(node));
            }
            JSXOpeningFragment => {
                out!(self, "<>");
            }
            JSXClosingFragment => {
                out!(self, "</>");
            }

            ExistsTypeAnnotation => {
                out!(self, "*");
            }
            EmptyTypeAnnotation => {
                out!(self, "empty");
            }
            StringTypeAnnotation => {
                out!(self, "string");
            }
            NumberTypeAnnotation => {
                out!(self, "number");
            }
            StringLiteralTypeAnnotation { value } => {
                out!(self, "\"");
                self.print_escaped_string_literal(value, '"');
                out!(self, "\"");
            }
            NumberLiteralTypeAnnotation { value, .. } => {
                out!(self, "{}", convert::number_to_string(*value));
            }
            BooleanTypeAnnotation => {
                out!(self, "boolean");
            }
            BooleanLiteralTypeAnnotation { value, .. } => {
                out!(self, "{}", if *value { "true" } else { "false" });
            }
            NullLiteralTypeAnnotation => {
                out!(self, "null");
            }
            SymbolTypeAnnotation => {
                out!(self, "symbol");
            }
            AnyTypeAnnotation => {
                out!(self, "any");
            }
            MixedTypeAnnotation => {
                out!(self, "mixed");
            }
            VoidTypeAnnotation => {
                out!(self, "void");
            }
            FunctionTypeAnnotation {
                params,
                this,
                return_type,
                rest,
                type_parameters,
            } => {
                if let Some(type_parameters) = type_parameters {
                    type_parameters.visit(ctx, self, Some(node));
                }
                let need_parens = type_parameters.is_some() || rest.is_some() || params.len() != 1;
                if need_parens {
                    out!(self, "(");
                }
                let mut need_comma = false;
                if let Some(this) = this {
                    match &this.get(ctx).kind {
                        FunctionTypeParam {
                            type_annotation, ..
                        } => {
                            out!(self, "this:");
                            self.space(ForceSpace::No);
                            type_annotation.visit(ctx, self, Some(node));
                        }
                        _ => {
                            unimplemented!("Malformed AST: Need to handle error");
                        }
                    }
                    this.visit(ctx, self, Some(node));
                    need_comma = true;
                }
                for param in params.iter() {
                    if need_comma {
                        self.comma();
                    }
                    param.visit(ctx, self, Some(node));
                    need_comma = true;
                }
                if let Some(rest) = rest {
                    if need_comma {
                        self.comma();
                    }
                    out!(self, "...");
                    rest.visit(ctx, self, Some(node));
                }
                if need_parens {
                    out!(self, ")");
                }
                if self.pretty == Pretty::Yes {
                    out!(self, " => ");
                } else {
                    out!(self, "=>");
                }
                return_type.visit(ctx, self, Some(node));
            }
            FunctionTypeParam {
                name,
                type_annotation,
                optional,
            } => {
                if let Some(name) = name {
                    name.visit(ctx, self, Some(node));
                    if *optional {
                        out!(self, "?");
                    }
                    out!(self, ":");
                    self.space(ForceSpace::No);
                }
                type_annotation.visit(ctx, self, Some(node));
            }
            NullableTypeAnnotation { type_annotation } => {
                out!(self, "?");
                type_annotation.visit(ctx, self, Some(node));
            }
            QualifiedTypeIdentifier { qualification, id } => {
                qualification.visit(ctx, self, Some(node));
                out!(self, ".");
                id.visit(ctx, self, Some(node));
            }
            TypeofTypeAnnotation { argument } => {
                out!(self, "typeof ");
                argument.visit(ctx, self, Some(node));
            }
            TupleTypeAnnotation { types } => {
                out!(self, "[");
                for (i, ty) in types.iter().enumerate() {
                    if i > 0 {
                        self.comma();
                    }
                    ty.visit(ctx, self, Some(node));
                }
                out!(self, "]");
            }
            ArrayTypeAnnotation { element_type } => {
                element_type.visit(ctx, self, Some(node));
                out!(self, "[]");
            }
            UnionTypeAnnotation { types } => {
                for (i, ty) in types.iter().enumerate() {
                    if i > 0 {
                        self.space(ForceSpace::No);
                        out!(self, "|");
                        self.space(ForceSpace::No);
                    }
                    self.print_child(ctx, Some(*ty), node, ChildPos::Anywhere);
                }
            }
            IntersectionTypeAnnotation { types } => {
                for (i, ty) in types.iter().enumerate() {
                    if i > 0 {
                        self.space(ForceSpace::No);
                        out!(self, "&");
                        self.space(ForceSpace::No);
                    }
                    self.print_child(ctx, Some(*ty), node, ChildPos::Anywhere);
                }
            }
            GenericTypeAnnotation {
                id,
                type_parameters,
            } => {
                id.visit(ctx, self, Some(node));
                if let Some(type_parameters) = type_parameters {
                    type_parameters.visit(ctx, self, Some(node));
                }
            }
            IndexedAccessType {
                object_type,
                index_type,
            } => {
                object_type.visit(ctx, self, Some(node));
                out!(self, "[");
                index_type.visit(ctx, self, Some(node));
                out!(self, "]");
            }
            OptionalIndexedAccessType {
                object_type,
                index_type,
                optional,
            } => {
                object_type.visit(ctx, self, Some(node));
                out!(self, "{}[", if *optional { "?." } else { "" });
                index_type.visit(ctx, self, Some(node));
                out!(self, "]");
            }
            InterfaceTypeAnnotation { extends, body } => {
                out!(self, "interface");
                if !extends.is_empty() {
                    out!(self, " extends ");
                    for (i, extend) in extends.iter().enumerate() {
                        if i > 0 {
                            self.comma();
                        }
                        extend.visit(ctx, self, Some(node));
                    }
                } else {
                    self.space(ForceSpace::No);
                }
                if let Some(body) = body {
                    body.visit(ctx, self, Some(node));
                }
            }

            TypeAlias {
                id,
                type_parameters,
                right,
            }
            | DeclareTypeAlias {
                id,
                type_parameters,
                right,
            } => {
                if matches!(&node.get(ctx).kind, DeclareTypeAlias { .. }) {
                    out!(self, "declare ");
                }
                out!(self, "type ");
                id.visit(ctx, self, Some(node));
                if let Some(type_parameters) = type_parameters {
                    type_parameters.visit(ctx, self, Some(node));
                }
                if self.pretty == Pretty::Yes {
                    out!(self, " = ");
                } else {
                    out!(self, "=");
                }
                right.visit(ctx, self, Some(node));
            }
            OpaqueType {
                id,
                type_parameters,
                impltype,
                supertype,
            } => {
                out!(self, "opaque type ");
                id.visit(ctx, self, Some(node));
                if let Some(type_parameters) = type_parameters {
                    type_parameters.visit(ctx, self, Some(node));
                }
                if let Some(supertype) = supertype {
                    out!(self, ":");
                    self.space(ForceSpace::No);
                    supertype.visit(ctx, self, Some(node));
                }
                if self.pretty == Pretty::Yes {
                    out!(self, " = ");
                } else {
                    out!(self, "=");
                }
                impltype.visit(ctx, self, Some(node));
            }
            InterfaceDeclaration {
                id,
                type_parameters,
                extends,
                body,
            }
            | DeclareInterface {
                id,
                type_parameters,
                extends,
                body,
            } => {
                self.visit_interface(
                    ctx,
                    if matches!(node.get(ctx).kind, InterfaceDeclaration { .. }) {
                        "interface"
                    } else {
                        "declare interface"
                    },
                    *id,
                    *type_parameters,
                    extends,
                    *body,
                    node,
                );
            }
            DeclareOpaqueType {
                id,
                type_parameters,
                impltype,
                supertype,
            } => {
                out!(self, "opaque type ");
                id.visit(ctx, self, Some(node));
                if let Some(type_parameters) = type_parameters {
                    type_parameters.visit(ctx, self, Some(node));
                }
                if let Some(supertype) = supertype {
                    out!(self, ":");
                    self.space(ForceSpace::No);
                    supertype.visit(ctx, self, Some(node));
                }
                if self.pretty == Pretty::Yes {
                    out!(self, " = ");
                } else {
                    out!(self, "=");
                }
                if let Some(impltype) = impltype {
                    impltype.visit(ctx, self, Some(node));
                }
            }
            DeclareClass {
                id,
                type_parameters,
                extends,
                implements,
                mixins,
                body,
            } => {
                out!(self, "declare class ");
                id.visit(ctx, self, Some(node));
                if let Some(type_parameters) = type_parameters {
                    type_parameters.visit(ctx, self, Some(node));
                }
                if !extends.is_empty() {
                    out!(self, " extends ");
                    for (i, extend) in extends.iter().enumerate() {
                        if i > 0 {
                            self.comma();
                        }
                        extend.visit(ctx, self, Some(node));
                    }
                }
                if !mixins.is_empty() {
                    out!(self, " mixins ");
                    for (i, mixin) in mixins.iter().enumerate() {
                        if i > 0 {
                            self.comma();
                        }
                        mixin.visit(ctx, self, Some(node));
                    }
                }
                if !implements.is_empty() {
                    out!(self, " implements ");
                    for (i, implement) in implements.iter().enumerate() {
                        if i > 0 {
                            self.comma();
                        }
                        implement.visit(ctx, self, Some(node));
                    }
                }
                self.space(ForceSpace::No);
                body.visit(ctx, self, Some(node));
            }
            DeclareFunction { id, predicate } => {
                // This AST type uses the Identifier/TypeAnnotation
                // pairing to put a name on a function header-looking construct,
                // so we have to do some deep matching to get it to come out right.
                out!(self, "declare function ");
                match &id.get(ctx).kind {
                    Identifier {
                        name,
                        type_annotation,
                        ..
                    } => {
                        out!(self, "{}", &name.str);
                        match type_annotation {
                            None => {
                                unimplemented!("Malformed AST: Need to handle error");
                            }
                            Some(type_annotation) => match &type_annotation.get(ctx).kind {
                                TypeAnnotation { type_annotation } => {
                                    match &type_annotation.get(ctx).kind {
                                        FunctionTypeAnnotation {
                                            params,
                                            this,
                                            return_type,
                                            rest,
                                            type_parameters,
                                        } => {
                                            self.visit_func_type_params(
                                                ctx,
                                                params,
                                                *this,
                                                *rest,
                                                *type_parameters,
                                                node,
                                            );
                                            out!(self, ":");
                                            self.space(ForceSpace::No);
                                            return_type.visit(ctx, self, Some(node));
                                        }
                                        _ => {
                                            unimplemented!("Malformed AST: Need to handle error");
                                        }
                                    }
                                }
                                _ => {
                                    unimplemented!("Malformed AST: Need to handle error");
                                }
                            },
                        }
                        if let Some(predicate) = predicate {
                            self.space(ForceSpace::No);
                            predicate.visit(ctx, self, Some(node));
                        }
                    }
                    _ => {
                        unimplemented!("Malformed AST: Need to handle error");
                    }
                }
            }
            DeclareVariable { id } => {
                if let Some(parent) = parent {
                    if !matches!(parent.get(ctx).kind, DeclareExportDeclaration { .. }) {
                        out!(self, "declare ");
                    }
                }
                id.visit(ctx, self, Some(node));
            }
            DeclareExportDeclaration {
                declaration,
                specifiers,
                source,
                default,
            } => {
                out!(self, "declare export ");
                if *default {
                    out!(self, "default ");
                }
                if let Some(declaration) = declaration {
                    declaration.visit(ctx, self, Some(node));
                } else {
                    out!(self, "{{");
                    for (i, spec) in specifiers.iter().enumerate() {
                        if i > 0 {
                            self.comma();
                        }
                        spec.visit(ctx, self, Some(node));
                    }
                    out!(self, "}}");
                    if let Some(source) = source {
                        out!(self, " from ");
                        source.visit(ctx, self, Some(node));
                    }
                }
            }
            DeclareExportAllDeclaration { source } => {
                out!(self, "declare export * from ");
                source.visit(ctx, self, Some(node));
            }
            DeclareModule { id, body, .. } => {
                out!(self, "declare module ");
                id.visit(ctx, self, Some(node));
                self.space(ForceSpace::No);
                body.visit(ctx, self, Some(node));
            }
            DeclareModuleExports { type_annotation } => {
                out!(self, "declare module.exports:");
                self.space(ForceSpace::No);
                type_annotation.visit(ctx, self, Some(node));
            }

            InterfaceExtends {
                id,
                type_parameters,
            }
            | ClassImplements {
                id,
                type_parameters,
            } => {
                id.visit(ctx, self, Some(node));
                if let Some(type_parameters) = type_parameters {
                    type_parameters.visit(ctx, self, Some(node));
                }
            }

            TypeAnnotation { type_annotation } => {
                type_annotation.visit(ctx, self, Some(node));
            }
            ObjectTypeAnnotation {
                properties,
                indexers,
                call_properties,
                internal_slots,
                inexact,
                exact,
            } => {
                out!(self, "{}", if *exact { "{|" } else { "{" });
                self.inc_indent();
                self.newline();

                let mut need_comma = false;

                for prop in properties {
                    if need_comma {
                        self.comma();
                    }
                    prop.visit(ctx, self, Some(node));
                    self.newline();
                    need_comma = true;
                }
                for prop in indexers {
                    if need_comma {
                        self.comma();
                    }
                    prop.visit(ctx, self, Some(node));
                    self.newline();
                    need_comma = true;
                }
                for prop in call_properties {
                    if need_comma {
                        self.comma();
                    }
                    prop.visit(ctx, self, Some(node));
                    self.newline();
                    need_comma = true;
                }
                for prop in internal_slots {
                    if need_comma {
                        self.comma();
                    }
                    prop.visit(ctx, self, Some(node));
                    self.newline();
                    need_comma = true;
                }

                if *inexact {
                    if need_comma {
                        self.comma();
                    }
                    out!(self, "...");
                }

                self.dec_indent();
                self.newline();
                out!(self, "{}", if *exact { "|}" } else { "}" });
            }
            ObjectTypeProperty {
                key,
                value,
                method,
                optional,
                is_static,
                proto,
                variance,
                ..
            } => {
                if let Some(variance) = variance {
                    variance.visit(ctx, self, Some(node));
                }
                if *is_static {
                    out!(self, "static ");
                }
                if *proto {
                    out!(self, "proto ");
                }
                key.visit(ctx, self, Some(node));
                if *optional {
                    out!(self, "?");
                }
                if *method {
                    match &value.get(ctx).kind {
                        FunctionTypeAnnotation {
                            params,
                            this,
                            return_type,
                            rest,
                            type_parameters,
                        } => {
                            self.visit_func_type_params(
                                ctx,
                                params,
                                *this,
                                *rest,
                                *type_parameters,
                                node,
                            );
                            out!(self, ":");
                            self.space(ForceSpace::No);
                            return_type.visit(ctx, self, Some(node));
                        }
                        _ => {
                            unimplemented!("Malformed AST: Need to handle error");
                        }
                    }
                } else {
                    out!(self, ":");
                    self.space(ForceSpace::No);
                    value.visit(ctx, self, Some(node));
                }
            }
            ObjectTypeSpreadProperty { argument } => {
                out!(self, "...");
                argument.visit(ctx, self, Some(node));
            }
            ObjectTypeInternalSlot {
                id,
                value,
                optional,
                is_static,
                method,
            } => {
                if *is_static {
                    out!(self, "static ");
                }
                out!(self, "[[");
                id.visit(ctx, self, Some(node));
                if *optional {
                    out!(self, "?");
                }
                out!(self, "]]");
                if *method {
                    match &value.get(ctx).kind {
                        FunctionTypeAnnotation {
                            params,
                            this,
                            return_type,
                            rest,
                            type_parameters,
                        } => {
                            self.visit_func_type_params(
                                ctx,
                                params,
                                *this,
                                *rest,
                                *type_parameters,
                                node,
                            );
                            out!(self, ":");
                            self.space(ForceSpace::No);
                            return_type.visit(ctx, self, Some(node));
                        }
                        _ => {
                            unimplemented!("Malformed AST: Need to handle error");
                        }
                    }
                } else {
                    out!(self, ":");
                    self.space(ForceSpace::No);
                    value.visit(ctx, self, Some(node));
                }
            }
            ObjectTypeCallProperty { value, is_static } => {
                if *is_static {
                    out!(self, "static ");
                }
                match &value.get(ctx).kind {
                    FunctionTypeAnnotation {
                        params,
                        this,
                        return_type,
                        rest,
                        type_parameters,
                    } => {
                        self.visit_func_type_params(
                            ctx,
                            params,
                            *this,
                            *rest,
                            *type_parameters,
                            node,
                        );
                        out!(self, ":");
                        self.space(ForceSpace::No);
                        return_type.visit(ctx, self, Some(node));
                    }
                    _ => {
                        unimplemented!("Malformed AST: Need to handle error");
                    }
                }
            }
            ObjectTypeIndexer {
                id,
                key,
                value,
                is_static,
                variance,
            } => {
                if *is_static {
                    out!(self, "static ");
                }
                if let Some(variance) = variance {
                    variance.visit(ctx, self, Some(node));
                }
                out!(self, "[");
                if let Some(id) = id {
                    id.visit(ctx, self, Some(node));
                    out!(self, ":");
                    self.space(ForceSpace::No);
                }
                key.visit(ctx, self, Some(node));
                out!(self, "]");
                out!(self, ":");
                self.space(ForceSpace::No);
                value.visit(ctx, self, Some(node));
            }
            Variance { kind } => {
                out!(
                    self,
                    "{}",
                    match kind.str.as_str() {
                        "plus" => "+",
                        "minus" => "-",
                        _ => unimplemented!("Malformed variance"),
                    }
                )
            }

            TypeParameterDeclaration { params } | TypeParameterInstantiation { params } => {
                out!(self, "<");
                for (i, param) in params.iter().enumerate() {
                    if i > 0 {
                        self.comma();
                    }
                    param.visit(ctx, self, Some(node));
                }
                out!(self, ">");
            }
            TypeParameter {
                name,
                bound,
                variance,
                default,
            } => {
                if let Some(variance) = variance {
                    variance.visit(ctx, self, Some(node));
                }
                out!(self, "{}", &name.str);
                if let Some(bound) = bound {
                    out!(self, ":");
                    self.space(ForceSpace::No);
                    bound.visit(ctx, self, Some(node));
                }
                if let Some(default) = default {
                    out!(self, "=");
                    self.space(ForceSpace::No);
                    default.visit(ctx, self, Some(node));
                }
            }
            TypeCastExpression {
                expression,
                type_annotation,
            } => {
                // Type casts are required to have parentheses.
                out!(self, "(");
                self.print_child(ctx, Some(*expression), node, ChildPos::Left);
                out!(self, ":");
                self.space(ForceSpace::No);
                self.print_child(ctx, Some(*type_annotation), node, ChildPos::Right);
            }
            InferredPredicate => {
                out!(self, "%checks");
            }
            DeclaredPredicate { value } => {
                out!(self, "%checks(");
                value.visit(ctx, self, Some(node));
                out!(self, ")");
            }

            EnumDeclaration { id, body } => {
                out!(self, "enum ");
                id.visit(ctx, self, Some(node));
                body.visit(ctx, self, Some(node));
            }
            EnumStringBody {
                members,
                explicit_type,
                has_unknown_members,
            } => {
                self.visit_enum_body(
                    ctx,
                    "string",
                    members,
                    *explicit_type,
                    *has_unknown_members,
                    node,
                );
            }
            EnumNumberBody {
                members,
                explicit_type,
                has_unknown_members,
            } => {
                self.visit_enum_body(
                    ctx,
                    "number",
                    members,
                    *explicit_type,
                    *has_unknown_members,
                    node,
                );
            }
            EnumBooleanBody {
                members,
                explicit_type,
                has_unknown_members,
            } => {
                self.visit_enum_body(
                    ctx,
                    "boolean",
                    members,
                    *explicit_type,
                    *has_unknown_members,
                    node,
                );
            }
            EnumSymbolBody {
                members,
                has_unknown_members,
            } => {
                self.visit_enum_body(ctx, "symbol", members, true, *has_unknown_members, node);
            }
            EnumDefaultedMember { id } => {
                id.visit(ctx, self, Some(node));
            }
            EnumStringMember { id, init }
            | EnumNumberMember { id, init }
            | EnumBooleanMember { id, init } => {
                id.visit(ctx, self, Some(node));
                out!(
                    self,
                    "{}",
                    match self.pretty {
                        Pretty::Yes => " = ",
                        Pretty::No => "=",
                    }
                );
                init.visit(ctx, self, Some(node));
            }

            _ => {
                unimplemented!("Cannot generate node kind: {}", node.get(ctx).kind.name());
            }
        };
    }

    /// Increase the indent level.
    fn inc_indent(&mut self) {
        self.indent += self.indent_step;
    }

    /// Decrease the indent level.
    fn dec_indent(&mut self) {
        self.indent -= self.indent_step;
    }

    /// Print a ',', with a trailing space in pretty mode.
    fn comma(&mut self) {
        out!(
            self,
            "{}",
            match self.pretty {
                Pretty::No => ",",
                Pretty::Yes => ", ",
            }
        )
    }

    /// Print a ' ' if forced by ForceSpace::Yes or pretty mode.
    fn space(&mut self, force: ForceSpace) {
        if self.pretty == Pretty::Yes || force == ForceSpace::Yes {
            out!(self, " ");
        }
    }

    /// Print a newline and indent if pretty.
    fn newline(&mut self) {
        if self.pretty == Pretty::Yes {
            self.force_newline();
        }
    }

    /// Print a newline and indent.
    fn force_newline(&mut self) {
        self.force_newline_without_indent();
        out!(self, "{:indent$}", "", indent = self.indent as usize);
    }

    /// Print a newline without any indent after.
    fn force_newline_without_indent(&mut self) {
        if self.error.is_none() {
            if let Err(e) = self.out.write(&[b'\n']) {
                self.error = Some(e);
            }
        }
        self.position.line += 1;
        self.position.col = 1;
    }

    /// Print the child of a `parent` node at the position `child_pos`.
    fn print_child(
        &mut self,
        ctx: &Context,
        child: Option<NodePtr>,
        parent: NodePtr,
        child_pos: ChildPos,
    ) {
        if let Some(child) = child {
            self.print_parens(
                ctx,
                child,
                parent,
                self.need_parens(ctx, parent, child, child_pos),
            );
        }
    }

    /// Print one expression in a sequence separated by comma. It needs parens
    /// if its precedence is <= comma.
    fn print_comma_expression(&mut self, ctx: &Context, child: NodePtr, parent: NodePtr) {
        self.print_parens(
            ctx,
            child,
            parent,
            NeedParens::from(self.get_precedence(child.get(ctx)).0 <= precedence::SEQ),
        )
    }

    fn print_parens(
        &mut self,
        ctx: &Context,
        child: NodePtr,
        parent: NodePtr,
        need_parens: NeedParens,
    ) {
        if need_parens == NeedParens::Yes {
            out!(self, "(");
        } else if need_parens == NeedParens::Space {
            out!(self, " ");
        }
        child.visit(ctx, self, Some(parent));
        if need_parens == NeedParens::Yes {
            out!(self, ")");
        }
    }

    fn print_escaped_string_literal(&mut self, value: &NodeString, esc: char) {
        for &c in &value.str {
            let c8 = char::from(c as u8);
            match c8 {
                '\\' => {
                    out!(self, "\\\\");
                    continue;
                }
                '\x08' => {
                    out!(self, "\\b");
                    continue;
                }
                '\x0c' => {
                    out!(self, "\\f");
                    continue;
                }
                '\n' => {
                    out!(self, "\\n");
                    continue;
                }
                '\r' => {
                    out!(self, "\\r");
                    continue;
                }
                '\t' => {
                    out!(self, "\\t");
                    continue;
                }
                '\x0b' => {
                    out!(self, "\\v");
                    continue;
                }
                _ => {}
            };
            if c == esc as u16 {
                out!(self, "\\");
            }
            if (0x20..=0x7f).contains(&c) {
                // Printable.
                out!(self, "{}", c8);
            } else {
                out!(self, "\\u{:04x}", c);
            }
        }
    }

    fn visit_props(&mut self, ctx: &Context, props: &[NodePtr], parent: NodePtr) {
        out!(self, "{{");
        for (i, prop) in props.iter().enumerate() {
            if i > 0 {
                self.comma();
            }
            prop.visit(ctx, self, Some(parent));
        }
        out!(self, "}}");
    }

    fn visit_func_params_body(
        &mut self,
        ctx: &Context,
        params: &[NodePtr],
        return_type: Option<NodePtr>,
        predicate: Option<NodePtr>,
        body: NodePtr,
        node: NodePtr,
    ) {
        out!(self, "(");
        for (i, param) in params.iter().enumerate() {
            if i > 0 {
                self.comma();
            }
            param.visit(ctx, self, Some(node));
        }
        out!(self, ")");
        if let Some(return_type) = return_type {
            out!(self, ":");
            self.space(ForceSpace::No);
            return_type.visit(ctx, self, Some(node));
        }
        if let Some(predicate) = predicate {
            self.space(ForceSpace::Yes);
            predicate.visit(ctx, self, Some(node));
        }
        self.space(ForceSpace::No);
        body.visit(ctx, self, Some(node));
    }

    fn visit_func_type_params(
        &mut self,
        ctx: &Context,
        params: &[NodePtr],
        this: Option<NodePtr>,
        rest: Option<NodePtr>,
        type_parameters: Option<NodePtr>,
        node: NodePtr,
    ) {
        use NodeKind::*;
        if let Some(type_parameters) = type_parameters {
            type_parameters.visit(ctx, self, Some(node));
        }
        out!(self, "(");
        let mut need_comma = false;
        if let Some(this) = this {
            match &this.get(ctx).kind {
                FunctionTypeParam {
                    type_annotation, ..
                } => {
                    out!(self, "this:");
                    self.space(ForceSpace::No);
                    type_annotation.visit(ctx, self, Some(node));
                }
                _ => {
                    unimplemented!("Malformed AST: Need to handle error");
                }
            }
            this.visit(ctx, self, Some(node));
            need_comma = true;
        }
        for param in params.iter() {
            if need_comma {
                self.comma();
            }
            param.visit(ctx, self, Some(node));
            need_comma = true;
        }
        if let Some(rest) = rest {
            if need_comma {
                self.comma();
            }
            out!(self, "...");
            rest.visit(ctx, self, Some(node));
        }
        out!(self, ")");
    }

    #[allow(clippy::too_many_arguments)]
    fn visit_interface(
        &mut self,
        ctx: &Context,
        decl: &str,
        id: NodePtr,
        type_parameters: Option<NodePtr>,
        extends: &[NodePtr],
        body: NodePtr,
        node: NodePtr,
    ) {
        out!(self, "{} ", decl);
        id.visit(ctx, self, Some(node));
        if let Some(type_parameters) = type_parameters {
            type_parameters.visit(ctx, self, Some(node));
        }
        self.space(ForceSpace::No);
        if !extends.is_empty() {
            out!(self, "extends ");
            for (i, extend) in extends.iter().enumerate() {
                if i > 0 {
                    self.comma();
                }
                extend.visit(ctx, self, Some(node));
            }
            self.space(ForceSpace::No);
        }
        body.visit(ctx, self, Some(node));
    }

    /// Generate the body of a Flow enum with type `kind`.
    fn visit_enum_body(
        &mut self,
        ctx: &Context,
        kind: &str,
        members: &[NodePtr],
        explicit_type: bool,
        has_unknown_members: bool,
        node: NodePtr,
    ) {
        if explicit_type {
            out!(self, ":");
            self.space(ForceSpace::No);
            out!(self, "{}", kind);
        }
        out!(self, "{{");
        self.inc_indent();
        self.newline();

        for (i, member) in members.iter().enumerate() {
            if i > 0 {
                self.comma();
                self.newline();
            }
            member.visit(ctx, self, Some(node));
        }

        if has_unknown_members {
            if !members.is_empty() {
                self.comma();
                self.newline();
            }
            out!(self, "...");
        }

        self.dec_indent();
        self.newline();
        out!(self, "}}");
    }

    /// Visit a statement node which is the body of a loop or a clause in an if.
    /// It could be a block statement.
    /// Return true if block
    fn visit_stmt_or_block(
        &mut self,
        ctx: &Context,
        node: NodePtr,
        force_block: ForceBlock,
        parent: NodePtr,
    ) -> bool {
        use NodeKind::*;
        if let BlockStatement { body } = &node.get(ctx).kind {
            if body.is_empty() {
                self.space(ForceSpace::No);
                out!(self, "{{}}");
                return true;
            }
            self.space(ForceSpace::No);
            out!(self, "{{");
            self.inc_indent();
            self.newline();
            self.visit_stmt_list(ctx, body, node);
            self.dec_indent();
            self.newline();
            out!(self, "}}");
            return true;
        }
        if force_block == ForceBlock::Yes {
            self.space(ForceSpace::No);
            out!(self, "{{");
            self.inc_indent();
            self.newline();
            self.visit_stmt_in_block(ctx, node, parent);
            self.dec_indent();
            self.newline();
            out!(self, "}}");
            true
        } else {
            self.inc_indent();
            self.newline();
            node.visit(ctx, self, Some(parent));
            self.dec_indent();
            self.newline();
            false
        }
    }

    fn visit_stmt_list(&mut self, ctx: &Context, list: &[NodePtr], parent: NodePtr) {
        for (i, stmt) in list.iter().enumerate() {
            if i > 0 {
                self.newline();
            }
            self.visit_stmt_in_block(ctx, *stmt, parent);
        }
    }

    fn visit_stmt_in_block(&mut self, ctx: &Context, stmt: NodePtr, parent: NodePtr) {
        stmt.visit(ctx, self, Some(parent));
        if !ends_with_block(ctx, Some(stmt)) {
            out!(self, ";");
        }
    }

    /// Return the precedence and associativity of `node`.
    fn get_precedence(&self, node: &Node) -> (precedence::Precedence, Assoc) {
        // Precedence order taken from
        // https://github.com/facebook/flow/blob/master/src/parser_utils/output/js_layout_generator.ml
        use precedence::*;
        use NodeKind::*;
        match &node.kind {
            Identifier { .. }
            | NullLiteral { .. }
            | BooleanLiteral { .. }
            | StringLiteral { .. }
            | NumericLiteral { .. }
            | RegExpLiteral { .. }
            | ThisExpression { .. }
            | Super { .. }
            | ArrayExpression { .. }
            | ObjectExpression { .. }
            | ObjectPattern { .. }
            | FunctionExpression { .. }
            | ClassExpression { .. }
            | TemplateLiteral { .. } => (PRIMARY, Assoc::Ltr),
            MemberExpression { .. }
            | OptionalMemberExpression { .. }
            | MetaProperty { .. }
            | CallExpression { .. }
            | OptionalCallExpression { .. } => (MEMBER, Assoc::Ltr),
            NewExpression { arguments, .. } => {
                // `new foo()` has higher precedence than `new foo`. In pretty mode we
                // always append the `()`, but otherwise we must check the number of args.
                if self.pretty == Pretty::Yes || !arguments.is_empty() {
                    (MEMBER, Assoc::Ltr)
                } else {
                    (NEW_NO_ARGS, Assoc::Ltr)
                }
            }
            TaggedTemplateExpression { .. } | ImportExpression { .. } => {
                (TAGGED_TEMPLATE, Assoc::Ltr)
            }
            UpdateExpression { prefix, .. } => {
                if *prefix {
                    (POST_UPDATE, Assoc::Ltr)
                } else {
                    (UNARY, Assoc::Rtl)
                }
            }
            UnaryExpression { .. } => (UNARY, Assoc::Rtl),
            BinaryExpression { operator, .. } => (get_binary_precedence(*operator), Assoc::Ltr),
            LogicalExpression { operator, .. } => (get_logical_precedence(*operator), Assoc::Ltr),
            ConditionalExpression { .. } => (COND, Assoc::Rtl),
            AssignmentExpression { .. } => (ASSIGN, Assoc::Rtl),
            YieldExpression { .. } | ArrowFunctionExpression { .. } => (YIELD, Assoc::Ltr),
            SequenceExpression { .. } => (SEQ, Assoc::Rtl),

            ExistsTypeAnnotation
            | EmptyTypeAnnotation
            | StringTypeAnnotation
            | NumberTypeAnnotation
            | StringLiteralTypeAnnotation { .. }
            | NumberLiteralTypeAnnotation { .. }
            | BooleanTypeAnnotation
            | BooleanLiteralTypeAnnotation { .. }
            | NullLiteralTypeAnnotation
            | SymbolTypeAnnotation
            | AnyTypeAnnotation
            | MixedTypeAnnotation
            | VoidTypeAnnotation => (PRIMARY, Assoc::Ltr),
            UnionTypeAnnotation { .. } => (UNION_TYPE, Assoc::Ltr),
            IntersectionTypeAnnotation { .. } => (INTERSECTION_TYPE, Assoc::Ltr),

            _ => (ALWAYS_PAREN, Assoc::Ltr),
        }
    }

    /// Return whether parentheses are needed around the `child` node,
    /// which is situated at `child_pos` position in relation to its `parent`.
    fn need_parens(
        &self,
        ctx: &Context,
        parent: NodePtr,
        child: NodePtr,
        child_pos: ChildPos,
    ) -> NeedParens {
        use NodeKind::*;

        let parent_node = parent.get(ctx);
        let child_node = child.get(ctx);

        #[allow(clippy::if_same_then_else)]
        if matches!(parent_node.kind, ArrowFunctionExpression { .. }) {
            // (x) => ({x: 10}) needs parens to avoid confusing it with a block and a
            // labelled statement.
            if child_pos == ChildPos::Right && matches!(child_node.kind, ObjectExpression { .. }) {
                return NeedParens::Yes;
            }
        } else if matches!(parent_node.kind, ForStatement { .. }) {
            // for((a in b);..;..) needs parens to avoid confusing it with for(a in b).
            return NeedParens::from(match &child_node.kind {
                BinaryExpression { operator, .. } => *operator == BinaryExpressionOperator::In,
                _ => false,
            });
        } else if matches!(parent_node.kind, ExpressionStatement { .. }) {
            // Expression statement like (function () {} + 1) needs parens.
            return NeedParens::from(self.root_starts_with(ctx, child, |kind| -> bool {
                matches!(
                    kind,
                    FunctionExpression { .. }
                        | ClassExpression { .. }
                        | ObjectExpression { .. }
                        | ObjectPattern { .. }
                )
            }));
        } else if (parent_node.kind.is_unary_op(UnaryExpressionOperator::Minus)
            && self.root_starts_with(ctx, child, NodeKind::check_minus))
            || (parent_node.kind.is_unary_op(UnaryExpressionOperator::Plus)
                && self.root_starts_with(ctx, child, NodeKind::check_plus))
            || (child_pos == ChildPos::Right
                && parent_node
                    .kind
                    .is_binary_op(BinaryExpressionOperator::Minus)
                && self.root_starts_with(ctx, child, NodeKind::check_minus))
            || (child_pos == ChildPos::Right
                && parent_node
                    .kind
                    .is_binary_op(BinaryExpressionOperator::Plus)
                && self.root_starts_with(ctx, child, NodeKind::check_plus))
        {
            // -(-x) or -(--x) or -(-5)
            // +(+x) or +(++x)
            // a-(-x) or a-(--x) or a-(-5)
            // a+(+x) or a+(++x)
            return if self.pretty == Pretty::Yes {
                NeedParens::Yes
            } else {
                NeedParens::Space
            };
        } else if matches!(
            parent_node.kind,
            MemberExpression { .. } | CallExpression { .. }
        ) && matches!(
            child_node.kind,
            OptionalMemberExpression { .. } | OptionalCallExpression { .. }
        ) && child_pos == ChildPos::Left
        {
            // When optional chains are terminated by non-optional member/calls,
            // we need the left hand side to be parenthesized.
            // Avoids confusing `(a?.b).c` with `a?.b.c`.
            return NeedParens::Yes;
        } else if (parent_node.kind.check_and_or() && child_node.kind.check_nullish())
            || (parent_node.kind.check_nullish() && child_node.kind.check_and_or())
        {
            // Nullish coalescing always requires parens when mixed with any
            // other logical operations.
            return NeedParens::Yes;
        }

        let (child_prec, _child_assoc) = self.get_precedence(child_node);
        if child_prec == precedence::ALWAYS_PAREN {
            return NeedParens::Yes;
        }

        let (parent_prec, parent_assoc) = self.get_precedence(parent_node);

        if child_prec < parent_prec {
            // Child is definitely a danger.
            return NeedParens::Yes;
        }
        if child_prec > parent_prec {
            // Definitely cool.
            return NeedParens::No;
        }
        // Equal precedence, so associativity (rtl/ltr) is what matters.
        if child_pos == ChildPos::Anywhere {
            // Child could be anywhere, so always paren.
            return NeedParens::Yes;
        }
        if child_prec == precedence::TOP {
            // Both precedences are safe.
            return NeedParens::No;
        }
        // Check if child is on the dangerous side.
        NeedParens::from(if parent_assoc == Assoc::Rtl {
            child_pos == ChildPos::Left
        } else {
            child_pos == ChildPos::Right
        })
    }

    fn root_starts_with<F: Fn(&NodeKind) -> bool>(
        &self,
        ctx: &Context,
        expr: NodePtr,
        pred: F,
    ) -> bool {
        self.expr_starts_with(ctx, expr, None, pred)
    }

    fn expr_starts_with<F: Fn(&NodeKind) -> bool>(
        &self,
        ctx: &Context,
        expr: NodePtr,
        parent: Option<NodePtr>,
        pred: F,
    ) -> bool {
        use NodeKind::*;

        if let Some(parent) = parent {
            if self.need_parens(ctx, parent, expr, ChildPos::Left) == NeedParens::Yes {
                return false;
            }
        }

        if pred(&expr.get(ctx).kind) {
            return true;
        }

        // Ensure the recursive calls are the last things to run,
        // hopefully the compiler makes this into a loop.
        match &expr.get(ctx).kind {
            CallExpression { callee, .. } => self.expr_starts_with(ctx, *callee, Some(expr), pred),
            OptionalCallExpression { callee, .. } => {
                self.expr_starts_with(ctx, *callee, Some(expr), pred)
            }
            BinaryExpression { left, .. } => self.expr_starts_with(ctx, *left, Some(expr), pred),
            LogicalExpression { left, .. } => self.expr_starts_with(ctx, *left, Some(expr), pred),
            ConditionalExpression { test, .. } => {
                self.expr_starts_with(ctx, *test, Some(expr), pred)
            }
            AssignmentExpression { left, .. } => {
                self.expr_starts_with(ctx, *left, Some(expr), pred)
            }
            UpdateExpression {
                prefix, argument, ..
            } => !*prefix && self.expr_starts_with(ctx, *argument, Some(expr), pred),
            UnaryExpression {
                prefix, argument, ..
            } => !*prefix && self.expr_starts_with(ctx, *argument, Some(expr), pred),
            MemberExpression { object, .. } | OptionalMemberExpression { object, .. } => {
                self.expr_starts_with(ctx, *object, Some(expr), pred)
            }
            TaggedTemplateExpression { tag, .. } => {
                self.expr_starts_with(ctx, *tag, Some(expr), pred)
            }
            _ => false,
        }
    }

    /// Adds the current location as a segment pointing to the start of `node`.
    fn add_segment(&mut self, node: &Node) {
        // Convert from 1-indexed to 0-indexed as expected by source map.
        let new_token = Some(RawToken {
            dst_line: self.position.line - 1,
            dst_col: self.position.col - 1,
            src_line: node.range.start.line - 1,
            src_col: node.range.start.col - 1,
            src_id: 0,
            name_id: !0,
        });
        self.flush_cur_token();
        self.cur_token = new_token;
    }

    /// Add the `cur_token` to the sourcemap and set `cur_token` to `None`.
    fn flush_cur_token(&mut self) {
        if let Some(cur) = self.cur_token {
            self.sourcemap.add_raw(
                cur.dst_line,
                cur.dst_col,
                cur.src_line,
                cur.src_col,
                if cur.src_id != !0 {
                    Some(cur.src_id)
                } else {
                    None
                },
                if cur.name_id != !0 {
                    Some(cur.name_id)
                } else {
                    None
                },
            );
        }
        self.cur_token = None;
    }
}

impl<W: Write> Visitor for GenJS<W> {
    fn call(&mut self, ctx: &Context, node: NodePtr, parent: Option<NodePtr>) {
        self.gen_node(ctx, node, parent);
    }
}

impl NodeKind {
    fn is_unary_op(&self, op: UnaryExpressionOperator) -> bool {
        match self {
            NodeKind::UnaryExpression { operator, .. } => *operator == op,
            _ => false,
        }
    }

    fn is_update_prefix(&self, op: UpdateExpressionOperator) -> bool {
        match self {
            NodeKind::UpdateExpression {
                prefix, operator, ..
            } => *prefix && *operator == op,
            _ => false,
        }
    }

    fn is_negative_number(&self) -> bool {
        match self {
            NodeKind::NumericLiteral { value, .. } => *value < 0f64,
            _ => false,
        }
    }

    fn is_binary_op(&self, op: BinaryExpressionOperator) -> bool {
        match self {
            NodeKind::BinaryExpression { operator, .. } => *operator == op,
            _ => false,
        }
    }

    fn is_if_without_else(&self) -> bool {
        match self {
            NodeKind::IfStatement { alternate, .. } => alternate.is_none(),
            _ => false,
        }
    }

    fn check_plus(&self) -> bool {
        self.is_unary_op(UnaryExpressionOperator::Plus)
            || self.is_update_prefix(UpdateExpressionOperator::Increment)
    }

    fn check_minus(&self) -> bool {
        self.is_unary_op(UnaryExpressionOperator::Minus)
            || self.is_update_prefix(UpdateExpressionOperator::Decrement)
    }

    fn check_and_or(&self) -> bool {
        matches!(
            self,
            NodeKind::LogicalExpression {
                operator: LogicalExpressionOperator::And | LogicalExpressionOperator::Or,
                ..
            }
        )
    }

    fn check_nullish(&self) -> bool {
        matches!(
            self,
            NodeKind::LogicalExpression {
                operator: LogicalExpressionOperator::NullishCoalesce,
                ..
            }
        )
    }
}

fn ends_with_block(ctx: &Context, node: Option<NodePtr>) -> bool {
    use NodeKind::*;
    match node {
        Some(node) => match &node.get(ctx).kind {
            BlockStatement { .. } | FunctionDeclaration { .. } => true,
            WhileStatement { body, .. } => ends_with_block(ctx, Some(*body)),
            ForStatement { body, .. } => ends_with_block(ctx, Some(*body)),
            ForInStatement { body, .. } => ends_with_block(ctx, Some(*body)),
            ForOfStatement { body, .. } => ends_with_block(ctx, Some(*body)),
            WithStatement { body, .. } => ends_with_block(ctx, Some(*body)),
            SwitchStatement { .. } => true,
            LabeledStatement { body, .. } => ends_with_block(ctx, Some(*body)),
            TryStatement {
                finalizer, handler, ..
            } => ends_with_block(ctx, finalizer.or(*handler)),
            CatchClause { body, .. } => ends_with_block(ctx, Some(*body)),
            IfStatement {
                alternate,
                consequent,
                ..
            } => ends_with_block(ctx, alternate.or(Some(*consequent))),
            ClassDeclaration { .. } => true,
            ExportDefaultDeclaration { declaration } => ends_with_block(ctx, Some(*declaration)),
            ExportNamedDeclaration { declaration, .. } => ends_with_block(ctx, *declaration),
            _ => false,
        },
        None => false,
    }
}
