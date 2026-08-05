//! zts extensions: Rust-style `match` expressions.
//!
//! `match` is a contextual keyword. `match (expr) {` opens a match
//! expression; anything else (`str.match(re)`, `match(1)`, `match(1) + 2`)
//! must keep parsing as vanilla ES. We speculatively parse the header
//! `match ( expr ) {` behind a checkpoint; only once the full header matches
//! do we commit, so arm-level syntax errors are reported as real
//! diagnostics instead of falling back to a bogus call expression.

use swc_atoms::atom;
use swc_common::BytePos;
use swc_ecma_ast::*;

use super::{PResult, Parser};
use crate::{
    context::Context,
    error::{Error, SyntaxError},
    lexer::Token,
    parser::input::Tokens,
};

impl<I: Tokens> Parser<I> {
    /// Cheap gate: zts enabled, cursor on the word `match`, `(` next.
    pub(super) fn is_zts_match_keyword(&mut self) -> bool {
        if !self.input().syntax().zts() {
            return false;
        }
        if !(self.input().cur().is_word()
            && self.input().cur().take_word(&self.input) == atom!("match"))
        {
            return false;
        }

        self.input_mut().peek() == Some(Token::LParen)
    }

    /// Speculatively parses the header `match ( expr ) { Word`.
    ///
    /// Returns `None` (with the parser fully backtracked and any errors
    /// recorded during the speculation rolled back) when the shape is not
    /// a match expression, so `match` falls through to the plain identifier
    /// path. When the header matches, parsing commits without a rewind —
    /// the discriminant is parsed exactly once, with diagnostics live, so
    /// nested matches in discriminant position stay linear-time and
    /// recoverable errors inside the discriminant are reported.
    ///
    /// Disambiguation rules (all deliberate):
    /// - The `{` must sit on the same line as the `)`. ASI makes `match(1)\n{
    ///   ... }` a call statement plus a block statement in vanilla TS, and that
    ///   must keep working.
    /// - The first token inside the braces must be a word (a variant name).
    ///   This rejects `{}` so a call-plus-empty-block never turns into a
    ///   zero-arm match.
    pub(super) fn try_parse_zts_match_expr(
        &mut self,
        start: BytePos,
    ) -> Option<PResult<Box<Expr>>> {
        // Packrat memo of failures: without it, nested `match (match (...`
        // re-runs the speculation once per enclosing attempt — exponential
        // time.
        if self.zts_match_speculation_failures.contains(&start) {
            return None;
        }

        let checkpoint = self.checkpoint_save();
        let err_counts = self.input().iter.error_counts();

        let header = (|| -> PResult<Option<Box<Expr>>> {
            // `match`
            self.bump();
            if !self.input_mut().eat(Token::LParen) {
                return Ok(None);
            }
            let discriminant = self.allow_in_expr(|p| p.parse_expr())?;
            if !self.input_mut().eat(Token::RParen) {
                return Ok(None);
            }
            if !self.input().is(Token::LBrace) || self.input_mut().had_line_break_before_cur() {
                return Ok(None);
            }
            if !self.input_mut().peek().is_some_and(|t| t.is_word()) {
                return Ok(None);
            }
            Ok(Some(discriminant))
        })();

        match header {
            Ok(Some(discriminant)) => Some(self.parse_zts_match_body(start, discriminant)),
            Ok(None) | Err(..) => {
                // Backtrack: rewind tokens AND drop any recoverable errors
                // the failed speculation recorded.
                self.checkpoint_load(checkpoint);
                self.input_mut().iter.truncate_errors(err_counts);
                self.zts_match_speculation_failures.insert(start);
                None
            }
        }
    }

    /// Parses `{ Arm, Arm, }` after a committed header. The cursor sits on
    /// the opening `{`.
    fn parse_zts_match_body(
        &mut self,
        start: BytePos,
        discriminant: Box<Expr>,
    ) -> PResult<Box<Expr>> {
        self.assert_and_bump(Token::LBrace);

        let mut arms = Vec::new();
        while !self.input().is(Token::RBrace) {
            arms.push(self.parse_zts_match_arm()?);
            if !self.input_mut().eat(Token::Comma) {
                break;
            }
        }
        expect!(self, Token::RBrace);

        Ok(Box::new(Expr::Match(MatchExpr {
            span: self.span(start),
            discriminant,
            arms,
        })))
    }

    /// Dispatches enum parsing: zts grammar when the zts flag is on, the
    /// stock TS enum otherwise. Call sites have already consumed `enum`.
    pub(super) fn parse_any_enum_decl(&mut self, start: BytePos, is_const: bool) -> PResult<Decl> {
        if self.input().syntax().zts() {
            return self.parse_zts_enum_decl(start, is_const);
        }
        self.parse_ts_enum_decl(start, is_const).map(Decl::from)
    }

    /// `enum Shape { Circle { radius: number }, Square { side: number } }`
    ///
    /// zts `enum` deliberately replaces TS `enum` (the one place zts is not
    /// a strict superset). TS member syntax gets a friendly hard error.
    fn parse_zts_enum_decl(&mut self, start: BytePos, is_const: bool) -> PResult<Decl> {
        if self.ctx().contains(Context::InDeclare) {
            return Err(Error::new(self.span(start), SyntaxError::ZtsDeclareEnum));
        }
        if is_const {
            // Recoverable: parse the body anyway for better diagnostics.
            self.emit_err(self.span(start), SyntaxError::ZtsConstEnum);
        }

        let ident = self.parse_ident_name()?;
        let ident = Ident::new_no_ctxt(ident.sym, ident.span);

        expect!(self, Token::LBrace);
        let mut variants = Vec::new();
        while !self.input().is(Token::RBrace) {
            variants.push(self.parse_zts_enum_variant()?);
            if !self.input_mut().eat(Token::Comma) {
                break;
            }
        }
        expect!(self, Token::RBrace);

        Ok(Decl::ZtsEnum(Box::new(ZtsEnumDecl {
            span: self.span(start),
            ident,
            variants,
        })))
    }

    /// `Variant { field: Type, ... }` — braces required, fields optional.
    fn parse_zts_enum_variant(&mut self) -> PResult<ZtsEnumVariant> {
        let start = self.input().cur_pos();

        let name = self.parse_ident_name()?;
        let name = Ident::new_no_ctxt(name.sym, name.span);

        if !self.input().is(Token::LBrace) {
            // `Red,` / `Red = 1` — TS member syntax.
            return Err(Error::new(
                self.span(start),
                SyntaxError::ZtsEnumVariantBody,
            ));
        }
        self.bump();

        let mut fields = Vec::new();
        while !self.input().is(Token::RBrace) {
            fields.push(self.parse_zts_enum_field()?);
            if !self.input_mut().eat(Token::Comma) {
                break;
            }
        }
        expect!(self, Token::RBrace);

        Ok(ZtsEnumVariant {
            span: self.span(start),
            name,
            fields,
        })
    }

    /// `field: Type`
    fn parse_zts_enum_field(&mut self) -> PResult<ZtsEnumField> {
        let start = self.input().cur_pos();
        let name = self.parse_ident_name()?;
        expect!(self, Token::Colon);
        let type_ann = self.in_type(|p| p.parse_ts_type())?;

        Ok(ZtsEnumField {
            span: self.span(start),
            name,
            type_ann,
        })
    }

    /// `Variant { bindings } => body`
    ///
    /// The body is an assignment expression, or — mirroring arrow-function
    /// bodies — a block expression when it opens with `{` (a block's value
    /// is its tail expression; object literals need parens, `=> ({ ... })`).
    fn parse_zts_match_arm(&mut self) -> PResult<MatchArm> {
        let arm_start = self.input().cur_pos();

        let variant = self.parse_ident_name()?;
        let variant = Ident::new_no_ctxt(variant.sym, variant.span);

        self.expect_without_advance(Token::LBrace)?;
        let binding = match self.parse_object_pat()? {
            Pat::Object(o) => Some(o),
            _ => unreachable!("parse_object_pat always returns Pat::Object"),
        };

        expect!(self, Token::Arrow);
        let body = if self.input().is(Token::LBrace) {
            let block = self.parse_zts_expr_block()?;
            Box::new(Expr::ZtsExprBlock(block))
        } else {
            self.allow_in_expr(Self::parse_assignment_expr)?
        };

        Ok(MatchArm {
            span: self.span(arm_start),
            variant,
            binding,
            body,
        })
    }

    /// `if (test) { ... } else { ... }` in expression position. `else` is
    /// mandatory; `else if` chains are allowed. The `if` token is current.
    pub(super) fn parse_zts_if_expr(&mut self, start: BytePos) -> PResult<Box<Expr>> {
        self.assert_and_bump(Token::If);
        expect!(self, Token::LParen);
        let test = self.allow_in_expr(|p| p.parse_expr())?;
        expect!(self, Token::RParen);

        let cons = self.parse_zts_expr_block()?;

        if !self.input_mut().eat(Token::Else) {
            return Err(Error::new(self.span(start), SyntaxError::ZtsIfWithoutElse));
        }

        let alt = if self.input().is(Token::If) {
            let alt_start = self.input().cur_pos();
            let expr = self.parse_zts_if_expr(alt_start)?;
            let Expr::ZtsIf(if_expr) = *expr else {
                unreachable!("parse_zts_if_expr returns Expr::ZtsIf")
            };
            ZtsIfAlt::If(Box::new(if_expr))
        } else {
            ZtsIfAlt::Block(self.parse_zts_expr_block()?)
        };

        Ok(Box::new(Expr::ZtsIf(ZtsIfExpr {
            span: self.span(start),
            test,
            cons,
            alt,
        })))
    }

    /// `{ stmts...; tail }` — a block whose value is its final expression.
    fn parse_zts_expr_block(&mut self) -> PResult<ZtsExprBlock> {
        let start = self.input().cur_pos();
        let block = self.parse_block(false)?;

        let mut stmts = block.stmts;
        let Some(Stmt::Expr(tail_stmt)) = stmts.pop() else {
            return Err(Error::new(
                self.span(start),
                SyntaxError::ZtsBlockWithoutTail,
            ));
        };

        Ok(ZtsExprBlock {
            span: self.span(start),
            stmts,
            tail: tail_stmt.expr,
        })
    }
}

#[cfg(test)]
mod tests {
    use swc_ecma_ast::*;

    use crate::{test_parser, Syntax, TsSyntax};

    fn zts() -> Syntax {
        Syntax::Typescript(TsSyntax {
            zts: true,
            ..Default::default()
        })
    }

    fn parse_expr(src: &'static str) -> Box<Expr> {
        test_parser(src, zts(), |p| p.parse_expr())
    }

    /// Parses a module, returning (module_result_is_err,
    /// had_recoverable_errors).
    fn parse_module_errs(src: &'static str) -> (bool, bool) {
        test_parser(src, zts(), |p| {
            let res = p.parse_module();
            let errs = p.take_errors();
            Ok((res.is_err(), !errs.is_empty()))
        })
    }

    #[test]
    fn if_expr_basic() {
        let e = parse_expr("if (b === 0) { 3 } else { 4 }");
        let i = e.expect_zts_if();
        assert!(i.cons.stmts.is_empty());
        assert!(i.alt.is_block());
    }

    #[test]
    fn if_expr_multi_stmt_and_else_if() {
        let e = parse_expr(
            "if (a) { const x = f(); x + 1 } else if (b) { 2 } else { const y = g(); y }",
        );
        let i = e.expect_zts_if();
        assert_eq!(i.cons.stmts.len(), 1);
        let ZtsIfAlt::If(chain) = &i.alt else {
            panic!("expected else-if chain")
        };
        assert!(chain.alt.is_block());
    }

    #[test]
    fn if_expr_without_else_is_an_error() {
        let (is_err, had_errs) = parse_module_errs("const a = if (b) { 1 };");
        assert!(is_err || had_errs, "if-expression without else must error");
    }

    #[test]
    fn if_expr_block_without_tail_is_an_error() {
        let (is_err, had_errs) = parse_module_errs("const a = if (b) { const x = 1; } else { 2 };");
        assert!(
            is_err || had_errs,
            "block without tail expression must error"
        );
    }

    #[test]
    fn if_statement_still_works() {
        let module = test_parser("if (a) { f(); } else { g(); }", zts(), |p| p.parse_module());
        assert!(matches!(module.body[0].as_stmt().unwrap(), Stmt::If(..)));
    }

    #[test]
    fn match_arm_block_body() {
        let e = parse_expr("match (t) { K { v } => { const d = v * 2; d + 1 }, L { w } => w }");
        let m = e.expect_match_expr();
        let body = m.arms[0].body.as_zts_expr_block().unwrap();
        assert_eq!(body.stmts.len(), 1);
    }

    #[test]
    fn zts_enum_basic() {
        let module = test_parser(
            "enum Shape { Circle { radius: number }, Square { side: number } }",
            zts(),
            |p| p.parse_module(),
        );
        let decl = module.body[0].as_stmt().unwrap().as_decl().unwrap();
        let e = decl.as_zts_enum().unwrap();
        assert_eq!(e.ident.sym, "Shape");
        assert_eq!(e.variants.len(), 2);
        assert_eq!(e.variants[0].name.sym, "Circle");
        assert_eq!(e.variants[0].fields.len(), 1);
        assert_eq!(e.variants[0].fields[0].name.sym, "radius");
    }

    #[test]
    fn zts_enum_empty_variant_and_trailing_commas() {
        let module = test_parser("enum E { A {}, B { x: string, }, }", zts(), |p| {
            p.parse_module()
        });
        let decl = module.body[0].as_stmt().unwrap().as_decl().unwrap();
        let e = decl.as_zts_enum().unwrap();
        assert_eq!(e.variants.len(), 2);
        assert!(e.variants[0].fields.is_empty());
    }

    #[test]
    fn zts_enum_exported() {
        let module = test_parser("export enum E { A { v: number } }", zts(), |p| {
            p.parse_module()
        });
        let export = module.body[0].as_module_decl().unwrap();
        let decl = &export.as_export_decl().unwrap().decl;
        assert!(decl.is_zts_enum());
    }

    #[test]
    fn ts_enum_member_syntax_is_a_hard_error() {
        let (is_err, _) = parse_module_errs("enum Color { Red, Green }");
        assert!(is_err, "TS enum member syntax must be rejected under zts");
    }

    #[test]
    fn const_enum_is_an_error_but_recovers() {
        let (is_err, had_errs) = parse_module_errs("const enum E { A { v: number } }");
        assert!(!is_err, "const enum should recover after the diagnostic");
        assert!(had_errs, "const enum must emit a diagnostic");
    }

    #[test]
    fn declare_enum_is_a_hard_error() {
        let (is_err, had_errs) = parse_module_errs("declare enum E { A { v: number } }");
        assert!(is_err || had_errs, "declare enum must be rejected");
    }

    #[test]
    fn ts_enum_still_works_without_zts_flag() {
        let module = test_parser(
            "enum Color { Red, Green }",
            Syntax::Typescript(Default::default()),
            |p| p.parse_module(),
        );
        let decl = module.body[0].as_stmt().unwrap().as_decl().unwrap();
        assert!(decl.is_ts_enum());
    }

    #[test]
    fn match_expr_basic() {
        let e = parse_expr(
            "match (shape) { Circle { radius } => radius * 2, Square { side } => side ** 2 }",
        );
        let m = e.expect_match_expr();
        assert_eq!(m.arms.len(), 2);
        assert_eq!(m.arms[0].variant.sym, "Circle");
        assert!(m.arms[0].binding.is_some());
        assert_eq!(m.arms[1].variant.sym, "Square");
    }

    #[test]
    fn match_expr_trailing_comma() {
        let e = parse_expr("match (x) { A { a } => a, }");
        assert_eq!(e.expect_match_expr().arms.len(), 1);
    }

    #[test]
    fn match_expr_nested() {
        let e = parse_expr(
            "match (o) { Left { inner } => match (inner) { A { x } => x }, Right { label } => \
             label }",
        );
        let m = e.expect_match_expr();
        assert!(m.arms[0].body.is_match_expr());
    }

    #[test]
    fn str_match_stays_call() {
        let e = parse_expr("str.match(re)");
        assert!(e.is_call());
    }

    #[test]
    fn match_call_stays_call() {
        let e = parse_expr("match(1)");
        assert!(e.is_call());
    }

    #[test]
    fn match_call_with_space_stays_call() {
        let e = parse_expr("match (1) + 2");
        assert!(e.is_bin());
    }

    #[test]
    fn match_call_ternary_stays_call() {
        let e = parse_expr("match(1) ? 2 : 3");
        assert!(e.is_cond());
    }

    #[test]
    fn match_asi_call_then_block_stays_call() {
        // ASI: `match(1)` is a call statement, `{ ... }` a block statement.
        let module = test_parser("match(1)\n{\n  log(2);\n}", zts(), |p| p.parse_module());
        assert_eq!(module.body.len(), 2);
        let stmt = module.body[0].as_stmt().unwrap().as_expr().unwrap();
        assert!(stmt.expr.is_call(), "match(1) side effect must survive");
    }

    #[test]
    fn match_call_then_empty_block_stays_call() {
        let module = test_parser("match(1)\n{}", zts(), |p| p.parse_module());
        assert_eq!(module.body.len(), 2);
        let stmt = module.body[0].as_stmt().unwrap().as_expr().unwrap();
        assert!(stmt.expr.is_call());
    }

    #[test]
    fn match_empty_braces_same_line_is_not_a_match() {
        // `{ }` has no word after `{`, so this must fall back to a call…
        let e = test_parser("match (x) {}", zts(), |p| {
            let e = p.parse_expr()?;
            let _ = p.take_errors();
            Ok(e)
        });
        // …meaning the call expression survives (the dangling `{}` is the
        // caller's problem, as in vanilla TS).
        assert!(e.is_call());
    }

    #[test]
    fn pathological_nesting_is_linear() {
        // `match (match (match (…x…)))` — every speculation fails at the
        // innermost level. Without the packrat memo this is O(2^n); with
        // it, parsing must be effectively instant.
        let n = 64;
        let src = format!("const a = {}x{};", "match (".repeat(n), ")".repeat(n));
        let src: &'static str = Box::leak(src.into_boxed_str());
        let started = std::time::Instant::now();
        let module = test_parser(src, zts(), |p| {
            let m = p.parse_module()?;
            let _ = p.take_errors();
            Ok(m)
        });
        assert_eq!(module.body.len(), 1);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "nested match speculation took {:?} — memoization is broken",
            started.elapsed()
        );
    }

    #[test]
    fn success_nesting_in_discriminant_is_linear() {
        // Matches nested in DISCRIMINANT position, all succeeding. The
        // failure memo does not apply here; this guards against the commit
        // path re-parsing discriminants (which would be exponential).
        let mut src = String::from("x");
        for _ in 0..48 {
            src = format!("match ({src}) {{ A {{ a }} => a }}");
        }
        let src: &'static str = Box::leak(format!("const r = {src};").into_boxed_str());
        let started = std::time::Instant::now();
        let module = test_parser(src, zts(), |p| p.parse_module());
        assert_eq!(module.body.len(), 1);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "success-path nesting took {:?} — discriminants are being re-parsed",
            started.elapsed()
        );
    }

    #[test]
    fn deep_paren_nesting_does_not_crash() {
        // 8000 paren levels would overflow the stack in a debug build;
        // with the zts flag on, expression recursion rides on maybe_grow
        // (stacker) and must parse cleanly. The nesting LIMIT is enforced
        // by the zts semantic pass, not the parser.
        let n = 8000;
        let src: &'static str =
            Box::leak(format!("const a = {}1{};", "(".repeat(n), ")".repeat(n)).into_boxed_str());
        let module = test_parser(src, zts(), |p| p.parse_module());
        assert_eq!(module.body.len(), 1);
    }

    #[test]
    fn match_multiline_body_still_parses() {
        // Only the `{` must share a line with `)`; arms can wrap freely.
        let e = parse_expr("match (x) { A { a } =>\n  a,\n  B { b } => b\n}");
        assert_eq!(e.expect_match_expr().arms.len(), 2);
    }

    #[test]
    fn match_disabled_without_flag() {
        let e = test_parser(
            "match (x) { A { a } => a }",
            Syntax::Typescript(Default::default()),
            |p| {
                let e = p.parse_expr()?;
                // Without the zts flag the parser must see a call; swallow the
                // errors from the trailing `{ ... }` garbage.
                let _ = p.take_errors();
                Ok(e)
            },
        );
        assert!(e.is_call());
    }
}
