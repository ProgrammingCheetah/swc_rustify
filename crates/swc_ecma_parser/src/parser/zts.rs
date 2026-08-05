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
use crate::{context::Context, lexer::Token, parser::input::Tokens};

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

    /// Speculatively checks for the header `match ( expr ) { Word`.
    ///
    /// Returns `None` (with the parser fully backtracked) when the shape is
    /// not a match expression, so `match` falls through to the plain
    /// identifier path. When the header matches, the parser rewinds and
    /// re-parses it for real — with diagnostics live — and commits: arm
    /// syntax errors are then reported as real errors instead of falling
    /// back to a bogus call expression.
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
        // Packrat memo: without it, nested `match (match (...` re-runs the
        // speculation once per enclosing attempt — exponential time.
        if self.zts_match_speculation_failures.contains(&start) {
            return None;
        }

        let prev_ignore_error = self.input().get_ctx().contains(Context::IgnoreError);
        let checkpoint = self.checkpoint_save();
        self.set_ctx(self.ctx() | Context::IgnoreError);

        let header_matches = (|| -> PResult<bool> {
            // `match`
            self.bump();
            if !self.input_mut().eat(Token::LParen) {
                return Ok(false);
            }
            let _discriminant = self.allow_in_expr(|p| p.parse_expr())?;
            if !self.input_mut().eat(Token::RParen) {
                return Ok(false);
            }
            if !self.input().is(Token::LBrace) || self.input_mut().had_line_break_before_cur() {
                return Ok(false);
            }
            Ok(self.input_mut().peek().is_some_and(|t| t.is_word()))
        })();

        // Restore error reporting and rewind unconditionally: even on a
        // header match we re-parse from `match` so that recoverable
        // diagnostics inside the discriminant (suppressed during
        // speculation) are emitted for real.
        let mut ctx = self.ctx();
        ctx.set(Context::IgnoreError, prev_ignore_error);
        self.input_mut().set_ctx(ctx);
        self.checkpoint_load(checkpoint);

        match header_matches {
            Ok(true) => Some(self.parse_zts_match_committed(start)),
            Ok(false) | Err(..) => {
                self.zts_match_speculation_failures.insert(start);
                None
            }
        }
    }

    /// Re-parses `match ( expr )` with diagnostics enabled, then the body.
    /// Only called after the speculative header check succeeded.
    fn parse_zts_match_committed(&mut self, start: BytePos) -> PResult<Box<Expr>> {
        // `match`
        self.bump();
        self.expect(Token::LParen)?;
        let discriminant = self.allow_in_expr(|p| p.parse_expr())?;
        self.expect(Token::RParen)?;
        self.parse_zts_match_body(start, discriminant)
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

    /// `Variant { bindings } => body`
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
        let body = self.allow_in_expr(Self::parse_assignment_expr)?;

        Ok(MatchArm {
            span: self.span(arm_start),
            variant,
            binding,
            body,
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
