//! Unit mismatches in what's written (language.md §5): units are constants, not types, so a
//! length and a time are both `f32`s. But where both sides of `+`, `-` or a comparison are
//! built from unit suffixes alone (`2.0m + 3.0s`, `1km < 30deg`), their dimensions are known,
//! and two that differ are an error that names both, as written (E0305). A bare number has no
//! dimension and fits any (`15cm + 0.3` is metres); a variable's is unknown, so it's never
//! checked.

use super::Checker;
use wrela_diag::{Diagnostic, codes};
use wrela_syntax::ast::{self, BinOp, ExprKind, LitKind};

/// Exponents of length, mass and time.
type Dim = [i8; 3];

/// Each unit of `std::units`, and its dimension.
fn unit_dim(unit: &str) -> Option<Dim> {
    Some(match unit {
        "km" | "m" | "cm" | "mm" | "um" => [1, 0, 0],
        "t" | "kg" | "g" | "mg" => [0, 1, 0],
        "h" | "s" | "ms" | "us" => [0, 0, 1],
        "rad" | "deg" => [0, 0, 0],
        "Hz" | "kHz" => [0, 0, -1],
        "N" => [1, 1, -2],
        "J" => [2, 1, -2],
        "W" => [2, 1, -3],
        "Pa" | "kPa" => [-1, 1, -2],
        _ => return None,
    })
}

/// What a dimension is called, for a message.
fn name(d: Dim) -> String {
    match d {
        [1, 0, 0] => "a length".into(),
        [0, 1, 0] => "a mass".into(),
        [0, 0, 1] => "a time".into(),
        [0, 0, 0] => "an angle or a ratio".into(),
        [0, 0, -1] => "a frequency".into(),
        [1, 0, -1] => "a speed".into(),
        [1, 0, -2] => "an acceleration".into(),
        [2, 0, 0] => "an area".into(),
        [3, 0, 0] => "a volume".into(),
        [-3, 1, 0] => "a density".into(),
        [1, 1, -2] => "a force".into(),
        [2, 1, -2] => "an energy".into(),
        [2, 1, -3] => "a power".into(),
        [-1, 1, -2] => "a pressure".into(),
        [l, m, t] => {
            let part = |s: &str, e: i8| match e {
                0 => String::new(),
                1 => s.to_string(),
                e => format!("{s}^{e}"),
            };
            let parts: Vec<String> = [part("m", l), part("kg", m), part("s", t)]
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect();
            format!("a quantity in {}", parts.join("·"))
        }
    }
}

/// An expression's dimension, when it's built from unit suffixes and numbers alone: `None`
/// for anything else, and `Some(None)` for a bare number (which fits any).
fn dim(e: &ast::Expr) -> Option<Option<Dim>> {
    match &e.kind {
        ExprKind::Lit(l) => match l.kind {
            LitKind::Int(_) | LitKind::Float(_) => Some(None),
            LitKind::Suffixed => {
                let (_, suffix) = wrela_syntax::lexer::split_suffix(&l.text);
                unit_dim(suffix).map(Some)
            }
            _ => None,
        },
        ExprKind::Paren(x) | ExprKind::Unary(ast::UnOp::Neg, x) => dim(x),
        ExprKind::Binary(op @ (BinOp::Mul | BinOp::Div), a, b) => {
            let (x, y) = (dim(a)?, dim(b)?);
            Some(match (x, y) {
                (None, None) => None,
                (Some(d), None) => Some(d),
                (None, Some(d)) if *op == BinOp::Mul => Some(d),
                (None, Some(d)) => Some(d.map(|e| -e)),
                (Some(p), Some(q)) => {
                    let s = if *op == BinOp::Mul { 1 } else { -1 };
                    Some([p[0] + s * q[0], p[1] + s * q[1], p[2] + s * q[2]])
                }
            })
        }
        // `m**3`: an integer power.
        ExprKind::Binary(BinOp::Pow, a, b) => {
            let n = match &b.kind {
                ExprKind::Lit(ast::Lit { kind: LitKind::Int(ast::IntValue::Ok(n)), .. }) => {
                    *n as i8
                }
                _ => return None,
            };
            Some(dim(a)?.map(|d| d.map(|e| e * n)))
        }
        ExprKind::Binary(BinOp::Add | BinOp::Sub, a, b) => match (dim(a)?, dim(b)?) {
            (Some(p), Some(q)) if p != q => None,
            (Some(d), _) | (_, Some(d)) => Some(Some(d)),
            (None, None) => Some(None),
        },
        _ => None,
    }
}

/// The expression as written, for the forms `dim` reads: literals exactly as written.
fn written(e: &ast::Expr) -> String {
    match &e.kind {
        ExprKind::Lit(l) => l.text.clone(),
        ExprKind::Paren(x) => format!("({})", written(x)),
        ExprKind::Unary(_, x) => format!("-{}", written(x)),
        ExprKind::Binary(op, a, b) => {
            let o = match op {
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Pow => "**",
                BinOp::Add => "+",
                _ => "-",
            };
            if *op == BinOp::Pow || *op == BinOp::Div {
                format!("{}{o}{}", written(a), written(b))
            } else {
                format!("{} {o} {}", written(a), written(b))
            }
        }
        _ => "…".into(),
    }
}

impl Checker<'_> {
    /// E0305 when `a op b` adds, subtracts or compares quantities whose units differ.
    pub(super) fn check_units(&mut self, op: BinOp, a: &ast::Expr, b: &ast::Expr) {
        if !matches!(op, BinOp::Add | BinOp::Sub) && !op.is_comparison() {
            return;
        }
        let (Some(Some(x)), Some(Some(y))) = (dim(a), dim(b)) else { return };
        if x == y {
            return;
        }
        let (ta, tb) = (written(a), written(b));
        let what = match op {
            BinOp::Add => "added to",
            BinOp::Sub => "subtracted from",
            _ => "compared with",
        };
        let (first, second) = if op == BinOp::Sub { (&tb, &ta) } else { (&ta, &tb) };
        let (d1, d2) = if op == BinOp::Sub { (y, x) } else { (x, y) };
        self.err(
            Diagnostic::new(
                codes::E0305,
                a.span.to(b.span),
                format!("`{first}` is {} and `{second}` is {}: one can't be {what} the other", name(d1), name(d2)),
            )
            .with_note("units are constants in SI (§5): both are numbers, but their units say they measure different things")
            .with_help("check which unit was meant"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::unit_dim;
    use wrela_syntax::ast::{ExprKind, ItemKind, LitKind};

    /// Every unit of `std::units` has a dimension in `unit_dim`, and a literal value: the
    /// checker reads a suffix's value from the literal (§5).
    #[test]
    fn every_std_unit_has_a_dimension_and_a_literal_value() {
        let (_, text) =
            crate::STD_SOURCES.iter().find(|(m, _)| *m == "std::units").expect("std::units");
        let parsed = wrela_syntax::parse(wrela_diag::FileId(0), text);
        assert!(parsed.diagnostics.is_empty());
        let mut units = 0;
        for item in &parsed.file.items {
            let ItemKind::Const(c) = &item.kind else { continue };
            let name = &c.name.name;
            assert!(unit_dim(name).is_some(), "`{name}` has no dimension in `unit_dim`");
            assert!(
                matches!(&c.value.kind, ExprKind::Lit(l) if matches!(l.kind, LitKind::Float(_) | LitKind::Int(_))),
                "`{name}`'s value isn't a literal number"
            );
            units += 1;
        }
        assert!(units > 0);
    }
}
