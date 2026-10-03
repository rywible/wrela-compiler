//! The formatter's layout engine: Wadler's "prettier printer", much as Prettier implements it.
//! The formatter describes its output as a [`Doc`] of text, line breaks, indentation and
//! groups, and [`print`] lays it out: a group goes on one line if it fits in the width, and
//! breaks its lines otherwise.

/// A document to lay out.
#[derive(Clone, Debug)]
pub enum Doc {
    /// Text without line breaks.
    Text(String),
    /// A space, or a line break if its group breaks.
    Line,
    /// Nothing, or a line break if its group breaks.
    SoftLine,
    /// A line break, always. It doesn't break its group: a block's lines inside a call's
    /// arguments leave the arguments on one line (`f(|x| {`, the body, `})`).
    HardLine,
    /// The first if its group breaks, the second if not (a trailing comma).
    IfBreak(Box<Doc>, Box<Doc>),
    /// Indents the lines its group breaks by one level.
    Indent(Box<Doc>),
    Group(Box<Doc>, Fit),
    Concat(Vec<Doc>),
    /// Text for the end of the line, before its break: a trailing comment.
    LineSuffix(String),
    /// Breaks the groups around it, up to a block: a comment that ends its line can't share
    /// it with what follows.
    BreakParent,
}

/// When a group breaks, and how the groups around it see it when they decide whether they fit.
///
/// Formatting has to be idempotent, and the output records some choices in the source (a list
/// that broke is written over several lines, so it's multi-line on the next pass; a block
/// that broke isn't on one line any more). So how a group looks to the groups around it must
/// not depend on those choices: a group is measured the same whichever way it went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Breaks when it doesn't fit. Measured on one line.
    IfNeeded,
    /// Always breaks (the author wrote it over several lines). Measured on one line all the
    /// same, as it was when its own break was decided.
    Never,
    /// Always breaks: a sequence of statements, one per line. A comment inside doesn't break
    /// the groups around it, since the comment's line ends inside.
    Block,
    /// Breaks when it doesn't fit, but measured as broken: up to its first line break (a block
    /// that can stay on one line, measured up to its `{` as a block that can't is).
    Alone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

pub fn text(s: impl Into<String>) -> Doc {
    Doc::Text(s.into())
}

pub fn concat(parts: impl IntoIterator<Item = Doc>) -> Doc {
    Doc::Concat(parts.into_iter().collect())
}

pub fn nil() -> Doc {
    Doc::Concat(Vec::new())
}

pub fn indent(d: Doc) -> Doc {
    Doc::Indent(Box::new(d))
}

/// A group that breaks if it doesn't fit, or if a [`Doc::BreakParent`] in it says so.
pub fn group(d: Doc) -> Doc {
    let fit = if d.breaks_parent() { Fit::Never } else { Fit::IfNeeded };
    Doc::Group(Box::new(d), fit)
}

/// A group that always breaks.
pub fn broken(d: Doc) -> Doc {
    Doc::Group(Box::new(d), Fit::Never)
}

/// A block of statements: always broken, and a comment inside breaks nothing outside.
pub fn block(d: Doc) -> Doc {
    Doc::Group(Box::new(d), Fit::Block)
}

/// A group that breaks if it doesn't fit, measured by the groups around it as broken.
pub fn alone(d: Doc) -> Doc {
    if d.breaks_parent() { broken(d) } else { Doc::Group(Box::new(d), Fit::Alone) }
}

pub fn if_break(broken: Doc, flat: Doc) -> Doc {
    Doc::IfBreak(Box::new(broken), Box::new(flat))
}

impl Doc {
    /// Whether a [`Doc::BreakParent`] reaches the group around this.
    fn breaks_parent(&self) -> bool {
        match self {
            Doc::BreakParent => true,
            Doc::Group(_, Fit::Block) => false,
            Doc::Group(d, Fit::IfNeeded | Fit::Never | Fit::Alone) | Doc::Indent(d) => {
                d.breaks_parent()
            }
            Doc::IfBreak(a, b) => a.breaks_parent() || b.breaks_parent(),
            Doc::Concat(v) => v.iter().any(Doc::breaks_parent),
            Doc::Text(_) | Doc::Line | Doc::SoftLine | Doc::HardLine | Doc::LineSuffix(_) => false,
        }
    }

    /// Whether it always has a line break or a trailing comment in it.
    pub fn has_hard_line(&self) -> bool {
        match self {
            Doc::HardLine | Doc::LineSuffix(_) | Doc::BreakParent => true,
            Doc::Group(d, _) | Doc::Indent(d) => d.has_hard_line(),
            Doc::IfBreak(a, b) => a.has_hard_line() || b.has_hard_line(),
            Doc::Concat(v) => v.iter().any(Doc::has_hard_line),
            Doc::Text(_) | Doc::Line | Doc::SoftLine => false,
        }
    }
}

/// The columns one level of indentation adds.
pub const INDENT: usize = 4;

/// Lays a document out in `width` columns.
pub fn print(doc: &Doc, width: usize) -> String {
    let mut p = Printer { out: String::new(), col: 0, suffix: Vec::new() };
    let mut stack: Vec<(usize, Mode, &Doc)> = vec![(0, Mode::Break, doc)];
    while let Some((ind, mode, d)) = stack.pop() {
        match d {
            Doc::Text(s) => {
                p.out.push_str(s);
                p.col += s.chars().count();
            }
            Doc::Concat(v) => stack.extend(v.iter().rev().map(|x| (ind, mode, x))),
            Doc::Indent(x) => {
                stack.push((if mode == Mode::Break { ind + 1 } else { ind }, mode, x))
            }
            Doc::Group(x, fit) => {
                let m = match (fit, mode) {
                    (Fit::Never | Fit::Block, _) => Mode::Break,
                    (Fit::IfNeeded | Fit::Alone, Mode::Flat) => Mode::Flat,
                    (Fit::IfNeeded | Fit::Alone, Mode::Break) => {
                        let room = width as isize - p.col as isize;
                        if fits(x, &stack, room) { Mode::Flat } else { Mode::Break }
                    }
                };
                stack.push((ind, m, x));
            }
            Doc::Line => match mode {
                Mode::Flat => {
                    p.out.push(' ');
                    p.col += 1;
                }
                Mode::Break => p.newline(ind),
            },
            Doc::SoftLine => {
                if mode == Mode::Break {
                    p.newline(ind);
                }
            }
            Doc::HardLine => p.newline(ind),
            Doc::IfBreak(b, f) => stack.push((ind, mode, if mode == Mode::Break { b } else { f })),
            Doc::LineSuffix(s) => p.suffix.push(s),
            Doc::BreakParent => {}
        }
    }
    p.flush_suffix();
    p.out
}

struct Printer<'d> {
    out: String,
    col: usize,
    suffix: Vec<&'d str>,
}

impl Printer<'_> {
    fn flush_suffix(&mut self) {
        // A trailing comment comes one space after the code: each suffix starts with it.
        if !self.suffix.is_empty() {
            while self.out.ends_with(' ') {
                self.out.pop();
            }
        }
        for s in self.suffix.drain(..) {
            self.out.push_str(s);
        }
    }

    fn newline(&mut self, ind: usize) {
        self.flush_suffix();
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        self.out.push('\n');
        self.out.extend(std::iter::repeat_n(' ', ind * INDENT));
        self.col = ind * INDENT;
    }
}

/// Whether `next`, flat, and what follows it up to the next line break fit in `room` columns.
fn fits(next: &Doc, rest: &[(usize, Mode, &Doc)], mut room: isize) -> bool {
    let mut stack: Vec<(Mode, &Doc)> = vec![(Mode::Flat, next)];
    let mut rest = rest.iter().rev();
    loop {
        let (mode, d) = match stack.pop() {
            Some(x) => x,
            None => match rest.next() {
                Some(&(_, m, d)) => (m, d),
                None => return true,
            },
        };
        match d {
            Doc::Text(s) => {
                room -= s.chars().count() as isize;
                if room < 0 {
                    return false;
                }
            }
            Doc::Concat(v) => stack.extend(v.iter().rev().map(|x| (mode, x))),
            Doc::Indent(x) => stack.push((mode, x)),
            Doc::Group(x, fit) => {
                stack.push((if *fit == Fit::Alone { Mode::Break } else { mode }, x))
            }
            Doc::Line => match mode {
                Mode::Flat => {
                    room -= 1;
                    if room < 0 {
                        return false;
                    }
                }
                Mode::Break => return true,
            },
            Doc::SoftLine => {
                if mode == Mode::Break {
                    return true;
                }
            }
            Doc::HardLine => return true,
            Doc::IfBreak(b, f) => stack.push((mode, if mode == Mode::Break { b } else { f })),
            Doc::LineSuffix(_) | Doc::BreakParent => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(args: &[&str]) -> Doc {
        let mut inner = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                inner.push(text(","));
                inner.push(Doc::Line);
            } else {
                inner.push(Doc::SoftLine);
            }
            inner.push(text(*a));
        }
        inner.push(if_break(text(","), nil()));
        group(concat([text("f("), indent(concat(inner)), Doc::SoftLine, text(")")]))
    }

    #[test]
    fn a_group_breaks_only_when_it_must() {
        assert_eq!(print(&call(&["a", "b"]), 20), "f(a, b)");
        assert_eq!(
            print(&call(&["aaaaaaaa", "bbbbbbbb"]), 16),
            "f(\n    aaaaaaaa,\n    bbbbbbbb,\n)"
        );
    }

    #[test]
    fn a_comment_breaks_its_group() {
        let d = group(concat([
            text("f("),
            indent(concat([
                Doc::SoftLine,
                text("a"),
                text(","),
                Doc::LineSuffix(" // why".into()),
                Doc::BreakParent,
                Doc::Line,
                text("b"),
                if_break(text(","), nil()),
            ])),
            Doc::SoftLine,
            text(")"),
        ]));
        assert_eq!(print(&d, 80), "f(\n    a, // why\n    b,\n)");
    }

    #[test]
    fn hard_lines_in_a_flat_group_keep_its_indent() {
        // `f(|x| {`, the body, `})`: the arguments stay flat around the block's lines.
        let block = broken(concat([
            text("{"),
            indent(concat([Doc::HardLine, text("x")])),
            Doc::HardLine,
            text("}"),
        ]));
        let d = group(concat([
            text("f("),
            indent(concat([Doc::SoftLine, text("|x| "), block])),
            Doc::SoftLine,
            text(")"),
        ]));
        assert_eq!(print(&d, 80), "f(|x| {\n    x\n})");
    }
}
