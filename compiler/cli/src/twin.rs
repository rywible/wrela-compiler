//! `wrela twin <package-dir> <module>::<fn> --box <lo>..<hi>[,<lo>..<hi>]... --within <eps>
//! [--name <twin>] [--write | --check] [--json]`: a cheaper twin of a function GPU code calls,
//! proven within `eps` of it over the box (language.md §14.3), as source.
//!
//! The function (of one `f32` or float vector, returning an `f32`) is called from a kernel in a
//! program written around the package (`<package>/build/twin/`), whose GPU module the error walk
//! (`wrela_ir::interp::error`) runs over the box: each value's range, and the error a twin's
//! changes carry to the result. The rewrites it knows, each with its own bound `δ`:
//! - a term dropped: `a + t` or `a - t` becomes `a` (and `t + a` becomes `a`), `δ = |t|` over the
//!   box;
//! - a power as products: `pow(x, 2.0)`, `3.0` or `4.0` becomes `x * x`, ..., `δ = 4·2⁻²³·|result|`;
//! - a reciprocal: `x / c`, `c` a constant, becomes `x * (1.0 / c)`, `δ = 2⁻²³·|result|`.
//!
//! The search takes them greedily, most work saved first, keeping each while the walk's bound on
//! the result stays within `eps`; where a walk is refused or over `eps`, the box is cut in two
//! along each axis and walked again, to depth 3, and the largest bound over the pieces counts.
//! A refusal means no proof, and the rewrite isn't made. `proven` covers real arithmetic over
//! the box; each target's rounding is §14.2's.
//!
//! The twin is the original's text with each rewrite's edit, named `<fn>_twin` (or `--name`),
//! with a doc comment that is its certificate. It's printed; `--write` puts it after the original
//! (in place of an earlier twin); `--check` derives it again from the original with the box and
//! tolerance its comment gives, and exits 1 if the twin in the file differs (the original
//! changed, or the twin was edited). Exit status: 0 done (or the twin is current), 1 the build
//! failed, the walk refused the box, or the check failed, 2 a usage error.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wrela_ir::interp::error::{A, R, Seen, walk_kernel};
use wrela_ir::{BinOp, Builtin, Const, Expr, FuncId, Module, ResourceId, Stmt, ValueId};

const USAGE: &str = "usage: wrela twin <package-dir> <module>::<fn> --box <lo>..<hi>[,<lo>..<hi>]... --within <eps> [--name <twin>] [--write | --check] [--json]";

/// How deep the box is cut where a walk is refused or over the tolerance.
const DEPTH: u32 = 3;

/// 2⁻²³: an f32's relative rounding.
const ULP: f64 = 1.0 / 8_388_608.0;

struct Args {
    dir: PathBuf,
    module: String,
    function: String,
    boxes: Option<Vec<(f64, f64)>>,
    within: Option<f64>,
    name: Option<String>,
    write: bool,
    check: bool,
    json: bool,
}

pub fn run(args: &[String]) -> ExitCode {
    let Some(a) = parse(args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match answer(&a) {
        Ok((text, ok)) => {
            print!("{text}");
            if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse(args: &[String]) -> Option<Args> {
    let mut a = Args {
        dir: PathBuf::new(),
        module: String::new(),
        function: String::new(),
        boxes: None,
        within: None,
        name: None,
        write: false,
        check: false,
        json: false,
    };
    let (mut dir, mut target) = (None, None);
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--box" => a.boxes = Some(parse_box(it.next()?)?),
            "--within" => a.within = Some(it.next()?.parse().ok().filter(|&e: &f64| e > 0.0)?),
            "--name" => a.name = Some(it.next()?.clone()),
            "--write" => a.write = true,
            "--check" => a.check = true,
            "--json" => a.json = true,
            _ if dir.is_none() && !x.starts_with('-') => dir = Some(PathBuf::from(x)),
            _ if target.is_none() && !x.starts_with('-') => target = Some(x.clone()),
            _ => return None,
        }
    }
    let (module, function) =
        target?.rsplit_once("::").map(|(m, f)| (m.to_string(), f.to_string()))?;
    if a.write && a.check || !a.check && (a.boxes.is_none() || a.within.is_none()) {
        return None;
    }
    a.dir = dir?;
    a.module = module;
    a.function = function;
    Some(a)
}

fn parse_box(s: &str) -> Option<Vec<(f64, f64)>> {
    s.split(',')
        .map(|r| {
            let (lo, hi) = r.trim().split_once("..")?;
            let (lo, hi): (f64, f64) = (lo.trim().parse().ok()?, hi.trim().parse().ok()?);
            (lo.is_finite() && hi.is_finite() && lo <= hi).then_some((lo, hi))
        })
        .collect::<Option<Vec<_>>>()
        .filter(|b| (1..=4).contains(&b.len()))
}

/// A rewrite the search can make: where, its edit, its bound, and the work it saves.
#[derive(Clone, Debug)]
struct Rewrite {
    value: ValueId,
    /// The source text to replace (byte range in the function's file) and its replacement.
    span: (u32, u32),
    with: String,
    delta: f64,
    saves: u64,
    what: String,
}

/// The kernel around the function, its module and where its box goes.
struct Kernel {
    m: Module,
    entry: FuncId,
    input: ResourceId,
    /// The function's file's text, and its id in the build's sources.
    text: String,
    file: wrela_diag::FileId,
}

fn glue(package: &str, module: &str, function: &str, dims: usize) -> String {
    let arg = match dims {
        1 => "p.x",
        2 => "p.xy",
        3 => "p.xyz",
        _ => "p",
    };
    format!(
        "// Written by `wrela twin`.\n\n\
         use std::gpu::{{GlobalId, GpuBuffer, Slots, buffer, dispatch}}\n\
         use {package}::{module}::{function}\n\n\
         pub struct S {{\n    points: GpuBuffer<vec4>,\n    out: GpuBuffer<vec4>,\n}}\n\n\
         pub fn init() -> S {{\n    S {{ points: buffer(64), out: buffer(64) }}\n}}\n\n\
         @compute(64)\n\
         fn twin_k(points: [vec4], out: mut Slots<vec4>, id: GlobalId) {{\n    \
         let p = points[id.x]\n    out[id] = vec4({function}({arg}), 0.0, 0.0, 0.0)\n}}\n\n\
         pub fn frame(s: mut S, time: f32, width: u32, height: u32) {{\n    \
         dispatch(twin_k.bind(s.points, mut s.out), groups: 1)\n}}\n"
    )
}

fn kernel(a: &Args, dims: usize, path: &Path) -> Result<Kernel, String> {
    let (glue_dir, _) =
        crate::glue_package(&a.dir, "twin", |package| glue(package, &a.module, &a.function, dims))?;
    let kind = wrela_driver::BuildKind { testing: true, ..Default::default() };
    let out = wrela_driver::gpu_modules(&glue_dir, kind)
        .map_err(|out| wrela_diag::render::render_all(&out.sources, &out.diagnostics))?;
    let g = out
        .gpu
        .into_iter()
        .find(|g| g.entries.iter().any(|e| e == "twin_k"))
        .ok_or("the kernel around the function wasn't built")?;
    let m = g.module;
    let input = m
        .resources
        .iter()
        .position(|r| r.kind == wrela_ir::ResourceKind::StorageRead)
        .ok_or("the kernel reads no buffer")?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file = (0..)
        .map(wrela_diag::FileId)
        .take_while(|&id| (id.0 as usize) < out.sources.len())
        .find(|&id| out.sources.file(id).text == text)
        .ok_or("the function's file isn't among the build's sources")?;
    Ok(Kernel { entry: m.entry_points[0].function, input: ResourceId(input as u32), m, text, file })
}

/// The kernel's input as a box: the function's numbers, then zeros to a `vec4`.
fn given(k: &Kernel, b: &[(f64, f64)]) -> HashMap<ResourceId, A> {
    let mut lanes: Vec<A> = b.iter().map(|&(lo, hi)| A::F(R { lo, hi, err: 0.0 })).collect();
    while lanes.len() < 4 {
        lanes.push(A::F(R::exact(0.0)));
    }
    HashMap::from([(k.input, A::P(lanes))])
}

/// The bound on the twin's error over `b` with `sites`: the walk's, the box cut where it's
/// refused or over `within`, to depth 3.
fn bound(
    k: &Kernel,
    b: &[(f64, f64)],
    sites: &HashMap<(FuncId, ValueId), f64>,
    within: f64,
    depth: u32,
) -> Result<f64, String> {
    let walked = walk_kernel(&k.m, k.entry, given(k, b), sites);
    match &walked {
        Ok((Some(A::P(w)), _)) => {
            if let Some(A::F(r)) = w.first()
                && (r.err <= within || depth == DEPTH)
            {
                return Ok(r.err);
            }
        }
        Ok(_) => return Err("the kernel wrote no float".into()),
        Err(e) if depth == DEPTH => return Err(e.to_string()),
        Err(_) => {}
    }
    let n = b.len();
    let mut worst: f64 = 0.0;
    for piece in 0..1u32 << n {
        let half: Vec<(f64, f64)> = b
            .iter()
            .enumerate()
            .map(|(i, &(lo, hi))| {
                let mid = 0.5 * (lo + hi);
                if piece >> i & 1 == 0 { (lo, mid) } else { (mid, hi) }
            })
            .collect();
        worst = worst.max(bound(k, &half, sites, within, depth + 1)?);
    }
    Ok(worst)
}

/// Each value's place in the source: the last `at` before its `let`.
fn places(
    stmts: &[Stmt],
    at: &mut Option<wrela_diag::Span>,
    out: &mut HashMap<ValueId, wrela_diag::Span>,
) {
    for s in stmts {
        match s {
            Stmt::At(sp) => *at = Some(*sp),
            Stmt::Let(v, _) => {
                if let Some(sp) = at {
                    out.insert(*v, *sp);
                }
            }
            Stmt::If { then, else_, .. } => {
                places(then, at, out);
                places(else_, at, out);
            }
            Stmt::Loop { body, continuing } => {
                places(body, at, out);
                places(continuing, at, out);
            }
            _ => {}
        }
    }
}

fn lets(stmts: &[Stmt], out: &mut Vec<(ValueId, Expr)>) {
    for s in stmts {
        match s {
            Stmt::Let(v, e) => out.push((*v, e.clone())),
            Stmt::If { then, else_, .. } => {
                lets(then, out);
                lets(else_, out);
            }
            Stmt::Loop { body, continuing } => {
                lets(body, out);
                lets(continuing, out);
            }
            _ => {}
        }
    }
}

/// The values an expression reads.
fn reads(e: &Expr) -> Vec<ValueId> {
    match e {
        Expr::Unary(_, a) | Expr::Extract(a, _) | Expr::Splat(a, _) | Expr::Swizzle(a, _) => {
            vec![*a]
        }
        Expr::Convert(a, _) | Expr::Bitcast(a, _) => vec![*a],
        Expr::Binary(_, a, b) | Expr::ExtractDyn(a, b) => vec![*a, *b],
        Expr::Builtin(_, xs) | Expr::Construct(_, xs) => xs.clone(),
        Expr::Select { cond, if_true, if_false } => vec![*cond, *if_true, *if_false],
        _ => Vec::new(),
    }
}

/// An expression's own work (§18.11).
fn own_work(m: &Module, f: &wrela_ir::Function, e: &Expr) -> u64 {
    let lanes = |v: ValueId| match m.types.get(f.value_ty(v)) {
        wrela_ir::TypeDef::Vector(_, n) => u64::from(*n),
        _ => 1,
    };
    match e {
        Expr::Builtin(b, xs) => {
            wrela_ir::work::builtin_cost(*b, xs.first().map_or(1, |&x| lanes(x)))
        }
        Expr::Binary(BinOp::Div | BinOp::Rem, a, _) => 4 * lanes(*a),
        Expr::Binary(_, a, _) | Expr::Unary(_, a) => lanes(*a),
        _ => 0,
    }
}

/// The work dropping `t` saves: its own and that of each value only it (and what only it reads)
/// reads.
fn cone_work(
    m: &Module,
    f: &wrela_ir::Function,
    defs: &HashMap<ValueId, Expr>,
    uses: &HashMap<ValueId, u32>,
    t: ValueId,
) -> u64 {
    let mut total = 0;
    let mut stack = vec![t];
    let mut seen = std::collections::HashSet::new();
    while let Some(v) = stack.pop() {
        if !seen.insert(v) {
            continue;
        }
        let Some(e) = defs.get(&v) else { continue };
        total += own_work(m, f, e);
        for r in reads(e) {
            if uses.get(&r).copied().unwrap_or(0) <= 1 {
                stack.push(r);
            }
        }
    }
    total
}

/// The size of a value's range over the box (its largest magnitude).
fn size(seen: &Seen, f: FuncId, v: ValueId) -> Option<f64> {
    match &seen.values.get(&(f, v))?.0 {
        A::F(r) => Some(r.size()),
        _ => None,
    }
}

/// Each expression of the function's body, by its span: where the IR's places lead.
fn exprs(e: &wrela_syntax::ast::Expr, out: &mut HashMap<(u32, u32), wrela_syntax::ast::Expr>) {
    use wrela_syntax::ast::ExprKind as K;
    out.insert((e.span.start, e.span.end), e.clone());
    let block = |b: &wrela_syntax::ast::Block,
                 out: &mut HashMap<(u32, u32), wrela_syntax::ast::Expr>| {
        let mut all = Vec::new();
        b.for_each_expr(&mut |x| all.push(x.clone()));
        for x in &all {
            exprs(x, out);
        }
    };
    match &e.kind {
        K::Unary(_, a) | K::Paren(a) | K::Take(a) | K::MutArg(a) => exprs(a, out),
        K::Binary(_, a, b) => {
            exprs(a, out);
            exprs(b, out);
        }
        K::Call { callee, args, .. } => {
            exprs(callee, out);
            args.iter().for_each(|a| exprs(&a.value, out));
        }
        K::MethodCall { receiver, args, .. } => {
            exprs(receiver, out);
            args.iter().for_each(|a| exprs(&a.value, out));
        }
        K::Field { base, .. } => exprs(base, out),
        K::Index { base, index } => {
            exprs(base, out);
            exprs(index, out);
        }
        K::Tuple(xs) | K::Array(xs) => xs.iter().for_each(|x| exprs(x, out)),
        K::Block(b) => block(b, out),
        K::If { cond, then, else_, .. } => {
            exprs(cond, out);
            block(then, out);
            if let Some(e) = else_ {
                exprs(e, out);
            }
        }
        _ => {}
    }
}

/// An operand's text, in parentheses unless it's one token.
fn operand(text: &str, e: &wrela_syntax::ast::Expr) -> String {
    let t = &text[e.span.start as usize..e.span.end as usize];
    if t.contains(|c: char| c.is_whitespace() || "+-*/".contains(c))
        && !matches!(e.kind, wrela_syntax::ast::ExprKind::Paren(_))
    {
        format!("({t})")
    } else {
        t.to_string()
    }
}

/// Every rewrite this stage knows that the kernel's function offers, with its edit in the
/// function's text, found through the syntax tree at each value's place.
fn candidates(k: &Kernel, seen: &Seen, fn_span: (u32, u32)) -> Vec<Rewrite> {
    use wrela_syntax::ast::{BinOp as SB, ExprKind as K, LitKind};
    let f = &k.m.functions[k.entry.index()];
    let mut spans = HashMap::new();
    places(&f.body, &mut None, &mut spans);
    let mut all = Vec::new();
    lets(&f.body, &mut all);
    let defs: HashMap<ValueId, Expr> = all.iter().cloned().collect();
    let mut uses: HashMap<ValueId, u32> = HashMap::new();
    for (_, e) in &all {
        for r in reads(e) {
            *uses.entry(r).or_default() += 1;
        }
    }
    // The function's expressions, by span.
    let parsed = wrela_syntax::parse(wrela_diag::FileId(0), &k.text);
    let mut ast = HashMap::new();
    for it in &parsed.file.items {
        if let wrela_syntax::ast::ItemKind::Fn(fd) = &it.kind
            && it.span.start == fn_span.0
            && let Some(body) = &fd.body
        {
            let mut tops = Vec::new();
            body.for_each_expr(&mut |x| tops.push(x.clone()));
            for x in &tops {
                exprs(x, &mut ast);
            }
        }
    }
    let node = |v: ValueId| -> Option<&wrela_syntax::ast::Expr> {
        let sp = spans.get(&v)?;
        (sp.file == k.file).then(|| ast.get(&(sp.start, sp.end))).flatten()
    };
    let is_float = |v: ValueId| matches!(seen.values.get(&(k.entry, v)), Some((A::F(_), _)));
    let text = |e: &wrela_syntax::ast::Expr| &k.text[e.span.start as usize..e.span.end as usize];
    let mut out = Vec::new();
    for (v, e) in &all {
        let Some(n) = node(*v) else { continue };
        let span = (n.span.start, n.span.end);
        match (e, &n.kind) {
            (
                Expr::Binary(op @ (BinOp::Add | BinOp::Sub), a, b),
                K::Binary(sop @ (SB::Add | SB::Sub), l, r),
            ) if is_float(*v) && ((*op == BinOp::Add) == (*sop == SB::Add)) => {
                let mut drop =
                    |t: ValueId, keep: &wrela_syntax::ast::Expr, gone: &wrela_syntax::ast::Expr| {
                        let Some(d) = size(seen, k.entry, t) else { return };
                        out.push(Rewrite {
                            value: *v,
                            span,
                            with: operand(&k.text, keep),
                            delta: d,
                            saves: cone_work(&k.m, f, &defs, &uses, t) + 1,
                            what: format!("drops `{}` (at most {})", text(gone), short(d)),
                        });
                    };
                drop(*b, l, r);
                if *op == BinOp::Add {
                    drop(*a, r, l);
                }
            }
            (Expr::Builtin(Builtin::Pow, xs), K::Call { args, .. })
                if xs.len() == 2 && args.len() == 2 =>
            {
                let c = match &args[1].value.kind {
                    K::Lit(l) => match l.kind {
                        LitKind::Float(x) => x,
                        _ => continue,
                    },
                    _ => continue,
                };
                if ![2.0, 3.0, 4.0].contains(&c) {
                    continue;
                }
                let Some(r) = size(seen, k.entry, *v) else { continue };
                let base = operand(&k.text, &args[0].value);
                let product = vec![base; c as usize].join(" * ");
                out.push(Rewrite {
                    value: *v,
                    span,
                    with: format!("({product})"),
                    delta: 4.0 * ULP * r,
                    saves: wrela_ir::work::builtin_cost(Builtin::Pow, 1)
                        .saturating_sub(c as u64 - 1),
                    what: format!("`{}` as a product", text(n)),
                });
            }
            (Expr::Binary(BinOp::Div, _, c), K::Binary(SB::Div, l, r)) if is_float(*v) => {
                let Some(Expr::Const(Const::F32(cv))) = defs.get(c) else { continue };
                if *cv == 0.0
                    || !matches!(&r.kind, K::Lit(l) if matches!(l.kind, LitKind::Float(_)))
                {
                    continue;
                }
                let Some(sz) = size(seen, k.entry, *v) else { continue };
                out.push(Rewrite {
                    value: *v,
                    span,
                    with: format!("{} * (1.0 / {})", operand(&k.text, l), text(r)),
                    delta: ULP * sz,
                    saves: 3,
                    what: format!("`{}` by a reciprocal", text(n)),
                });
            }
            _ => {}
        }
    }
    out.sort_by(|a, b| b.saves.cmp(&a.saves).then(a.span.cmp(&b.span)));
    out
}

fn short(x: f64) -> String {
    format!("{x:.3e}").replace("e0", "")
}

/// The function's item in its file: the whole item's byte range, and its name's.
fn item(text: &str, name: &str) -> Option<((u32, u32), (u32, u32))> {
    let parsed = wrela_syntax::parse(wrela_diag::FileId(0), text);
    parsed.file.items.iter().find_map(|it| match &it.kind {
        wrela_syntax::ast::ItemKind::Fn(f) if f.name.name == name => {
            Some(((it.span.start, it.span.end), (f.name.span.start, f.name.span.end)))
        }
        _ => None,
    })
}

/// An item's range widened back over the doc comment above it.
fn with_docs(text: &str, start: u32) -> u32 {
    let mut s = start as usize;
    loop {
        let before = &text[..s];
        let line_start = before
            .trim_end_matches([' ', '\t'])
            .strip_suffix('\n')
            .map(|b| b.rfind('\n').map_or(0, |i| i + 1));
        match line_start {
            Some(ls) if text[ls..].trim_start().starts_with("///") => s = ls,
            _ => return s as u32,
        }
    }
}

/// What a twin's certificate says: its box, its tolerance, the walk's bound, and the work
/// before and after.
struct Proof<'b> {
    within: f64,
    proven: f64,
    work: (u64, u64),
    b: &'b [(f64, f64)],
}

/// The twin's text: the original's with the chosen edits, renamed, under its certificate.
fn twin_text(
    a: &Args,
    k: &Kernel,
    chosen: &[Rewrite],
    spans: ((u32, u32), (u32, u32)),
    proof: &Proof<'_>,
    twin: &str,
) -> String {
    let (span, name_span) = spans;
    let Proof { within, proven, work, b } = *proof;
    let mut edits: Vec<(u32, u32, String)> =
        chosen.iter().map(|r| (r.span.0, r.span.1, r.with.clone())).collect();
    edits.push((name_span.0, name_span.1, twin.to_string()));
    edits.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut body = k.text[span.0 as usize..span.1 as usize].to_string();
    for (s, e, w) in edits {
        body.replace_range((s - span.0) as usize..(e - span.0) as usize, &w);
    }
    let ranges: Vec<String> = b.iter().map(|(lo, hi)| format!("{lo}..{hi}")).collect();
    let made: Vec<&str> = chosen.iter().map(|r| r.what.as_str()).collect();
    let made = if made.is_empty() {
        "no rewrite (none kept the bound)".to_string()
    } else {
        made.join("; ")
    };
    format!(
        "/// `{f}`'s twin, proven within {within} of it over {box_} (`wrela twin`; the walk's bound: {proven}):\n\
         /// {made}.\n\
         /// Work {after}, from {before}. Check with `wrela twin <package> {m}::{f} --check`.\n\
         /// wrela twin: --box {box_} --within {within}\n{body}",
        f = a.function,
        m = a.module,
        box_ = ranges.join(","),
        proven = short(proven),
        before = work.0,
        after = work.1,
    )
}

fn answer(a: &Args) -> Result<(String, bool), String> {
    let path = a.dir.join(format!("{}.wrela", a.module.replace("::", "/")));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let twin = a.name.clone().unwrap_or_else(|| format!("{}_twin", a.function));
    // `--check`: the box and tolerance from the twin's certificate.
    let (b, within) = if a.check {
        let (tspan, _) = item(&text, &twin).ok_or(format!("no `{twin}` in {}", path.display()))?;
        let doc = &text[with_docs(&text, tspan.0) as usize..tspan.0 as usize];
        let line = doc
            .lines()
            .find_map(|l| l.trim().strip_prefix("/// wrela twin: "))
            .ok_or("the twin's comment doesn't say its box and tolerance")?;
        let mut it = line.split_whitespace();
        let (mut bx, mut w) = (None, None);
        while let Some(x) = it.next() {
            match x {
                "--box" => bx = it.next().and_then(parse_box),
                "--within" => w = it.next().and_then(|v| v.parse().ok()),
                _ => {}
            }
        }
        (bx.ok_or("the certificate's box")?, w.ok_or("the certificate's tolerance")?)
    } else {
        (a.boxes.clone().expect("a box"), a.within.expect("a tolerance"))
    };
    let k = kernel(a, b.len(), &path)?;
    let (span, name_span) =
        item(&k.text, &a.function).ok_or(format!("no `{}` in {}", a.function, path.display()))?;
    let none = HashMap::new();
    let seen = match walk_kernel(&k.m, k.entry, given(&k, &b), &none) {
        Ok((_, seen)) => seen,
        Err(e) => return Err(format!("the walk refuses `{}` over the box: {e}", a.function)),
    };
    let cands = candidates(&k, &seen, span);
    let before = wrela_ir::work::entry_work(&k.m, k.entry).work.unwrap_or(0);
    let mut sites: HashMap<(FuncId, ValueId), f64> = HashMap::new();
    let mut chosen: Vec<Rewrite> = Vec::new();
    let mut proven = 0.0;
    for c in cands {
        // One rewrite a value, and none inside another's text.
        let overlaps = chosen
            .iter()
            .any(|r| r.value == c.value || (c.span.0 < r.span.1 && r.span.0 < c.span.1));
        if overlaps {
            continue;
        }
        let mut trial = sites.clone();
        *trial.entry((k.entry, c.value)).or_default() += c.delta;
        if let Ok(e) = bound(&k, &b, &trial, within, 0)
            && e <= within
        {
            sites = trial;
            proven = e;
            chosen.push(c);
        }
    }
    let saved: u64 = chosen.iter().map(|r| r.saves).sum();
    let after = before.saturating_sub(saved);
    let twin_src = twin_text(
        a,
        &k,
        &chosen,
        (span, name_span),
        &Proof { within, proven, work: (before, after), b: &b },
        &twin,
    );
    let normal = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    if a.check {
        let (tspan, _) = item(&text, &twin).expect("found above");
        let have = &text[with_docs(&text, tspan.0) as usize..tspan.1 as usize];
        let same = normal(have) == normal(&twin_src);
        let msg = if same {
            format!("`{twin}` is current\n")
        } else {
            format!("`{twin}` differs from the twin of `{}` now:\n{twin_src}\n", a.function)
        };
        return Ok((msg, same));
    }
    if a.write {
        let mut new = text.clone();
        match item(&text, &twin) {
            Some((t, _)) => {
                new.replace_range(with_docs(&text, t.0) as usize..t.1 as usize, &twin_src);
            }
            None => new.insert_str(span.1 as usize, &format!("\n\n{twin_src}")),
        }
        std::fs::write(&path, new).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if a.json {
        let j = serde_json::json!({
            "function": a.function, "twin": twin, "within": within, "proven": proven,
            "work": {"before": before, "after": after},
            "rewrites": chosen.iter().map(|r| serde_json::json!({"what": r.what, "delta": r.delta, "saves": r.saves})).collect::<Vec<_>>(),
            "source": twin_src, "written": a.write,
        });
        return Ok((serde_json::to_string_pretty(&j).expect("JSON") + "\n", true));
    }
    Ok((twin_src + "\n", true))
}
