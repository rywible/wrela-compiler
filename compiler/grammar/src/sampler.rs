//! Reads the GBNF subset that [`crate::gbnf`] writes, and samples random strings from it, to
//! check the export's soundness: every sampled program must parse without errors.
//!
//! The subset: one rule per line, `name ::= alternatives`; `#` comments; literals `"..."` with
//! the escapes `\\ \" \n \r \t`; character classes `[...]` and `[^...]` with ranges; rule
//! names; groups `( ... )`; and the postfix operators `?`, `*` and `+`.
//!
//! Sampling expands `root` top-down, choosing uniformly among alternatives until the text
//! reaches its length budget or a depth limit, then taking the shortest way to finish.

use crate::agree::Checker;
use crate::earley::Scratch;
use crate::rng::{FxHashMap, FxHashSet, Rng, par_seeded};
use std::collections::HashMap;
use wrela_diag::FileId;

#[derive(Clone, Debug)]
pub struct Gbnf {
    pub rules: Vec<RuleDef>,
    pub root: usize,
    /// The shortest string each rule derives, in characters.
    cost: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct RuleDef {
    pub name: String,
    pub alts: Alts,
}

/// Alternatives: a rule's body, or a group's.
#[derive(Clone, Debug)]
pub struct Alts {
    pub seqs: Vec<Vec<Item>>,
    /// The alternatives that derive the shortest strings, which sampling takes once the budget
    /// is spent. Filled in when the rule costs are known.
    cheapest: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub atom: Atom,
    pub rep: Rep,
}

#[derive(Clone, Debug)]
pub enum Atom {
    Lit(String),
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
        /// The characters sampling draws from: those in the ranges or, for a negated class,
        /// those of `NEGATED_POOL` outside them.
        pool: Vec<char>,
    },
    Ref(usize),
    Group(Alts),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rep {
    One,
    Opt,
    Star,
    Plus,
}

/// Characters to draw from for a negated class (`[^\r\n]` in comments): printable ASCII, tab,
/// and some non-ASCII text, which comments may hold.
const NEGATED_POOL: &str = " \t!\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~éλ→😀";

struct Reader<'a> {
    s: &'a [char],
    pos: usize,
    names: &'a HashMap<String, usize>,
    line: usize,
}

impl Reader<'_> {
    fn err(&self, what: &str) -> String {
        format!("GBNF line {}: {what} at column {}", self.line, self.pos + 1)
    }

    fn skip_space(&mut self) {
        while let Some(c) = self.s.get(self.pos) {
            if *c == ' ' || *c == '\t' {
                self.pos += 1;
            } else if *c == '#' {
                self.pos = self.s.len();
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.pos).copied()
    }

    /// Reads `c`, the character at the current position, or the escape it starts (`\`).
    fn char(&mut self, c: char) -> Result<char, String> {
        self.pos += 1;
        if c != '\\' {
            return Ok(c);
        }
        let c = self.peek().ok_or_else(|| self.err("unfinished escape"))?;
        self.pos += 1;
        Ok(match c {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '\\' | '"' | ']' | '[' | '-' | '^' => c,
            _ => return Err(self.err(&format!("unknown escape `\\{c}`"))),
        })
    }

    fn alts(&mut self) -> Result<Alts, String> {
        let mut seqs = vec![self.seq()?];
        while self.peek() == Some('|') {
            self.pos += 1;
            seqs.push(self.seq()?);
        }
        Ok(Alts { seqs, cheapest: Vec::new() })
    }

    fn seq(&mut self) -> Result<Vec<Item>, String> {
        let mut items = Vec::new();
        loop {
            self.skip_space();
            let atom = match self.peek() {
                None | Some('|') | Some(')') => return Ok(items),
                Some('"') => {
                    self.pos += 1;
                    let mut lit = String::new();
                    loop {
                        match self.peek() {
                            None => return Err(self.err("unterminated literal")),
                            Some('"') => break,
                            Some(c) => lit.push(self.char(c)?),
                        }
                    }
                    self.pos += 1;
                    Atom::Lit(lit)
                }
                Some('[') => {
                    self.pos += 1;
                    let negated = self.peek() == Some('^');
                    if negated {
                        self.pos += 1;
                    }
                    let mut ranges = Vec::new();
                    loop {
                        let lo = match self.peek() {
                            None => return Err(self.err("unterminated class")),
                            Some(']') => break,
                            Some(c) => self.char(c)?,
                        };
                        let hi =
                            if self.peek() == Some('-') && self.s.get(self.pos + 1) != Some(&']') {
                                self.pos += 1;
                                match self.peek() {
                                    Some(c) => self.char(c)?,
                                    None => return Err(self.err("unterminated range")),
                                }
                            } else {
                                lo
                            };
                        ranges.push((lo, hi));
                    }
                    self.pos += 1;
                    let inside = |c: char| ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c));
                    let pool = if negated {
                        NEGATED_POOL.chars().filter(|c| !inside(*c)).collect()
                    } else {
                        ranges.iter().flat_map(|(lo, hi)| *lo..=*hi).collect()
                    };
                    Atom::Class { negated, ranges, pool }
                }
                Some('(') => {
                    self.pos += 1;
                    let alts = self.alts()?;
                    self.skip_space();
                    if self.peek() != Some(')') {
                        return Err(self.err("expected `)`"));
                    }
                    self.pos += 1;
                    Atom::Group(alts)
                }
                Some(c) if c.is_ascii_alphanumeric() || c == '-' => {
                    let start = self.pos;
                    while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == '-') {
                        self.pos += 1;
                    }
                    let name: String = self.s[start..self.pos].iter().collect();
                    let id = *self
                        .names
                        .get(&name)
                        .ok_or_else(|| self.err(&format!("undefined rule `{name}`")))?;
                    Atom::Ref(id)
                }
                Some(c) => return Err(self.err(&format!("unexpected `{c}`"))),
            };
            let rep = match self.peek() {
                Some('?') => Rep::Opt,
                Some('*') => Rep::Star,
                Some('+') => Rep::Plus,
                _ => Rep::One,
            };
            if rep != Rep::One {
                self.pos += 1;
            }
            items.push(Item { atom, rep });
        }
    }
}

impl Gbnf {
    pub fn parse(text: &str) -> Result<Gbnf, String> {
        let mut defs = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let t = line.trim_start();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            let (name, body) = t
                .split_once("::=")
                .ok_or_else(|| format!("GBNF line {}: expected `name ::= ...`", i + 1))?;
            defs.push((name.trim().to_string(), body.to_string(), i + 1));
        }
        let mut names = HashMap::new();
        for (i, (n, _, line)) in defs.iter().enumerate() {
            if names.insert(n.clone(), i).is_some() {
                return Err(format!("GBNF line {line}: `{n}` is defined twice"));
            }
        }
        let mut rules = Vec::new();
        for (name, body, line) in &defs {
            let chars: Vec<char> = body.chars().collect();
            let mut r = Reader { s: &chars, pos: 0, names: &names, line: *line };
            let alts = r.alts()?;
            r.skip_space();
            if r.pos != chars.len() {
                return Err(r.err("unexpected text"));
            }
            rules.push(RuleDef { name: name.clone(), alts });
        }
        let root = *names.get("root").ok_or("GBNF has no `root` rule")?;
        let mut g = Gbnf { rules, root, cost: Vec::new() };
        g.cost = g.min_costs();
        if let Some(r) = g.rules.iter().zip(&g.cost).find(|(_, c)| **c == usize::MAX) {
            return Err(format!("GBNF rule `{}` derives no finite string", r.0.name));
        }
        for r in &mut g.rules {
            mark_cheapest(&mut r.alts, &g.cost);
        }
        Ok(g)
    }

    fn min_costs(&self) -> Vec<usize> {
        let mut cost = vec![usize::MAX; self.rules.len()];
        loop {
            let mut changed = false;
            for (i, r) in self.rules.iter().enumerate() {
                let c = alts_cost(&r.alts.seqs, &cost);
                if c < cost[i] {
                    cost[i] = c;
                    changed = true;
                }
            }
            if !changed {
                return cost;
            }
        }
    }

    /// A random string from `root`, of roughly `budget` characters at most.
    pub fn sample(&self, rng: &mut Rng, budget: usize, used: &mut [u64]) -> String {
        let mut s = Sampler { g: self, out: String::new(), budget, used };
        s.rule(self.root, 0, rng);
        s.out
    }
}

impl Gbnf {
    pub fn rule_id(&self, name: &str) -> Option<usize> {
        self.rules.iter().position(|r| r.name == name)
    }

    /// Whether rule `rule` derives exactly `text`. A memoized top-down recognizer (the export
    /// has no left recursion); fine for program-sized texts.
    pub fn recognizes(&self, rule: usize, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        let mut ends = Vec::new();
        self.rule_ends(rule, 0, &chars, &mut Memo::default(), &mut ends);
        ends.contains(&chars.len())
    }

    // Each `*_ends` function appends to `out` the positions where its part, started at `pos`,
    // can end (in any order, maybe more than once).

    fn rule_ends(&self, r: usize, pos: usize, s: &[char], memo: &mut Memo, out: &mut Vec<usize>) {
        if let Some(e) = memo.get(&(r, pos)) {
            out.extend_from_slice(e);
            return;
        }
        // A rule reached again at the same position before finishing would be left recursion,
        // which the export rules out; the placeholder makes such a cycle fail instead of loop.
        memo.insert((r, pos), Vec::new());
        let mut ends = Vec::new();
        self.alts_ends(&self.rules[r].alts.seqs, pos, s, memo, &mut ends);
        ends.sort_unstable();
        ends.dedup();
        out.extend_from_slice(&ends);
        memo.insert((r, pos), ends);
    }

    fn alts_ends(
        &self,
        alts: &[Vec<Item>],
        pos: usize,
        s: &[char],
        memo: &mut Memo,
        out: &mut Vec<usize>,
    ) {
        let (mut cur, mut next) = (Vec::new(), Vec::new());
        for a in alts {
            cur.clear();
            cur.push(pos);
            for it in a {
                next.clear();
                for &p in &cur {
                    self.item_ends(it, p, s, memo, &mut next);
                }
                next.sort_unstable();
                next.dedup();
                std::mem::swap(&mut cur, &mut next);
                if cur.is_empty() {
                    break;
                }
            }
            out.extend_from_slice(&cur);
        }
    }

    fn item_ends(&self, it: &Item, pos: usize, s: &[char], memo: &mut Memo, out: &mut Vec<usize>) {
        match it.rep {
            Rep::One => self.atom_ends(&it.atom, pos, s, memo, out),
            Rep::Opt => {
                self.atom_ends(&it.atom, pos, s, memo, out);
                out.push(pos);
            }
            Rep::Star | Rep::Plus => {
                if it.rep == Rep::Star {
                    out.push(pos);
                }
                let (mut frontier, mut once) = (vec![pos], Vec::new());
                let mut seen = FxHashSet::default();
                while let Some(p) = frontier.pop() {
                    once.clear();
                    self.atom_ends(&it.atom, p, s, memo, &mut once);
                    for &e in &once {
                        if e > p && seen.insert(e) {
                            out.push(e);
                            frontier.push(e);
                        }
                    }
                }
            }
        }
    }

    fn atom_ends(
        &self,
        atom: &Atom,
        pos: usize,
        s: &[char],
        memo: &mut Memo,
        out: &mut Vec<usize>,
    ) {
        match atom {
            Atom::Lit(l) => {
                let n = l.chars().count();
                if s.get(pos..pos + n).is_some_and(|w| w.iter().copied().eq(l.chars())) {
                    out.push(pos + n);
                }
            }
            Atom::Class { negated, ranges, .. } => {
                let inside = |c: &char| ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(c));
                if s.get(pos).is_some_and(|c| inside(c) != *negated) {
                    out.push(pos + 1);
                }
            }
            Atom::Ref(r) => self.rule_ends(*r, pos, s, memo, out),
            Atom::Group(alts) => self.alts_ends(&alts.seqs, pos, s, memo, out),
        }
    }
}

/// The recognizer's memo: the ends of each (rule, start position).
type Memo = FxHashMap<(usize, usize), Vec<usize>>;

/// Fills in [`Alts::cheapest`] for `alts` and the groups inside it.
fn mark_cheapest(alts: &mut Alts, cost: &[usize]) {
    let costs: Vec<usize> = alts.seqs.iter().map(|a| seq_cost(a, cost)).collect();
    let min = costs.iter().copied().min().unwrap_or(0);
    alts.cheapest = (0..costs.len()).filter(|&i| costs[i] == min).collect();
    for it in alts.seqs.iter_mut().flatten() {
        if let Atom::Group(inner) = &mut it.atom {
            mark_cheapest(inner, cost);
        }
    }
}

fn alts_cost(alts: &[Vec<Item>], cost: &[usize]) -> usize {
    alts.iter().map(|a| seq_cost(a, cost)).min().unwrap_or(usize::MAX)
}

fn seq_cost(seq: &[Item], cost: &[usize]) -> usize {
    seq.iter().fold(0usize, |acc, it| acc.saturating_add(item_cost(it, cost)))
}

fn item_cost(it: &Item, cost: &[usize]) -> usize {
    let one = match &it.atom {
        Atom::Lit(s) => s.chars().count(),
        Atom::Class { .. } => 1,
        Atom::Ref(r) => cost[*r],
        Atom::Group(alts) => alts_cost(&alts.seqs, cost),
    };
    match it.rep {
        Rep::Opt | Rep::Star => 0,
        Rep::One | Rep::Plus => one,
    }
}

struct Sampler<'a> {
    g: &'a Gbnf,
    out: String,
    budget: usize,
    used: &'a mut [u64],
}

const MAX_DEPTH: usize = 1500;

impl Sampler<'_> {
    fn free(&self, depth: usize) -> bool {
        self.out.len() < self.budget && depth < MAX_DEPTH
    }

    fn rule(&mut self, r: usize, depth: usize, rng: &mut Rng) {
        self.used[r] += 1;
        let alts = &self.g.rules[r].alts;
        self.alts(alts, depth + 1, rng);
    }

    fn alts(&mut self, alts: &Alts, depth: usize, rng: &mut Rng) {
        let pick =
            if self.free(depth) { rng.below(alts.seqs.len()) } else { *rng.pick(&alts.cheapest) };
        for it in &alts.seqs[pick] {
            self.item(it, depth, rng);
        }
    }

    fn item(&mut self, it: &Item, depth: usize, rng: &mut Rng) {
        let n = match it.rep {
            Rep::One => 1,
            Rep::Opt => usize::from(self.free(depth) && rng.chance(0.5)),
            Rep::Star | Rep::Plus => {
                let mut n = usize::from(it.rep == Rep::Plus);
                while self.free(depth) && rng.chance(0.4) {
                    n += 1;
                }
                n
            }
        };
        for _ in 0..n {
            match &it.atom {
                Atom::Lit(s) => self.out.push_str(s),
                Atom::Class { pool, .. } => self.out.push(*rng.pick(pool)),
                Atom::Ref(r) => self.rule(*r, depth, rng),
                Atom::Group(alts) => self.alts(alts, depth + 1, rng),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_samples_the_subset() {
        let g = Gbnf::parse(
            "# comment\nroot ::= greeting ( \",\" sp greeting )*  # trailing\ngreeting ::= \"hi\" | [a-c] [^a-z\\n]? | \"q\\\"\\\\\"\nsp ::= [ \\t]+\n",
        )
        .unwrap();
        let mut used = vec![0; g.rules.len()];
        let mut rng = Rng::new(3);
        for _ in 0..50 {
            let s = g.sample(&mut rng, 30, &mut used);
            for piece in s.split(',') {
                let p = piece.trim_start_matches([' ', '\t']);
                let ok = p == "hi"
                    || p == "q\"\\"
                    || (p.starts_with(['a', 'b', 'c'])
                        && p.chars().count() <= 2
                        && !p[1..].contains(|c: char| c.is_ascii_lowercase()));
                assert!(ok, "{s:?}");
            }
        }
        assert!(used.iter().all(|u| *u > 0));
    }

    #[test]
    fn recognizes_what_it_samples() {
        let g = Gbnf::parse("root ::= x ( \",\" x )*\nx ::= \"a\" x? | [0-9]+\n").unwrap();
        for (s, ok) in [
            ("a", true),
            ("aa1", true),
            ("12,a3,7", true),
            ("", false),
            ("a,", false),
            ("b", false),
        ] {
            assert_eq!(g.recognizes(g.root, s), ok, "{s:?}");
        }
    }

    #[test]
    fn rejects_undefined_rules() {
        assert!(Gbnf::parse("root ::= nope\n").unwrap_err().contains("undefined rule `nope`"));
        assert!(Gbnf::parse("other ::= \"x\"\n").unwrap_err().contains("no `root`"));
    }
}

/// The result of checking sampled programs.
#[derive(Clone, Debug, Default)]
pub struct SampleStats {
    pub seed: u64,
    pub programs: u64,
    pub chars: u64,
    pub multi_line: u64,
    pub with_comments: u64,
    /// Programs the hand-written parser rejected, with the first error (must stay empty).
    pub rejected: Vec<(u64, String, String)>,
    /// Oracle disagreements (must stay empty).
    pub disagreements: Vec<(u64, String, String)>,
    pub rules_used: usize,
    pub rules_total: usize,
}

/// Samples `n` programs from `g` (program `i` from `mix(seed, i)`) and checks that the
/// hand-written parser accepts each without any diagnostic, and, given a checker, that the
/// oracle agrees.
pub fn check_samples(
    g: &Gbnf,
    checker: Option<&Checker>,
    n: u64,
    seed: u64,
    threads: usize,
) -> SampleStats {
    let workers = par_seeded(
        n,
        seed,
        threads,
        256 << 20,
        || (SampleStats::default(), vec![0u64; g.rules.len()], Scratch::default()),
        |(stats, used, scratch), i, mut rng| {
            let budget = 10 + rng.below(400);
            let src = g.sample(&mut rng, budget, used);
            stats.programs += 1;
            stats.chars += src.len() as u64;
            stats.multi_line += u64::from(src.trim_end().contains('\n'));
            stats.with_comments += u64::from(src.contains("//"));
            // The checker parses the source too; without one, it's parsed here.
            let (parsed, disagreement) = match checker {
                Some(c) => {
                    let check = c.check(&src, scratch);
                    (check.parsed, check.disagreement)
                }
                None => (wrela_syntax::parse(FileId(0), &src), None),
            };
            if let Some(d) = parsed.diagnostics.first() {
                stats.rejected.push((i, src.clone(), format!("{} {}", d.code.as_str(), d.message)));
            }
            if let Some(d) = disagreement {
                stats.disagreements.push((i, src, d));
            }
        },
    );
    let mut total = SampleStats { seed, rules_total: g.rules.len(), ..SampleStats::default() };
    let mut used_all = vec![0u64; g.rules.len()];
    for (s, used, _) in workers {
        total.programs += s.programs;
        total.chars += s.chars;
        total.multi_line += s.multi_line;
        total.with_comments += s.with_comments;
        total.rejected.extend(s.rejected);
        total.disagreements.extend(s.disagreements);
        used_all.iter_mut().zip(&used).for_each(|(a, b)| *a += b);
    }
    total.rules_used = used_all.iter().filter(|u| **u > 0).count();
    total
}

impl SampleStats {
    pub fn report(&self) -> String {
        let mut s = format!(
            "{} programs sampled from the GBNF ({:.0} characters on average; {} span several lines, {} have comments)\n\
             GBNF rules used: {}/{}\n\
             rejected by the parser: {}, oracle disagreements: {}\n",
            self.programs,
            self.chars as f64 / self.programs.max(1) as f64,
            self.multi_line,
            self.with_comments,
            self.rules_used,
            self.rules_total,
            self.rejected.len(),
            self.disagreements.len(),
        );
        for (i, src, why) in self.rejected.iter().chain(&self.disagreements).take(5) {
            s.push_str(&format!(
                "\nFAILURE seed {} program {i}: {why}\n--- source ---\n{src}\n--------------\n",
                self.seed
            ));
        }
        s
    }
}
