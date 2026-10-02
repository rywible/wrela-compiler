//! MIR as text, for debugging and tests.

use super::*;
use std::fmt::Write;

fn place(p: &Place) -> String {
    let mut s = format!("_{}", p.local.0);
    for proj in &p.proj {
        match proj {
            Proj::Field(i) => {
                let _ = write!(s, ".{i}");
            }
            Proj::Downcast(v) => {
                let _ = write!(s, " as {v}");
            }
            Proj::Comp(c) => {
                let _ = write!(s, ".c{c}");
            }
            Proj::Swizzle(cs) => {
                let _ = write!(s, ".{cs:?}");
            }
            Proj::Index(l) => {
                let _ = write!(s, "[_{}]", l.0);
            }
        }
    }
    s
}

fn operand(o: &Operand) -> String {
    match &o.kind {
        OperandKind::Copy(p) => place(p),
        OperandKind::Move(p, k) => format!("move({:?}) {}", k, place(p)),
        OperandKind::Const(l) => format!("{l:?}"),
    }
}

fn arg(a: &Arg) -> String {
    match a {
        Arg::Borrow(p, _) => place(p),
        Arg::Mut(p, _) => format!("mut {}", place(p)),
        Arg::Take(o) => format!("take {}", operand(o)),
    }
}

fn rvalue(r: &Rvalue) -> String {
    let list = |xs: &[Operand]| xs.iter().map(operand).collect::<Vec<_>>().join(", ");
    match r {
        Rvalue::Use(o) => operand(o),
        Rvalue::Unary(op, o) => format!("{op:?} {}", operand(o)),
        Rvalue::Binary(op, a, b) => format!("{} {} {}", operand(a), op.text(), operand(b)),
        Rvalue::Adt { adt, variant, fields, .. } => {
            format!(
                "adt#{}{} {{ {} }}",
                adt.0,
                variant.map_or(String::new(), |v| format!("::{v}")),
                list(fields)
            )
        }
        Rvalue::Tuple(xs) => format!("({})", list(xs)),
        Rvalue::Array(xs) => format!("[{}]", list(xs)),
        Rvalue::ArrayRepeat(x, n) => format!("[{}; {n}]", operand(x)),
        Rvalue::Construct(xs) => format!("construct({})", list(xs)),
        Rvalue::Convert(x) => format!("convert({})", operand(x)),
        Rvalue::Discriminant(p) => format!("discriminant({})", place(p)),
        Rvalue::Len(p) => format!("len({})", place(p)),
        Rvalue::Call(c) => {
            let callee = match &c.callee {
                Callee::Fn { func, .. } => format!("fn#{}", func.0),
                Callee::TraitMethod { method, .. } => format!("method#{}", method.0),
                Callee::Builtin(b) => format!("{b:?}"),
                Callee::Local(l) => format!("_{}", l.0),
                Callee::Clone => "clone".into(),
            };
            let args = c.args.iter().map(arg).collect::<Vec<_>>().join(", ");
            format!("{callee}({args})")
        }
        Rvalue::Closure(c) => format!("closure#{}", c.0),
        Rvalue::FnRef(f, _) => format!("fnref#{}", f.0),
        Rvalue::Dispatch(_) => "dispatch(..)".into(),
        Rvalue::Draw(_) => "draw(..)".into(),
    }
}

/// One function's MIR as text.
pub fn print_fn(body: &Body, f: &FnBody) -> String {
    let mut s = String::new();
    for (i, l) in body.locals.iter().enumerate() {
        let _ = writeln!(s, "  _{i}: {} ({:?})", l.name, l.kind);
    }
    for (i, c) in body.closures.iter().enumerate() {
        let caps: Vec<String> = c
            .captures
            .iter()
            .map(|(l, w)| format!("_{}{}", l.0, if *w { " (written)" } else { "" }))
            .collect();
        let _ = writeln!(s, "  closure#{i} captures [{}]", caps.join(", "));
    }
    for (i, b) in f.blocks.iter().enumerate() {
        let _ = writeln!(s, "bb{i}:");
        for st in &b.stmts {
            let line = match &st.kind {
                StatementKind::Assign(p, r) => format!("{} = {}", place(p), rvalue(r)),
                StatementKind::Eval(r) => rvalue(r),
                StatementKind::Bind { local, place: p, mutable } => {
                    format!("_{} = {}{}", local.0, if *mutable { "&mut " } else { "&" }, place(p))
                }
                StatementKind::Live(l) => format!("live _{}", l.0),
                StatementKind::Dead(l) => format!("dead _{}", l.0),
            };
            let _ = writeln!(s, "    {line}");
        }
        let t = match &b.term.kind {
            TerminatorKind::Goto(t) => format!("goto bb{}", t.0),
            TerminatorKind::If { cond, then, else_, merge } => format!(
                "if {} then bb{} else bb{} merge bb{}",
                operand(cond),
                then.0,
                else_.0,
                merge.0
            ),
            TerminatorKind::Loop { body, continuing, merge } => {
                format!("loop bb{} continuing bb{} merge bb{}", body.0, continuing.0, merge.0)
            }
            TerminatorKind::LoopBack(l) => format!("loop back to bb{}", l.0),
            TerminatorKind::Break(l) => format!("break bb{}", l.0),
            TerminatorKind::Continue(l) => format!("continue bb{}", l.0),
            TerminatorKind::Match { arms, merge } => format!(
                "match [{}] merge bb{}",
                arms.iter().map(|a| format!("bb{}", a.0)).collect::<Vec<_>>().join(", "),
                merge.0
            ),
            TerminatorKind::ArmMatched(m) => format!("arm matched (bb{})", m.0),
            TerminatorKind::ArmFailed { match_, next } => {
                format!("arm failed (bb{}) to bb{}", match_.0, next.0)
            }
            TerminatorKind::Return(o) => {
                format!("return {}", o.as_ref().map_or(String::new(), operand))
            }
            TerminatorKind::ReturnPlace(p) => format!("return place {}", place(p)),
            TerminatorKind::Unreachable => "unreachable".into(),
        };
        let _ = writeln!(s, "    {t}");
    }
    s
}
