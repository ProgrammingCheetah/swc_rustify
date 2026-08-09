# FORK_NOTES — the `zts` branch

This is ProgrammingCheetah/swc_rustify, the ZesTTY fork of swc. `main`
tracks upstream unchanged; every zts extension lives on the `zts` branch.
This file records the things a future rebase, crate migration, or upstream
merge would otherwise silently break. It is not a changelog — it is the
list of assumptions that are load-bearing and invisible.

## Where the zts grammar lives

- `crates/swc_ecma_ast` — the zts AST nodes (`expr.rs`, `decl.rs`,
  `typescript.rs`). New variants are always **appended**; variant ordinals
  are the encoding.
- `crates/swc_ecma_parser` — everything else:
  - `src/parser/zts.rs` — ALL zts parsing, plus its test module.
  - `src/lexer/token.rs`, `src/lexer/mod.rs` — the one zts token (`..=`,
    `Token::DotDotEq`) and its two lexing sites.
  - `src/error.rs` — the `Zts*` `SyntaxError` variants and messages.
  - `src/syntax.rs` — the `zts` flag (`SyntaxFlags::ZTS`).
- `crates/swc_ecma_visit`, `crates/swc_ecma_hooks` — `generated.rs`,
  regenerated with `cargo test -p generate-code test_ecmascript`. NEVER
  hand-edited.

## `swc_ecma_lexer` HAS NO ZTS SUPPORT

`crates/swc_ecma_lexer` is a SECOND, independent lexer + token type that
upstream maintains alongside `swc_ecma_parser`'s own. **Nothing in ZesTTY
uses it**, and it has deliberately not been extended:

- no `Token::DotDotEq` (the `..=` range-pattern token),
- no zts syntax flag, and therefore
- none of the zts contextual-keyword or lexing behaviour.

Two consequences, both important:

1. **It must still be built after any token work.** It shares enough
   surface with `swc_ecma_parser` to break without a compile error in the
   crate you edited; it has been broken twice this way (19d601db47,
   dbc73325ea). `cargo check -p swc_ecma_lexer` is part of the gate.
2. **A migration to it would silently drop the entire zts grammar.** If
   upstream ever consolidates on `swc_ecma_lexer`, or a downstream crate
   is repointed at it, zts source will lex as vanilla TypeScript: `..=`
   becomes a syntax error, and every contextual keyword reverts. That is a
   port, not a swap — budget for re-doing `zts.rs`'s lexer-facing half.

## Breaking changes beyond the appended-variant discipline

The "append, never insert" rule keeps the ENCODING stable. It does not
keep the generated TRAIT SURFACE stable, and one 0.5.0 change moved it:

- **`ZtsUnionDecl.members` changed from `Vec<Str>` to
  `Vec<ZtsUnionMember>`** (numeric and mixed `union` members, issue #73).
  Because the field's element type changed, the code generator stopped
  emitting the `visit_strs` / `visit_mut_strs` / `fold_strs` family — that
  method group existed ONLY because this was the sole `Vec<Str>` field in
  the AST. Any out-of-tree visitor that overrode one of those methods now
  fails to compile (or, worse, silently stops being called if it was
  written as a free function). Nothing in ZesTTY did; recorded because
  nothing in the AST diff says so.

Rule of thumb: changing the element type of a `Vec<T>` field, or the type
of any field, can add or remove whole generated method families. Diff
`crates/swc_ecma_visit/src/generated.rs` for removed `fn` names after
regenerating, not just added ones.

## Token work is the one place "compile errors are the checklist" fails

`Token` is `#[repr(u8)]` and several hot classifiers are ORDINAL RANGE
checks (`is_keyword`, `is_known_ident`, `is_bin_op`,
`needs_unary_expr_prefix_parse`, `is_assign_op`), while `before_expr` and
`starts_expr` are `match`es with `_` fallthroughs. A new variant inserted
in the wrong place is reclassified silently, and a missing `before_expr`
registration compiles fine. See the comment on `Token::DotDotEq` in
`src/lexer/token.rs` for the audit that has to be redone by hand every
time a token is added, and record the audit in the commit message.
