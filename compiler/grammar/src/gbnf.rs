//! Exports the grammar as GBNF (llama.cpp's grammar format) over **characters**, so a language
//! model can be constrained to write wrela that the compiler parses.
//!
//! spec/grammar.ebnf is written over tokens, and three things between tokens are
//! context-dependent at the character level. The export handles each by specializing every
//! nonterminal of the (right-recursive) lowered grammar on a small amount of context, the
//! classic intersection of a context-free grammar with a finite automaton, keeping only the
//! variants reachable from `file`:
//!
//! - **Brackets (L19).** A variant is specialized on its innermost open bracket: `b` for `{` or
//!   the top level, `p` for `(` or `[`. Brackets always open and close within one sequence of a
//!   rule (the export checks this), so each position's bracket is known statically. An f-string's
//!   hole (L22, the rule `hole`) is a context of its own, `h`, and a bracket inside one is `hn`:
//!   a hole is on one line, so nothing in it breaks a line, and at its top a `:` or `}` would
//!   end it, so neither is generated there.
//! - **The previous token (L17).** A variant is specialized on the class of the token before it
//!   and the class of its own last token: a number, another word (name, keyword, `_`) or
//!   punctuation, that can or can't end a statement (`nc`, `wc`, `wn`, `pc`, `pn`; the file
//!   start counts as `pn`), or `|`/`||` (`bar`).
//!   NEWLINE is only generated after a token that can end a statement, in a `b` context, as a
//!   real line break (optionally after a comment); the lexer then produces exactly that NEWLINE.
//!   The export checks that no NEWLINE can be followed by `.`, the one token that would cancel
//!   it.
//! - **Whitespace.** Between two tokens in a `b` context, a line break is allowed only where L17
//!   makes it whitespace: after a token that can't end a statement, or before a `.`. Inside `p`
//!   contexts line breaks and comments are free. Where the two tokens could merge when re-lexed
//!   (two words; a number then `.`; punctuation pairs such as `-` `>`, `*` `*`, `.` `.`, `:` `:`,
//!   `<` `=`, worked out with the real lexer), at least one space or tab is required; a GT_CLOSE
//!   may touch a following `>`, `=` or `>=`, which L16 splits again. Which
//!   tokens can come before a given token is over-approximated (from the specialized grammar),
//!   so some layouts that would be fine are forbidden; none that would merge is allowed.
//!
//! IDENT excludes the keywords (through a trie: a regular grammar for the complement) and `_`.
//! A SUFFIXED number's unit is letters only, so it's never a type suffix (L13). An INT
//! after `.` (a tuple index) is limited to four digits so it always fits in `u32`. It's a
//! terminal of its own here, written right after its `.`, since another `.` may then touch it
//! (`t.0.1`); a number takes a `.` only when a digit follows it (L12), so before `.` and a name
//! no number needs a space (`2.0.sqrt()`).
//!
//! The result is meant to be sound, not complete: every string it generates should parse, which
//! the tests check by sampling (see [`crate::sampler`]); and it accepts the formatter's layout
//! and the usual hand layouts (also tested), but not every valid one.

use crate::cfg::{Cfg, Recursion, Sym};
use crate::ebnf::{Grammar, Terminal};
use crate::generate::{glued_gt, is_number, is_word, no_glue_pairs};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use wrela_syntax::TokenKind;

/// The innermost open bracket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Ctx {
    /// `{`, or no bracket.
    Brace,
    /// `(` or `[`.
    Paren,
    /// The top of an f-string's hole.
    Hole,
    /// A bracket inside an f-string's hole.
    HoleNested,
}

impl Ctx {
    fn in_hole(self) -> bool {
        matches!(self, Ctx::Hole | Ctx::HoleNested)
    }

    fn code(self) -> &'static str {
        match self {
            Ctx::Brace => "b",
            Ctx::Paren => "p",
            Ctx::Hole => "h",
            Ctx::HoleNested => "hn",
        }
    }
}

/// What a production itself opens at a position: a bracket, or an f-string's hole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Inner {
    Brace,
    Paren,
    Hole,
}

/// The context at a position that's `inner` of the production, which is in `outer`.
fn resolve(inner: Option<Inner>, outer: Ctx) -> Ctx {
    match inner {
        None => outer,
        Some(Inner::Hole) => Ctx::Hole,
        Some(_) if outer.in_hole() => Ctx::HoleNested,
        Some(Inner::Brace) => Ctx::Brace,
        Some(Inner::Paren) => Ctx::Paren,
    }
}

/// The class of the previous token: a number, another word, or punctuation; and whether it can
/// end a statement (L17). Numbers are their own class because `.` and a digit can't follow them
/// directly (`1.0` is a FLOAT), while they can follow any other word; a tuple index is a class
/// of its own because they can follow it (`t.0.1`, L12).
/// `|` and `||` are a class of their own because a closure can start right after one (a closure
/// whose body is a closure), and `|` `|` would lex as `||`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Prev {
    Nc,
    /// A tuple index.
    Ti,
    Wc,
    Wn,
    Pc,
    Pn,
    Bar,
    /// An f-string piece that opens a hole (FSTRING_HEAD, FSTRING_MID): a `{` right after it
    /// would read as `{{` (L22), which no other token minds.
    Hole,
}

const PREVS: [Prev; 8] =
    [Prev::Nc, Prev::Ti, Prev::Wc, Prev::Wn, Prev::Pc, Prev::Pn, Prev::Bar, Prev::Hole];

impl Prev {
    fn bit(self) -> u8 {
        1 << (self as u8)
    }

    fn can_end(self) -> bool {
        matches!(self, Prev::Nc | Prev::Ti | Prev::Wc | Prev::Pc)
    }

    fn code(self) -> &'static str {
        match self {
            Prev::Nc => "nc",
            Prev::Ti => "ti",
            Prev::Wc => "wc",
            Prev::Wn => "wn",
            Prev::Pc => "pc",
            Prev::Pn => "pn",
            Prev::Bar => "bar",
            Prev::Hole => "ho",
        }
    }
}

fn class(t: Terminal) -> Prev {
    let k = t.lexer_kind();
    if t == Terminal::TupleIndex {
        return Prev::Ti;
    }
    if is_number(k) {
        return Prev::Nc;
    }
    if matches!(k, TokenKind::Pipe | TokenKind::OrOr) {
        return Prev::Bar;
    }
    if matches!(k, TokenKind::FStringHead | TokenKind::FStringMid) {
        return Prev::Hole;
    }
    match (is_word(k), k.can_end_statement()) {
        (true, true) => Prev::Wc,
        (true, false) => Prev::Wn,
        (false, true) => Prev::Pc,
        (false, false) => Prev::Pn,
    }
}

const NEWLINE: Terminal = Terminal::Token(TokenKind::Newline);
const EOF: Terminal = Terminal::Token(TokenKind::Eof);
const DOT: Terminal = Terminal::Token(TokenKind::Dot);
const IDENT: Terminal = Terminal::Token(TokenKind::Ident);
const INT: Terminal = Terminal::Token(TokenKind::Int);

/// What can't be exported faithfully.
#[derive(Debug)]
pub struct ExportError(pub String);

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ExportError {}

/// A specialized nonterminal: (nonterminal, bracket, class before, class of its last token).
type Key = (usize, Ctx, Prev, Prev);

/// A symbol of a specialized production.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum SSym {
    /// A terminal, with the class of the token before it and its bracket context.
    T(Terminal, Prev, Ctx),
    N(Key),
}

struct Specializer<'a> {
    cfg: &'a Cfg,
    /// Per production and position: the bracket (or hole) the position is inside, if the
    /// production itself opened it.
    inner: Vec<Vec<Option<Inner>>>,
    excluded: HashSet<Terminal>,
    /// For each (nonterminal, bracket, class before): the classes it can end in.
    outs: BTreeMap<(usize, Ctx, Prev), u8>,
}

fn opener(t: Terminal) -> Option<Inner> {
    match t.lexer_kind() {
        TokenKind::LBrace => Some(Inner::Brace),
        TokenKind::LParen | TokenKind::LBracket => Some(Inner::Paren),
        _ => None,
    }
}

/// The bracket each position of each production is inside, when the production opened it; the
/// rule `hole` is an f-string's hole wherever it's used.
fn bracket_contexts(cfg: &Cfg) -> Result<Vec<Vec<Option<Inner>>>, ExportError> {
    let hole = cfg.nts.iter().position(|n| n.name == "hole");
    let mut out = Vec::new();
    for p in &cfg.prods {
        let mut stack: Vec<TokenKind> = Vec::new();
        let mut ctxs = Vec::new();
        for s in &p.rhs {
            let here = if hole.is_some() && *s == Sym::N(hole.unwrap_or(usize::MAX)) {
                Some(Inner::Hole)
            } else {
                stack
                    .last()
                    .map(|k| if *k == TokenKind::LBrace { Inner::Brace } else { Inner::Paren })
            };
            ctxs.push(here);
            if let Sym::T(t) = s {
                if opener(*t).is_some() {
                    stack.push(t.lexer_kind());
                } else if let Some(open) = t.lexer_kind().opener()
                    && stack.pop() != Some(open)
                {
                    return Err(ExportError(format!(
                        "in `{}`, a closing bracket doesn't match an opening one in the same sequence",
                        cfg.nts[p.lhs].name
                    )));
                }
            }
        }
        if !stack.is_empty() {
            return Err(ExportError(format!(
                "in `{}`, a bracket isn't closed in the same sequence",
                cfg.nts[p.lhs].name
            )));
        }
        out.push(ctxs);
    }
    Ok(out)
}

impl Specializer<'_> {
    fn allowed(&self, t: Terminal, prev: Prev, ctx: Ctx) -> bool {
        if self.excluded.contains(&t) {
            return false;
        }
        if t == NEWLINE {
            return ctx == Ctx::Brace && prev.can_end();
        }
        // At the top of a hole, `:` starts the format spec and `}` ends the hole (L22).
        if ctx == Ctx::Hole && matches!(t.lexer_kind(), TokenKind::Colon | TokenKind::RBrace) {
            return false;
        }
        true
    }

    fn out_mask(&mut self, nt: usize, ctx: Ctx, prev: Prev) -> u8 {
        *self.outs.entry((nt, ctx, prev)).or_insert(0)
    }

    /// The classes production `p`, started after a `prev` token in `ctx`, can end in.
    fn production_outs(&mut self, p: usize, ctx: Ctx, prev: Prev) -> u8 {
        let cfg = self.cfg;
        let mut cur = prev.bit();
        for (i, s) in cfg.prods[p].rhs.iter().enumerate() {
            let c = resolve(self.inner[p][i], ctx);
            let mut next = 0;
            for s_prev in PREVS {
                if cur & s_prev.bit() == 0 {
                    continue;
                }
                match *s {
                    Sym::T(t) => {
                        if self.allowed(t, s_prev, c) {
                            next |= class(t).bit();
                        }
                    }
                    Sym::N(n) => next |= self.out_mask(n, c, s_prev),
                }
            }
            cur = next;
            if cur == 0 {
                break;
            }
        }
        cur
    }

    fn solve(&mut self) {
        let cfg = self.cfg;
        self.outs.insert((cfg.start, Ctx::Brace, Prev::Pn), 0);
        loop {
            let keys: Vec<_> = self.outs.keys().copied().collect();
            let mut changed = false;
            for &(nt, ctx, prev) in &keys {
                let mut mask = 0;
                for &p in &cfg.nts[nt].prods {
                    mask |= self.production_outs(p, ctx, prev);
                }
                let old = self.outs[&(nt, ctx, prev)];
                if mask | old != old {
                    self.outs.insert((nt, ctx, prev), mask | old);
                    changed = true;
                }
            }
            // Keys first met during this pass haven't been evaluated yet.
            if !changed && self.outs.len() == keys.len() {
                break;
            }
        }
    }

    /// The specialized alternatives of a variant.
    fn alternatives(&self, key: Key) -> Vec<Vec<SSym>> {
        let (nt, ctx, prev, last) = key;
        let mut out = Vec::new();
        for &p in &self.cfg.nts[nt].prods {
            self.paths(p, 0, ctx, prev, last, &mut Vec::new(), &mut out);
        }
        out.sort();
        out.dedup();
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn paths(
        &self,
        p: usize,
        i: usize,
        ctx: Ctx,
        cur: Prev,
        last: Prev,
        acc: &mut Vec<SSym>,
        out: &mut Vec<Vec<SSym>>,
    ) {
        let rhs = &self.cfg.prods[p].rhs;
        if i == rhs.len() {
            if cur == last {
                out.push(acc.clone());
            }
            return;
        }
        let c = resolve(self.inner[p][i], ctx);
        match rhs[i] {
            Sym::T(t) => {
                if self.allowed(t, cur, c) {
                    acc.push(SSym::T(t, cur, c));
                    self.paths(p, i + 1, ctx, class(t), last, acc, out);
                    acc.pop();
                }
            }
            Sym::N(n) => {
                let mask = self.outs.get(&(n, c, cur)).copied().unwrap_or(0);
                for next in PREVS {
                    if mask & next.bit() != 0 {
                        acc.push(SSym::N((n, c, cur, next)));
                        self.paths(p, i + 1, ctx, next, last, acc, out);
                        acc.pop();
                    }
                }
            }
        }
    }
}

/// Sets of terminals as bitmasks over the grammar's terminals, plus one bit for "the start of
/// the file".
struct TermSet {
    terms: Vec<Terminal>,
    /// Each terminal's bit.
    index: HashMap<Terminal, u32>,
}

impl TermSet {
    const START: u128 = 1 << 127;

    fn new(terms: Vec<Terminal>) -> TermSet {
        assert!(terms.len() < 127, "too many terminals for the bitmask");
        let index = terms.iter().enumerate().map(|(i, t)| (*t, i as u32)).collect();
        TermSet { terms, index }
    }

    fn bit(&self, t: Terminal) -> u128 {
        1 << self.index.get(&t).unwrap_or_else(|| unreachable!("unknown terminal"))
    }

    fn members(&self, mask: u128) -> impl Iterator<Item = Terminal> + '_ {
        self.terms.iter().enumerate().filter(move |(i, _)| mask & (1 << i) != 0).map(|(_, t)| *t)
    }
}

/// The specialized grammar, reduced to what's reachable and nonempty.
struct Specialized {
    rules: BTreeMap<Key, Vec<Vec<SSym>>>,
    root: Key,
}

impl Specialized {
    fn nullable(&self) -> HashSet<Key> {
        let mut nullable = HashSet::new();
        loop {
            let before = nullable.len();
            for (k, alts) in &self.rules {
                if alts
                    .iter()
                    .any(|a| a.iter().all(|s| matches!(s, SSym::N(n) if nullable.contains(n))))
                {
                    nullable.insert(*k);
                }
            }
            if nullable.len() == before {
                return nullable;
            }
        }
    }

    /// Drops variants that derive only the empty string, and references to them.
    fn remove_empty(&mut self) {
        loop {
            let empty: HashSet<Key> = self
                .rules
                .iter()
                .filter(|(k, alts)| **k != self.root && alts.iter().all(Vec::is_empty))
                .map(|(k, _)| *k)
                .collect();
            if empty.is_empty() {
                return;
            }
            self.rules.retain(|k, _| !empty.contains(k));
            for alts in self.rules.values_mut() {
                for a in alts.iter_mut() {
                    a.retain(|s| !matches!(s, SSym::N(n) if empty.contains(n)));
                }
                alts.sort();
                alts.dedup();
            }
        }
    }

    /// For each variant, the terminals it can end with; and for each, the terminals (and the
    /// file start) that can come right before it.
    fn last_and_precede(
        &self,
        ts: &TermSet,
        nullable: &HashSet<Key>,
    ) -> (HashMap<Key, u128>, HashMap<Key, u128>) {
        let mut last: HashMap<Key, u128> = self.rules.keys().map(|k| (*k, 0)).collect();
        loop {
            let mut changed = false;
            for (k, alts) in &self.rules {
                let mut m = 0;
                for a in alts {
                    m |= Self::scan_back(a, a.len(), &last, nullable, ts).0;
                }
                if last[k] | m != last[k] {
                    last.insert(*k, last[k] | m);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut precede: HashMap<Key, u128> = self.rules.keys().map(|k| (*k, 0)).collect();
        precede.insert(self.root, TermSet::START);
        loop {
            let mut changed = false;
            for (k, alts) in &self.rules {
                for a in alts {
                    for (i, s) in a.iter().enumerate() {
                        if let SSym::N(n) = s {
                            let (m, reached_start) = Self::scan_back(a, i, &last, nullable, ts);
                            let m = if reached_start { m | precede[k] } else { m };
                            if precede[n] | m != precede[n] {
                                precede.insert(*n, precede[n] | m);
                                changed = true;
                            }
                        }
                    }
                }
            }
            if !changed {
                return (last, precede);
            }
        }
    }

    /// The terminals that can end `a[..end]`, and whether it can be empty.
    fn scan_back(
        a: &[SSym],
        end: usize,
        last: &HashMap<Key, u128>,
        nullable: &HashSet<Key>,
        ts: &TermSet,
    ) -> (u128, bool) {
        let mut m = 0;
        for s in a[..end].iter().rev() {
            match s {
                SSym::T(t, ..) => return (m | ts.bit(*t), false),
                SSym::N(n) => {
                    m |= last[n];
                    if !nullable.contains(n) {
                        return (m, false);
                    }
                }
            }
        }
        (m, true)
    }

    fn check_no_left_recursion(
        &self,
        nullable: &HashSet<Key>,
        names: &impl Fn(Key) -> String,
    ) -> Result<(), ExportError> {
        let left: HashMap<Key, Vec<Key>> = self
            .rules
            .iter()
            .map(|(k, alts)| {
                let mut v = Vec::new();
                for a in alts {
                    for s in a {
                        match s {
                            SSym::T(..) => break,
                            SSym::N(n) => {
                                v.push(*n);
                                if !nullable.contains(n) {
                                    break;
                                }
                            }
                        }
                    }
                }
                (*k, v)
            })
            .collect();
        for start in self.rules.keys() {
            let mut seen = HashSet::new();
            let mut stack = left[start].clone();
            while let Some(k) = stack.pop() {
                if k == *start {
                    return Err(ExportError(format!(
                        "`{}` is left-recursive, which GBNF can't express",
                        names(*start)
                    )));
                }
                if seen.insert(k) {
                    stack.extend(&left[&k]);
                }
            }
        }
        Ok(())
    }
}

/// Makes each INT right after a `.` a [`Terminal::TupleIndex`]. A group of single terminals
/// right after a `.` (`"." (IDENT | INT)`) is first spread into one production per terminal, so
/// the `.` in each knows what follows it.
fn mark_tuple_indexes(cfg: &mut Cfg) {
    let single = |cfg: &Cfg, n: usize| -> Option<Vec<Sym>> {
        let nt = &cfg.nts[n];
        let alts: Vec<Sym> = nt
            .prods
            .iter()
            .map(|&q| match cfg.prods[q].rhs.as_slice() {
                [s @ Sym::T(_)] => Some(*s),
                _ => None,
            })
            .collect::<Option<_>>()?;
        nt.synthetic.then_some(alts)
    };
    let mut p = 0;
    while p < cfg.prods.len() {
        let rhs = cfg.prods[p].rhs.clone();
        let spread = rhs.windows(2).enumerate().find_map(|(i, w)| match w {
            [Sym::T(DOT), Sym::N(n)] => single(cfg, *n).map(|alts| (i + 1, alts)),
            _ => None,
        });
        if let Some((at, alts)) = spread {
            let lhs = cfg.prods[p].lhs;
            for (j, s) in alts.into_iter().enumerate() {
                let mut r = rhs.clone();
                r[at] = s;
                if j == 0 {
                    cfg.prods[p].rhs = r;
                } else {
                    cfg.nts[lhs].prods.push(cfg.prods.len());
                    cfg.prods.push(crate::cfg::Prod { lhs, rhs: r });
                }
            }
            continue;
        }
        for i in 1..rhs.len() {
            if rhs[i - 1] == Sym::T(DOT) && rhs[i] == Sym::T(INT) {
                cfg.prods[p].rhs[i] = Sym::T(Terminal::TupleIndex);
            }
        }
        p += 1;
    }
}

/// Where the export of spec/grammar.ebnf is checked in: spec/wrela.gbnf.
pub fn spec_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/wrela.gbnf")
}

/// The GBNF text for spec/grammar.ebnf.
pub fn export(g: &Grammar) -> Result<String, ExportError> {
    let mut cfg = Cfg::lower(g, Recursion::Right);
    mark_tuple_indexes(&mut cfg);
    let inner = bracket_contexts(&cfg)?;
    let excluded: HashSet<Terminal> = HashSet::new();
    let mut sp = Specializer { cfg: &cfg, inner, excluded, outs: BTreeMap::new() };
    sp.solve();

    // The variants reachable from the root.
    let root_out = sp.outs[&(cfg.start, Ctx::Brace, Prev::Pn)];
    let root_last = PREVS
        .into_iter()
        .find(|p| root_out & p.bit() != 0)
        .ok_or_else(|| ExportError("`file` derives nothing".into()))?;
    let root: Key = (cfg.start, Ctx::Brace, Prev::Pn, root_last);
    let mut rules = BTreeMap::new();
    let mut todo = vec![root];
    while let Some(k) = todo.pop() {
        if rules.contains_key(&k) {
            continue;
        }
        let alts = sp.alternatives(k);
        for a in &alts {
            for s in a {
                if let SSym::N(n) = s
                    && !rules.contains_key(n)
                {
                    todo.push(*n);
                }
            }
        }
        rules.insert(k, alts);
    }
    let mut spec = Specialized { rules, root };
    spec.remove_empty();
    let nullable = spec.nullable();

    let names = |k: Key| -> String {
        let (nt, ctx, prev, last) = k;
        let base = cfg.nts[nt].name.replace(['_', '#'], "-");
        format!("{base}-{}-{}-{}", ctx.code(), prev.code(), last.code())
    };
    spec.check_no_left_recursion(&nullable, &names)?;
    let mut seen_names = HashSet::new();
    for k in spec.rules.keys() {
        if !seen_names.insert(names(*k)) {
            return Err(ExportError(format!("two variants are both named `{}`", names(*k))));
        }
    }

    let mut terminals = g.terminals();
    terminals.push(Terminal::TupleIndex);
    let ts = TermSet::new(terminals);
    let (last, precede) = spec.last_and_precede(&ts, &nullable);
    let no_glue = no_glue_pairs();
    let merges = |a: Terminal, b: Terminal| {
        // L16: a GT_CLOSE written right before `>`, `=`, `>=` or another GT_CLOSE makes a
        // `>>`, `>=` or `>>=` token, which the parser splits back where it expects GT_CLOSE.
        if a == Terminal::GtClose && glued_gt(TokenKind::Gt, b.lexer_kind()).is_some() {
            return false;
        }
        // L12: a number directly after a `.` takes no `.`.
        if a == Terminal::TupleIndex && b == DOT {
            return false;
        }
        no_glue.contains(&(a.lexer_kind(), b.lexer_kind()))
    };

    // Each rule's body, as the words of its GBNF text.
    let mut rules: Vec<(String, Vec<String>)> = Vec::new();
    for (k, alts) in &spec.rules {
        let mut parts: Vec<String> = Vec::new();
        let mut has_empty = false;
        for a in alts {
            if a.is_empty() {
                has_empty = true;
                continue;
            }
            if !parts.is_empty() {
                parts.push("|".into());
            }
            for (i, s) in a.iter().enumerate() {
                match *s {
                    SSym::N(n) => parts.push(names(n)),
                    SSym::T(t, prev, ctx) => {
                        let (before, reached_start) =
                            Specialized::scan_back(a, i, &last, &nullable, &ts);
                        let before = if reached_start { before | precede[k] } else { before };
                        if t == NEWLINE {
                            parts.push("nl".into());
                            continue;
                        }
                        if t == Terminal::Token(TokenKind::Dot) && before & ts.bit(NEWLINE) != 0 {
                            return Err(ExportError(
                                "a `.` can follow a NEWLINE, which the lexer would drop (L17)"
                                    .into(),
                            ));
                        }
                        let preds: Vec<Terminal> = ts.members(before).collect();
                        // L12: a number takes a `.` only when a digit follows the `.`.
                        let dot_then_name =
                            t == DOT && matches!(a.get(i + 1), Some(SSym::T(n, ..)) if *n == IDENT);
                        let mandatory = preds.iter().any(|p| {
                            merges(*p, t) && !(dot_then_name && is_number(p.lexer_kind()))
                        });
                        // Nothing breaks a line in an f-string's hole, which ends at an
                        // FSTRING_MID or FSTRING_TAIL (L22).
                        let closes_hole = matches!(
                            t.lexer_kind(),
                            TokenKind::FStringMid | TokenKind::FStringTail
                        );
                        let line_breaks = !ctx.in_hole()
                            && !closes_hole
                            && (ctx == Ctx::Paren
                                || !prev.can_end()
                                || t.lexer_kind() == TokenKind::Dot);
                        // A gap may start with a comment unless a `/` can come before it.
                        let slash = preds.contains(&Terminal::Token(TokenKind::Slash));
                        let gap = if line_breaks {
                            match (mandatory, slash) {
                                (true, _) => "ws1",
                                (false, false) => "lead",
                                (false, true) => "ws",
                            }
                        } else if mandatory {
                            "sp1"
                        } else {
                            "sp"
                        };
                        // L12 keeps a `.` out of a number only when it's directly after a `.`
                        // token: a tuple index touches its `.`.
                        if t != Terminal::TupleIndex {
                            parts.push(gap.into());
                        }
                        if t != EOF {
                            parts.push(terminal_text(t));
                        }
                    }
                }
            }
        }
        if has_empty {
            assert!(!parts.is_empty(), "an empty-only variant survived");
            parts.insert(0, "(".into());
            parts.push(")?".into());
        }
        rules.push((names(*k), parts));
    }
    let root_name = merge_identical_rules(&mut rules, &names(spec.root));

    let mut out = String::new();
    out.push_str(HEADER);
    let _ = writeln!(out, "root ::= {root_name}\n");
    for (name, body) in &rules {
        let _ = writeln!(out, "{name} ::= {}", body.join(" "));
    }
    out.push('\n');
    out.push_str(&lexical_rules());
    Ok(out)
}

/// Merges rules with identical bodies into one (keeping the first name), repeatedly, since a
/// merge can make other bodies identical. Returns the root's final name.
fn merge_identical_rules(rules: &mut Vec<(String, Vec<String>)>, root: &str) -> String {
    let mut root = root.to_string();
    loop {
        let mut first: HashMap<&[String], &str> = HashMap::new();
        let mut alias: HashMap<String, String> = HashMap::new();
        for (name, body) in rules.iter() {
            match first.get(body.as_slice()) {
                Some(keep) => {
                    alias.insert(name.clone(), keep.to_string());
                }
                None => {
                    first.insert(body, name);
                }
            }
        }
        if alias.is_empty() {
            return root;
        }
        rules.retain(|(name, _)| !alias.contains_key(name));
        for (_, body) in rules.iter_mut() {
            for part in body.iter_mut() {
                if let Some(a) = alias.get(part) {
                    *part = a.clone();
                }
            }
        }
        if let Some(a) = alias.get(&root) {
            root = a.clone();
        }
    }
}

fn terminal_text(t: Terminal) -> String {
    match t {
        Terminal::Token(TokenKind::Ident) => "ident".into(),
        Terminal::TupleIndex => "tuple-index".into(),
        Terminal::Token(TokenKind::Int) => "int".into(),
        Terminal::Token(TokenKind::Float) => "float".into(),
        Terminal::Token(TokenKind::Suffixed) => "suffixed".into(),
        Terminal::Token(TokenKind::Str) => "string".into(),
        Terminal::Token(TokenKind::FString) => "fstring".into(),
        Terminal::Token(TokenKind::FStringHead) => "fstring-head".into(),
        Terminal::Token(TokenKind::FStringMid) => "fstring-mid".into(),
        Terminal::Token(TokenKind::FStringTail) => "fstring-tail".into(),
        t => {
            let text = t.fixed_text().unwrap_or_else(|| unreachable!("{t:?} has no text"));
            format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
        }
    }
}

const HEADER: &str = "\
# wrela source, as a GBNF grammar (llama.cpp) over characters.
# GENERATED from spec/grammar.ebnf by `cargo run -p wrela-grammar --bin export-gbnf`; do not edit.
#
# The rules are the grammar's, specialized on context. `name-B-X-Y` is `name` inside bracket B
# (b: `{` or none, p: `(` or `[`, h: an f-string's hole, hn: a bracket in one), after a token of
# class X, ending with a token of class Y.
# Classes (spec/lexical.md L17): nc a number; ti a tuple index; wc, wn another word; pc, pn
# punctuation (c: can end a statement, n: can't); bar `|` or `||`; ho an f-string piece that
# opens a hole. NEWLINE is `nl`, a real line break. The gaps between
# tokens are `sp` (spaces and tabs), `ws` (also line breaks and comments), `lead` (`ws` that may
# start with a comment), and `sp1`/`ws1` (nonempty, where the tokens would otherwise merge).
#
# It is meant to be sound, not complete: every string it generates parses (checked by sampling:
# `cargo run -p wrela-grammar --bin gbnf-sample`), while some valid layouts are left out.

";

/// IDENT without the keywords and `_`, numbers, and whitespace.
fn lexical_rules() -> String {
    let mut out = String::new();
    out.push_str("# IDENT (L10) minus the keywords (L11) and `_`: a trie over the keywords.\n");
    let keywords: Vec<&str> = TokenKind::KEYWORDS.iter().filter_map(|k| k.fixed_text()).collect();
    let prefixes: BTreeSet<&str> =
        keywords.iter().flat_map(|k| (1..=k.len()).map(move |i| &k[..i])).collect();
    let mut ids: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, p) in prefixes.iter().enumerate() {
        ids.insert(p, i + 1);
    }
    let cont: BTreeSet<u8> =
        (b'a'..=b'z').chain(b'A'..=b'Z').chain(b'0'..=b'9').chain(*b"_").collect();
    let children = |p: &str| -> BTreeSet<u8> {
        prefixes
            .iter()
            .filter(|q| q.len() == p.len() + 1 && q.starts_with(p))
            .map(|q| q.as_bytes()[p.len()])
            .collect()
    };
    let node =
        |p: &str| if p.is_empty() { "ident".to_string() } else { format!("ident-{}", ids[p]) };
    for p in std::iter::once("").chain(prefixes.iter().copied()) {
        let kids = children(p);
        let mut alts = Vec::new();
        let first: BTreeSet<u8> = if p.is_empty() {
            cont.iter().copied().filter(|b| !b.is_ascii_digit() && *b != b'_').collect()
        } else {
            cont.clone()
        };
        let others: BTreeSet<u8> = first.difference(&kids).copied().collect();
        if !others.is_empty() {
            alts.push(format!("{} [A-Za-z0-9_]*", char_class(&others)));
        }
        for c in &kids {
            let q = format!("{p}{}", *c as char);
            alts.push(format!("\"{}\" {}", *c as char, node(&q)));
        }
        if p.is_empty() {
            alts.push("\"_\" [A-Za-z0-9_]+".into());
        }
        let body = alts.join(" | ");
        let is_keyword = keywords.contains(&p);
        let line = if p.is_empty() || is_keyword {
            format!("{} ::= {body}", node(p))
        } else {
            format!("{} ::= ( {body} )?", node(p))
        };
        let _ = writeln!(
            out,
            "{line}{}",
            if p.is_empty() { String::new() } else { format!("  # after \"{p}\"") }
        );
    }
    out.push_str(
        "\n# INT and FLOAT (L12, L13). A tuple index (an INT after `.`) is kept small enough for u32.
int ::= [0-9] [0-9_]* | \"0x\" [0-9a-fA-F] [0-9a-fA-F_]* | \"0b\" [01] [01_]* | \"0o\" [0-7] [0-7_]*
tuple-index ::= [0-9] [0-9]? [0-9]? [0-9]?
float ::= [0-9] [0-9_]* \".\" [0-9] [0-9_]* exponent? | [0-9] [0-9_]* exponent
exponent ::= [eE] [+-]? [0-9] [0-9_]*
# SUFFIXED (L13): a decimal number and a unit of letters, not starting with `e` (never a type
# suffix, which has digits; nor `x`, `b` or `o`, which after a `0` read as a radix prefix).
suffixed ::= ( [0-9] [0-9_]* ( \".\" [0-9] [0-9_]* )? exponent? ) [acdf-np-wyzACDF-NP-WYZ] [a-zA-Z]*

# STRING (L14), and the f-string tokens (L22).
string ::= \"\\\"\" string-char* \"\\\"\"
string-char ::= [^\"\\\\\\r\\n] | \"\\\\\" [\\\\\"nrt0]
fstring-char ::= [^\"\\\\{}\\r\\n] | \"\\\\\" [\\\\\"nrt0] | \"{{\" | \"}}\"
fstring-spec ::= \":\" ( [^:}\"\\r\\n] [^}\"\\r\\n]* )?
fstring ::= \"f\\\"\" fstring-char* \"\\\"\"
fstring-head ::= \"f\\\"\" fstring-char* \"{\"
fstring-mid ::= fstring-spec? \"}\" fstring-char* \"{\"
fstring-tail ::= fstring-spec? \"}\" fstring-char* \"\\\"\"

# Gaps between tokens, and NEWLINE (L5, L6, L17, L18). In `ws` and `ws1` a comment follows a
# space or line break, so it can't join a `/` before it into `//`; `lead` is for gaps no `/`
# can come before.
sp ::= [ \\t]*
sp1 ::= [ \\t]+
ws ::= ( [ \\t\\n] ( \"//\" [^\\r\\n]* \"\\n\" )? )*
ws1 ::= ( [ \\t\\n] ( \"//\" [^\\r\\n]* \"\\n\" )? )+
lead ::= ( [ \\t\\n] | \"//\" [^\\r\\n]* \"\\n\" )*
nl ::= [ \\t]* ( \"//\" [^\\r\\n]* )? \"\\n\"
",
    );
    out
}

/// A GBNF character class for a set of ASCII bytes, as ranges.
fn char_class(set: &BTreeSet<u8>) -> String {
    let v: Vec<u8> = set.iter().copied().collect();
    let mut out = String::from("[");
    let mut i = 0;
    while i < v.len() {
        let mut j = i;
        while j + 1 < v.len() && v[j + 1] == v[j] + 1 {
            j += 1;
        }
        if j >= i + 2 {
            let _ = write!(out, "{}-{}", v[i] as char, v[j] as char);
        } else {
            for b in &v[i..=j] {
                out.push(*b as char);
            }
        }
        i = j + 1;
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_classes_use_ranges() {
        let s: BTreeSet<u8> = b"abcdxyz_0".iter().copied().collect();
        assert_eq!(char_class(&s), "[0_a-dx-z]");
    }

    #[test]
    fn unbalanced_brackets_are_refused() {
        let g = Grammar::parse("file ::= x EOF\nx ::= \"(\" IDENT | \")\"\n").unwrap();
        assert!(export(&g).unwrap_err().0.contains("bracket"));
    }

    #[test]
    fn a_dot_after_newline_is_refused() {
        // The lexer drops a NEWLINE before `.` (L17), so this can't be written as characters.
        let g = Grammar::parse("file ::= IDENT NEWLINE \".\" IDENT EOF\n").unwrap();
        assert!(export(&g).unwrap_err().0.contains("NEWLINE"));
    }

    #[test]
    fn newlines_follow_only_tokens_that_end_statements() {
        let g = Grammar::parse("file ::= (IDENT | \"=\") NEWLINE? IDENT EOF\n").unwrap();
        let text = export(&g).unwrap();
        let gbnf = crate::sampler::Gbnf::parse(&text).unwrap();
        for (src, ok) in
            [("a\nb", true), ("a b", true), ("=\nb", true), ("= b", true), ("a\n", false)]
        {
            assert_eq!(gbnf.recognizes(gbnf.root, src), ok, "{src:?}");
        }
        // After `=` the line break is whitespace, so only after IDENT is it a NEWLINE (`nl`).
        assert_eq!(text.matches(" nl").count(), 1, "{text}");
    }
}
