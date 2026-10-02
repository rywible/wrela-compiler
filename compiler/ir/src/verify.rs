//! Checks a module's structural invariants. A failure is a bug in the compiler (lowering or a
//! transform), never in the user's program.

use crate::*;

/// Checks every function: values are defined once and used only where their definition is
/// visible (earlier in the same block or an enclosing one); `break` and `continue` are inside
/// loops; locals, parameters, resources and callees exist.
pub fn verify(m: &Module) -> Result<(), String> {
    for (i, f) in m.functions.iter().enumerate() {
        let mut v = Verifier {
            m,
            f,
            defined: vec![false; f.values.len()],
            scopes: vec![Vec::new()],
            loops: 0,
        };
        v.block(&f.body).map_err(|e| format!("fn{i} {}: {e}", f.name))?;
    }
    for e in &m.entry_points {
        if e.function.index() >= m.functions.len() {
            return Err(format!("entry {} names a missing function", e.name));
        }
    }
    Ok(())
}

struct Verifier<'a> {
    m: &'a Module,
    f: &'a Function,
    defined: Vec<bool>,
    scopes: Vec<Vec<ValueId>>,
    loops: u32,
}

impl Verifier<'_> {
    fn visible(&self, v: ValueId) -> Result<(), String> {
        if v.index() >= self.f.values.len() {
            return Err(format!("v{} doesn't exist", v.0));
        }
        if !self.scopes.iter().any(|s| s.contains(&v)) {
            return Err(format!("v{} is used where its definition isn't visible", v.0));
        }
        Ok(())
    }

    fn place(&self, p: &Place) -> Result<(), String> {
        match &p.root {
            PlaceRoot::Local(l) if l.index() >= self.f.locals.len() => {
                return Err(format!("l{} doesn't exist", l.0));
            }
            PlaceRoot::Param(i) if !self.f.params.get(*i as usize).is_some_and(|p| p.by_ref) => {
                return Err(format!("p{i} isn't a by-reference parameter"));
            }
            PlaceRoot::Resource(r) if r.index() >= self.m.resources.len() => {
                return Err(format!("r{} doesn't exist", r.0));
            }
            PlaceRoot::Ptr(v) => self.visible(*v)?,
            _ => {}
        }
        for proj in &p.path {
            if let Proj::Index(v) = proj {
                self.visible(*v)?;
            }
        }
        Ok(())
    }

    fn expr(&self, e: &Expr) -> Result<(), String> {
        let vs: Vec<ValueId> = match e {
            Expr::Const(_) | Expr::Zero(_) | Expr::EntryInput(_) => Vec::new(),
            Expr::Param(i) => {
                if !self.f.params.get(*i as usize).is_some_and(|p| !p.by_ref) {
                    return Err(format!("param {i} isn't a by-value parameter"));
                }
                Vec::new()
            }
            Expr::Load(p) | Expr::Run(p) | Expr::Addr(p) | Expr::ArrayLength(p) => {
                self.place(p)?;
                Vec::new()
            }
            Expr::Unary(_, v)
            | Expr::Extract(v, _)
            | Expr::Splat(v, _)
            | Expr::Swizzle(v, _)
            | Expr::Convert(v, _)
            | Expr::Bitcast(v, _) => vec![*v],
            Expr::Binary(_, a, b) | Expr::ExtractDyn(a, b) => vec![*a, *b],
            Expr::Call(f, args) => {
                if f.index() >= self.m.functions.len() {
                    return Err(format!("call to missing fn{}", f.0));
                }
                let callee = &self.m.functions[f.index()];
                if callee.params.len() != args.len() {
                    return Err(format!(
                        "call to {} with {} arguments, not {}",
                        callee.name,
                        args.len(),
                        callee.params.len()
                    ));
                }
                let mut vs = Vec::new();
                for (a, p) in args.iter().zip(&callee.params) {
                    match (a, p.by_ref) {
                        (Arg::Value(v), false) => vs.push(*v),
                        (Arg::Place(pl), true) => self.place(pl)?,
                        _ => {
                            return Err(format!(
                                "argument {} of {} passed the wrong way",
                                p.name, callee.name
                            ));
                        }
                    }
                }
                vs
            }
            Expr::Builtin(_, args) | Expr::Construct(_, args) | Expr::Host(_, args) => args.clone(),
            Expr::Select { cond, if_true, if_false } => vec![*cond, *if_true, *if_false],
        };
        for v in vs {
            self.visible(v)?;
        }
        Ok(())
    }

    fn block(&mut self, b: &Block) -> Result<(), String> {
        self.scopes.push(Vec::new());
        for s in b {
            match s {
                Stmt::Let(v, e) => {
                    self.expr(e)?;
                    if self.defined[v.index()] {
                        return Err(format!("v{} is defined twice", v.0));
                    }
                    self.defined[v.index()] = true;
                    if let Some(scope) = self.scopes.last_mut() {
                        scope.push(*v);
                    }
                }
                Stmt::Eval(e) => self.expr(e)?,
                Stmt::Store(p, v) => {
                    self.place(p)?;
                    self.visible(*v)?;
                }
                Stmt::If { cond, then, else_ } => {
                    self.visible(*cond)?;
                    self.block(then)?;
                    self.block(else_)?;
                }
                Stmt::Loop { body, continuing } => {
                    self.loops += 1;
                    self.block(body)?;
                    self.loops -= 1;
                    // `continuing` sees the body's values in WGSL only if they're defined at
                    // the body's top level; keep it simple: it gets its own scope.
                    let saved = self.loops;
                    self.loops = 0;
                    self.block(continuing)?;
                    self.loops = saved;
                }
                Stmt::Break | Stmt::Continue if self.loops == 0 => {
                    return Err("break or continue outside a loop".into());
                }
                Stmt::Break | Stmt::Continue | Stmt::Trap => {}
                Stmt::Return(v) => {
                    if let Some(v) = v {
                        self.visible(*v)?;
                    }
                }
            }
        }
        self.scopes.pop();
        Ok(())
    }
}
