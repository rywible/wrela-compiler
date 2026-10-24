//! Three refactors that move code between an expression, a function and its callers, each
//! planned as text edits and checked before anything is written, as the others are:
//!
//! - **extract-const `<file>:<line>:<col>` `<NAME>`**: the expression there, made of literals,
//!   unit suffixes and constants alone, becomes a constant of the module (with the type the
//!   program gives it there), and the expression its name.
//! - **inline `<fn>`**: each call of a free function whose body is one expression becomes that
//!   expression, and the function goes if nothing else names it. It keeps what each call means
//!   only where nothing about the call can be observed to move: so every parameter is used
//!   exactly once, in parameter order, and every argument is a local whose type is written
//!   where it's declared (a literal takes its type from where it lands, and an untyped local from
//!   its uses, so either could change type in the body); a body with a bare number keeps it typed
//!   by wrapping the expression in a conversion to the function's (scalar) result.
//! - **extract-fn `<file>:<first>-<last>` `<name>`**: those whole lines, statements of one block,
//!   become a function after the one they're in, which takes the locals they use that are bound
//!   before them and returns the one bound in them that's used after (two or more are refused),
//!   and the lines a call of it. A parameter is `borrow` unless the lines move it (`take`) or the
//!   check says it's changed (then `mut`, and the call lends it `mut`).

use super::*;
use wrela_syntax::ast::{self, ExprKind, LitKind, StmtKind};

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

/// Every block in `b`, nested ones included, `b` first.
fn blocks<'a>(b: &'a ast::Block, out: &mut Vec<&'a ast::Block>) {
    out.push(b);
    let expr = |e: &'a ast::Expr, out: &mut Vec<&'a ast::Block>| expr_blocks(e, out);
    for s in &b.stmts {
        match &s.kind {
            StmtKind::Let { init, else_, .. } => {
                expr(init, out);
                if let Some(e) = else_ {
                    blocks(e, out);
                }
            }
            StmtKind::Var { init, .. } => expr(init, out),
            StmtKind::Assign { target, value, .. } => {
                expr(target, out);
                expr(value, out);
            }
            StmtKind::Expr(e) => expr(e, out),
            StmtKind::While { cond, body, .. } => {
                expr(cond, out);
                blocks(body, out);
            }
            StmtKind::Loop { body } => blocks(body, out),
            StmtKind::For { iter, body, .. } => {
                match iter {
                    ast::ForIter::Range { start, end, .. } => {
                        expr(start, out);
                        expr(end, out);
                    }
                    ast::ForIter::Expr(e) => expr(e, out),
                }
                blocks(body, out);
            }
        }
    }
}

fn expr_blocks<'a>(e: &'a ast::Expr, out: &mut Vec<&'a ast::Block>) {
    match &e.kind {
        ExprKind::Block(b) | ExprKind::Unsafe(b) => blocks(b, out),
        ExprKind::If { cond, then, else_, .. } => {
            expr_blocks(cond, out);
            blocks(then, out);
            if let Some(e) = else_ {
                expr_blocks(e, out);
            }
        }
        _ => e.for_each_child(&mut |c| expr_blocks(c, out)),
    }
}

/// Calls `f` on every expression in `stmts`, nested ones included.
fn each_expr(stmts: &[ast::Stmt], f: &mut impl FnMut(&ast::Expr)) {
    let b = ast::Block { stmts: stmts.to_vec(), span: Span::new(wrela_diag::FileId(0), 0, 0) };
    b.for_each_expr(&mut |e| e.walk(&mut |x| f(x)));
}

/// The name an expression is, if it's a single name.
fn name_of(e: &ast::Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Path(p) if p.is_single() => Some(p.segments[0].ident.name.as_str()),
        _ => None,
    }
}

/// Whether an expression leaves its code early: `return`, `break`, `continue`, `?`, `yield`.
fn leaves(stmts: &[ast::Stmt]) -> bool {
    let mut found = false;
    each_expr(stmts, &mut |e| {
        found |= matches!(
            e.kind,
            ExprKind::Return(_)
                | ExprKind::Break
                | ExprKind::Continue
                | ExprKind::Try(_)
                | ExprKind::Yield
        );
    });
    found
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
        each_expr(&body.stmts, &mut |e| {
            let inside = e.span.file == fid && e.span.start <= offset && offset < e.span.end;
            let size = |s: Span| s.end - s.start;
            let smaller = best.as_ref().is_none_or(|b| size(e.span) < size(b.span));
            if inside && smaller {
                best = Some(e.clone());
            }
        });
        let e = best.ok_or("no expression is there")?;
        // Literals, unit suffixes and constants only, with no local of the same name.
        let locals: BTreeSet<&str> = checked.mir[&f]
            .locals
            .iter()
            .filter(|l| matches!(l.kind, wrela_sema::mir::LocalKind::User(_)))
            .map(|l| l.name.as_str())
            .collect();
        let mut bad = None;
        e.walk(&mut |x| match &x.kind {
            ExprKind::Lit(_) | ExprKind::Unary(..) | ExprKind::Binary(..) | ExprKind::Paren(_) => {}
            ExprKind::Path(_) => match name_of(x) {
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
        // After the file's imports, or else after the comment it opens with.
        let (at, insertion) = match use_statements(source).last() {
            Some(&(_, end)) => (end as u32, format!("\n{decl}\n")),
            None => {
                let mut at = 0;
                for l in source.split_inclusive('\n') {
                    if l.trim_start().starts_with("//") || l.trim().is_empty() {
                        at += l.len();
                    } else {
                        break;
                    }
                }
                (at as u32, format!("{decl}\n\n"))
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
    let c = calls_of(root, func)?;
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
    let (edits, delete, count) = inspect(root, |checked, sources| {
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
        let mut numbers = false;
        tail.walk(&mut |x| {
            if let Some(n) = name_of(x)
                && let Some(i) = def.params.iter().position(|q| q.name == n)
            {
                uses[i].push(x.span);
            }
            if let ExprKind::Lit(l) = &x.kind {
                numbers |=
                    matches!(l.kind, LitKind::Int(_) | LitKind::Float(_) | LitKind::Suffixed);
            }
        });
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
        // A bare number takes its type from where it is: kept by a conversion to the result.
        let convert = if numbers {
            match p.types.kind(def.ret) {
                wrela_sema::ty::TyKind::Int(_) | wrela_sema::ty::TyKind::Float(_) => {
                    Some(p.display_ty(def.ret))
                }
                _ => {
                    return refuse(format!(
                        "`{path}`'s body has a number whose type would come from where it's inlined"
                    ));
                }
            }
        } else {
            None
        };
        let body_text = p.text(tail.span).unwrap_or_default();
        let mut edits = Vec::new();
        for call in calls {
            if call.args.len() != def.params.len() {
                return refuse(format!(
                    "a call of `{path}` at {} has a default argument",
                    ix.at(call.span)
                ));
            }
            // The function the call is in: its parameters and the locals typed where they're
            // declared are the arguments that can be passed through.
            let caller =
                ix.fn_at(call.span.file, call.span.start).ok_or("a call outside a function")?;
            let typed = typed_names(p, caller);
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
            // The body, each parameter its argument.
            let mut out = String::new();
            let mut pos = tail.span.start;
            let mut spans: Vec<(Span, &str)> =
                uses.iter().zip(&args).map(|(u, a)| (u[0], a.as_str())).collect();
            spans.sort_by_key(|(s, _)| s.start);
            for (s, a) in spans {
                out.push_str(
                    &body_text
                        [(pos - tail.span.start) as usize..(s.start - tail.span.start) as usize],
                );
                out.push_str(a);
                pos = s.end;
            }
            out.push_str(&body_text[(pos - tail.span.start) as usize..]);
            let expr = match &convert {
                Some(t) => format!("{t}({out})"),
                None => format!("({out})"),
            };
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
        Ok((edits, delete, calls.len()))
    })?;
    let mut texts = Texts::new(root);
    let mut all = edits;
    if let Some((file, s, e)) = delete {
        all.push((file, s, e, String::new()));
    }
    texts.replace_all(all)?;
    check_clean(root, &texts, "the program wouldn't check with the function inlined")?;
    texts.plan(
        format!("inline `{}`", c.path),
        vec![format!("{count} call{} inlined", wrela_diag::plural(count))],
    )
}

/// The names in function `f` whose type is written where they're declared: its parameters, and
/// its `let x: T` and `var x: T` bindings.
fn typed_names(p: &wrela_sema::program::Program, f: wrela_sema::ty::FnId) -> BTreeSet<String> {
    let def = p.func(f);
    let mut out: BTreeSet<String> = def.params.iter().map(|q| q.name.clone()).collect();
    if let Some(body) = &def.body {
        let mut all = Vec::new();
        blocks(body, &mut all);
        for b in all {
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
        }
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
        let mut all = Vec::new();
        blocks(fbody, &mut all);
        let found = all.into_iter().find_map(|b| {
            let inside: Vec<&ast::Stmt> =
                b.stmts.iter().filter(|s| s.span.start >= cs && s.span.end <= ce).collect();
            let partly = b.stmts.iter().any(|s| {
                s.span.start < ce && s.span.end > cs && !(s.span.start >= cs && s.span.end <= ce)
            });
            let whole = inside.first().is_some_and(|s| s.span.start == cs)
                && inside.last().is_some_and(|s| s.span.end == ce);
            (whole && !partly).then(|| inside.into_iter().cloned().collect::<Vec<_>>())
        });
        let Some(stmts) = found else {
            return refuse("the lines aren't whole statements of one block");
        };
        if leaves(&stmts) {
            return refuse(
                "the lines leave early (`return`, `break`, `continue`, `?`): extract them by hand",
            );
        }
        // Names the lines use, the code after them uses, and that are `self`.
        let mut used: Vec<String> = Vec::new();
        let mut taken: BTreeSet<String> = BTreeSet::new();
        let mut uses_self = false;
        each_expr(&stmts, &mut |e| {
            if let Some(n) = name_of(e)
                && !used.iter().any(|u| u == n)
            {
                used.push(n.to_string());
            }
            if let ExprKind::Take(inner) = &e.kind
                && let Some(n) = name_of(inner)
            {
                taken.insert(n.to_string());
            }
            uses_self |= matches!(&e.kind, ExprKind::Path(q) if q.segments[0].ident.name == "self");
        });
        if uses_self {
            return refuse("the lines use `self`: extract them as a method by hand");
        }
        let mut after_uses: BTreeSet<String> = BTreeSet::new();
        let rest: Vec<ast::Stmt> = {
            let mut all = Vec::new();
            blocks(fbody, &mut all);
            all.iter()
                .flat_map(|b| b.stmts.iter())
                .filter(|s| s.span.start >= ce)
                .cloned()
                .collect()
        };
        each_expr(&rest, &mut |e| {
            if let Some(n) = name_of(e)
                && e.span.start >= ce
            {
                after_uses.insert(n.to_string());
            }
        });
        // The names the code after the lines changes: assigned, or lent `mut`.
        let mut changed_after: BTreeSet<String> = BTreeSet::new();
        let root_name = |e: &ast::Expr| {
            let mut e = e;
            loop {
                match &e.kind {
                    ExprKind::Field { base, .. } | ExprKind::Index { base, .. } => e = base,
                    _ => return name_of(e).map(String::from),
                }
            }
        };
        {
            let mut all = Vec::new();
            blocks(fbody, &mut all);
            for b in all {
                for s in b.stmts.iter().filter(|s| s.span.start >= ce) {
                    if let StmtKind::Assign { target, .. } = &s.kind
                        && let Some(n) = root_name(target)
                    {
                        changed_after.insert(n);
                    }
                }
            }
        }
        each_expr(&rest, &mut |e| {
            if let ExprKind::MutArg(inner) = &e.kind
                && let Some(n) = root_name(inner)
            {
                changed_after.insert(n);
            }
        });
        let body = &checked.mir[&f];
        let user: Vec<&wrela_sema::mir::LocalDecl> = body
            .locals
            .iter()
            .filter(|l| matches!(l.kind, wrela_sema::mir::LocalKind::User(_)))
            .collect();
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
        if p.module(def.module).scope.contains_key(name) {
            return refuse(format!("`{name}` is a name its module has already"));
        }
        // The new function goes after the top-level item the lines are in.
        let parsed = wrela_syntax::parse(fid, text);
        let item = parsed
            .file
            .items
            .iter()
            .find(|it| it.span.start <= cs && ce <= it.span.end)
            .ok_or("the lines aren't in an item")?;
        let (_, after) = whole_lines(src, item.span);
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
        if !out.has_errors() {
            let ps: Vec<&str> = x.params.iter().map(|(n, ..)| n.as_str()).collect();
            return texts
                .plan(format!("extract the lines as `{name}({})`", ps.join(", ")), Vec::new());
        }
        let before = muts.len();
        for (_, _, _, n) in &found {
            if x.params.iter().any(|(q, _, take)| q == n && !take) {
                muts.insert(n.clone());
            }
        }
        if muts.len() == before {
            clean(out, "the program wouldn't check with the lines extracted")?;
            return refuse("the program wouldn't check with the lines extracted");
        }
    }
    refuse(format!("the program still had errors after {ROUNDS} rounds"))
}

/// The new function's text and its call's, with the parameters in `muts` lent `mut`.
fn extraction_texts(x: &Extraction, name: &str, muts: &BTreeSet<String>) -> (String, String) {
    let params: Vec<String> = x
        .params
        .iter()
        .map(|(n, t, take)| {
            let mode = if *take {
                "take "
            } else if muts.contains(n) {
                "mut "
            } else {
                ""
            };
            format!("{n}: {mode}{t}")
        })
        .collect();
    let args: Vec<String> = x
        .params
        .iter()
        .map(|(n, _, take)| {
            if *take {
                format!("take {n}")
            } else if muts.contains(n) {
                format!("mut {n}")
            } else {
                n.clone()
            }
        })
        .collect();
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
