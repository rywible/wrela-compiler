//! Lowers the EBNF [`Grammar`] to a plain context-free grammar: productions over terminals and
//! nonterminals, with no `?`, `*`, `+` or nested alternatives.
//!
//! Each EBNF rule becomes a nonterminal with the same index. Each `?`, `*`, `+` and each group of
//! alternatives inside a sequence becomes a *synthetic* nonterminal, owned by its rule. Tools
//! that build trees splice a synthetic node's children into its parent, so a parse tree has one
//! node per EBNF rule and a flat child list in source order (`add_expr` over `a + b - c` has the
//! children `mul_expr "+" mul_expr "-" mul_expr`).
//!
//! The lowering is unambiguous: each EBNF derivation corresponds to exactly one derivation of
//! the lowered grammar, so counting derivations here counts them in the EBNF grammar.

use crate::ebnf::{Expr, ExprKind, Grammar, RuleId, Terminal};

/// How `x*` and `x+` recurse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recursion {
    /// `S ::= ε | S x`: the natural choice for an Earley parser (each repetition completes once).
    Left,
    /// `S ::= ε | x S`: needed for top-down engines such as llama.cpp's GBNF matcher, which
    /// can't handle left recursion.
    Right,
}

pub type NtId = usize;
pub type ProdId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sym {
    T(Terminal),
    N(NtId),
}

#[derive(Clone, Debug)]
pub struct Nt {
    /// The rule's name, or `rule#k` for the k-th synthetic nonterminal of a rule.
    pub name: String,
    /// The EBNF rule this nonterminal is, or belongs to.
    pub rule: RuleId,
    pub synthetic: bool,
    pub prods: Vec<ProdId>,
}

#[derive(Clone, Debug)]
pub struct Prod {
    pub lhs: NtId,
    pub rhs: Vec<Sym>,
}

#[derive(Clone, Debug)]
pub struct Cfg {
    pub nts: Vec<Nt>,
    pub prods: Vec<Prod>,
    pub start: NtId,
}

impl Cfg {
    pub fn lower(g: &Grammar, rec: Recursion) -> Cfg {
        let mut cfg = Cfg {
            nts: g
                .rules
                .iter()
                .enumerate()
                .map(|(i, r)| Nt {
                    name: r.name.clone(),
                    rule: i,
                    synthetic: false,
                    prods: Vec::new(),
                })
                .collect(),
            prods: Vec::new(),
            start: g.start,
        };
        let mut lw = Lowerer { cfg: &mut cfg, rec, counters: vec![0; g.rules.len()] };
        for (i, r) in g.rules.iter().enumerate() {
            for rhs in lw.alternatives(&r.body, i) {
                lw.add_prod(i, rhs);
            }
        }
        cfg
    }

    /// Whether each nonterminal derives the empty sequence.
    pub fn nullable(&self) -> Vec<bool> {
        let mut nullable = vec![false; self.nts.len()];
        loop {
            let mut changed = false;
            for p in &self.prods {
                if !nullable[p.lhs] && p.rhs.iter().all(|s| matches!(s, Sym::N(n) if nullable[*n]))
                {
                    nullable[p.lhs] = true;
                    changed = true;
                }
            }
            if !changed {
                return nullable;
            }
        }
    }
}

struct Lowerer<'a> {
    cfg: &'a mut Cfg,
    rec: Recursion,
    counters: Vec<u32>,
}

impl Lowerer<'_> {
    fn add_prod(&mut self, lhs: NtId, rhs: Vec<Sym>) {
        let id = self.cfg.prods.len();
        self.cfg.prods.push(Prod { lhs, rhs });
        self.cfg.nts[lhs].prods.push(id);
    }

    fn synthetic(&mut self, rule: RuleId) -> NtId {
        self.counters[rule] += 1;
        let name = format!("{}#{}", self.cfg.nts[rule].name, self.counters[rule]);
        self.cfg.nts.push(Nt { name, rule, synthetic: true, prods: Vec::new() });
        self.cfg.nts.len() - 1
    }

    /// The right-hand sides `e` stands for, as alternatives.
    fn alternatives(&mut self, e: &Expr, rule: RuleId) -> Vec<Vec<Sym>> {
        match &e.kind {
            ExprKind::Alt(xs) => xs.iter().map(|x| self.sequence(x, rule)).collect(),
            _ => vec![self.sequence(e, rule)],
        }
    }

    fn sequence(&mut self, e: &Expr, rule: RuleId) -> Vec<Sym> {
        let mut out = Vec::new();
        self.append(e, rule, &mut out);
        out
    }

    fn append(&mut self, e: &Expr, rule: RuleId, out: &mut Vec<Sym>) {
        match &e.kind {
            ExprKind::Term(t) => out.push(Sym::T(*t)),
            ExprKind::Rule(r) => out.push(Sym::N(*r)),
            ExprKind::Seq(xs) => xs.iter().for_each(|x| self.append(x, rule, out)),
            ExprKind::Alt(_) => {
                let n = self.synthetic(rule);
                for rhs in self.alternatives(e, rule) {
                    self.add_prod(n, rhs);
                }
                out.push(Sym::N(n));
            }
            ExprKind::Opt(x) => {
                let n = self.synthetic(rule);
                self.add_prod(n, Vec::new());
                for rhs in self.alternatives(x, rule) {
                    self.add_prod(n, rhs);
                }
                out.push(Sym::N(n));
            }
            ExprKind::Star(x) | ExprKind::Plus(x) => {
                let n = self.synthetic(rule);
                let alts = self.alternatives(x, rule);
                if matches!(e.kind, ExprKind::Star(_)) {
                    self.add_prod(n, Vec::new());
                } else {
                    for rhs in &alts {
                        self.add_prod(n, rhs.clone());
                    }
                }
                for rhs in alts {
                    let mut full = Vec::with_capacity(rhs.len() + 1);
                    match self.rec {
                        Recursion::Left => {
                            full.push(Sym::N(n));
                            full.extend(rhs);
                        }
                        Recursion::Right => {
                            full.extend(rhs);
                            full.push(Sym::N(n));
                        }
                    }
                    self.add_prod(n, full);
                }
                out.push(Sym::N(n));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_repetition_both_ways() {
        let g = Grammar::parse("file ::= (IDENT | INT)+ \",\"? EOF\n").unwrap();
        for rec in [Recursion::Left, Recursion::Right] {
            let cfg = Cfg::lower(&g, rec);
            assert_eq!(cfg.prods[cfg.nts[0].prods[0]].rhs.len(), 3);
            assert!(cfg.nts.iter().skip(1).all(|n| n.synthetic && n.rule == 0));
            let nullable = cfg.nullable();
            assert!(!nullable[0]);
            assert_eq!(nullable.iter().filter(|n| **n).count(), 1, "only `\",\"?` is nullable");
        }
    }
}
