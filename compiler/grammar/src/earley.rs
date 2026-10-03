//! The oracle: an Earley parser for spec/grammar.ebnf over the token stream of
//! `wrela_syntax::lex`, the same lexer the compiler uses.
//!
//! It answers three questions about a token sequence: does the grammar accept it; if so, is the
//! derivation unique; and what is the parse tree. Ambiguity is a grammar bug, so an ambiguous
//! input is reported with the innermost place where two derivations differ.
//!
//! **GT_CLOSE (L16).** The input isn't a plain sequence but a small lattice. A `>` token can be
//! read as `">"` or as GT_CLOSE. A `>>`, `>=` or `>>=` token can be read whole, or as GT_CLOSE
//! followed by the rest of its text as the next token: `>>` is GT_CLOSE then `>` (which can be
//! GT_CLOSE again), `>=` is GT_CLOSE then `=`, and `>>=` is GT_CLOSE then `>=` (which can split
//! again). So each such token adds intermediate lattice nodes between its start and its end, and
//! the parser follows every edge.
//!
//! **Algorithm.** Earley's recognizer with the Aycock–Horspool treatment of nullable
//! nonterminals (predicting a nullable nonterminal also steps over it, so completions never have
//! to look at the current set). Derivations are then counted over the recognizer's item sets,
//! saturating at 2 ("more than one"), and the unique tree is read off the same way.

use crate::cfg::{Cfg, Recursion, Sym};
use crate::ebnf::{Grammar, RuleId, Terminal};
use crate::rng::{FxHashMap, FxHashSet};
use std::collections::hash_map::Entry;
use wrela_diag::{FileId, Span};
use wrela_syntax::{Token, TokenKind};

const TERM_BIT: u32 = 1 << 31;
/// The terminal id of a token kind the grammar never mentions (such as `?`): no production can
/// consume it.
const NO_TERM: u32 = TERM_BIT | 0x7fff_ffff;
const IN_PROGRESS: u8 = u8::MAX;

/// A node of the parse tree: one per EBNF rule application, children in source order.
#[derive(Clone, Debug)]
pub struct Node {
    pub rule: RuleId,
    pub children: Vec<Child>,
}

#[derive(Clone, Debug)]
pub enum Child {
    Node(Node),
    Leaf(Leaf),
}

/// A terminal the parse consumed, with the bytes it covers. A GT_CLOSE split off a `>>` token
/// covers just its `>`; NEWLINE and EOF cover nothing.
#[derive(Clone, Copy, Debug)]
pub struct Leaf {
    pub term: Terminal,
    pub span: Span,
}

/// Where the grammar has two derivations for the same tokens.
#[derive(Clone, Debug)]
pub struct Ambiguity {
    /// The innermost EBNF rule whose derivation isn't unique.
    pub rule: String,
    /// The bytes it covers.
    pub span: (u32, u32),
    pub how: &'static str,
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Accepted(Node),
    Ambiguous(Ambiguity),
    /// Rejected; `at` is the byte offset of the first token the grammar can't consume, and
    /// `expected` what it could have consumed there.
    Rejected {
        at: u32,
        expected: Vec<Terminal>,
    },
}

impl Outcome {
    pub fn is_accepted(&self) -> bool {
        matches!(self, Outcome::Accepted(_))
    }
}

pub struct Oracle {
    cfg: Cfg,
    nullable: Vec<bool>,
    /// Production right-hand sides, encoded: a terminal id has `TERM_BIT` set, otherwise the
    /// value is a nonterminal id.
    rhs: Vec<Vec<u32>>,
    lhs: Vec<u32>,
    /// Each token kind's terminal id (an index into `terms`, with `TERM_BIT` set), by
    /// `kind as usize`; `NO_TERM` for kinds the grammar never mentions.
    term_of_kind: Vec<u32>,
    gt_close: u32,
    terms: Vec<Terminal>,
}

/// Reusable buffers, so parsing many small inputs doesn't allocate per input.
#[derive(Default)]
pub struct Scratch {
    sets: Vec<EarleySet>,
    predicted: Vec<u64>,
    memo: FxHashMap<(u32, u32, u32, u32), u8>,
    lattice: Lattice,
}

#[derive(Default)]
struct EarleySet {
    items: Vec<u64>,
    seen: FxHashSet<u64>,
    /// Items whose next symbol is a nonterminal, keyed by it (sorted once the set is done).
    waiting: Vec<(u32, u64)>,
    /// Completed items: (nonterminal, origin, production), sorted once the set is done.
    completed: Vec<(u32, u32, u32)>,
}

impl EarleySet {
    fn clear(&mut self) {
        self.items.clear();
        self.seen.clear();
        self.waiting.clear();
        self.completed.clear();
    }

    fn add(&mut self, item: u64) {
        if self.seen.insert(item) {
            self.items.push(item);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    from: u32,
    to: u32,
    term: u32,
    span: Span,
}

#[derive(Default)]
struct Lattice {
    edges: Vec<Edge>,
    /// Edge indices sorted by `from`, with `out_start[u]..out_start[u + 1]` leaving node `u`.
    out_start: Vec<u32>,
    by_to: Vec<u32>,
    in_start: Vec<u32>,
    /// The byte offset of each node.
    pos: Vec<u32>,
}

impl Lattice {
    fn nodes(&self) -> usize {
        self.pos.len()
    }

    fn out(&self, u: u32) -> &[Edge] {
        &self.edges[self.out_start[u as usize] as usize..self.out_start[u as usize + 1] as usize]
    }

    fn into(&self, v: u32) -> impl Iterator<Item = &Edge> {
        self.by_to[self.in_start[v as usize] as usize..self.in_start[v as usize + 1] as usize]
            .iter()
            .map(|&i| &self.edges[i as usize])
    }
}

fn pack(prod: u32, dot: u32, origin: u32) -> u64 {
    (u64::from(origin) << 32) | (u64::from(prod) << 8) | u64::from(dot)
}

fn unpack(item: u64) -> (u32, u32, u32) {
    (((item >> 8) & 0xff_ffff) as u32, (item & 0xff) as u32, (item >> 32) as u32)
}

fn sat_add(a: u8, b: u8) -> u8 {
    (a + b).min(2)
}

fn sat_mul(a: u8, b: u8) -> u8 {
    (a * b).min(2)
}

impl Oracle {
    pub fn new(g: &Grammar) -> Oracle {
        let cfg = Cfg::lower(g, Recursion::Left);
        let nullable = cfg.nullable();
        let terms = g.terminals();
        let term_id =
            |t: Terminal| TERM_BIT | terms.iter().position(|x| *x == t).unwrap_or(0) as u32;
        let rhs: Vec<Vec<u32>> = cfg
            .prods
            .iter()
            .map(|p| {
                p.rhs
                    .iter()
                    .map(|s| match s {
                        Sym::T(t) => term_id(*t),
                        Sym::N(n) => *n as u32,
                    })
                    .collect()
            })
            .collect();
        assert!(cfg.prods.len() < 1 << 24, "too many productions for the item encoding");
        assert!(rhs.iter().all(|r| r.len() < 256), "a production too long for the item encoding");
        let lhs = cfg.prods.iter().map(|p| p.lhs as u32).collect();
        let kinds = TokenKind::CLASSES.len() + TokenKind::KEYWORDS.len() + TokenKind::PUNCT.len();
        let mut term_of_kind = vec![NO_TERM; kinds];
        for t in &terms {
            if let Terminal::Token(k) = t {
                term_of_kind[*k as usize] = term_id(*t);
            }
        }
        let gt_close =
            if terms.contains(&Terminal::GtClose) { term_id(Terminal::GtClose) } else { NO_TERM };
        Oracle { cfg, nullable, rhs, lhs, term_of_kind, gt_close, terms }
    }

    /// Lexes `src` with the compiler's lexer and parses the tokens.
    pub fn parse_source(&self, src: &str) -> Outcome {
        let lexed = wrela_syntax::lex(FileId(0), src);
        self.parse(&lexed.tokens, &mut Scratch::default())
    }

    /// Parses a token stream as the lexer produced it (NEWLINEs inserted, EOF last).
    pub fn parse(&self, tokens: &[Token], s: &mut Scratch) -> Outcome {
        self.build_lattice(tokens, &mut s.lattice);
        let nodes = s.lattice.nodes();
        if s.sets.len() < nodes {
            s.sets.resize_with(nodes, EarleySet::default);
        }
        for set in &mut s.sets[..nodes] {
            set.clear();
        }
        let words = self.cfg.nts.len().div_ceil(64);
        s.predicted.resize(words, 0);

        for &p in &self.cfg.nts[self.cfg.start].prods {
            s.sets[0].add(pack(p as u32, 0, 0));
        }
        let mut furthest = 0u32;
        for u in 0..nodes as u32 {
            s.predicted.iter_mut().for_each(|w| *w = 0);
            if !s.sets[u as usize].items.is_empty() {
                furthest = u;
            }
            self.process_set(u, s);
        }

        let last = nodes as u32 - 1;
        let start = self.cfg.start as u32;
        let roots: Vec<u32> = s.sets[last as usize]
            .completed
            .iter()
            .filter(|&&(nt, origin, _)| nt == start && origin == 0)
            .map(|&(_, _, p)| p)
            .collect();
        if roots.is_empty() {
            let at = s.lattice.pos[furthest as usize];
            let mut expected: Vec<Terminal> = s.sets[furthest as usize]
                .items
                .iter()
                .filter_map(|&it| {
                    let (p, d, _) = unpack(it);
                    let sym = *self.rhs[p as usize].get(d as usize)?;
                    (sym & TERM_BIT != 0 && sym != NO_TERM)
                        .then(|| self.terms[(sym & !TERM_BIT) as usize])
                })
                .collect();
            expected.sort();
            expected.dedup();
            return Outcome::Rejected { at, expected };
        }
        s.memo.clear();
        let mut forest = Forest { o: self, sets: &s.sets, lattice: &s.lattice, memo: &mut s.memo };
        let mut total = 0;
        for &p in &roots {
            total = sat_add(total, forest.count_prod(p, 0, last));
        }
        match total {
            0 => unreachable!("a completed start item has no derivation"),
            1 => {
                let p = roots
                    .iter()
                    .copied()
                    .find(|&p| forest.count_prod(p, 0, last) == 1)
                    .unwrap_or(roots[0]);
                let children = forest.build_prod(p, 0, last);
                Outcome::Accepted(Node { rule: self.cfg.nts[self.cfg.start].rule, children })
            }
            _ => Outcome::Ambiguous(forest.explain_nt(start, 0, last)),
        }
    }

    fn process_set(&self, u: u32, s: &mut Scratch) {
        let (before, rest) = s.sets.split_at_mut(u as usize);
        let Some((current, after)) = rest.split_first_mut() else { unreachable!() };
        let lattice = &s.lattice;
        let mut i = 0;
        while i < current.items.len() {
            let item = current.items[i];
            i += 1;
            let (p, d, o) = unpack(item);
            let rhs = &self.rhs[p as usize];
            if let Some(&sym) = rhs.get(d as usize) {
                if sym & TERM_BIT != 0 {
                    // Scan: every lattice edge from here labelled with this terminal.
                    for e in lattice.out(u) {
                        if e.term == sym {
                            after[(e.to - u - 1) as usize].add(pack(p, d + 1, o));
                        }
                    }
                } else {
                    let (w, bit) = (sym as usize / 64, 1u64 << (sym % 64));
                    if s.predicted[w] & bit == 0 {
                        s.predicted[w] |= bit;
                        for &q in &self.cfg.nts[sym as usize].prods {
                            current.add(pack(q as u32, 0, u));
                        }
                    }
                    if self.nullable[sym as usize] {
                        current.add(pack(p, d + 1, o));
                    }
                    current.waiting.push((sym, item));
                }
            } else {
                let a = self.lhs[p as usize];
                current.completed.push((a, o, p));
                // Completions with origin `u` are covered by the nullable step above.
                if o < u {
                    let waiting = &before[o as usize].waiting;
                    let lo = waiting.partition_point(|w| w.0 < a);
                    for &(nt, w) in &waiting[lo..] {
                        if nt != a {
                            break;
                        }
                        let (wp, wd, wo) = unpack(w);
                        current.add(pack(wp, wd + 1, wo));
                    }
                }
            }
        }
        current.waiting.sort_unstable();
        current.completed.sort_unstable();
    }

    fn build_lattice(&self, tokens: &[Token], l: &mut Lattice) {
        l.edges.clear();
        l.pos.clear();
        let term = |k: TokenKind| self.term_of_kind[k as usize];
        let mut node = 0u32;
        for t in tokens {
            let (s, e) = (t.span.start, t.span.end);
            let sp = |a: u32, b: u32| Span::new(t.span.file, a, b);
            let mut edge = |from: u32, to: u32, term: u32, span: Span| {
                l.edges.push(Edge { from, to, term, span });
            };
            // Each `>` of the token text may begin a new node: n0 at the token, n1 after its
            // first `>`, n2 after its second.
            match t.kind {
                TokenKind::Gt => {
                    l.pos.push(s);
                    edge(node, node + 1, term(TokenKind::Gt), t.span);
                    edge(node, node + 1, self.gt_close, t.span);
                    node += 1;
                }
                TokenKind::Shr => {
                    l.pos.extend([s, s + 1]);
                    edge(node, node + 2, term(TokenKind::Shr), t.span);
                    edge(node, node + 1, self.gt_close, sp(s, s + 1));
                    edge(node + 1, node + 2, term(TokenKind::Gt), sp(s + 1, e));
                    edge(node + 1, node + 2, self.gt_close, sp(s + 1, e));
                    node += 2;
                }
                TokenKind::Ge => {
                    l.pos.extend([s, s + 1]);
                    edge(node, node + 2, term(TokenKind::Ge), t.span);
                    edge(node, node + 1, self.gt_close, sp(s, s + 1));
                    edge(node + 1, node + 2, term(TokenKind::Eq), sp(s + 1, e));
                    node += 2;
                }
                TokenKind::ShrEq => {
                    l.pos.extend([s, s + 1, s + 2]);
                    edge(node, node + 3, term(TokenKind::ShrEq), t.span);
                    edge(node, node + 1, self.gt_close, sp(s, s + 1));
                    edge(node + 1, node + 3, term(TokenKind::Ge), sp(s + 1, e));
                    edge(node + 1, node + 2, self.gt_close, sp(s + 1, s + 2));
                    edge(node + 2, node + 3, term(TokenKind::Eq), sp(s + 2, e));
                    node += 3;
                }
                k => {
                    l.pos.push(s);
                    edge(node, node + 1, term(k), t.span);
                    node += 1;
                }
            }
        }
        l.pos.push(tokens.last().map_or(0, |t| t.span.end));
        let n = l.pos.len();
        l.edges.sort_by_key(|e| (e.from, e.to));
        l.out_start.clear();
        l.out_start.resize(n + 1, 0);
        for e in &l.edges {
            l.out_start[e.from as usize + 1] += 1;
        }
        for i in 0..n {
            l.out_start[i + 1] += l.out_start[i];
        }
        l.by_to.clear();
        l.by_to.extend(0..l.edges.len() as u32);
        l.by_to.sort_by_key(|&i| l.edges[i as usize].to);
        l.in_start.clear();
        l.in_start.resize(n + 1, 0);
        for e in &l.edges {
            l.in_start[e.to as usize + 1] += 1;
        }
        for i in 0..n {
            l.in_start[i + 1] += l.in_start[i];
        }
    }
}

/// The derivations of an accepted input, read off the recognizer's sets.
struct Forest<'a> {
    o: &'a Oracle,
    sets: &'a [EarleySet],
    lattice: &'a Lattice,
    memo: &'a mut FxHashMap<(u32, u32, u32, u32), u8>,
}

/// One way the last symbol of an item's prefix can be derived: it spans `k..e`, and is a lattice
/// edge (a terminal) or a completed production `q` (a nonterminal).
#[derive(Clone, Copy)]
enum Step {
    Edge(Edge),
    Prod(u32),
}

/// The completed items of nonterminal `nt` in set `set`.
fn completed(sets: &[EarleySet], set: u32, nt: u32) -> &[(u32, u32, u32)] {
    let c = &sets[set as usize].completed;
    let lo = c.partition_point(|x| x.0 < nt);
    let hi = c.partition_point(|x| x.0 <= nt);
    &c[lo..hi]
}

impl<'a> Forest<'a> {
    /// The ways the prefix `rhs[..d]` of production `p`, started at `o`, splits at its last
    /// symbol when it ends at `e`: each is `(k, step)`, with `rhs[..d-1]` spanning `o..k`.
    /// The iterator borrows the sets and the lattice, not `self`, so the memo can be updated
    /// while it runs.
    fn steps(&self, p: u32, d: u32, o: u32, e: u32) -> impl Iterator<Item = (u32, Step)> + use<'a> {
        let (sets, lattice) = (self.sets, self.lattice);
        let sym = self.o.rhs[p as usize][d as usize - 1];
        // `rhs[..d-1]` must have been recognized from `o` to the split point.
        let prev = pack(p, d - 1, o);
        let recognized = move |k: u32| k >= o && sets[k as usize].seen.contains(&prev);
        let is_term = sym & TERM_BIT != 0;
        let edges = is_term.then(|| {
            lattice
                .into(e)
                .filter(move |edge| edge.term == sym && recognized(edge.from))
                .map(|edge| (edge.from, Step::Edge(*edge)))
        });
        let prods = (!is_term).then(|| {
            completed(sets, e, sym)
                .iter()
                .filter(move |&&(_, k, _)| recognized(k))
                .map(|&(_, k, q)| (k, Step::Prod(q)))
        });
        edges.into_iter().flatten().chain(prods.into_iter().flatten())
    }

    fn count_prod(&mut self, q: u32, k: u32, e: u32) -> u8 {
        let len = self.o.rhs[q as usize].len() as u32;
        self.count_item(q, len, k, e)
    }

    /// How many derivations (0, 1 or 2 meaning "more") `rhs[..d]` of `p` has over `o..e`.
    fn count_item(&mut self, p: u32, d: u32, o: u32, e: u32) -> u8 {
        if d == 0 {
            return u8::from(o == e);
        }
        let key = (p, d, o, e);
        match self.memo.entry(key) {
            // A derivation cycle: infinitely many derivations. The grammar reader rejects
            // grammars that allow this, so it's reported as ambiguity if it ever happens.
            Entry::Occupied(c) if *c.get() == IN_PROGRESS => return 2,
            Entry::Occupied(c) => return *c.get(),
            Entry::Vacant(v) => {
                v.insert(IN_PROGRESS);
            }
        }
        let mut total = 0;
        for (k, step) in self.steps(p, d, o, e) {
            let left = self.count_item(p, d - 1, o, k);
            if left == 0 {
                continue;
            }
            let right = match step {
                Step::Edge(_) => 1,
                Step::Prod(q) => self.count_prod(q, k, e),
            };
            total = sat_add(total, sat_mul(left, right));
        }
        self.memo.insert(key, total);
        total
    }

    /// The children of production `q` over `k..e`, whose derivation is unique.
    fn build_prod(&mut self, q: u32, k: u32, e: u32) -> Vec<Child> {
        let mut out = Vec::new();
        let len = self.o.rhs[q as usize].len() as u32;
        self.build_item(q, len, k, e, &mut out);
        out
    }

    fn build_item(&mut self, p: u32, d: u32, o: u32, e: u32, out: &mut Vec<Child>) {
        if d == 0 {
            return;
        }
        for (k, step) in self.steps(p, d, o, e) {
            if self.count_item(p, d - 1, o, k) == 0 {
                continue;
            }
            match step {
                Step::Edge(edge) => {
                    self.build_item(p, d - 1, o, k, out);
                    let term = self.o.terms[(edge.term & !TERM_BIT) as usize];
                    out.push(Child::Leaf(Leaf { term, span: edge.span }));
                }
                Step::Prod(q) => {
                    if self.count_prod(q, k, e) == 0 {
                        continue;
                    }
                    self.build_item(p, d - 1, o, k, out);
                    let children = self.build_prod(q, k, e);
                    let nt = &self.o.cfg.nts[self.o.lhs[q as usize] as usize];
                    if nt.synthetic {
                        out.extend(children);
                    } else {
                        out.push(Child::Node(Node { rule: nt.rule, children }));
                    }
                }
            }
            return;
        }
        unreachable!("build_item on a prefix with no derivation");
    }

    fn ambiguity(&self, nt: u32, k: u32, e: u32, how: &'static str) -> Ambiguity {
        let n = &self.o.cfg.nts[nt as usize];
        let rule = if n.synthetic {
            format!("{} (in {})", self.o.cfg.nts[n.rule].name, n.name)
        } else {
            n.name.clone()
        };
        Ambiguity { rule, span: (self.lattice.pos[k as usize], self.lattice.pos[e as usize]), how }
    }

    /// Finds the innermost place where nonterminal `nt` over `k..e` has two derivations.
    fn explain_nt(&mut self, nt: u32, k: u32, e: u32) -> Ambiguity {
        let prods: Vec<(u32, u8)> = completed(self.sets, e, nt)
            .iter()
            .filter(|&&(_, origin, _)| origin == k)
            .map(|&(_, _, q)| (q, self.count_prod(q, k, e)))
            .filter(|&(_, c)| c > 0)
            .collect();
        match prods.as_slice() {
            [(q, 2)] => self.explain_item(*q, self.o.rhs[*q as usize].len() as u32, k, e),
            [_] => unreachable!("explain_nt on an unambiguous derivation"),
            _ => self.ambiguity(nt, k, e, "two alternatives derive the same tokens"),
        }
    }

    fn explain_item(&mut self, p: u32, d: u32, o: u32, e: u32) -> Ambiguity {
        let mut live = Vec::new();
        for (k, step) in self.steps(p, d, o, e) {
            let left = self.count_item(p, d - 1, o, k);
            let right = match step {
                Step::Edge(_) => 1,
                Step::Prod(q) => self.count_prod(q, k, e),
            };
            if left > 0 && right > 0 {
                live.push((k, step, left, right));
            }
        }
        let lhs = self.o.lhs[p as usize];
        // Several steps at one split point are several productions of the same nonterminal over
        // the same tokens: the ambiguity is between that nonterminal's alternatives.
        if let [(k, Step::Prod(q), ..), rest @ ..] = live.as_slice()
            && !rest.is_empty()
            && rest.iter().all(|(other, ..)| other == k)
        {
            let nt = self.o.lhs[*q as usize];
            return self.explain_nt(nt, *k, e);
        }
        match live.as_slice() {
            [(k, step, left, right)] => {
                if *left > 1 {
                    self.explain_item(p, d - 1, o, *k)
                } else if let (Step::Prod(q), 2) = (step, right) {
                    let nt = self.o.lhs[*q as usize];
                    self.explain_nt(nt, *k, e)
                } else {
                    unreachable!("explain_item on an unambiguous derivation")
                }
            }
            _ => self.ambiguity(lhs, o, e, "the tokens split between its parts in two ways"),
        }
    }
}

impl Node {
    /// The bytes from the first to the last token the node covers, ignoring NEWLINE and EOF
    /// (which cover nothing). `None` if it covers no such token.
    pub fn span(&self) -> Option<(u32, u32)> {
        let first = self.children.iter().find_map(Child::span)?;
        let last = self.children.iter().rev().find_map(Child::span)?;
        Some((first.0, last.1))
    }

    /// Every node in the tree, parents first.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Node)) {
        f(self);
        for c in &self.children {
            if let Child::Node(n) = c {
                n.walk(f);
            }
        }
    }
}

impl Child {
    pub fn span(&self) -> Option<(u32, u32)> {
        match self {
            Child::Node(n) => n.span(),
            Child::Leaf(l) if l.span.start < l.span.end => Some((l.span.start, l.span.end)),
            Child::Leaf(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oracle(src: &str) -> Oracle {
        Oracle::new(&Grammar::parse(src).unwrap())
    }

    fn leaves(n: &Node, out: &mut Vec<String>) {
        for c in &n.children {
            match c {
                Child::Leaf(l) => out.push(l.term.name().to_string()),
                Child::Node(m) => leaves(m, out),
            }
        }
    }

    #[test]
    fn accepts_rejects_and_counts() {
        let o = oracle("file ::= sum EOF\nsum ::= INT (\"+\" INT)*\n");
        assert!(o.parse_source("1 + 2 + 3").is_accepted());
        match o.parse_source("1 + + 3") {
            Outcome::Rejected { at, expected } => {
                assert_eq!(at, 4);
                assert_eq!(expected, vec![Terminal::Token(TokenKind::Int)]);
            }
            other => panic!("{other:?}"),
        }
        // `?` is a token the grammar never mentions.
        assert!(!o.parse_source("1 ?").is_accepted());
    }

    #[test]
    fn detects_ambiguity() {
        let o = oracle("file ::= e EOF\ne ::= e \"+\" e | INT\n");
        assert!(o.parse_source("1 + 2").is_accepted());
        match o.parse_source("1 + 2 + 3") {
            Outcome::Ambiguous(a) => {
                assert_eq!(a.rule, "e");
                assert_eq!(a.span, (0, 9));
            }
            other => panic!("{other:?}"),
        }
        let o = oracle("file ::= a? IDENT? IDENT? EOF\na ::= IDENT\n");
        match o.parse_source("x") {
            Outcome::Ambiguous(a) => assert_eq!(a.span, (0, 1)),
            other => panic!("{other:?}"),
        }
        // Two alternatives of an inner rule derive the same tokens: that rule is named, not
        // the rule around it.
        let o = oracle("file ::= s EOF\ns ::= \"let\" a\na ::= IDENT | b\nb ::= IDENT\n");
        match o.parse_source("let x") {
            Outcome::Ambiguous(a) => {
                assert_eq!(a.rule, "a");
                assert_eq!(a.span, (4, 5));
                assert_eq!(a.how, "two alternatives derive the same tokens");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn splits_gt_tokens_as_l16_says() {
        let o = oracle(
            "file ::= ty (\"=\" IDENT | \">\" IDENT | \">=\" IDENT)? EOF\nty ::= IDENT (\"<\" ty GT_CLOSE)?\n",
        );
        for (src, ok) in [
            ("A<B>", true),
            ("A<B<C>>", true),
            ("A<B<C>> > x", true),
            ("A<B<C>>> x", true),
            ("A<B>= x", true),
            ("A<B<C>>= x", true),
            ("A<B<C>>>= x", true),
            ("A<B> >= x", true),
            ("A<B<C>", false),
            ("A<B>>>> x", false),
        ] {
            assert_eq!(o.parse_source(src).is_accepted(), ok, "{src}");
        }
        let Outcome::Accepted(tree) = o.parse_source("A<B<C>>= x") else { panic!() };
        let mut names = Vec::new();
        leaves(&tree, &mut names);
        assert_eq!(
            names,
            ["IDENT", "<", "IDENT", "<", "IDENT", "GT_CLOSE", "GT_CLOSE", "=", "IDENT", "EOF"]
        );
    }

    #[test]
    fn trees_flatten_synthetic_nodes() {
        let o = oracle("file ::= sum EOF\nsum ::= INT ((\"+\" | \"-\") INT)*\n");
        let Outcome::Accepted(tree) = o.parse_source("1 + 2 - 3") else { panic!() };
        let Child::Node(sum) = &tree.children[0] else { panic!() };
        assert_eq!(sum.children.len(), 5);
        assert_eq!(sum.span(), Some((0, 9)));
    }
}
