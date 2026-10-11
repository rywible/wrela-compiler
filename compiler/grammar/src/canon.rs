//! What it means for the oracle and the hand-written parser to agree on structure.
//!
//! Both parses are reduced to a **shape**: the set of `(start, end, category)` triples, one for
//! every syntactic unit, where `start..end` are the bytes from the unit's first token to its
//! last (NEWLINE and EOF cover no bytes; a GT_CLOSE split off `>>` covers its one `>`). Two
//! parses agree when their shapes are equal.
//!
//! The categories are items, attributes, trait and impl members, parameters, struct fields,
//! enum variants, use trees, blocks, statements, match arms, expressions, patterns and types. A
//! set of spans determines a bracketing, so equal shapes mean the same boundaries for every unit
//! and, for expressions, the same precedence and associativity: `a - b - c` contributes
//! `a - b` while `a - (b - c)` would contribute `b - c`, and `-x ** 2` contributes `x ** 2`
//! while `(-x) ** 2` would contribute `-x`. Units that coincide (an expression statement and its
//! expression, `primary_expr` and the `if_expr` it is) have different categories or collapse in
//! the set, so they don't need to be told apart.
//!
//! How each side produces its shape:
//!
//! - **Grammar.** A node of the categories above contributes its span (`expr`, `closure`,
//!   `jump`, `cmp_expr`, `unary_expr`, `power_expr`, `primary_expr`, `if_expr` and their `_ns`
//!   twins are expressions; `type` and `path_type` are types). The repetition rules
//!   `x ::= y (op y)*` (`or_expr` … `mul_expr`) associate to the left, as the grammar's header
//!   says, so each contributes the span of every prefix `y op y … op y`; and `postfix_expr`
//!   contributes the span from its start to the end of each postfix in turn (a call, index,
//!   method call or field access wraps everything before it).
//! - **AST.** Each node contributes its span. Three AST nodes have no grammar counterpart and
//!   contribute nothing: the block expression wrapping an `else { }` block or a closure's
//!   `-> T { }` body (the grammar has just the block there), and the pattern the parser makes
//!   for the name in `var x = …` / `mut x = …` (the grammar has just an IDENT).
//! - **Parenthesized patterns.** The AST folds `(p)` into `p`, giving it the outer span, so on
//!   the grammar side a pattern that is the sole content of `( )` contributes nothing.
//!
//! What a shape leaves out: which kind of unit within a category a span is (a call or an index,
//! `-` or `!`, a tuple or a parenthesized type). Those follow from the tokens, which both
//! parsers share, once the boundaries agree.
//!
//! Every rule name used here must exist in the grammar ([`Roles::new`] panics otherwise), so
//! renaming a rule can't silently switch a check off.

use crate::earley::{Child, Node};
use crate::ebnf::{Grammar, Terminal};
use std::collections::BTreeSet;
use std::fmt;
use wrela_syntax::TokenKind;
use wrela_syntax::ast::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cat {
    Item,
    Attr,
    Member,
    Param,
    Field,
    Variant,
    UseTree,
    Block,
    Stmt,
    Arm,
    Expr,
    Pat,
    Type,
}

impl fmt::Display for Cat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// A parse reduced to its units: `(start byte, end byte, category)`.
pub type Shape = BTreeSet<(u32, u32, Cat)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    None,
    Unit(Cat),
    /// `x ::= y (op y)*`, associating to the left.
    LeftChain,
    /// `primary postfixes`.
    Postfix,
    Pattern,
}

/// How each grammar rule contributes to a shape.
pub struct Roles {
    roles: Vec<Role>,
}

const UNITS: &[(&str, Cat)] = &[
    ("item", Cat::Item),
    ("attribute", Cat::Attr),
    ("trait_member", Cat::Member),
    ("impl_member", Cat::Member),
    ("param", Cat::Param),
    ("field_decl", Cat::Field),
    ("borrow_field", Cat::Field),
    ("variant", Cat::Variant),
    ("use_tree", Cat::UseTree),
    ("block", Cat::Block),
    ("stmt", Cat::Stmt),
    ("arm", Cat::Arm),
    ("type", Cat::Type),
    ("trait_type", Cat::Type),
    ("path_type", Cat::Type),
    ("bound", Cat::Type),
    ("closure", Cat::Expr),
    ("jump", Cat::Expr),
    ("if_expr", Cat::Expr),
    ("let_else_init", Cat::Expr),
    ("let_else_primary", Cat::Expr),
];

/// Expression rules with an `_ns` twin (the rule again, for the head of an `if`, `while`,
/// `for` or `match`), which has the same role.
const TWINNED_UNITS: &[&str] = &["expr", "cmp_expr", "unary_expr", "power_expr", "primary_expr"];

/// The `x ::= y (op y)*` rules, each with an `_ns` twin.
const TWINNED_LEFT_CHAINS: &[&str] = &[
    "or_expr",
    "and_expr",
    "bitor_expr",
    "bitxor_expr",
    "bitand_expr",
    "shift_expr",
    "add_expr",
    "mul_expr",
];

impl Roles {
    pub fn new(g: &Grammar) -> Roles {
        let mut roles = vec![Role::None; g.rules.len()];
        let mut set = |name: &str, role: Role| {
            let id = g
                .rule_id(name)
                .unwrap_or_else(|| panic!("canon: spec/grammar.ebnf has no rule `{name}`"));
            roles[id] = role;
        };
        let mut set_twins = |name: &str, role: Role| {
            set(name, role);
            set(&format!("{name}_ns"), role);
        };
        for name in TWINNED_UNITS {
            set_twins(name, Role::Unit(Cat::Expr));
        }
        for name in TWINNED_LEFT_CHAINS {
            set_twins(name, Role::LeftChain);
        }
        set_twins("postfix_expr", Role::Postfix);
        set("let_else_postfix", Role::Postfix);
        for (name, cat) in UNITS {
            set(name, Role::Unit(*cat));
        }
        set("pattern", Role::Pattern);
        Roles { roles }
    }

    /// The shape of an oracle parse tree.
    pub fn shape(&self, root: &Node) -> Shape {
        let mut out = Shape::new();
        self.walk(root, false, &mut out);
        out
    }

    fn walk(&self, n: &Node, skip: bool, out: &mut Shape) {
        let span = n.span();
        let mut skip_child = None;
        match (self.roles[n.rule], span) {
            (_, None) | (Role::None, _) => {}
            (Role::Unit(cat), Some((s, e))) => {
                out.insert((s, e, cat));
            }
            (Role::LeftChain, Some((s, _))) => {
                for c in &n.children {
                    if let (Child::Node(_), Some((_, e))) = (c, c.span()) {
                        out.insert((s, e, Cat::Expr));
                    }
                }
            }
            (Role::Postfix, Some((s, e))) => {
                out.insert((s, e, Cat::Expr));
                if let Some(Child::Node(postfixes)) = n.children.get(1) {
                    for p in &postfixes.children {
                        if let Some((_, pe)) = p.span() {
                            out.insert((s, pe, Cat::Expr));
                        }
                    }
                }
            }
            (Role::Pattern, Some((s, e))) => {
                if !skip {
                    out.insert((s, e, Cat::Pat));
                }
                // `"(" pattern ")"`: the AST keeps one pattern, with the outer span.
                if let [Child::Leaf(open), Child::Node(_), Child::Leaf(close)] =
                    n.children.as_slice()
                    && open.term == Terminal::Token(TokenKind::LParen)
                    && close.term == Terminal::Token(TokenKind::RParen)
                {
                    skip_child = Some(1);
                }
            }
        }
        for (i, c) in n.children.iter().enumerate() {
            if let Child::Node(m) = c {
                self.walk(m, skip_child == Some(i), out);
            }
        }
    }
}

/// The shape of the hand-written parser's AST.
pub fn ast_shape(file: &File) -> Shape {
    let mut w = AstWalker { out: Shape::new() };
    for item in &file.items {
        w.item(item);
    }
    w.out
}

struct AstWalker {
    out: Shape,
}

impl AstWalker {
    fn add(&mut self, span: wrela_diag::Span, cat: Cat) {
        self.out.insert((span.start, span.end, cat));
    }

    fn item(&mut self, item: &Item) {
        self.add(item.span, Cat::Item);
        self.attrs(&item.attrs);
        match &item.kind {
            ItemKind::Error(_) => {}
            ItemKind::Fn(f) => self.fn_decl(f),
            ItemKind::Struct(s) => {
                self.generics(&s.generics);
                self.types(&s.traits);
                self.fields(&s.fields);
            }
            ItemKind::Enum(e) => {
                self.generics(&e.generics);
                self.types(&e.traits);
                for v in &e.variants {
                    self.add(v.span, Cat::Variant);
                    match &v.kind {
                        VariantKind::Unit => {}
                        VariantKind::Tuple(tys) => self.types(tys),
                        VariantKind::Struct(fields) => self.fields(fields),
                    }
                }
            }
            ItemKind::Trait(t) => {
                self.generics(&t.generics);
                self.types(&t.supertraits);
                for m in &t.members {
                    self.add(m.span, Cat::Member);
                    self.attrs(&m.attrs);
                    match &m.kind {
                        TraitMemberKind::Fn(f) => self.fn_decl(f),
                        TraitMemberKind::Type { bounds, .. } => self.types(bounds),
                    }
                }
            }
            ItemKind::TraitSet(t) => {
                self.generics(&t.generics);
                self.types(&t.traits);
            }
            ItemKind::Impl(i) => {
                self.generics(&i.generics);
                if let Some(t) = &i.trait_ {
                    self.ty(t);
                }
                self.ty(&i.self_ty);
                for m in &i.members {
                    self.add(m.span, Cat::Member);
                    self.attrs(&m.attrs);
                    match &m.kind {
                        ImplMemberKind::Fn(f) => self.fn_decl(f),
                        ImplMemberKind::Type { ty, .. } => self.ty(ty),
                    }
                }
            }
            ItemKind::Const(c) => {
                if let Some(t) = &c.ty {
                    self.ty(t);
                }
                self.expr(&c.value);
            }
            ItemKind::Use(u) => self.use_tree(u),
            ItemKind::TypeAlias(t) => {
                self.generics(&t.generics);
                self.ty(&t.ty);
            }
        }
    }

    fn attrs(&mut self, attrs: &[Attribute]) {
        for a in attrs {
            self.add(a.span, Cat::Attr);
            if let Some(args) = &a.args {
                self.args(args);
            }
        }
    }

    fn use_tree(&mut self, u: &UseTree) {
        self.add(u.span, Cat::UseTree);
        if let UseKind::Group(trees) = &u.kind {
            for t in trees {
                self.use_tree(t);
            }
        }
    }

    fn fn_decl(&mut self, f: &FnDecl) {
        self.generics(&f.generics);
        for p in &f.params {
            self.add(p.span(), Cat::Param);
            if let Param::Named { ty, default, .. } = p {
                self.ty(ty);
                if let Some(d) = default {
                    self.expr(d);
                }
            }
        }
        if let Some(r) = &f.ret {
            self.ty(&r.ty);
        }
        if let Some(b) = &f.body {
            self.block(b);
        }
    }

    fn generics(&mut self, g: &[GenericParam]) {
        for p in g {
            self.types(&p.bounds);
            if let Some(t) = &p.const_ty {
                self.ty(t);
            }
        }
    }

    fn fields(&mut self, fields: &[FieldDecl]) {
        for f in fields {
            self.add(f.span, Cat::Field);
            self.ty(&f.ty);
            if let Some(d) = &f.default {
                self.expr(d);
            }
        }
    }

    fn types(&mut self, tys: &[TypeExpr]) {
        for t in tys {
            self.ty(t);
        }
    }

    fn ty(&mut self, t: &TypeExpr) {
        // A constant generic argument is an INT in the grammar, not a type.
        if let TypeExprKind::Int(_) = t.kind {
            return;
        }
        self.add(t.span, Cat::Type);
        match &t.kind {
            TypeExprKind::Path(p) => self.path(p),
            TypeExprKind::Array(elem, len) => {
                self.ty(elem);
                if let Some(l) = len {
                    self.expr(l);
                }
            }
            TypeExprKind::Tuple(tys) | TypeExprKind::Traits(tys) => self.types(tys),
            TypeExprKind::Paren(inner) => self.ty(inner),
            TypeExprKind::Error | TypeExprKind::Int(_) => {}
            TypeExprKind::Fn(f) => {
                for p in &f.params {
                    self.ty(&p.ty);
                }
                if let Some(r) = &f.ret {
                    self.ty(r);
                }
            }
        }
    }

    fn path(&mut self, p: &Path) {
        for seg in &p.segments {
            if let Some(g) = &seg.generics {
                self.types(g);
            }
        }
    }

    fn block(&mut self, b: &Block) {
        self.add(b.span, Cat::Block);
        for s in &b.stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        self.add(s.span, Cat::Stmt);
        match &s.kind {
            StmtKind::Let { pat, ty, init, else_ } => {
                self.pat(pat);
                if let Some(t) = ty {
                    self.ty(t);
                }
                self.expr(init);
                if let Some(b) = else_ {
                    self.block(b);
                }
            }
            StmtKind::Var { ty, init, .. } => {
                if let Some(t) = ty {
                    self.ty(t);
                }
                self.expr(init);
            }
            StmtKind::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::While { pat, cond, body } => {
                if let Some(p) = pat {
                    self.pat(p);
                }
                self.expr(cond);
                self.block(body);
            }
            StmtKind::Loop { body } => self.block(body),
            StmtKind::For { pat, iter, body, .. } => {
                self.pat(pat);
                match iter {
                    ForIter::Range { start, end, .. } => {
                        self.expr(start);
                        self.expr(end);
                    }
                    ForIter::Expr(e) => self.expr(e),
                }
                self.block(body);
            }
        }
    }

    fn args(&mut self, args: &[Arg]) {
        for a in args {
            self.expr(&a.value);
        }
    }

    /// A block in a position where the grammar has a block, not an expression.
    fn block_expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Block(b) => self.block(b),
            _ => self.expr(e),
        }
    }

    fn expr(&mut self, e: &Expr) {
        // An arm's assignment is the arm's body in the grammar, not an expression.
        if let ExprKind::Assign { target, value, .. } = &e.kind {
            self.expr(target);
            self.expr(value);
            return;
        }
        self.add(e.span, Cat::Expr);
        match &e.kind {
            ExprKind::Lit(_)
            | ExprKind::Break
            | ExprKind::Continue
            | ExprKind::Yield
            | ExprKind::Error
            | ExprKind::Assign { .. } => {}
            ExprKind::Path(p) => self.path(p),
            ExprKind::Unary(_, x)
            | ExprKind::Take(x)
            | ExprKind::MutArg(x)
            | ExprKind::Try(x)
            | ExprKind::Paren(x) => self.expr(x),
            ExprKind::Unsafe(b) => self.block(b),
            ExprKind::FString(parts) => {
                for p in parts {
                    if let FPart::Hole { expr, .. } = p {
                        self.expr(expr);
                    }
                }
            }
            ExprKind::Binary(_, l, r) => {
                self.expr(l);
                self.expr(r);
            }
            ExprKind::Call { callee, args, .. } => {
                self.expr(callee);
                self.args(args);
            }
            ExprKind::MethodCall { receiver, generics, args, .. } => {
                self.expr(receiver);
                if let Some(g) = generics {
                    self.types(g);
                }
                self.args(args);
            }
            ExprKind::Field { base, .. } => self.expr(base),
            ExprKind::Index { base, index } => {
                self.expr(base);
                self.expr(index);
            }
            ExprKind::StructLit { path, fields, base, .. } => {
                self.path(path);
                for f in fields {
                    if let Some(v) = &f.value {
                        self.expr(v);
                    }
                }
                if let Some(b) = base {
                    self.expr(b);
                }
            }
            ExprKind::Tuple(xs) | ExprKind::Array(xs) => xs.iter().for_each(|x| self.expr(x)),
            ExprKind::ArrayRepeat { value, count } => {
                self.expr(value);
                self.expr(count);
            }
            ExprKind::ArrayFill { items, fill } => {
                items.iter().for_each(|x| self.expr(x));
                self.expr(fill);
            }
            ExprKind::Block(b) => self.block(b),
            ExprKind::If { pat, cond, then, else_ } => {
                if let Some(p) = pat {
                    self.pat(p);
                }
                self.expr(cond);
                self.block(then);
                if let Some(e) = else_ {
                    self.block_expr(e);
                }
            }
            ExprKind::Match { scrutinee, arms, .. } => {
                self.expr(scrutinee);
                for a in arms {
                    self.add(a.span, Cat::Arm);
                    a.pats.iter().for_each(|p| self.pat(p));
                    if let Some(g) = &a.guard {
                        self.expr(g);
                    }
                    self.expr(&a.body);
                }
            }
            ExprKind::Closure { params, ret, body } => {
                for p in params {
                    if let Some(t) = &p.ty {
                        self.ty(t);
                    }
                }
                match ret {
                    Some(r) => {
                        self.ty(r);
                        self.block_expr(body);
                    }
                    None => self.expr(body),
                }
            }
            ExprKind::Return(v) => {
                if let Some(v) = v {
                    self.expr(v);
                }
            }
        }
    }

    fn pat(&mut self, p: &Pat) {
        self.add(p.span, Cat::Pat);
        match &p.kind {
            PatKind::Wild | PatKind::Ident(_) | PatKind::Lit { .. } | PatKind::Error => {}
            PatKind::Path(path) => self.path(path),
            PatKind::TupleStruct(path, pats) => {
                self.path(path);
                pats.iter().for_each(|p| self.pat(p));
            }
            PatKind::Struct { path, fields, .. } => {
                self.path(path);
                for f in fields {
                    if let Some(p) = &f.pat {
                        self.pat(p);
                    }
                }
            }
            PatKind::Tuple(pats) => pats.iter().for_each(|p| self.pat(p)),
        }
    }
}
