use is_macro::Is;
use string_enum::StringEnum;
use swc_common::{ast_node, util::take::Take, EqIgnoreSpan, Span, SyntaxContext, DUMMY_SP};

use crate::{
    class::Class,
    expr::Expr,
    function::Function,
    ident::{Ident, IdentName},
    lit::Str,
    pat::Pat,
    typescript::{TsEnumDecl, TsInterfaceDecl, TsModuleDecl, TsType, TsTypeAliasDecl},
};

#[ast_node]
#[derive(Eq, Hash, Is, EqIgnoreSpan)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
pub enum Decl {
    #[tag("ClassDeclaration")]
    Class(ClassDecl),
    #[tag("FunctionDeclaration")]
    #[is(name = "fn_decl")]
    Fn(FnDecl),
    #[tag("VariableDeclaration")]
    Var(Box<VarDecl>),
    #[tag("UsingDeclaration")]
    Using(Box<UsingDecl>),

    #[tag("TsInterfaceDeclaration")]
    TsInterface(Box<TsInterfaceDecl>),
    #[tag("TsTypeAliasDeclaration")]
    TsTypeAlias(Box<TsTypeAliasDecl>),
    #[tag("TsEnumDeclaration")]
    TsEnum(Box<TsEnumDecl>),
    #[tag("TsModuleDeclaration")]
    TsModule(Box<TsModuleDecl>),

    /// zts extension: Rust-style enum-with-data. Must be lowered to a
    /// tagged-union type alias + factory object before codegen.
    #[tag("ZtsEnumDeclaration")]
    ZtsEnum(Box<ZtsEnumDecl>),

    /// zts extension: `newtype AccountId = string;`. Must be lowered to a
    /// branded type alias + factory function before codegen.
    #[tag("ZtsNewtypeDeclaration")]
    ZtsNewtype(Box<ZtsNewtypeDecl>),

    /// zts extension: `union Level = 'a' | 'b';` — a closed string-literal
    /// vocabulary. Must be lowered to a type alias + values/has object
    /// before codegen.
    #[tag("ZtsUnionDeclaration")]
    ZtsUnion(Box<ZtsUnionDecl>),

    /// zts extension: `impl Display for Shape { fn fmt(self) -> string {
    /// ... } }`. Must be lowered (methods merged into the type's factory
    /// const, `satisfies` conformance appended) before codegen.
    #[tag("ZtsImplDeclaration")]
    ZtsImpl(Box<ZtsImplDecl>),
}

boxed!(
    Decl,
    [
        VarDecl,
        UsingDecl,
        TsInterfaceDecl,
        TsTypeAliasDecl,
        TsEnumDecl,
        TsModuleDecl
    ]
);

macro_rules! decl_from {
    ($($variant_ty:ty),*) => {
        $(
            bridge_from!(crate::Stmt, Decl, $variant_ty);
            bridge_from!(crate::ModuleItem, crate::Stmt, $variant_ty);
        )*
    };
}

decl_from!(
    ClassDecl,
    FnDecl,
    VarDecl,
    UsingDecl,
    TsInterfaceDecl,
    TsTypeAliasDecl,
    TsEnumDecl,
    TsModuleDecl
);

macro_rules! decl_from_boxed {
    ($($variant_ty:ty),*) => {
        $(
            bridge_from!(Box<crate::Stmt>, Decl, $variant_ty);
            bridge_from!(Box<crate::Stmt>, Decl, Box<$variant_ty>);
            bridge_from!(crate::Stmt, Decl, Box<$variant_ty>);
            bridge_from!(crate::ModuleItem, crate::Stmt, Box<$variant_ty>);
        )*
    };
}

decl_from_boxed!(
    VarDecl,
    UsingDecl,
    TsInterfaceDecl,
    TsTypeAliasDecl,
    TsEnumDecl,
    TsModuleDecl
);

impl Take for Decl {
    fn dummy() -> Self {
        Decl::Var(Default::default())
    }
}

#[ast_node("FunctionDeclaration")]
#[derive(Eq, Hash, EqIgnoreSpan)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
pub struct FnDecl {
    #[cfg_attr(feature = "serde-impl", serde(rename = "identifier"))]
    pub ident: Ident,

    #[cfg_attr(feature = "serde-impl", serde(default))]
    pub declare: bool,

    #[cfg_attr(feature = "serde-impl", serde(flatten))]
    #[span]
    pub function: Box<Function>,
}

impl Take for FnDecl {
    fn dummy() -> Self {
        FnDecl {
            ident: Take::dummy(),
            declare: Default::default(),
            function: Take::dummy(),
        }
    }
}

#[ast_node("ClassDeclaration")]
#[derive(Eq, Hash, EqIgnoreSpan)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
pub struct ClassDecl {
    #[cfg_attr(feature = "serde-impl", serde(rename = "identifier"))]
    pub ident: Ident,

    #[cfg_attr(feature = "serde-impl", serde(default))]
    pub declare: bool,

    #[cfg_attr(feature = "serde-impl", serde(flatten))]
    #[span]
    pub class: Box<Class>,
}

impl Take for ClassDecl {
    fn dummy() -> Self {
        ClassDecl {
            ident: Take::dummy(),
            declare: Default::default(),
            class: Take::dummy(),
        }
    }
}

#[ast_node("VariableDeclaration")]
#[derive(Eq, Hash, EqIgnoreSpan, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
pub struct VarDecl {
    pub span: Span,

    pub ctxt: SyntaxContext,

    pub kind: VarDeclKind,

    #[cfg_attr(feature = "serde-impl", serde(default))]
    pub declare: bool,

    #[cfg_attr(feature = "serde-impl", serde(rename = "declarations"))]
    pub decls: Vec<VarDeclarator>,
}

impl Take for VarDecl {
    fn dummy() -> Self {
        Default::default()
    }
}

#[derive(StringEnum, Clone, Copy, Eq, PartialEq, PartialOrd, Ord, Hash, EqIgnoreSpan, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
#[cfg_attr(
    feature = "encoding-impl",
    derive(::swc_common::Encode, ::swc_common::Decode)
)]
#[cfg_attr(swc_ast_unknown, non_exhaustive)]
pub enum VarDeclKind {
    /// `var`
    #[default]
    Var,
    /// `let`
    Let,
    /// `const`
    Const,
}

#[ast_node("VariableDeclarator")]
#[derive(Eq, Hash, EqIgnoreSpan)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
pub struct VarDeclarator {
    pub span: Span,
    #[cfg_attr(feature = "serde-impl", serde(rename = "id"))]
    pub name: Pat,

    /// Initialization expression.
    #[cfg_attr(feature = "serde-impl", serde(default))]
    #[cfg_attr(
        feature = "encoding-impl",
        encoding(with = "cbor4ii::core::types::Maybe")
    )]
    pub init: Option<Box<Expr>>,

    /// Typescript only
    #[cfg_attr(feature = "serde-impl", serde(default))]
    pub definite: bool,
}

impl Take for VarDeclarator {
    fn dummy() -> Self {
        VarDeclarator {
            span: DUMMY_SP,
            name: Take::dummy(),
            init: Take::dummy(),
            definite: Default::default(),
        }
    }
}

#[ast_node("UsingDeclaration")]
#[derive(Eq, Hash, EqIgnoreSpan)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
pub struct UsingDecl {
    #[cfg_attr(feature = "serde-impl", serde(default))]
    pub span: Span,

    #[cfg_attr(feature = "serde-impl", serde(default))]
    pub is_await: bool,

    #[cfg_attr(feature = "serde-impl", serde(default))]
    pub decls: Vec<VarDeclarator>,
}

impl Take for UsingDecl {
    fn dummy() -> Self {
        Self {
            span: DUMMY_SP,
            is_await: Default::default(),
            decls: Take::dummy(),
        }
    }
}

/// zts extension: `enum Shape { Circle { radius: number }, Square { side:
/// number } }`
///
/// Deliberately replaces TypeScript's `enum` when the zts syntax flag is
/// on. Never reaches codegen — the zts compiler lowers it to a tagged
/// union type alias plus a factory-function object.
#[ast_node("ZtsEnumDeclaration")]
#[derive(Eq, Hash, EqIgnoreSpan, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
pub struct ZtsEnumDecl {
    pub span: Span,

    #[cfg_attr(feature = "serde-impl", serde(rename = "identifier"))]
    pub ident: Ident,

    #[cfg_attr(feature = "serde-impl", serde(default))]
    pub variants: Vec<ZtsEnumVariant>,
}

impl Take for ZtsEnumDecl {
    fn dummy() -> Self {
        Default::default()
    }
}

/// One variant of a [ZtsEnumDecl]: `Circle { radius: number }`.
#[ast_node("ZtsEnumVariant")]
#[derive(Eq, Hash, EqIgnoreSpan, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
pub struct ZtsEnumVariant {
    pub span: Span,

    pub name: Ident,

    #[cfg_attr(feature = "serde-impl", serde(default))]
    pub fields: Vec<ZtsEnumField>,
}

impl Take for ZtsEnumVariant {
    fn dummy() -> Self {
        Default::default()
    }
}

/// One payload field of a [ZtsEnumVariant]: `radius: number`.
#[ast_node("ZtsEnumField")]
#[derive(Eq, Hash, EqIgnoreSpan)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
pub struct ZtsEnumField {
    pub span: Span,

    pub name: IdentName,

    #[cfg_attr(feature = "serde-impl", serde(rename = "typeAnnotation"))]
    pub type_ann: Box<TsType>,
}

/// zts extension: `newtype AccountId = string;`
///
/// Never reaches codegen — the zts compiler lowers it to a branded type
/// alias (`string & { readonly __ztsNewtype: "AccountId" }`) plus a
/// factory function const of the same name.
#[ast_node("ZtsNewtypeDeclaration")]
#[derive(Eq, Hash, EqIgnoreSpan)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
pub struct ZtsNewtypeDecl {
    pub span: Span,

    #[cfg_attr(feature = "serde-impl", serde(rename = "identifier"))]
    pub ident: Ident,

    #[cfg_attr(feature = "serde-impl", serde(rename = "typeAnnotation"))]
    pub type_ann: Box<TsType>,
}

impl Take for ZtsNewtypeDecl {
    fn dummy() -> Self {
        Self {
            span: DUMMY_SP,
            ident: Take::dummy(),
            type_ann: Box::new(TsType::TsKeywordType(crate::TsKeywordType {
                span: DUMMY_SP,
                kind: crate::TsKeywordTypeKind::TsAnyKeyword,
            })),
        }
    }
}

/// zts extension: `impl Display for Shape { fn fmt(self) -> string { ... } }`
///
/// Never reaches codegen — the zts compiler merges the methods into the
/// factory const of the type named after `for` (which must be declared in
/// the same module) and appends a `satisfies Trait<Type>` conformance
/// clause. The trait itself is a plain TS interface, not a zts node.
#[ast_node("ZtsImplDeclaration")]
#[derive(Eq, Hash, EqIgnoreSpan, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
pub struct ZtsImplDecl {
    pub span: Span,

    /// The trait being implemented (`Display`).
    #[cfg_attr(feature = "serde-impl", serde(rename = "traitIdentifier"))]
    pub trait_ident: Ident,

    /// The type receiving the impl (`Shape`).
    #[cfg_attr(feature = "serde-impl", serde(rename = "forIdentifier"))]
    pub for_ident: Ident,

    #[cfg_attr(feature = "serde-impl", serde(default))]
    pub methods: Vec<ZtsImplMethod>,
}

impl Take for ZtsImplDecl {
    fn dummy() -> Self {
        Default::default()
    }
}

/// One method of a [ZtsImplDecl]: `fn fmt(self) -> string { ... }`.
///
/// The `self` receiver is `function.params[0]`, an untyped binding ident —
/// the zts lowering annotates it with the `for` type. The `->` return type
/// is stored as the function's ordinary return type annotation.
#[ast_node("ZtsImplMethod")]
#[derive(Eq, Hash, EqIgnoreSpan, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
pub struct ZtsImplMethod {
    pub span: Span,

    pub name: Ident,

    pub function: Box<Function>,
}

impl Take for ZtsImplMethod {
    fn dummy() -> Self {
        Default::default()
    }
}

/// zts extension: `union Level = 'a' | 'b';`
///
/// Never reaches codegen — the zts compiler lowers it to a string-literal
/// union type alias plus a `{ values, has }` const of the same name.
#[ast_node("ZtsUnionDeclaration")]
#[derive(Eq, Hash, EqIgnoreSpan, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "shrink-to-fit", derive(shrink_to_fit::ShrinkToFit))]
pub struct ZtsUnionDecl {
    pub span: Span,

    #[cfg_attr(feature = "serde-impl", serde(rename = "identifier"))]
    pub ident: Ident,

    #[cfg_attr(feature = "serde-impl", serde(default))]
    pub members: Vec<Str>,
}

impl Take for ZtsUnionDecl {
    fn dummy() -> Self {
        Default::default()
    }
}
