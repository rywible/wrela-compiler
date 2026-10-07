//! A deterministic text form of a module, for golden tests and debugging.

use crate::*;
use std::fmt::Write;

pub fn print(m: &Module) -> String {
    let mut s = String::new();
    for (t, d) in m.types.iter() {
        let i = t.0;
        if let TypeDef::Struct { name, fields } = d {
            let fs: Vec<String> =
                fields.iter().map(|(n, t)| format!("{n}: {}", m.types.display(*t))).collect();
            let _ = writeln!(s, "type %{i} = struct {name} {{ {} }}", fs.join(", "));
        }
        if let TypeDef::Enum { name, variants } = d {
            let vs: Vec<String> = variants
                .iter()
                .map(|(n, p)| match p {
                    Some(t) => format!("{n}({})", m.types.display(*t)),
                    None => n.clone(),
                })
                .collect();
            let _ = writeln!(s, "type %{i} = enum {name} {{ {} }}", vs.join(", "));
        }
    }
    for r in &m.resources {
        let _ = writeln!(
            s,
            "resource {} @binding({}) {:?}: {}",
            r.name,
            r.binding,
            r.kind,
            m.types.display(r.ty)
        );
    }
    for (i, d) in m.data.iter().enumerate() {
        let _ = writeln!(s, "data d{i} {}: {} = {:?}", d.name, m.types.display(d.ty), d.value);
    }
    for e in &m.entry_points {
        let _ = writeln!(
            s,
            "entry {} {:?} = fn{} inputs {:?}",
            e.name, e.stage, e.function.0, e.inputs
        );
    }
    for (name, f) in &m.exports {
        let _ = writeln!(s, "export {name} = fn{}", f.0);
    }
    for (i, f) in m.functions.iter().enumerate() {
        let _ = writeln!(s);
        print_fn(m, i, f, &mut s);
    }
    s
}

fn print_fn(m: &Module, i: usize, f: &Function, s: &mut String) {
    let ps: Vec<String> = f
        .params
        .iter()
        .map(|p| {
            format!(
                "{}{}: {}",
                if p.by_ref { if p.mutable { "&mut " } else { "&" } } else { "" },
                p.name,
                m.types.display(p.ty)
            )
        })
        .collect();
    let ret = match f.ret {
        Some(t) => format!(" -> {}{}", if f.ret_ref { "&" } else { "" }, m.types.display(t)),
        None => String::new(),
    };
    let _ = writeln!(s, "fn{i} {}({}){ret} {{", f.name, ps.join(", "));
    for (j, l) in f.locals.iter().enumerate() {
        let _ = writeln!(s, "    local l{j} {}: {}", l.name, m.types.display(l.ty));
    }
    block(m, f, &f.body, 1, s);
    let _ = writeln!(s, "}}");
}

fn place(p: &Place) -> String {
    let mut out = match &p.root {
        PlaceRoot::Local(l) => format!("l{}", l.0),
        PlaceRoot::Param(i) => format!("p{i}"),
        PlaceRoot::Resource(r) => format!("r{}", r.0),
        PlaceRoot::Ptr(v) => format!("*v{}", v.0),
        PlaceRoot::Data(d) => format!("d{}", d.0),
    };
    for proj in &p.path {
        match proj {
            Proj::Field(i) => {
                let _ = write!(out, ".{i}");
            }
            Proj::Comp(c) => {
                let _ = write!(out, ".c{c}");
            }
            Proj::Index(v) => {
                let _ = write!(out, "[v{}]", v.0);
            }
        }
    }
    out
}

fn vals(vs: &[ValueId]) -> String {
    vs.iter().map(|v| format!("v{}", v.0)).collect::<Vec<_>>().join(", ")
}

fn expr(m: &Module, e: &Expr) -> String {
    match e {
        Expr::Const(c) => format!("{c:?}"),
        Expr::Zero(t) => format!("zero {}", m.types.display(*t)),
        Expr::Load(p) => format!("load {}", place(p)),
        Expr::ArrayLength(p) => format!("array_length {}", place(p)),
        Expr::Atomic(op, p, xs) => format!("atomic {op:?} {}, {}", place(p), vals(xs)),
        Expr::Barrier => "barrier".to_string(),
        Expr::Discard => "discard".to_string(),
        Expr::Texture(op, t, s, xs) => match s {
            Some(s) => format!("texture {op:?} r{}, r{}, {}", t.0, s.0, vals(xs)),
            None => format!("texture {op:?} r{}, {}", t.0, vals(xs)),
        },
        Expr::Unary(op, v) => format!("{op:?} v{}", v.0),
        Expr::Binary(op, a, b) => format!("{op:?} v{}, v{}", a.0, b.0),
        Expr::Call(f, args) => {
            let a: Vec<String> = args
                .iter()
                .map(|a| match a {
                    Arg::Value(v) => format!("v{}", v.0),
                    Arg::Place(p) => format!("&{}", place(p)),
                })
                .collect();
            format!("call fn{}({})", f.0, a.join(", "))
        }
        Expr::Builtin(b, args) => format!("{b:?}({})", vals(args)),
        Expr::Construct(t, args) => format!("{} {{ {} }}", m.types.display(*t), vals(args)),
        Expr::Variant(t, k, payload) => match payload {
            Some(p) => format!("{}#{k}(v{})", m.types.display(*t), p.0),
            None => format!("{}#{k}", m.types.display(*t)),
        },
        Expr::Extract(v, i) => format!("v{}.{i}", v.0),
        Expr::ExtractDyn(v, i) => format!("v{}[v{}]", v.0, i.0),
        Expr::Splat(v, n) => format!("splat{n} v{}", v.0),
        Expr::Swizzle(v, c) => format!("v{}.{c:?}", v.0),
        Expr::Convert(v, s) => format!("{}(v{})", s.name(), v.0),
        Expr::Bitcast(v, s) => format!("bitcast<{}>(v{})", s.name(), v.0),
        Expr::Select { cond, if_true, if_false } => {
            format!("select v{} ? v{} : v{}", cond.0, if_true.0, if_false.0)
        }
        Expr::Run(p) => format!("run {}", place(p)),
        Expr::Addr(p) => format!("&{}", place(p)),
        Expr::Host(op, args) => format!("host {op:?}({})", vals(args)),
        Expr::Mem(op, args) => format!("mem {op:?}({})", vals(args)),
        Expr::EntryInput(i) => format!("input {i}"),
        Expr::Param(i) => format!("param {i}"),
    }
}

fn block(m: &Module, f: &Function, b: &Block, depth: usize, s: &mut String) {
    let pad = "    ".repeat(depth);
    for st in b {
        match st {
            Stmt::Let(v, e) => {
                let _ = writeln!(
                    s,
                    "{pad}v{}: {} = {}",
                    v.0,
                    m.types.display(f.value_ty(*v)),
                    expr(m, e)
                );
            }
            Stmt::Eval(e) => {
                let _ = writeln!(s, "{pad}{}", expr(m, e));
            }
            Stmt::Store(p, v) => {
                let _ = writeln!(s, "{pad}{} = v{}", place(p), v.0);
            }
            Stmt::If { cond, then, else_ } => {
                let _ = writeln!(s, "{pad}if v{} {{", cond.0);
                block(m, f, then, depth + 1, s);
                if !else_.is_empty() {
                    let _ = writeln!(s, "{pad}}} else {{");
                    block(m, f, else_, depth + 1, s);
                }
                let _ = writeln!(s, "{pad}}}");
            }
            Stmt::At(span) => {
                let _ = writeln!(s, "{pad}at {}:{}..{}", span.file.0, span.start, span.end);
            }
            Stmt::Loop { body, continuing } => {
                let _ = writeln!(s, "{pad}loop {{");
                block(m, f, body, depth + 1, s);
                if !continuing.is_empty() {
                    let _ = writeln!(s, "{pad}}} continuing {{");
                    block(m, f, continuing, depth + 1, s);
                }
                let _ = writeln!(s, "{pad}}}");
            }
            Stmt::Break => {
                let _ = writeln!(s, "{pad}break");
            }
            Stmt::Continue => {
                let _ = writeln!(s, "{pad}continue");
            }
            Stmt::Return(v) => {
                let _ = writeln!(
                    s,
                    "{pad}return{}",
                    v.map(|v| format!(" v{}", v.0)).unwrap_or_default()
                );
            }
            Stmt::Trap => {
                let _ = writeln!(s, "{pad}trap");
            }
        }
    }
}
