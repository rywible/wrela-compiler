//! Random programs from the grammar, rendered to source text.
//!
//! **Derivations.** [`Generator::generate`] expands `file` top-down, choosing uniformly among
//! alternatives and taking optional and repeated parts at random, until the program reaches its
//! token budget (drawn per program, so most programs are small) or a depth limit. From then on
//! every choice takes the cheapest way to finish (fewest tokens), so generation always ends.
//! Each program also has a goal drawn at random: a rule, an alternative, an optional part or a
//! repetition of the grammar. Until the goal is generated, most choices take the shortest way
//! toward it, and the goal itself is generated generously (a goal repetition repeats). Parts
//! that a uniform walk rarely reaches (a three-type tuple variant, a struct pattern in a `for`
//! head) then come up about as often as any other, and since the goal depends only on the
//! program's own seed, programs stay reproducible one by one.
//! [`Coverage`] counts which rules, alternatives, optional parts and repetitions were used.
//!
//! **Rendering.** [`Generator::render`] turns the terminal sequence into text that re-lexes to
//! the same tokens: invented names and numbers for the token classes, a space or nothing
//! between tokens (never nothing where the two would lex as one, such as `-` `>` or two words),
//! and a line break for each NEWLINE. It also exercises the lexer: it sometimes breaks a line
//! where L17 makes the break plain whitespace (inside `( )`, after an operator, before a leading
//! `.`), adds comments and blank lines, and glues a GT_CLOSE to the token after it when L16 lets
//! the parser split them again (`>>`, `>=`, `>>=`). A NEWLINE the lexer can't produce where the
//! grammar put it (after `{` or `,`, say) is still rendered as a line break, which is then just
//! whitespace; [`Rendered::dropped_newlines`] counts those, because the program then differs
//! from the derivation. The renderer checks its output by lexing it and panics if the tokens,
//! NEWLINEs included, differ from the ones it meant to write (a NEWLINE for each line break L17
//! keeps): that would be a bug here, or in the lexer, not in the derivation.

use crate::ebnf::{Expr, ExprId, expr_min_len};
use crate::ebnf::{ExprKind, Grammar, RuleId, Terminal};
use crate::rng::Rng;
use std::collections::HashSet;
use wrela_diag::FileId;
use wrela_syntax::TokenKind;
use wrela_syntax::lexer::{Brackets, line_break_is_newline};

#[derive(Clone, Debug)]
pub struct GenConfig {
    /// A program's token budget is drawn from this range.
    pub budget: (usize, usize),
    /// Rule nesting beyond which every choice is the cheapest.
    pub max_depth: u32,
    /// The chance of taking an optional part.
    pub optional: f64,
    /// The chance of each further repetition of a `*` or `+`.
    pub repeat: f64,
}

impl Default for GenConfig {
    fn default() -> GenConfig {
        GenConfig { budget: (4, 100), max_depth: 70, optional: 0.5, repeat: 0.35 }
    }
}

/// How often each part of the grammar was used.
#[derive(Clone, Debug, Default)]
pub struct Coverage {
    /// Per rule: how many times it was expanded.
    pub rules: Vec<u64>,
    /// Per expression node: how many times it was generated.
    pub nodes: Vec<u64>,
    /// Per `?` or `*` node: how many times it was left out.
    pub skipped: Vec<u64>,
    /// Per `+` or `*` node: how many times it repeated more than once.
    pub repeated: Vec<u64>,
}

impl Coverage {
    pub fn new(g: &Grammar) -> Coverage {
        let n = g.expr_count();
        Coverage {
            rules: vec![0; g.rules.len()],
            nodes: vec![0; n],
            skipped: vec![0; n],
            repeated: vec![0; n],
        }
    }

    pub fn merge(&mut self, other: &Coverage) {
        for (a, b) in [
            (&mut self.rules, &other.rules),
            (&mut self.nodes, &other.nodes),
            (&mut self.skipped, &other.skipped),
            (&mut self.repeated, &other.repeated),
        ] {
            a.iter_mut().zip(b).for_each(|(x, y)| *x += y);
        }
    }

    /// What the generator never did, where it got to: each line names a rule and the part of
    /// it. (A part inside one never generated isn't listed: generating the outer part first
    /// is what's missing.) Empty exactly when [`Coverage::hits`] is [`Coverage::branches`].
    pub fn gaps(&self, g: &Grammar) -> Vec<String> {
        let mut out = Vec::new();
        for (r, rule) in g.rules.iter().enumerate() {
            if self.rules[r] == 0 {
                out.push(format!("{}: never used", rule.name));
                continue;
            }
            crate::ebnf::visit(&rule.body, &mut |e| {
                if self.nodes[e.id] == 0 {
                    return;
                }
                for (i, (what, hit)) in self.branches_of(e).into_iter().enumerate() {
                    if !hit {
                        let what = what.replace("{}", &(i + 1).to_string());
                        out.push(format!("{} (line {}): {what}", rule.name, rule.line));
                    }
                }
            });
        }
        out
    }

    /// The branches of node `e`, each with whether the generator took it.
    fn branches_of(&self, e: &Expr) -> Vec<(&'static str, bool)> {
        let taken = |x: &Expr| self.nodes[x.id] > 0;
        match &e.kind {
            ExprKind::Alt(xs) => {
                xs.iter().map(|x| ("alternative {} never taken", taken(x))).collect()
            }
            ExprKind::Opt(x) => vec![
                ("an optional part never taken", taken(x)),
                ("an optional part never left out", self.skipped[e.id] > 0),
            ],
            ExprKind::Star(_) => vec![
                ("a `*` never empty", self.skipped[e.id] > 0),
                ("a repetition never repeated", self.repeated[e.id] > 0),
            ],
            ExprKind::Plus(_) => vec![("a repetition never repeated", self.repeated[e.id] > 0)],
            ExprKind::Term(_) | ExprKind::Rule(_) | ExprKind::Seq(_) => Vec::new(),
        }
    }

    /// How many of the [`Coverage::branches`] the generator took.
    pub fn hits(&self, g: &Grammar) -> usize {
        let mut n = 0;
        for (r, rule) in g.rules.iter().enumerate() {
            n += usize::from(self.rules[r] > 0);
            crate::ebnf::visit(&rule.body, &mut |e| {
                n += self.branches_of(e).iter().filter(|(_, hit)| *hit).count();
            });
        }
        n
    }

    /// How many rules, alternatives, optional parts and repetitions the grammar has, for the
    /// coverage report (a `?` counts twice, taken and left out, and so does a `*`, left out and
    /// repeated).
    pub fn branches(g: &Grammar) -> usize {
        let mut n = g.rules.len();
        for rule in &g.rules {
            crate::ebnf::visit(&rule.body, &mut |e| {
                n += match &e.kind {
                    ExprKind::Alt(xs) => xs.len(),
                    ExprKind::Opt(_) | ExprKind::Star(_) => 2,
                    ExprKind::Plus(_) => 1,
                    _ => 0,
                };
            });
        }
        n
    }
}

/// Rendered source text and what the lexer must make of it.
#[derive(Clone, Debug)]
pub struct Rendered {
    pub text: String,
    /// The tokens the text must lex to, NEWLINEs and EOF left out.
    pub tokens: Vec<TokenKind>,
    /// Grammar NEWLINEs the lexer turns into whitespace here (L17).
    pub dropped_newlines: u32,
}

pub struct Generator<'g> {
    g: &'g Grammar,
    config: GenConfig,
    /// The fewest tokens each expression node can derive, by [`crate::ebnf::ExprId`].
    cost: Vec<u32>,
    /// `toward[t][e]`: the fewest rule expansions from expression node `e` to an expansion of
    /// rule `t` (`u32::MAX` if `e` can't reach it).
    toward: Vec<Vec<u32>>,
    /// The possible goals: (rule, expression node in its body).
    goals: Vec<(RuleId, ExprId)>,
    /// Each expression node's parent within its rule body.
    parent: Vec<Option<ExprId>>,
    /// Pairs of tokens that lex as something else when written with nothing between them.
    no_glue: HashSet<(TokenKind, TokenKind)>,
    terminals: Vec<Terminal>,
}

const IDENTS: &[&str] = &[
    "a", "b", "x", "y", "n", "i", "t", "foo", "bar_baz", "T", "U", "Vec3", "f32", "u32", "vec3",
    "_tmp", "x1", "self_", "iff", "format", "Option", "len", "Self_", "_0",
];
const SUFFIXES: &[&str] = &["cm", "m", "kg", "deg", "px", "ms"];
const STRINGS: &[&str] = &["\"hi\"", "\"\"", "\"a \\\"q\\\" b\"", "\"tab\\t\\n\"", "\"\\\\\""];
const FSTRINGS: &[&str] = &["f\"hi\"", "f\"\"", "f\"{{x}}\"", "f\"a\\tb\""];
const FSTRING_HEADS: &[&str] = &["f\"{", "f\"w: {", "f\"{{{"];
const FSTRING_MIDS: &[&str] = &["}{", "} and {", ":.1}, {", ":>4}{"];
const FSTRING_TAILS: &[&str] = &["}\"", "} kg\"", ":.2}\"", ":08}}}\""];

/// A name, keyword, number or `_`: two of them written together lex as one token.
pub(crate) fn is_word(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Ident
            | TokenKind::Int
            | TokenKind::Float
            | TokenKind::Suffixed
            | TokenKind::Underscore
    ) || k.is_keyword()
}

/// Whether `a` then `b`, written with nothing between them, lex as exactly those two tokens.
/// An f-string's pieces are lexed where they'd be (L22): what's around them opens and closes
/// the holes they need.
fn lexes_apart(a: TokenKind, a_text: &str, b: TokenKind, b_text: &str) -> bool {
    use TokenKind as T;
    let opens = |k: TokenKind| matches!(k, T::FStringHead | T::FStringMid);
    let closes = |k: TokenKind| matches!(k, T::FStringMid | T::FStringTail);
    // The holes the pair closes without opening (`need`), and those it leaves open.
    let (mut need, mut open) = (0usize, 0usize);
    for k in [a, b] {
        if closes(k) {
            if open == 0 {
                need += 1;
            } else {
                open -= 1;
            }
        }
        if opens(k) {
            open += 1;
        }
    }
    let mut pre = "f\"{".repeat(need);
    let mut pre_k = vec![T::FStringHead; need];
    if closes(a) {
        pre.push('x');
        pre_k.push(T::Ident);
    } else if need > 0 && a == T::RBrace {
        // In a hole, a `}` closes a `{` opened there (a space keeps it from reading `{{`).
        pre.push_str(" {");
        pre_k.push(T::LBrace);
    }
    let (mut post, mut post_k): (String, Vec<TokenKind>) = (String::new(), Vec::new());
    if opens(b) {
        post.push('x');
        post_k.push(T::Ident);
    } else if open > 0 {
        // A bracket `b` opens in a hole closes before it.
        let close = match b {
            T::LParen => Some((")", T::RParen)),
            T::LBracket => Some(("]", T::RBracket)),
            T::LBrace => Some(("}", T::RBrace)),
            _ => None,
        };
        if let Some((text, k)) = close {
            post.push_str(text);
            post_k.push(k);
        }
    }
    for _ in 0..open {
        post.push_str("}\"");
        post_k.push(T::FStringTail);
    }
    let src = format!("{pre}{a_text}{b_text}{post}");
    let lexed = wrela_syntax::lex(FileId(0), &src);
    let kinds: Vec<TokenKind> =
        lexed.tokens.iter().map(|t| t.kind).filter(|k| *k != TokenKind::Eof).collect();
    let expected: Vec<TokenKind> = pre_k.into_iter().chain([a, b]).chain(post_k).collect();
    lexed.diagnostics.is_empty() && kinds == expected && lexed.comments.is_empty()
}

/// Representative texts of a token kind: for the classes, one of each form that lexes
/// differently next to a neighbour (a `.` in a FLOAT or SUFFIXED, a radix prefix, an exponent).
fn sample_texts(k: TokenKind) -> &'static [&'static str] {
    match k {
        TokenKind::Ident => &["a", "_a", "e1", "f"],
        TokenKind::Int => &["1", "0x1F", "1_0", "0b1"],
        TokenKind::Float => &["1.5", "1e5", "1.5e-3"],
        TokenKind::Suffixed => &["1cm", "1.5cm"],
        TokenKind::Str => &["\"s\""],
        TokenKind::FString => &["f\"s\""],
        TokenKind::FStringHead => &["f\"{"],
        TokenKind::FStringMid => &["}{", ":.1}{"],
        TokenKind::FStringTail => &["}\"", ":.1}\""],
        _ => &[],
    }
}

/// Every token kind but NEWLINE and EOF.
fn all_kinds() -> impl Iterator<Item = TokenKind> {
    let all = TokenKind::CLASSES.iter().chain(TokenKind::KEYWORDS).chain(TokenKind::PUNCT);
    all.copied().filter(|k| !matches!(k, TokenKind::Newline | TokenKind::Eof))
}

/// The pairs of token kinds that can't be written next to each other without a space: two words
/// (names, keywords, numbers, `_`), a number then `.` (`1.5` is one FLOAT), and every pair the
/// lexer reads differently when joined (`-` `>`, `/` `/`, `.` `.`, …).
pub fn no_glue_pairs() -> HashSet<(TokenKind, TokenKind)> {
    let kinds: Vec<(TokenKind, Vec<&str>)> = all_kinds()
        .map(|k| (k, k.fixed_text().map_or_else(|| sample_texts(k).to_vec(), |t| vec![t])))
        .collect();
    let mut out = HashSet::new();
    for (a, a_texts) in &kinds {
        for (b, b_texts) in &kinds {
            // The cheap tests first: the last one lexes every pair of texts.
            if (is_word(*a) && is_word(*b))
                || (a.is_number() && *b == TokenKind::Dot)
                || a_texts.iter().any(|x| b_texts.iter().any(|y| !lexes_apart(*a, x, *b, y)))
            {
                out.insert((*a, *b));
            }
        }
    }
    out
}

impl<'g> Generator<'g> {
    pub fn new(g: &'g Grammar, config: GenConfig) -> Generator<'g> {
        let rule_len = g.min_lengths();
        let mut cost = vec![0; g.expr_count()];
        for r in &g.rules {
            crate::ebnf::visit(&r.body, &mut |e| cost[e.id] = expr_min_len(e, &rule_len));
        }
        let terminals = g.terminals();
        let toward = (0..g.rules.len()).map(|t| distances_to(g, t)).collect();
        let mut goals = Vec::new();
        let mut parent = vec![None; g.expr_count()];
        for (r, rule) in g.rules.iter().enumerate() {
            goals.push((r, rule.body.id));
            crate::ebnf::visit(&rule.body, &mut |e| match &e.kind {
                ExprKind::Alt(xs) => {
                    for x in xs {
                        goals.push((r, x.id));
                        parent[x.id] = Some(e.id);
                    }
                }
                ExprKind::Seq(xs) => xs.iter().for_each(|x| parent[x.id] = Some(e.id)),
                ExprKind::Opt(x) | ExprKind::Star(x) | ExprKind::Plus(x) => {
                    goals.push((r, e.id));
                    parent[x.id] = Some(e.id);
                }
                ExprKind::Term(_) | ExprKind::Rule(_) => {}
            });
        }
        Generator { g, config, cost, toward, goals, parent, no_glue: no_glue_pairs(), terminals }
    }

    /// A random derivation of `file`: the grammar's terminals, EOF last. A derivation that no
    /// text can spell ([`renderable`]: a `:` or `}` where an f-string's hole would end) is
    /// drawn again, and only the one kept counts toward `cov`.
    pub fn generate(&self, rng: &mut Rng, cov: &mut Coverage) -> Vec<Terminal> {
        loop {
            let mut c = Coverage::new(self.g);
            let terms = self.generate_once(rng, &mut c);
            if renderable(&terms) {
                cov.merge(&c);
                return terms;
            }
        }
    }

    fn generate_once(&self, rng: &mut Rng, cov: &mut Coverage) -> Vec<Terminal> {
        let (lo, hi) = self.config.budget;
        let (rule, node) = *rng.pick(&self.goals);
        let path = std::iter::successors(Some(node), |&e| self.parent[e]).collect();
        let mut w = Walk {
            budget: lo + rng.below(hi - lo + 1),
            goal: Some(Goal { rule, node, path }),
            exploring: false,
            repeat_here: None,
            out: Vec::new(),
        };
        self.rule(self.g.start, 0, true, &mut w, rng, cov);
        w.out
    }

    /// Expands rule `r`. `route` says whether this expansion is on the way to the goal: only
    /// one part of a sequence is (the nearest), so the goal is approached along a single path
    /// instead of by every part at once.
    fn rule(
        &self,
        r: RuleId,
        depth: u32,
        route: bool,
        w: &mut Walk,
        rng: &mut Rng,
        cov: &mut Coverage,
    ) {
        cov.rules[r] += 1;
        self.expr(&self.g.rules[r].body, depth + 1, route, w, rng, cov);
    }

    /// How far `e` is from the goal, if it's on the route and can reach it: 0 for the goal and
    /// the parts of its rule's body around it, otherwise one more than the rule expansions it
    /// takes to get into the goal's rule.
    fn distance(&self, w: &Walk, e: &Expr, route: bool) -> Option<u32> {
        let goal = w.goal.as_ref().filter(|_| route)?;
        if goal.path.contains(&e.id) {
            return Some(0);
        }
        let d = self.toward[goal.rule][e.id];
        (d != u32::MAX).then(|| d + 1)
    }

    /// Whether to head for the goal through `e` at this choice: always once the budget is
    /// spent (so the goal is still reached, by the shortest way), otherwise usually.
    fn seek(&self, w: &Walk, e: &Expr, route: bool, depth: u32, rng: &mut Rng) -> bool {
        self.distance(w, e, route).is_some()
            && depth < self.config.max_depth
            && (w.out.len() >= w.budget || rng.chance(GOAL_PULL))
    }

    fn expr(
        &self,
        e: &Expr,
        depth: u32,
        route: bool,
        w: &mut Walk,
        rng: &mut Rng,
        cov: &mut Coverage,
    ) {
        if route && w.goal.as_ref().is_some_and(|g| g.node == e.id) {
            // Reached: give the goal room to be generated in full, not just its cheapest form,
            // and explore its optional and repeated parts more than usual.
            w.goal = None;
            w.budget = w.budget.max(w.out.len() + GOAL_ROOM);
            if matches!(e.kind, ExprKind::Star(_) | ExprKind::Plus(_)) {
                w.repeat_here = Some(e.id);
            }
            let outer = std::mem::replace(&mut w.exploring, true);
            self.expr(e, depth, false, w, rng, cov);
            w.exploring = outer;
            return;
        }
        cov.nodes[e.id] += 1;
        let free = |w: &Walk| w.out.len() < w.budget && depth < self.config.max_depth;
        let (optional, repeat) = if w.exploring {
            (EXPLORE, EXPLORE)
        } else {
            (self.config.optional, self.config.repeat)
        };
        match &e.kind {
            ExprKind::Term(t) => w.out.push(*t),
            ExprKind::Rule(r) => self.rule(*r, depth, route, w, rng, cov),
            ExprKind::Seq(xs) => {
                // The route goes through the nearest part; the others are generated as usual.
                let nearest = self.distance(w, e, route).and_then(|best| {
                    xs.iter().position(|x| self.distance(w, x, true) == Some(best))
                });
                for (i, x) in xs.iter().enumerate() {
                    self.expr(x, depth, nearest == Some(i), w, rng, cov);
                }
            }
            ExprKind::Alt(xs) => {
                let pick = if self.seek(w, e, route, depth, rng) {
                    let best = self.distance(w, e, route);
                    let nearest: Vec<usize> =
                        (0..xs.len()).filter(|&i| self.distance(w, &xs[i], true) == best).collect();
                    *rng.pick(&nearest)
                } else if free(w) {
                    rng.below(xs.len())
                } else {
                    let min = xs.iter().map(|x| self.cost[x.id]).min().unwrap_or(0);
                    let cheapest: Vec<usize> =
                        (0..xs.len()).filter(|&i| self.cost[xs[i].id] == min).collect();
                    *rng.pick(&cheapest)
                };
                // The route goes on through the choice as long as the goal is reachable from it.
                let on_route = self.distance(w, &xs[pick], route).is_some();
                self.expr(&xs[pick], depth, on_route, w, rng, cov);
            }
            ExprKind::Opt(x) => {
                if self.seek(w, x, route, depth, rng) || (free(w) && rng.chance(optional)) {
                    let on_route = self.distance(w, x, route).is_some();
                    self.expr(x, depth, on_route, w, rng, cov);
                } else {
                    cov.skipped[e.id] += 1;
                }
            }
            ExprKind::Star(x) | ExprKind::Plus(x) => {
                let mut n = 0;
                let plus = matches!(e.kind, ExprKind::Plus(_));
                // A goal repetition repeats.
                let is_goal = w.repeat_here.take_if(|id| *id == e.id).is_some();
                if is_goal
                    || plus
                    || self.seek(w, x, route, depth, rng)
                    || (free(w) && rng.chance(repeat))
                {
                    let on_route = self.distance(w, x, route).is_some();
                    self.expr(x, depth, on_route, w, rng, cov);
                    n += 1;
                    while (is_goal && n < 2) || (free(w) && rng.chance(repeat)) {
                        self.expr(x, depth, false, w, rng, cov);
                        n += 1;
                    }
                }
                match n {
                    0 => cov.skipped[e.id] += 1,
                    1 => {}
                    _ => cov.repeated[e.id] += 1,
                }
            }
        }
    }

    /// A random small edit of a derivation (delete, insert, replace or swap a token), for
    /// checking that the two parsers also agree on near-miss programs, most of them invalid.
    /// Only edits that a text can spell ([`renderable`]) are made.
    pub fn mutate(&self, terms: &[Terminal], rng: &mut Rng) -> Vec<Terminal> {
        loop {
            let t = self.mutate_once(terms, rng);
            if renderable(&t) {
                return t;
            }
        }
    }

    fn mutate_once(&self, terms: &[Terminal], rng: &mut Rng) -> Vec<Terminal> {
        let mut t = terms.to_vec();
        let body = t.len() - 1; // EOF stays last
        let random = |rng: &mut Rng| loop {
            let x = *rng.pick(&self.terminals);
            if x != Terminal::Token(TokenKind::Eof) {
                return x;
            }
        };
        match rng.below(4) {
            0 if body > 0 => {
                t.remove(rng.below(body));
            }
            1 => {
                let x = random(rng);
                t.insert(rng.below(body + 1), x);
            }
            2 if body > 0 => {
                let i = rng.below(body);
                t[i] = random(rng);
            }
            _ if body > 1 => {
                let i = rng.below(body - 1);
                t.swap(i, i + 1);
            }
            _ => {
                let x = random(rng);
                t.insert(0, x);
            }
        }
        t
    }

    fn invent(&self, k: TokenKind, rng: &mut Rng) -> String {
        match k {
            TokenKind::Ident => {
                if rng.chance(0.7) {
                    return rng.pick(IDENTS).to_string();
                }
                loop {
                    let len = 1 + rng.below(6);
                    let mut s = String::new();
                    for i in 0..len {
                        let set: &[u8] = if i == 0 {
                            b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_"
                        } else {
                            b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789"
                        };
                        s.push(*rng.pick(set) as char);
                    }
                    if s != "_" && TokenKind::keyword(&s).is_none() {
                        return s;
                    }
                }
            }
            // Any value: a tuple index too large for a `u32` is the parser's limit (E0006),
            // which agreement leaves out.
            TokenKind::Int => match rng.below(7) {
                0 => rng.below(10).to_string(),
                1 => rng.below(1000).to_string(),
                2 => rng.next_u64().to_string(),
                3 => format!("{}_{:03}", 1 + rng.below(999), rng.below(1000)),
                4 => format!("0x{:X}", rng.below(1 << 16)),
                5 => format!("0b{:b}", rng.below(64)),
                _ => format!("0o{:o}", rng.below(512)),
            },
            TokenKind::Float => match rng.below(5) {
                0 => format!("{}.{}", rng.below(100), rng.below(100)),
                1 => format!("{}e{}", 1 + rng.below(9), rng.below(20)),
                2 => format!("{}.{}e-{}", rng.below(10), rng.below(10), rng.below(9)),
                3 => format!("{}E+{}", rng.below(10), rng.below(9)),
                _ => format!("1_0{}.5", rng.below(10)),
            },
            TokenKind::Suffixed => {
                let n = if rng.chance(0.5) {
                    rng.below(100).to_string()
                } else {
                    format!("{}.5", rng.below(10))
                };
                format!("{n}{}", rng.pick(SUFFIXES))
            }
            TokenKind::Str => rng.pick(STRINGS).to_string(),
            TokenKind::FString => rng.pick(FSTRINGS).to_string(),
            TokenKind::FStringHead => rng.pick(FSTRING_HEADS).to_string(),
            TokenKind::FStringMid => rng.pick(FSTRING_MIDS).to_string(),
            TokenKind::FStringTail => rng.pick(FSTRING_TAILS).to_string(),
            k => k.fixed_text().unwrap_or_default().to_string(),
        }
    }

    /// Renders a terminal sequence (EOF last) as source text.
    pub fn render(&self, terms: &[Terminal], rng: &mut Rng) -> Rendered {
        let mut r = Renderer {
            g: self,
            text: String::new(),
            tokens: Vec::new(),
            dropped: 0,
            expected: Vec::new(),
            brackets: Brackets::default(),
            gt_run: None,
            newline_pending: false,
            holes: Vec::new(),
        };
        for (i, &t) in terms.iter().enumerate() {
            match t {
                Terminal::Token(TokenKind::Eof) => break,
                Terminal::Token(TokenKind::Newline) => {
                    let next = terms[i + 1..]
                        .iter()
                        .map(|t| t.lexer_kind())
                        .find(|k| *k != TokenKind::Newline)
                        .unwrap_or(TokenKind::Eof);
                    r.newline(next, rng);
                }
                t => r.token(t, rng),
            }
        }
        let expected = r.expected;
        let rendered = Rendered { text: r.text, tokens: r.tokens, dropped_newlines: r.dropped };
        let lexed = wrela_syntax::lex(FileId(0), &rendered.text);
        let got: Vec<TokenKind> =
            lexed.tokens.iter().map(|t| t.kind).filter(|k| *k != TokenKind::Eof).collect();
        assert!(
            got == expected && lexed.diagnostics.is_empty(),
            "the renderer wrote text that doesn't lex to the tokens it meant:\n{}\nmeant: {expected:?}\ngot:   {got:?}\n{:?}",
            rendered.text,
            lexed.diagnostics
        );
        rendered
    }
}

/// The chance that a choice heads for the program's goal rather than being uniform.
const GOAL_PULL: f64 = 0.7;
/// The tokens a program may grow by once it reaches its goal.
const GOAL_ROOM: usize = 24;
/// The chance of taking an optional part, and of each further repetition, in the goal.
const EXPLORE: f64 = 0.6;

/// One derivation in progress.
struct Walk {
    budget: usize,
    /// What this program heads for until it has generated it.
    goal: Option<Goal>,
    /// Generating the goal.
    exploring: bool,
    /// The goal, when it is a repetition about to be generated: it repeats at least twice.
    repeat_here: Option<ExprId>,
    out: Vec<Terminal>,
}

struct Goal {
    rule: RuleId,
    node: ExprId,
    /// `node` and its ancestors in the rule's body.
    path: Vec<ExprId>,
}

/// For each expression node: the fewest rule expansions it takes to expand rule `target`.
fn distances_to(g: &Grammar, target: RuleId) -> Vec<u32> {
    fn node(e: &Expr, rule_dist: &[u32]) -> u32 {
        match &e.kind {
            ExprKind::Term(_) => u32::MAX,
            ExprKind::Rule(r) => rule_dist[*r],
            ExprKind::Seq(xs) | ExprKind::Alt(xs) => {
                xs.iter().map(|x| node(x, rule_dist)).min().unwrap_or(u32::MAX)
            }
            ExprKind::Opt(x) | ExprKind::Star(x) | ExprKind::Plus(x) => node(x, rule_dist),
        }
    }
    let mut rule_dist = vec![u32::MAX; g.rules.len()];
    rule_dist[target] = 0;
    loop {
        let mut changed = false;
        for (r, rule) in g.rules.iter().enumerate() {
            let d = node(&rule.body, &rule_dist).saturating_add(1);
            if d < rule_dist[r] {
                rule_dist[r] = d;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut out = vec![u32::MAX; g.expr_count()];
    for rule in &g.rules {
        crate::ebnf::visit(&rule.body, &mut |e| out[e.id] = node(e, &rule_dist));
    }
    out
}

struct Renderer<'a, 'g> {
    g: &'a Generator<'g>,
    text: String,
    tokens: Vec<TokenKind>,
    dropped: u32,
    /// The tokens the text must lex to: `tokens`, with the NEWLINEs the lexer must make.
    expected: Vec<TokenKind>,
    /// Open brackets, as the lexer tracks them (L19).
    brackets: Brackets,
    /// The kind of the last token (`>` or `>>`) when it ends with a GT_CLOSE that nothing
    /// follows yet, so the next `>`, `=`, `>=` or GT_CLOSE may be glued to it as L16 allows.
    gt_run: Option<TokenKind>,
    /// A line break has been written since the last token.
    newline_pending: bool,
    /// The f-string holes open, innermost last, each with the brackets open in it: a hole is
    /// on one line, so nothing in it breaks a line (L22).
    holes: Vec<u32>,
}

impl Renderer<'_, '_> {
    fn prev(&self) -> Option<TokenKind> {
        self.tokens.last().copied()
    }

    /// Whether a line break before a token of kind `next` would be plain whitespace (L17).
    fn break_is_whitespace(&self, next: TokenKind) -> bool {
        self.prev().is_none_or(|prev| !line_break_is_newline(self.brackets.in_brace(), prev, next))
    }

    fn line_break(&mut self, rng: &mut Rng) {
        if rng.chance(0.15) {
            self.text.push_str(if rng.chance(0.2) { " /// doc" } else { " // note: a + b" });
        }
        self.text.push('\n');
        if rng.chance(0.1) {
            self.text.push_str(if rng.chance(0.5) { "\n" } else { "    // own line\n" });
        }
        for _ in 0..self.brackets.depth() {
            self.text.push_str("    ");
        }
        self.newline_pending = true;
        self.gt_run = None;
    }

    /// Writes a grammar NEWLINE as a line break. `next` is the kind of the next token that
    /// isn't a NEWLINE (EOF if there is none). In an f-string's hole there's no line break to
    /// write: the NEWLINE is dropped.
    fn newline(&mut self, next: TokenKind, rng: &mut Rng) {
        if !self.holes.is_empty() {
            self.dropped += 1;
            return;
        }
        if self.newline_pending {
            // L18: line breaks with no token between them make at most one NEWLINE.
        } else if self.break_is_whitespace(next) {
            self.dropped += 1;
        } else {
            self.expected.push(TokenKind::Newline);
        }
        self.line_break(rng);
    }

    fn token(&mut self, t: Terminal, rng: &mut Rng) {
        let kind = t.lexer_kind();
        let text = self.g.invent(kind, rng);
        if let Some(run) = self.gt_run.take()
            && !self.newline_pending
            && let Some(joined) = glued_gt(run, kind)
            && rng.chance(0.5)
        {
            // L16: the parser splits `joined` again.
            self.text.push_str(&text);
            if let Some(last) = self.tokens.last_mut() {
                *last = joined;
            }
            if let Some(last) = self.expected.last_mut() {
                *last = joined;
            }
            self.gt_run = (t == Terminal::GtClose).then_some(joined);
            return;
        }
        if let Some(prev) = self.prev()
            && !self.newline_pending
        {
            let glue = !self.g.no_glue.contains(&(prev, kind)) && rng.chance(0.5);
            if !glue {
                if self.holes.is_empty() && self.break_is_whitespace(kind) && rng.chance(0.06) {
                    self.line_break(rng);
                } else {
                    self.text.push(' ');
                }
            }
        }
        self.text.push_str(&text);
        self.tokens.push(kind);
        self.expected.push(kind);
        self.newline_pending = false;
        self.gt_run = (t == Terminal::GtClose).then_some(TokenKind::Gt);
        self.brackets.track(kind);
        track_hole(&mut self.holes, kind);
    }
}

/// Follows f-string holes through a token (L22): returns false where the lexer would read the
/// token otherwise, a `:` or `}` outside brackets in a hole (which end it), or an FSTRING_MID
/// or FSTRING_TAIL outside one.
fn track_hole(holes: &mut Vec<u32>, k: TokenKind) -> bool {
    use TokenKind as T;
    match k {
        T::FStringHead => holes.push(0),
        T::FStringMid | T::FStringTail => {
            if holes.last() != Some(&0) {
                return false;
            }
            if k == T::FStringTail {
                holes.pop();
            }
        }
        _ => {
            if let Some(d) = holes.last_mut() {
                match k {
                    T::LParen | T::LBracket | T::LBrace => *d += 1,
                    T::RBrace | T::Colon if *d == 0 => return false,
                    T::RParen | T::RBracket | T::RBrace => *d = d.saturating_sub(1),
                    _ => {}
                }
            }
        }
    }
    true
}

/// Whether a text can spell this sequence of terminals: every f-string's hole closes, and
/// nothing in one would end it early ([`track_hole`]).
pub fn renderable(terms: &[Terminal]) -> bool {
    let mut holes = Vec::new();
    for t in terms {
        let k = t.lexer_kind();
        if k == TokenKind::Eof {
            break;
        }
        if !track_hole(&mut holes, k) {
            return false;
        }
    }
    holes.is_empty()
}

/// A token `run` that ends with a GT_CLOSE (`>` or `>>`) joined with a following token `next`,
/// when the result is one token that L16 splits back into them: `>` `>` → `>>`, `>` `=` → `>=`,
/// `>` `>=` and `>>` `=` → `>>=`.
pub(crate) fn glued_gt(run: TokenKind, next: TokenKind) -> Option<TokenKind> {
    use TokenKind::{Eq, Ge, Gt, Shr, ShrEq};
    match (run, next) {
        (Gt, Gt) => Some(Shr),
        (Gt, Eq) => Some(Ge),
        (Gt, Ge) | (Shr, Eq) => Some(ShrEq),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glue_rules_match_the_lexer() {
        let pairs = no_glue_pairs();
        for (a, b) in [
            (TokenKind::Minus, TokenKind::Gt),
            (TokenKind::Slash, TokenKind::Slash),
            (TokenKind::Dot, TokenKind::Dot),
            (TokenKind::Ident, TokenKind::Fn),
            (TokenKind::Int, TokenKind::Dot),
            (TokenKind::Underscore, TokenKind::Ident),
            (TokenKind::Colon, TokenKind::Colon),
            (TokenKind::Gt, TokenKind::Ge),
        ] {
            assert!(pairs.contains(&(a, b)), "{a:?} {b:?}");
        }
        assert!(pairs.contains(&(TokenKind::Dot, TokenKind::Suffixed)), "`.` `1.5cm`");
        for (a, b) in [
            (TokenKind::Ident, TokenKind::LParen),
            (TokenKind::Dot, TokenKind::Int),
            (TokenKind::Minus, TokenKind::Int),
        ] {
            assert!(!pairs.contains(&(a, b)), "{a:?} {b:?}");
        }
    }

    #[test]
    fn generated_programs_render_faithfully() {
        let g = Grammar::spec();
        let gen_ = Generator::new(&g, GenConfig::default());
        let mut cov = Coverage::new(&g);
        let mut sizes = 0;
        for seed in 0..200 {
            let mut rng = Rng::new(seed);
            let terms = gen_.generate(&mut rng, &mut cov);
            assert_eq!(terms.last(), Some(&Terminal::Token(TokenKind::Eof)));
            sizes += terms.len();
            gen_.render(&terms, &mut rng); // panics if the text lexes wrong
            let m = gen_.mutate(&terms, &mut rng);
            gen_.render(&m, &mut rng);
        }
        assert!(sizes / 200 < 400, "programs are too big on average: {}", sizes / 200);
    }
}
