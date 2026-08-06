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

/// Is this a bare identifier, or a comma sequence of bare identifiers?
/// (`{ a }` / `{ a, b }` — the shapes that used to parse as shorthand
/// object literals in match arm bodies.)
fn expr_is_bare_idents(e: &Expr) -> bool {
    match e {
        Expr::Ident(..) => true,
        Expr::Seq(seq) => seq.exprs.iter().all(|e| e.is_ident()),
        _ => false,
    }
}

impl<I: Tokens> Parser<I> {
    /// `not <operand>` — negation only when unambiguous with vanilla TS:
    /// the operand token must be one that can NEVER legally follow an
    /// identifier (a word or literal), on the same line (ASI: `not\nx` is
    /// two statements). Everything else — `not(x)` calls, `not.foo`,
    /// `not => x` arrows, `not instanceof F`, `not!` assertions, tagged
    /// templates — keeps its vanilla meaning. Binary-operator words
    /// (`in`, `instanceof`, `as`, `satisfies`, `of`) are excluded from
    /// the operand set for the same reason.
    pub(super) fn is_zts_not_operator(&mut self) -> bool {
        if !self.input().syntax().zts() {
            return false;
        }
        if !(self.input().cur().is_word()
            && self.input().cur().take_word(&self.input) == atom!("not"))
        {
            return false;
        }
        if self.input_mut().has_linebreak_between_cur_and_peeked() {
            return false;
        }
        match self.input_mut().peek() {
            Some(Token::In | Token::InstanceOf | Token::As | Token::Satisfies | Token::Of) => false,
            Some(
                Token::Num | Token::Str | Token::BigInt | Token::True | Token::False | Token::Null,
            ) => true,
            Some(t) => t.is_word(),
            None => false,
        }
    }

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
            let arm_starter = self.input_mut().peek().is_some_and(|t| {
                t.is_word()
                    || matches!(
                        t,
                        Token::Str | Token::Num | Token::Minus | Token::True | Token::False
                    )
            });
            if !arm_starter {
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

    /// `newtype AccountId = string;`
    ///
    /// The caller (parse_ts_decl) has already committed: the `newtype` word
    /// is consumed and the current token is the name identifier on the same
    /// line — exactly the `type`-alias commit rule, so `newtype` stays a
    /// valid identifier everywhere else. No type params in v1 (the `=` is
    /// expected right after the name).
    pub(super) fn parse_zts_newtype_decl(&mut self, start: BytePos) -> PResult<Decl> {
        if self.ctx().contains(Context::InDeclare) {
            return Err(Error::new(self.span(start), SyntaxError::ZtsDeclareNewtype));
        }

        let ident = self.parse_ident_name()?;
        let ident = Ident::new_no_ctxt(ident.sym, ident.span);
        let type_ann = self.expect_then_parse_ts_type(Token::Eq, "=")?;
        self.expect_general_semi()?;

        Ok(Decl::ZtsNewtype(Box::new(ZtsNewtypeDecl {
            span: self.span(start),
            ident,
            type_ann,
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

    /// `<pattern> => body`
    ///
    /// The body is an assignment expression, or — mirroring arrow-function
    /// bodies — a block expression when it opens with `{` (a block's value
    /// is its tail expression; object literals need parens, `=> ({ ... })`).
    fn parse_zts_match_arm(&mut self) -> PResult<MatchArm> {
        let arm_start = self.input().cur_pos();

        let pattern = self.parse_zts_match_pattern()?;

        expect!(self, Token::Arrow);
        let body = if self.input().is(Token::LBrace) {
            let block = self.parse_zts_expr_block()?;
            // `=> { a }` / `=> { a, b }` parsed as an object literal before
            // block bodies existed; silently changing its value is the one
            // thing we never do. Reject the ambiguous shape outright.
            if block.stmts.is_empty() && expr_is_bare_idents(&block.tail) {
                return Err(Error::new(block.span, SyntaxError::ZtsAmbiguousArmBody));
            }
            Box::new(Expr::ZtsExprBlock(block))
        } else {
            self.allow_in_expr(Self::parse_assignment_expr)?
        };

        Ok(MatchArm {
            span: self.span(arm_start),
            pattern,
            body,
        })
    }

    /// Wildcard `_`, a literal (`"active"`, `404`, `-1`, `true`, `null`),
    /// or `Variant { bindings }`. Mode consistency (no mixing) is the
    /// semantic pass's job.
    fn parse_zts_match_pattern(&mut self) -> PResult<MatchPat> {
        let start = self.input().cur_pos();
        let cur = self.input().cur();

        // `_ =>` — wildcard. (`_ { ... }` falls through to the variant
        // path and is rejected there by the semantic pass.)
        if cur.is_word()
            && self.input().cur().take_word(&self.input) == atom!("_")
            && self.input_mut().peek() == Some(Token::Arrow)
        {
            self.bump();
            return Ok(MatchPat::Wildcard(MatchWildcardPat {
                span: self.span(start),
            }));
        }

        // Literal patterns.
        if matches!(
            cur,
            Token::Str | Token::Num | Token::True | Token::False | Token::Null
        ) {
            let lit = self.parse_lit()?;
            return Ok(MatchPat::Lit(MatchLitPat {
                span: self.span(start),
                lit,
                neg: false,
            }));
        }
        if cur == Token::Minus {
            self.bump();
            if !self.input().is(Token::Num) {
                syntax_error!(self, self.span(start), SyntaxError::TS1109);
            }
            let lit = self.parse_lit()?;
            return Ok(MatchPat::Lit(MatchLitPat {
                span: self.span(start),
                lit,
                neg: true,
            }));
        }

        // Variant pattern.
        let name = self.parse_ident_name()?;
        let name = Ident::new_no_ctxt(name.sym, name.span);
        self.expect_without_advance(Token::LBrace)?;
        let binding = match self.parse_object_pat()? {
            Pat::Object(o) => Some(o),
            _ => unreachable!("parse_object_pat always returns Pat::Object"),
        };
        Ok(MatchPat::Variant(MatchVariantPat {
            span: self.span(start),
            name,
            binding,
        }))
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
            // else-if links recurse outside parse_assignment_expr's
            // maybe_grow; grow here too or long chains SIGABRT the parser
            // before the zts semantic depth limit can reject them.
            let expr = crate::maybe_grow(256 * 1024, 1024 * 1024, || {
                self.parse_zts_if_expr(alt_start)
            })?;
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
    fn arm_bare_ident_block_is_ambiguous_error() {
        // `=> { a }` used to be a shorthand object literal; silently
        // returning `a` instead would change program values.
        let (is_err, had) = parse_module_errs("const r = match (s) { A { a } => { a } };");
        assert!(is_err || had);
        let (is_err2, had2) = parse_module_errs("const r = match (s) { A { a, b } => { a, b } };");
        assert!(is_err2 || had2);
    }

    #[test]
    fn arm_non_ident_tail_block_is_fine() {
        let e = parse_expr("match (s) { A { a } => { f(a) } }");
        let m = e.expect_match_expr();
        assert!(m.arms[0].body.is_zts_expr_block());
    }

    #[test]
    fn enum_in_single_statement_position_is_an_error() {
        for src in [
            "if (c) enum E { A { v: number } }",
            "while (c) enum E { A { v: number } }",
            "for (;;) enum E { A { v: number } }",
            "lbl: enum E { A { v: number } }",
        ] {
            let src: &'static str = Box::leak(src.to_string().into_boxed_str());
            let (is_err, had) = parse_module_errs(src);
            assert!(is_err || had, "expected error for: {src}");
        }
    }

    #[test]
    fn export_default_enum_is_an_error() {
        let (is_err, had) =
            parse_module_errs("export default enum Shape { Circle { radius: number } }");
        assert!(is_err || had);
    }

    #[test]
    fn not_operator_negates() {
        for (src, desc) in [
            ("not ready", "ident operand"),
            ("not true", "bool literal"),
            ("not 0", "num literal"),
            ("not not x", "double negation"),
            ("not typeof x", "typeof operand"),
        ] {
            let e = parse_expr(Box::leak(src.to_string().into_boxed_str()));
            assert!(e.is_unary(), "{desc}: expected unary, got {e:?}");
        }
        // Binds like `!`: `not a === b` is `(!a) === b`.
        let e = parse_expr("not a === b");
        let bin = e.as_bin().unwrap();
        assert!(bin.left.is_unary());
    }

    #[test]
    fn not_stays_an_identifier_in_vanilla_positions() {
        assert!(parse_expr("not(1)").is_call(), "call");
        assert!(parse_expr("not.foo").is_member(), "member");
        assert!(parse_expr("not => not").is_arrow(), "arrow param");
        assert!(parse_expr("not instanceof Foo").is_bin(), "instanceof");
        assert!(parse_expr("not as string").is_ts_as(), "as-cast");
        assert!(parse_expr("not + 1").is_bin(), "binop");
        let m = test_parser("const not = 1;\nnot;\n", zts(), |p| p.parse_module());
        assert_eq!(m.body.len(), 2, "binding named not");
    }

    #[test]
    fn not_respects_asi() {
        // `not\nx` must remain TWO expression statements.
        let m = test_parser("not\nx", zts(), |p| p.parse_module());
        assert_eq!(m.body.len(), 2);
        assert!(m.body[0]
            .as_stmt()
            .unwrap()
            .as_expr()
            .unwrap()
            .expr
            .is_ident());
    }

    #[test]
    fn wildcard_arm_parses() {
        let e = parse_expr("match (t) { K { v } => v, _ => 0 }");
        let m = e.expect_match_expr();
        assert!(m.arms[1].pattern.is_wildcard());
    }

    #[test]
    fn literal_arms_parse() {
        let e = parse_expr(
            "match (s) { \"active\" => 1, 404 => 2, -1 => 3, true => 4, null => 5, _ => 0 }",
        );
        let m = e.expect_match_expr();
        assert_eq!(m.arms.len(), 6);
        assert!(m.arms[0].pattern.is_lit());
        let neg = m.arms[2].pattern.as_lit().unwrap();
        assert!(neg.neg);
        assert!(m.arms[5].pattern.is_wildcard());
    }

    #[test]
    fn underscore_stays_an_identifier_outside_arm_patterns() {
        let m = test_parser("const _ = 1;\nconst x = _ + 1;", zts(), |p| {
            p.parse_module()
        });
        assert_eq!(m.body.len(), 2);
        // And `_` as a match DISCRIMINANT is just an identifier.
        let e = parse_expr("match (_) { K { v } => v }");
        assert!(e.expect_match_expr().discriminant.is_ident());
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
        let v0 = m.arms[0].pattern.as_variant().unwrap();
        assert_eq!(v0.name.sym, "Circle");
        assert!(v0.binding.is_some());
        assert_eq!(m.arms[1].pattern.as_variant().unwrap().name.sym, "Square");
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
    fn newtype_decl_parses() {
        let module = test_parser("newtype AccountId = string;", zts(), |p| p.parse_module());
        let decl = module.body[0].as_stmt().unwrap().as_decl().unwrap();
        let n = decl.as_zts_newtype().unwrap();
        assert_eq!(n.ident.sym, "AccountId");
        assert!(n.type_ann.is_ts_keyword_type());
    }

    #[test]
    fn export_newtype_decl_parses() {
        let module = test_parser("export newtype UserId = string;", zts(), |p| p.parse_module());
        let item = &module.body[0];
        let export = item.as_module_decl().unwrap().as_export_decl().unwrap();
        assert!(export.decl.is_zts_newtype());
    }

    #[test]
    fn newtype_stays_an_identifier_elsewhere() {
        // Assignment, call, member access, and ASI-split lines must all
        // keep `newtype` as a plain identifier.
        for src in [
            "const newtype = 1;",
            "newtype = 5;",
            "newtype(x);",
            "newtype.foo;",
            "newtype\nAccountId;",
        ] {
            let src: &'static str = Box::leak(src.to_string().into_boxed_str());
            let module = test_parser(src, zts(), |p| p.parse_module());
            assert!(
                !matches!(
                    module.body[0].as_stmt(),
                    Some(Stmt::Decl(Decl::ZtsNewtype(..)))
                ),
                "{src:?} must not parse as a newtype decl"
            );
        }
    }

    #[test]
    fn declare_newtype_is_an_error() {
        let (is_err, had) = parse_module_errs("declare newtype AccountId = string;");
        assert!(is_err || had, "`declare newtype` must error");
    }

    #[test]
    fn newtype_disabled_without_flag() {
        // Without the zts flag, `newtype X = string;` stays what it is in
        // vanilla TS: a syntax error (two identifiers on one line), NOT a
        // newtype decl.
        let (is_err, had) = test_parser(
            "newtype X = string;",
            Syntax::Typescript(Default::default()),
            |p| {
                let res = p.parse_module();
                let errs = p.take_errors();
                Ok((res.is_err(), !errs.is_empty()))
            },
        );
        assert!(is_err || had, "vanilla TS must reject `newtype X = ...`");
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
