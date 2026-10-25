//! Three refactors that move code between an expression, a function and its callers, each
//! planned as text edits and checked before anything is written, as the others are:
//!
//! - **extract-const `<file>:<line>:<col>` `<NAME>`**: the expression there, made of literals,
//!   unit suffixes and constants alone, becomes a constant of the module (with the type the
//!   program gives it there), and the expression its name.
//! - **inline `<fn>`**: each call of a free function whose body is one expression becomes that
//!   expression, and the function goes if nothing else names it. It keeps what each call means
//!   only where nothing about the call can be observed to move: so every parameter is used
//!   exactly once, in parameter order, the body binds no parameter's name again, and every
//!   argument is a local whose type is written where it's declared (a literal takes its type
//!   from where it lands, and an untyped local from its uses, so either could change type in
//!   the body). The expression is the value of a binding typed as the function's result, in a
//!   block of its own (`{ let f: T = body; f }`): its numbers keep their types, and its
//!   temporaries end where they ended in the function.
//! - **extract-fn `<file>:<first>-<last>` `<name>`**: those whole lines, statements of one block,
//!   become a function after the one they're in, which takes the locals they use that are bound
//!   before them and returns the one bound in them that's used after (two or more are refused),
//!   and the lines a call of it. A parameter is `borrow` unless the lines move it (`take`) or the
//!   check says it's changed (then `mut`, and the call lends it `mut`). Lines that bind a value
//!   that owns something (a buffer, a `Vec`) and isn't used after are refused unless they end
//!   their block: the new function would drop it sooner.

use super::*;
use wrela_syntax::ast::{self, ExprKind, StmtKind};

/// `file:line:column`, each from 1.
fn parse_place(at: &str) -> Result<(String, u32, u32), Refusal> {
    let mut parts = at.rsplitn(3, ':');
    let (Some(col), Some(line), Some(file)) = (parts.next(), parts.next(), parts.next()) else {
        return refuse(format!("a place is `<file>:<line>:<column>`, not `{at}`"));
    };
    match (line.parse(), col.parse()) {
        (Ok(l), Ok(c)) if l > 0 && c > 0 => Ok((file.to_string(), l, c)),
        _ => refuse(format!("a place is `<file>:<line>:<column>`, not `{at}`")),
    }
}

/// `file:first-last`, lines from 1.
fn parse_lines(at: &str) -> Result<(String, u32, u32), Refusal> {
    let bad = || refuse(format!("lines are `<file>:<first>-<last>`, not `{at}`"));
    let Some((file, range)) = at.rsplit_once(':') else { return bad() };
    let Some((a, b)) = range.split_once('-') else { return bad() };
    match (a.parse::<u32>(), b.parse::<u32>()) {
        (Ok(a), Ok(b)) if a > 0 && a <= b => Ok((file.to_string(), a, b)),
        _ => bad(),
    }
}

/// Whether statements leave their code early: `return`, `break`, `continue`, `?`, `yield`.
fn leaves<'a>(stmts: impl IntoIterator<Item = &'a ast::Stmt>) -> bool {
    let mut found = false;
    for s in stmts {
        s.for_each_expr(&mut |e| {
            e.walk(&mut |x| {
                found |= matches!(
                    x.kind,
                    ExprKind::Return(_)
                        | ExprKind::Break
                        | ExprKind::Continue
                        | ExprKind::Try(_)
                        | ExprKind::Yield
                );
            })
        });
    }
    found
}

/// A function's locals as written, its parameters among them: none the compiler made.
fn user_locals(body: &wrela_sema::mir::Body) -> impl Iterator<Item = &wrela_sema::mir::LocalDecl> {
    body.locals.iter().filter(|l| matches!(l.kind, wrela_sema::mir::LocalKind::User(_)))
}

/// Calls `f` with each name that `x` reads itself: `x`, if it's a single name, and each field
/// of a struct literal written as shorthand (`S { x }` reads `x`).
fn each_name(x: &ast::Expr, f: &mut impl FnMut(&str)) {
    if let Some(n) = x.single_name() {
        f(n);
    }
    if let ExprKind::StructLit { fields, .. } = &x.kind {
        fields.iter().filter(|i| i.value.is_none()).for_each(|i| f(&i.name.name));
    }
}

// ---- extract-const ----------------------------------------------------------------------------

pub fn extract_const(root: &Path, at: &str, name: &str) -> Result<Plan, Refusal> {
    if !wrela_syntax::lexer::is_name(name) {
        return refuse(format!("`{name}` isn't a name"));
    }
    let (file, line, column) = parse_place(at)?;
    let (rel, insert_at, insertion, span, text, ty) = inspect(root, |checked, sources| {
        let ix = Index::new(checked, sources);
        let p = ix.p;
        let fid = ix.file(&file)?;
        let offset = ix.offset(fid, line, column)?;
        let f = ix.fn_at(fid, offset).ok_or("no function's code is there")?;
        let def = p.func(f);
        let Some(body) = &def.body else { return refuse("no function's code is there") };
        // The smallest expression there.
        let mut best: Option<ast::Expr> = None;
        body.for_each_expr(&mut |e| {
            e.walk(&mut |x| {
                let inside = x.span.file == fid && x.span.start <= offset && offset < x.span.end;
                let size = |s: Span| s.end - s.start;
                let smaller = best.as_ref().is_none_or(|b| size(x.span) < size(b.span));
                if inside && smaller {
                    best = Some(x.clone());
                }
            })
        });
        let e = best.ok_or("no expression is there")?;
        // Literals, unit suffixes and constants only, with no local of the same name.
        let locals: BTreeSet<&str> =
            user_locals(&checked.mir[&f]).map(|l| l.name.as_str()).collect();
        let mut bad = None;
        e.walk(&mut |x| match &x.kind {
            ExprKind::Lit(_) | ExprKind::Unary(..) | ExprKind::Binary(..) | ExprKind::Paren(_) => {}
            ExprKind::Path(_) => match x.single_name() {
                Some(n)
                    if !locals.contains(n)
                        && matches!(
                            wrela_sema::resolve::lookup_name(p, def.module, n),
                            Some(Res::Const(_))
                        ) => {}
                _ => bad = bad.take().or_else(|| p.text(x.span).map(String::from)),
            },
            _ => bad = bad.take().or_else(|| p.text(x.span).map(String::from)),
        });
        if let Some(b) = bad {
            return refuse(format!(
                "it depends on `{b}`, which a constant can't: a constant is literals, unit suffixes and other constants"
            ));
        }
        if p.module(def.module).scope.contains_key(name) || locals.contains(name) {
            return refuse(format!("`{name}` is a name its module or function has already"));
        }
        let ty = ix
            .typed_spans(f)
            .into_iter()
            .find(|(s, _, _)| *s == e.span)
            .map(|(_, t, _)| p.display_ty(t))
            .ok_or("can't tell the expression's type: choose all of it")?;
        let rel = program_file(sources, e.span).ok_or("it isn't in the program's own files")?;
        let source = &sources.file(fid).text;
        let text = p.text(e.span).unwrap_or_default().to_string();
        let decl = format!("const {name}: {ty} = {text}");
        // After the file's imports, or else after the comment it opens with and the blank lines
        // after that: before the first item and its comments.
        let (at, insertion) = match use_statements(source).last() {
            Some(&(_, end)) => (end as u32, format!("\n{decl}\n")),
            None => {
                let intro = intro_end(source);
                let blank: usize = (source[intro..].split_inclusive('\n'))
                    .take_while(|l| l.trim().is_empty())
                    .map(str::len)
                    .sum();
                ((intro + blank) as u32, format!("{decl}\n\n"))
            }
        };
        Ok((rel, at, insertion, e.span, text, ty))
    })?;
    let mut texts = Texts::new(root);
    texts.replace(
        &rel,
        vec![(insert_at, insert_at, insertion), (span.start, span.end, name.to_string())],
    )?;
    check_clean(root, &texts, "the program wouldn't check with the constant")?;
    texts.plan(format!("extract `{text}` as `const {name}: {ty}`"), Vec::new())
}

// ---- inline -----------------------------------------------------------------------------------

pub fn inline(root: &Path, func: &str) -> Result<Plan, Refusal> {
    let (path, edits, delete, count) = inspect(root, |checked, sources| {
        let c = calls_in(checked, sources, func)?;
        if !c.values.is_empty() {
            return refuse(format!(
                "`{}` is used as a value at {}: only its calls can be inlined",
                c.path,
                c.values.join(", ")
            ));
        }
        if c.calls.is_empty() {
            return refuse(format!("nothing calls `{}`", c.path));
        }
        let calls = &c.calls;
        let ix = Index::new(checked, sources);
        let p = ix.p;
        let (Item::Fn(f), path) = ix.one(func)? else {
            return refuse(format!("`{func}` isn't a function"));
        };
        let def = p.func(f);
        if !matches!(def.owner, FnOwner::Free) {
            return refuse(format!("`{path}` is a method: only a free function is inlined"));
        }
        if !p.fn_all_generics(f).is_empty() {
            return refuse(format!("`{path}` is generic"));
        }
        if def.public && p.module(def.module).path == ["main"] {
            return refuse(format!("`{path}` is an export: the host calls it"));
        }
        if let Some(x) = def.params.iter().find(|x| x.default.is_some() || x.mode != Mode::Borrow) {
            return refuse(format!(
                "`{}` has a default or isn't borrowed: only plain parameters are inlined",
                x.name
            ));
        }
        if def.ret_mode != ast::RetMode::Owned || def.opaque.is_some() {
            return refuse(format!(
                "`{path}` returns a projection or a type that names traits: only a value of a written type is inlined"
            ));
        }
        // A name the body binds again (a `let`, a pattern, a closure's parameter) would be the
        // argument where it's inlined.
        if let Some(l) = user_locals(&checked.mir[&f]).find(|l| {
            !matches!(
                l.kind,
                wrela_sema::mir::LocalKind::User(wrela_sema::thir::LocalKind::Param(_))
            ) && def.params.iter().any(|q| q.name == l.name)
        }) {
            return refuse(format!(
                "the body binds `{}` again, which its parameter names: rename one first",
                l.name
            ));
        }
        let Some(body) = &def.body else { return refuse(format!("`{path}` has no body")) };
        let [stmt] = body.stmts.as_slice() else {
            return refuse(format!("`{path}`'s body isn't one expression"));
        };
        let StmtKind::Expr(tail) = &stmt.kind else {
            return refuse(format!("`{path}`'s body isn't one expression"));
        };
        if leaves(&body.stmts) {
            return refuse(format!("`{path}` leaves early (`return`, `?`)"));
        }
        if calls.iter().any(|call| {
            call.span.file == def.span.file
                && def.span.start <= call.span.start
                && call.span.end <= def.span.end
        }) {
            return refuse(format!("`{path}` calls itself"));
        }
        // Each parameter used exactly once, in the parameter order.
        let mut uses: Vec<Vec<Span>> = vec![Vec::new(); def.params.len()];
        let mut shorthand = None;
        tail.walk(&mut |x| {
            if let Some(n) = x.single_name()
                && let Some(i) = def.params.iter().position(|q| q.name == n)
            {
                uses[i].push(x.span);
            }
            if let ExprKind::StructLit { fields, .. } = &x.kind
                && let Some(i) = fields
                    .iter()
                    .find(|i| i.value.is_none() && def.params.iter().any(|q| q.name == i.name.name))
            {
                shorthand = Some(i.name.name.clone());
            }
        });
        if let Some(n) = shorthand {
            return refuse(format!(
                "the body passes `{n}` as a struct literal's shorthand field: write `{n}: {n}` first"
            ));
        }
        if let Some(i) = uses.iter().position(|u| u.len() != 1) {
            return refuse(format!(
                "`{}` is used {} times in the body: each argument would be evaluated a different number of times",
                def.params[i].name,
                uses[i].len()
            ));
        }
        let firsts: Vec<u32> = uses.iter().map(|u| u[0].start).collect();
        if firsts.windows(2).any(|w| w[0] > w[1]) {
            return refuse(
                "the body uses its parameters in another order than they're declared: the arguments would be evaluated in another order",
            );
        }
        // The body is the value of a binding of the function's result type, in a block of its
        // own: its numbers take their types from that type, as they took them from the result,
        // and its temporaries end with the binding's statement, as they ended with the function.
        let ret = p.display_ty(def.ret);
        let body_text = p.text(tail.span).unwrap_or_default();
        // The other names the body uses, each must name the same where it's inlined: every name
        // that starts a path, a struct literal's or a type's too, read off the tokens so none is
        // missed. Not a field or a method (after `.`), a path's later part (after `::`), or a
        // label (before `:`).
        let tokens: Vec<_> = crate::edit::lex(body_text).collect();
        let mut names = BTreeSet::new();
        for (i, t) in tokens.iter().enumerate() {
            use wrela_syntax::token::TokenKind as K;
            let after = |k: K| i > 0 && tokens[i - 1].kind == k;
            if t.kind == K::Ident
                && !after(K::Dot)
                && !after(K::ColonColon)
                && tokens.get(i + 1).is_none_or(|n| n.kind != K::Colon)
            {
                let n = &body_text[t.span.range()];
                if !def.params.iter().any(|q| q.name == n) {
                    names.insert(n.to_string());
                }
            }
        }
        // What each of those names where the function is.
        let named: Vec<(&String, Option<Res>)> =
            names.iter().map(|n| (n, wrela_sema::resolve::lookup_name(p, def.module, n))).collect();
        // Each function a call is in, and its names typed where they're declared.
        let mut typed_in: BTreeMap<wrela_sema::ty::FnId, BTreeSet<String>> = BTreeMap::new();
        let mut edits = Vec::new();
        for call in calls {
            if call.args.len() != def.params.len() {
                return refuse(format!(
                    "a call of `{path}` at {} has a default argument",
                    ix.at(call.span)
                ));
            }
            // The function the call is in: its parameters and the locals typed where they're
            // declared are the arguments that can be passed through. The body's names are
            // checked there at its first call.
            let caller =
                ix.fn_at(call.span.file, call.span.start).ok_or("a call outside a function")?;
            let typed = match typed_in.entry(caller) {
                std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::btree_map::Entry::Vacant(e) => {
                    let locals: BTreeSet<&str> =
                        user_locals(&checked.mir[&caller]).map(|l| l.name.as_str()).collect();
                    let module = p.func(caller).module;
                    for &(n, res) in &named {
                        if locals.contains(n.as_str()) {
                            return refuse(format!(
                                "the body's `{n}` would name the local `{n}` of the call at {}: rename one first",
                                ix.at(call.span)
                            ));
                        }
                        if wrela_sema::resolve::lookup_name(p, module, n) != res {
                            return refuse(format!(
                                "the body's `{n}` names something else at the call at {}: qualify it in `{path}` first",
                                ix.at(call.span)
                            ));
                        }
                    }
                    e.insert(typed_names(p, caller))
                }
            };
            let mut args = Vec::new();
            for &(span, _) in &call.args {
                let text = p.text(span).unwrap_or_default();
                if !wrela_syntax::lexer::is_name(text) || !typed.contains(text) {
                    return refuse(format!(
                        "the call at {} passes `{text}`: only a local whose type is written where it's declared (or a parameter) is passed through, so nothing about the call changes",
                        ix.at(call.span)
                    ));
                }
                args.push(text.to_string());
            }
            // The body, each parameter its argument: the uses are in the parameters' order.
            let mut out = String::new();
            let mut pos = tail.span.start;
            for (s, a) in uses.iter().map(|u| u[0]).zip(&args) {
                out.push_str(
                    &body_text
                        [(pos - tail.span.start) as usize..(s.start - tail.span.start) as usize],
                );
                out.push_str(a);
                pos = s.end;
            }
            out.push_str(&body_text[(pos - tail.span.start) as usize..]);
            // The function's name, which the body can't use (it would call itself, or be a
            // value), so the binding hides nothing the body names.
            let n = &def.name;
            let expr = format!("{{ let {n}: {ret} = {out}; {n} }}");
            edits.push((call.file.clone(), call.span.start, call.span.end, expr));
        }
        // The function goes when it's private: every use was a call, and each is inlined.
        let delete = if def.public {
            None
        } else {
            let f_file = program_file(sources, def.span).ok_or("it isn't the program's own")?;
            let (s, e) = whole_lines(sources.file(def.span.file), def.span);
            Some((f_file, s, e))
        };
        Ok((path, edits, delete, calls.len()))
    })?;
    let mut texts = Texts::new(root);
    let mut all = edits;
    if let Some((file, s, e)) = delete {
        all.push((file, s, e, String::new()));
    }
    texts.replace_all(all)?;
    check_clean(root, &texts, "the program wouldn't check with the function inlined")?;
    texts.plan(
        format!("inline `{path}`"),
        vec![format!("{count} call{} inlined", wrela_diag::plural(count))],
    )
}

/// The names in function `f` whose type is written where they're declared: its parameters, and
/// its `let x: T` and `var x: T` bindings.
fn typed_names(p: &wrela_sema::program::Program, f: wrela_sema::ty::FnId) -> BTreeSet<String> {
    let def = p.func(f);
    let mut out: BTreeSet<String> = def.params.iter().map(|q| q.name.clone()).collect();
    if let Some(body) = &def.body {
        body.for_each_block(&mut |b| {
            for s in &b.stmts {
                match &s.kind {
                    StmtKind::Let { pat, ty: Some(_), .. } => {
                        if let ast::PatKind::Ident(i) = &pat.kind {
                            out.insert(i.name.clone());
                        }
                    }
                    StmtKind::Var { name, ty: Some(_), .. } => {
                        out.insert(name.name.clone());
                    }
                    _ => {}
                }
            }
        });
    }
    out
}

// ---- extract-fn -------------------------------------------------------------------------------

/// What extract-fn found in the program: where the lines are and what flows in and out.
struct Extraction {
    file: String,
    /// The lines replaced: from their first line's start to their last's end.
    lines: (u32, u32),
    indent: String,
    body: String,
    /// Each parameter: its name, its type, and whether the lines move it.
    params: Vec<(String, String, bool)>,
    /// The local the lines bind and the code after uses: its name, type and whether it's `var`.
    result: Option<(String, String, bool)>,
    /// Where the new function goes: after the item the lines are in.
    after: u32,
}

pub fn extract_fn(root: &Path, at: &str, name: &str) -> Result<Plan, Refusal> {
    if !wrela_syntax::lexer::is_name(name) {
        return refuse(format!("`{name}` isn't a name"));
    }
    let (file, first, last) = parse_lines(at)?;
    let x = inspect(root, |checked, sources| {
        let ix = Index::new(checked, sources);
        let p = ix.p;
        let fid = ix.file(&file)?;
        let src = sources.file(fid);
        if last as usize > src.line_count() {
            return refuse(format!("{} has {} lines", src.name, src.line_count()));
        }
        let rs = src.line_start(first as usize - 1);
        let re = if (last as usize) < src.line_count() {
            src.line_start(last as usize)
        } else {
            src.text.len() as u32
        };
        let text = &src.text;
        // The code in the lines, without the blank space around it.
        let body_text = &text[rs as usize..re as usize];
        let lead = body_text.len() - body_text.trim_start().len();
        let (cs, ce) = (rs + lead as u32, rs + body_text.trim_end().len() as u32);
        let f = ix.fn_at(fid, cs).ok_or("the lines aren't in a function's code")?;
        let def = p.func(f);
        if !p.fn_all_generics(f).is_empty() {
            return refuse("the lines are in a generic function: extract them by hand");
        }
        let Some(fbody) = &def.body else { return refuse("the lines aren't in a function's code") };
        // The block whose statements the lines are, whole.
        let mut all: Vec<&ast::Block> = Vec::new();
        fbody.for_each_block(&mut |b| all.push(b));
        let found = all.iter().find_map(|b| {
            let inside: Vec<&ast::Stmt> =
                b.stmts.iter().filter(|s| s.span.start >= cs && s.span.end <= ce).collect();
            let partly = b.stmts.iter().any(|s| {
                s.span.start < ce && s.span.end > cs && !(s.span.start >= cs && s.span.end <= ce)
            });
            let whole = inside.first().is_some_and(|s| s.span.start == cs)
                && inside.last().is_some_and(|s| s.span.end == ce);
            let ends_block = b.stmts.last().is_some_and(|s| s.span.end == ce);
            (whole && !partly).then_some((inside, ends_block))
        });
        let Some((stmts, ends_block)) = found else {
            return refuse("the lines aren't whole statements of one block");
        };
        if leaves(stmts.iter().copied()) {
            return refuse(
                "the lines leave early (`return`, `break`, `continue`, `?`): extract them by hand",
            );
        }
        // Names the lines use, the code after them uses, and that are `self`.
        let mut used: Vec<String> = Vec::new();
        let mut taken: BTreeSet<String> = BTreeSet::new();
        let mut uses_self = false;
        for s in &stmts {
            s.for_each_expr(&mut |e| {
                e.walk(&mut |x| {
                    each_name(x, &mut |n| {
                        if !used.iter().any(|u| u == n) {
                            used.push(n.to_string());
                        }
                    });
                    if let ExprKind::Take(inner) = &x.kind
                        && let Some(n) = inner.single_name()
                    {
                        taken.insert(n.to_string());
                    }
                    uses_self |=
                        matches!(&x.kind, ExprKind::Path(q) if q.segments[0].ident.name == "self");
                })
            });
        }
        if uses_self {
            return refuse("the lines use `self`: extract them as a method by hand");
        }
        // The statements after the lines, in any block: the names they use, and those they
        // change (assigned, or lent `mut`).
        let mut after: Vec<&ast::Stmt> =
            all.iter().flat_map(|b| &b.stmts).filter(|s| s.span.start >= ce).collect();
        let root_name = |e: &ast::Expr| {
            let mut e = e;
            loop {
                match &e.kind {
                    ExprKind::Field { base, .. } | ExprKind::Index { base, .. } => e = base,
                    _ => return e.single_name().map(String::from),
                }
            }
        };
        let mut changed_after: BTreeSet<String> = after
            .iter()
            .filter_map(|s| match &s.kind {
                StmtKind::Assign { target, .. } => root_name(target),
                _ => None,
            })
            .collect();
        let mut after_uses: BTreeSet<String> = BTreeSet::new();
        // Each statement is walked once, as part of the outermost of them it's in: statements
        // nest or are apart, so in order of start, one that starts inside the last walked is in it.
        after.sort_by_key(|s| (s.span.start, std::cmp::Reverse(s.span.end)));
        let mut walked_to = 0;
        for s in after {
            if s.span.start < walked_to {
                continue;
            }
            walked_to = s.span.end;
            s.for_each_expr(&mut |e| {
                e.walk(&mut |x| {
                    each_name(x, &mut |n| {
                        after_uses.insert(n.to_string());
                    });
                    if let ExprKind::MutArg(inner) = &x.kind
                        && let Some(n) = root_name(inner)
                    {
                        changed_after.insert(n);
                    }
                })
            });
        }
        let user: Vec<&wrela_sema::mir::LocalDecl> = user_locals(&checked.mir[&f]).collect();
        // The blocks inside the lines: what's bound in one ends there, in the function too.
        let mut nested: Vec<Span> = Vec::new();
        for s in &stmts {
            s.for_each_block(&mut |b| nested.push(b.span));
        }
        let mut params = Vec::new();
        let mut result = None;
        for n in &used {
            let decls: Vec<&&wrela_sema::mir::LocalDecl> =
                user.iter().filter(|l| &l.name == n).collect();
            let Some(d) = decls.first() else { continue };
            if decls.len() > 1 {
                return refuse(format!("`{n}` names more than one local here: rename one first"));
            }
            if d.span.start < cs {
                params.push((n.clone(), p.display_ty(d.ty), taken.contains(n)));
            }
        }
        for d in &user {
            let bound_inside = d.span.start >= cs && d.span.start < ce;
            // A value that owns something (a buffer, a `Vec`) the lines bind for the rest of
            // their block would be dropped when the new function returns, before the code after
            // the lines runs: what that code observes (`std::gpu::memory()`) would change.
            let owns = matches!(
                d.kind,
                wrela_sema::mir::LocalKind::User(wrela_sema::thir::LocalKind::Owned { .. })
            ) && d.closure.is_none()
                && !nested.iter().any(|b| b.start <= d.span.start && d.span.end <= b.end)
                && wrela_sema::traits::may_need_drop(p, d.ty);
            if bound_inside && owns && !ends_block && !after_uses.contains(&d.name) {
                return refuse(format!(
                    "`{}` owns what it holds to the end of its block, and would be dropped where the new function returns: extract the lines to the end of the block",
                    d.name
                ));
            }
            if bound_inside && after_uses.contains(&d.name) {
                if result.is_some() {
                    return refuse(
                        "more than one value the lines make is used after them: extract fewer lines",
                    );
                }
                if matches!(
                    d.kind,
                    wrela_sema::mir::LocalKind::User(
                        wrela_sema::thir::LocalKind::Projection { .. }
                    )
                ) {
                    return refuse(format!(
                        "`{}` is a projection, which a function can't return here",
                        d.name
                    ));
                }
                // A `var` there stays one only if what comes after changes it.
                let var = matches!(
                    d.kind,
                    wrela_sema::mir::LocalKind::User(wrela_sema::thir::LocalKind::Owned {
                        mutable: true
                    })
                ) && changed_after.contains(&d.name);
                result = Some((d.name.clone(), p.display_ty(d.ty), var));
            }
        }
        // The call is a bare name: one a local, an item or a built-in has would call that.
        if wrela_sema::resolve::lookup_name(p, def.module, name).is_some()
            || user.iter().any(|l| l.name == name)
        {
            return refuse(format!("`{name}` is a name its module or function has already"));
        }
        // The new function goes after the top-level item the lines are in: the function, or
        // its impl or trait (their spans end where the item does).
        let item = match def.owner {
            FnOwner::Free => def.span,
            FnOwner::Impl(im) => p.impl_(im).span,
            FnOwner::Trait(t) => p.trait_(t).span,
            FnOwner::Const(c) => p.const_(c).span,
        };
        let (_, after) = whole_lines(src, item);
        let indent: String = body_text.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let rel = program_file(sources, def.span).ok_or("the lines aren't the program's own")?;
        Ok(Extraction {
            file: rel,
            lines: (rs, re),
            indent,
            body: body_text.to_string(),
            params,
            result,
            after,
        })
    })?;
    // Each round writes the function and the call from the modes so far; a check that says a
    // parameter is changed makes it `mut`, until the program checks.
    let mut muts: BTreeSet<String> = BTreeSet::new();
    for _ in 0..ROUNDS {
        let mut texts = Texts::new(root);
        let (decl, call) = extraction_texts(&x, name, &muts);
        texts.replace(&x.file, vec![(x.after, x.after, decl), (x.lines.0, x.lines.1, call)])?;
        let (out, found) = check_round(root, &texts);
        let before = muts.len();
        for (_, _, _, n) in &found {
            if x.params.iter().any(|(q, _, take)| q == n && !take) {
                muts.insert(n.clone());
            }
        }
        // No new `mut`: the program checks (it found no errors), or it never will.
        if muts.len() == before {
            clean(out, "the program wouldn't check with the lines extracted")?;
            let ps: Vec<&str> = x.params.iter().map(|(n, ..)| n.as_str()).collect();
            return texts
                .plan(format!("extract the lines as `{name}({})`", ps.join(", ")), Vec::new());
        }
    }
    refuse(format!("the program still had errors after {ROUNDS} rounds"))
}

/// The new function's text and its call's, with the parameters in `muts` lent `mut`.
fn extraction_texts(x: &Extraction, name: &str, muts: &BTreeSet<String>) -> (String, String) {
    let mode = |n: &String, take: bool| {
        if take {
            "take "
        } else if muts.contains(n) {
            "mut "
        } else {
            ""
        }
    };
    let params: Vec<String> =
        x.params.iter().map(|(n, t, take)| format!("{n}: {}{t}", mode(n, *take))).collect();
    let args: Vec<String> =
        x.params.iter().map(|(n, _, take)| format!("{}{n}", mode(n, *take))).collect();
    // The lines, from their own indentation to the function's.
    let lines: Vec<&str> = x.body.lines().collect();
    let strip = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut body = String::new();
    for l in &lines {
        if l.trim().is_empty() {
            body.push('\n');
        } else {
            body.push_str(&format!("    {}\n", &l[strip.min(l.len())..]));
        }
    }
    let (ret, tail, bind) = match &x.result {
        Some((n, t, var)) => {
            let kw = if *var { "var" } else { "let" };
            (format!(" -> {t}"), format!("    {n}\n"), format!("{kw} {n} = "))
        }
        None => (String::new(), String::new(), String::new()),
    };
    let decl = format!("\nfn {name}({}){ret} {{\n{body}{tail}}}\n", params.join(", "));
    let call = format!("{}{bind}{name}({})\n", x.indent, args.join(", "));
    (decl, call)
}
