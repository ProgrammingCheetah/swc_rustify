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

    /// Speculatively parses the header `match ( expr ) {`.
    ///
    /// Returns `None` (with the parser fully backtracked) when the shape is
    /// not a match expression, so `match` falls through to the plain
    /// identifier path. Once the header matches, parsing is committed.
    pub(super) fn try_parse_zts_match_expr(
        &mut self,
        start: BytePos,
    ) -> Option<PResult<Box<Expr>>> {
        let prev_ignore_error = self.input().get_ctx().contains(Context::IgnoreError);
        let checkpoint = self.checkpoint_save();
        self.set_ctx(self.ctx() | Context::IgnoreError);

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
            if !self.input().is(Token::LBrace) {
                return Ok(None);
            }
            Ok(Some(discriminant))
        })();

        // Restore error reporting before either committing or backtracking.
        let mut ctx = self.ctx();
        ctx.set(Context::IgnoreError, prev_ignore_error);
        self.input_mut().set_ctx(ctx);

        match header {
            Ok(Some(discriminant)) => Some(self.parse_zts_match_body(start, discriminant)),
            Ok(None) | Err(..) => {
                self.checkpoint_load(checkpoint);
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
