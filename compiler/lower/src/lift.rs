//! Lifted builds (language.md §22): each `f32` literal of the chosen packages is read from a
//! table rather than built into the code, so a tool can change it while the program runs.
//!
//! - **Which literals.** Every operand the MIR has for an `f32` literal written in a lifted
//!   package's files, wherever it's used: a constant whose value is literal is built where it's
//!   used, from its own literals (so every use of `EAR_H` reads one entry), and so is a
//!   parameter's or a field's default. A literal only a computed constant reads isn't lifted:
//!   the build runs that code once (§10).
//! - **The CPU** reads entry `i` from the table in memory: `Load(table[i])`. A change writes the
//!   entry and counts a generation.
//! - **The GPU** reads it from a storage buffer that every pipeline of a lifted build binds
//!   last. The program keeps that buffer: before a dispatch or draw, a helper makes it the first
//!   time, and writes the table to it when the generation has changed since it last did. So an
//!   edit reaches the GPU with the next dispatch or draw, as a buffer write.

use crate::body::Fl;
use crate::{Cx, ModuleBuilder};
use std::collections::HashMap;
use wrela_diag::FileId;
use wrela_ir as ir;
use wrela_sema::Checked;
use wrela_sema::mir::{self, OperandKind, Rvalue, StatementKind, TerminatorKind};
use wrela_sema::thir::Lit;
use wrela_sema::ty::{FloatTy, TyKind};

/// One lifted literal.
#[derive(Clone, Debug, PartialEq)]
pub struct LiftedLiteral {
    /// Into [`LiftTable::files`].
    pub file: u32,
    pub start: u32,
    pub end: u32,
    /// From 1.
    pub line: u32,
    pub column: u32,
    /// The value its source gives it.
    pub value: f32,
    /// A negation is applied to it where it's used (`-0.5`): its sign is the expression's.
    pub negated: bool,
}

/// A file holding lifted literals.
#[derive(Clone, Debug, PartialEq)]
pub struct LiftedFile {
    pub id: FileId,
    /// As diagnostics name it.
    pub name: String,
    /// On disk, relative to the program's package.
    pub path: String,
    pub text: String,
    /// FNV-1a 64 of its bytes, 16 hex digits.
    pub hash: String,
}

/// A lifted build's literals, in index order (by file, then place).
#[derive(Clone, Debug, Default)]
pub struct LiftTable {
    pub literals: Vec<LiftedLiteral>,
    pub files: Vec<LiftedFile>,
    /// By the literal's place: its index.
    pub index: HashMap<(FileId, u32, u32), u32>,
}

impl LiftTable {
    /// The index of the literal written at `span`, if it's lifted.
    pub fn of(&self, span: wrela_diag::Span) -> Option<u32> {
        self.index.get(&(span.file, span.start, span.end)).copied()
    }
}

/// Every `f32` literal operand in the program's MIR written in a file `lifted` says yes to: its
/// place, its value, and whether it's negated where it's used. Constants the build computes
/// and tests aren't looked in.
pub fn candidates(
    checked: &Checked,
    lifted: &dyn Fn(FileId) -> bool,
) -> Vec<(FileId, u32, u32, f32, bool)> {
    let p = &checked.program;
    let mut found: HashMap<(FileId, u32, u32), (f32, bool)> = HashMap::new();
    for (&f, body) in &checked.mir {
        let def = p.func(f);
        if matches!(def.owner, wrela_sema::defs::FnOwner::Const(_)) || def.attrs.test.is_some() {
            continue;
        }
        let mut see = |o: &mir::Operand, negated: bool| {
            let OperandKind::Const(l) = &o.kind else { return };
            if !matches!(p.types.kind(o.ty), TyKind::Float(FloatTy::F32)) || !lifted(o.span.file) {
                return;
            }
            let Some(v) = lit_f32(l) else { return };
            let e = found.entry((o.span.file, o.span.start, o.span.end)).or_insert((v, negated));
            e.1 |= negated;
        };
        for code in &body.fns {
            for b in &code.blocks {
                for s in &b.stmts {
                    if let StatementKind::Assign(_, r) | StatementKind::Eval(r) = &s.kind {
                        // A negation's operand is negated where it's used.
                        let negated = matches!(r, Rvalue::Unary(wrela_syntax::ast::UnOp::Neg, _));
                        r.for_each_operand(&mut |o| see(o, negated));
                    }
                }
                match &b.term.kind {
                    TerminatorKind::If { cond, .. } => see(cond, false),
                    TerminatorKind::Return(Some(o)) => see(o, false),
                    _ => {}
                }
            }
        }
    }
    // Constants whose values are literal are also read in place (a constant passed by
    // reference), where no operand names their literals: their own values' literals.
    for (&c, (_, e)) in &checked.consts {
        if crate::eval::is_computed(checked, c) || !lifted(e.span.file) {
            continue;
        }
        const_literals(p, e, &mut |span, v| {
            found.entry((span.file, span.start, span.end)).or_insert((v, false));
        });
    }
    let mut out: Vec<_> = found.into_iter().map(|((f, s, e), (v, n))| (f, s, e, v, n)).collect();
    out.sort_by_key(|x| (x.0, x.1, x.2));
    out
}

/// The `f32` literals of a literal value: each one's place and value (a negative one's place
/// includes its minus, as the build folds it).
fn const_literals(
    p: &wrela_sema::program::Program,
    e: &wrela_sema::thir::Expr,
    see: &mut impl FnMut(wrela_diag::Span, f32),
) {
    use wrela_sema::thir::ExprKind;
    let f32_ty = p.types.f32;
    match &e.kind {
        ExprKind::Lit(l) if e.ty == f32_ty => {
            if let Some(v) = lit_f32(l) {
                see(e.span, v);
            }
        }
        ExprKind::Unary(wrela_syntax::ast::UnOp::Neg, x) if e.ty == f32_ty => {
            if let ExprKind::Lit(l) = &x.kind
                && let Some(v) = lit_f32(l)
            {
                see(e.span, -v);
            }
        }
        _ => e.for_each_child(&mut |c| {
            if let wrela_sema::thir::Child::Expr(x) = c {
                const_literals(p, x, see);
            }
        }),
    }
}

/// An `f32` literal's value: a float's, or an integer's (`1` is an `f32` where one is wanted).
fn lit_f32(l: &Lit) -> Option<f32> {
    match *l {
        Lit::Float(_, f) => Some(f),
        Lit::Int(i) => Some(i as f32),
        Lit::Bool(_) => None,
    }
}

/// The CPU module's table and the state its GPU copy keeps.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CpuTable {
    /// `[f32; N]`, written by `set`.
    pub table: ir::DataId,
    /// `[f32; N]`: the values the source gives.
    pub built: ir::DataId,
    /// `{ handle, generation, uploaded }`: the GPU copy's buffer (0 before it's made), how
    /// many changes the table has had, and how many the buffer has.
    pub state: ir::DataId,
    /// `[Source; N]` and `[Text; 4 × files]`, for tools: made when code first reads them, with
    /// the types the program has for `Source` and `Text`.
    pub sources: Option<ir::DataId>,
    pub files: Option<ir::DataId>,
    /// The helper that gives the GPU copy's handle, uploading it first if it's behind.
    pub buffer_fn: ir::FuncId,
}

/// The number of literals.
pub(crate) fn len(cx: &Cx) -> u32 {
    cx.lift.map_or(0, |t| t.literals.len() as u32)
}

impl Cx<'_> {
    /// Makes the CPU module's table, its state and the upload helper (a lifted build with
    /// literals).
    pub(crate) fn make_table(&mut self, mb: &mut ModuleBuilder) {
        let Some(lift) = self.lift else { return };
        let n = lift.literals.len() as u32;
        let f32_t = mb.m.types.f32();
        let u = mb.m.types.u32();
        let arr = mb.m.types.intern(ir::TypeDef::Array(f32_t, n));
        let values: Vec<ir::ConstValue> =
            lift.literals.iter().map(|l| ir::ConstValue::Scalar(ir::Const::F32(l.value))).collect();
        let table = mb.m.add_data(ir::Data {
            name: "literals".into(),
            ty: arr,
            value: ir::ConstValue::Parts(values.clone()),
        });
        let built = mb.m.add_data(ir::Data {
            name: "built_literals".into(),
            ty: arr,
            value: ir::ConstValue::Parts(values),
        });
        let state_t = mb.m.types.intern(ir::TypeDef::Struct {
            name: "LiteralBuffer".into(),
            fields: vec![("handle".into(), u), ("generation".into(), u), ("uploaded".into(), u)],
        });
        let scalar = |v: u32| ir::ConstValue::Scalar(ir::Const::U32(v));
        let state = mb.m.add_data(ir::Data {
            name: "literal_buffer".into(),
            ty: state_t,
            value: ir::ConstValue::Parts(vec![scalar(0), scalar(1), scalar(0)]),
        });
        mb.m.writable.extend([table, state]);
        let buffer_fn = buffer_helper(mb, table, state, n);
        let set = set_export(mb, table, state, n);
        mb.m.exports.push((wrela_abi::EXPORT_LIFT_SET.into(), set));
        mb.lift = Some(CpuTable { table, built, state, buffer_fn, sources: None, files: None });
    }
}

/// `literal_buffer() -> u32`: the GPU copy's handle, made the first time and written whenever
/// the table has changed since it last was.
fn buffer_helper(
    mb: &mut ModuleBuilder,
    table: ir::DataId,
    state: ir::DataId,
    n: u32,
) -> ir::FuncId {
    let u = mb.m.types.u32();
    let b = mb.m.types.bool();
    let f32_t = mb.m.types.f32();
    let run_t = mb.m.types.intern(ir::TypeDef::Run(f32_t));
    let mut f = ir::Function::new("literal_buffer", Vec::new(), Some(u));
    let field =
        |k: u32| ir::Place { root: ir::PlaceRoot::Data(state), path: vec![ir::Proj::Field(k)] };
    let mut body = Vec::new();
    let handle = f.let_(&mut body, u, ir::Expr::Load(field(0)));
    let zero = f.let_(&mut body, u, ir::Expr::Const(ir::Const::U32(0)));
    let none = f.let_(&mut body, b, ir::Expr::Binary(ir::BinOp::Eq, handle, zero));
    let mut make = Vec::new();
    let count = f.let_(&mut make, u, ir::Expr::Const(ir::Const::U32(n)));
    let made = f.let_(
        &mut make,
        u,
        ir::Expr::Host(ir::HostOp::CreateBuffer { elem_size: 4 }, vec![count]),
    );
    make.push(ir::Stmt::Store(field(0), made));
    body.push(ir::Stmt::If { cond: none, then: make, else_: Vec::new() });
    let generation = f.let_(&mut body, u, ir::Expr::Load(field(1)));
    let uploaded = f.let_(&mut body, u, ir::Expr::Load(field(2)));
    let behind = f.let_(&mut body, b, ir::Expr::Binary(ir::BinOp::Ne, generation, uploaded));
    let mut write = Vec::new();
    let h = f.let_(&mut write, u, ir::Expr::Load(field(0)));
    let at = f.let_(&mut write, u, ir::Expr::Const(ir::Const::U32(0)));
    let run = f.let_(&mut write, run_t, ir::Expr::Run(ir::Place::root(ir::PlaceRoot::Data(table))));
    write.push(ir::Stmt::Eval(ir::Expr::Host(
        ir::HostOp::WriteBuffer { elem: f32_t, elem_size: 4 },
        vec![h, at, run],
    )));
    write.push(ir::Stmt::Store(field(2), generation));
    body.push(ir::Stmt::If { cond: behind, then: write, else_: Vec::new() });
    let out = f.let_(&mut body, u, ir::Expr::Load(field(0)));
    body.push(ir::Stmt::Return(Some(out)));
    f.body = body;
    mb.m.add_function(f)
}

/// `__lift_set(i, value)`, exported: what a host calls to change literal `i` (a tool's edit,
/// hot reload), as `std::lift::set` does. An index past the last changes nothing.
fn set_export(mb: &mut ModuleBuilder, table: ir::DataId, state: ir::DataId, n: u32) -> ir::FuncId {
    let u = mb.m.types.u32();
    let b = mb.m.types.bool();
    let f32_t = mb.m.types.f32();
    let params = vec![
        ir::Param { name: "i".into(), ty: u, by_ref: false, mutable: false },
        ir::Param { name: "value".into(), ty: f32_t, by_ref: false, mutable: false },
    ];
    let mut f = ir::Function::new("lift_set", params, None);
    let mut body = Vec::new();
    let i = f.let_(&mut body, u, ir::Expr::Param(0));
    let v = f.let_(&mut body, f32_t, ir::Expr::Param(1));
    let count = f.let_(&mut body, u, ir::Expr::Const(ir::Const::U32(n)));
    let inside = f.let_(&mut body, b, ir::Expr::Binary(ir::BinOp::Lt, i, count));
    let mut set = Vec::new();
    set.push(ir::Stmt::Store(
        ir::Place { root: ir::PlaceRoot::Data(table), path: vec![ir::Proj::Index(i)] },
        v,
    ));
    let counter = ir::Place { root: ir::PlaceRoot::Data(state), path: vec![ir::Proj::Field(1)] };
    let g = f.let_(&mut set, u, ir::Expr::Load(counter.clone()));
    let one = f.let_(&mut set, u, ir::Expr::Const(ir::Const::U32(1)));
    let next = f.let_(&mut set, u, ir::Expr::Binary(ir::BinOp::WrappingAdd, g, one));
    set.push(ir::Stmt::Store(counter, next));
    body.push(ir::Stmt::If { cond: inside, then: set, else_: Vec::new() });
    body.push(ir::Stmt::Return(None));
    f.body = body;
    mb.m.add_function(f)
}

/// `std::lift`'s table intrinsics (§22), on the CPU.
pub(crate) fn intrinsic(
    fl: &mut Fl,
    lang: wrela_sema::defs::Lang,
    c: &mir::Call,
    ty: Option<wrela_sema::ty::TyId>,
) -> Option<ir::ValueId> {
    use wrela_sema::defs::Lang;
    let span = c.span;
    let f32_t = fl.mb.m.types.f32();
    let u = fl.mb.m.types.u32();
    let at =
        |root: ir::PlaceRoot, i: ir::ValueId| ir::Place { root, path: vec![ir::Proj::Index(i)] };
    let table = fl.mb.lift;
    match lang {
        Lang::LiftCount => Some(fl.u32c(len(fl.cx))),
        Lang::LiftFiles => {
            let n = fl.cx.lift.map_or(0, |t| t.files.len() as u32);
            Some(fl.u32c(n))
        }
        Lang::LiftValue | Lang::LiftBuiltValue => {
            let i = fl.arg_value(&c.args[0])?;
            match table {
                Some(t) => {
                    let d = if lang == Lang::LiftValue { t.table } else { t.built };
                    Some(fl.value(f32_t, ir::Expr::Load(at(ir::PlaceRoot::Data(d), i))))
                }
                None => Some(fl.value(f32_t, ir::Expr::Const(ir::Const::F32(0.0)))),
            }
        }
        Lang::LiftSet => {
            let i = fl.arg_value(&c.args[0])?;
            let v = fl.arg_value(&c.args[1])?;
            if let Some(t) = table {
                fl.emit(ir::Stmt::Store(at(ir::PlaceRoot::Data(t.table), i), v));
                // One more change for the GPU's copy to catch up with.
                let counter = ir::Place {
                    root: ir::PlaceRoot::Data(t.state),
                    path: vec![ir::Proj::Field(1)],
                };
                let g = fl.value(u, ir::Expr::Load(counter.clone()));
                let one = fl.u32c(1);
                let next = fl.value(u, ir::Expr::Binary(ir::BinOp::WrappingAdd, g, one));
                fl.emit(ir::Stmt::Store(counter, next));
            }
            None
        }
        Lang::LiftGeneration => match table {
            // The state's counter starts at 1, so the first upload happens.
            Some(t) => {
                let counter =
                    ir::Place { root: ir::PlaceRoot::Data(t.state), path: vec![ir::Proj::Field(1)] };
                let g = fl.value(u, ir::Expr::Load(counter));
                let one = fl.u32c(1);
                Some(fl.value(u, ir::Expr::Binary(ir::BinOp::WrappingSub, g, one)))
            }
            None => Some(fl.u32c(0)),
        },
        Lang::LiftSource => {
            let i = fl.arg_value(&c.args[0])?;
            let st = fl.ty(ty?, span)?;
            match fl.sources_data(st) {
                Some(d) => Some(fl.value(st, ir::Expr::Load(at(ir::PlaceRoot::Data(d), i)))),
                None => Some(fl.value(st, ir::Expr::Zero(st))),
            }
        }
        Lang::LiftFile => {
            let i = fl.arg_value(&c.args[0])?;
            let what = fl.arg_value(&c.args[1])?;
            let tt = fl.ty(ty?, span)?;
            match fl.files_data(tt) {
                Some(d) => {
                    let four = fl.u32c(4);
                    let base = fl.value(u, ir::Expr::Binary(ir::BinOp::Mul, i, four));
                    let k = fl.value(u, ir::Expr::Binary(ir::BinOp::Add, base, what));
                    Some(fl.value(tt, ir::Expr::Load(at(ir::PlaceRoot::Data(d), k))))
                }
                None => {
                    fl.emit(ir::Stmt::Trap);
                    Some(fl.value(tt, ir::Expr::Zero(tt)))
                }
            }
        }
        _ => None,
    }
}

impl Fl<'_, '_> {
    /// The data holding each literal's `Source` (of IR type `source_t`).
    pub(crate) fn sources_data(&mut self, source_t: ir::TypeId) -> Option<ir::DataId> {
        let t = self.mb.lift?;
        if let Some(d) = t.sources {
            return Some(d);
        }
        let lift = self.cx.lift?;
        let scalar = |v: u32| ir::ConstValue::Scalar(ir::Const::U32(v));
        let parts = lift
            .literals
            .iter()
            .map(|l| {
                ir::ConstValue::Parts(
                    [l.file, l.start, l.end, l.line, l.column].map(scalar).to_vec(),
                )
            })
            .collect();
        let ty = self.mb.m.types.intern(ir::TypeDef::Array(source_t, lift.literals.len() as u32));
        let d = self.mb.m.add_data(ir::Data {
            name: "literal_sources".into(),
            ty,
            value: ir::ConstValue::Parts(parts),
        });
        if let Some(t) = self.mb.lift.as_mut() {
            t.sources = Some(d);
        }
        Some(d)
    }

    /// The data holding each lifted file's name, path, text and hash, four `Text`s (of IR type
    /// `text_t`) a file.
    pub(crate) fn files_data(&mut self, text_t: ir::TypeId) -> Option<ir::DataId> {
        let t = self.mb.lift?;
        if let Some(d) = t.files {
            return Some(d);
        }
        let lift = self.cx.lift?;
        let scalar = |v: u32| ir::ConstValue::Scalar(ir::Const::U32(v));
        let mut texts = Vec::new();
        for f in &lift.files {
            for s in [&f.name, &f.path, &f.text, &f.hash] {
                let (d, len) = self.cx.text_data(self.mb, s);
                texts.push(ir::ConstValue::Parts(vec![ir::ConstValue::Addr(d), scalar(len)]));
            }
        }
        let ty = self.mb.m.types.intern(ir::TypeDef::Array(text_t, texts.len() as u32));
        let d = self.mb.m.add_data(ir::Data {
            name: "lifted_files".into(),
            ty,
            value: ir::ConstValue::Parts(texts),
        });
        if let Some(t) = self.mb.lift.as_mut() {
            t.files = Some(d);
        }
        Some(d)
    }

    /// Literal `i`'s value: the table's entry, in memory on the CPU, from the storage buffer on
    /// the GPU.
    pub(crate) fn lifted(&mut self, i: u32) -> Option<ir::ValueId> {
        let f32_t = self.mb.m.types.f32();
        let idx = self.u32c(i);
        let root = if let Some(g) = self.mb.gpu.as_ref() {
            ir::PlaceRoot::Resource(g.literals?)
        } else {
            ir::PlaceRoot::Data(self.mb.lift?.table)
        };
        Some(
            self.value(f32_t, ir::Expr::Load(ir::Place { root, path: vec![ir::Proj::Index(idx)] })),
        )
    }

    /// Brings the GPU's copy of the table up to date, if this is a lifted build: before a
    /// pass, which holds only draws.
    pub(crate) fn upload_literals(&mut self) {
        if let Some(t) = self.mb.lift {
            let u = self.mb.m.types.u32();
            let _ = self.value(u, ir::Expr::Call(t.buffer_fn, Vec::new()));
        }
    }

    /// The literal table's binding for a dispatch or a draw: its handle, 0, and its size. A
    /// dispatch brings the GPU's copy up to date first (`upload`); a draw, inside a pass, uses
    /// the copy its pass began with.
    pub(crate) fn literal_binding(&mut self, upload: bool) -> Option<[ir::ValueId; 3]> {
        let t = self.mb.lift?;
        let u = self.mb.m.types.u32();
        let handle = if upload {
            self.value(u, ir::Expr::Call(t.buffer_fn, Vec::new()))
        } else {
            let at =
                ir::Place { root: ir::PlaceRoot::Data(t.state), path: vec![ir::Proj::Field(0)] };
            self.value(u, ir::Expr::Load(at))
        };
        let zero = self.u32c(0);
        let size = self.u32c(4 * len(self.cx));
        Some([handle, zero, size])
    }
}
