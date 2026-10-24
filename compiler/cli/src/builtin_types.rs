//! The built-in types in `wrela doc`'s index: the scalars, vectors, matrices, `str`, runs and
//! arrays, which no file declares. Each one's documentation is the spec's own section about it,
//! read from the spec the CLI carries, so the two can't disagree; its page lists the built-in
//! functions that take it as a method, and std's impls of it (`impl<T> [T]`, `impl str`) attach
//! their methods to it as any impl does.

use crate::doc::{Entry, Index, Module};

/// The module the built-in types are listed under. No program imports it: they're in scope
/// everywhere.
pub(crate) const MODULE: &str = "builtins";

/// One built-in type, or a family of them, with one page: its names (the first names the page,
/// and `wrela doc` finds the page by any of them), the spec section that defines it, and what
/// its page shows as its signature (how a value is made and read).
struct Builtin {
    names: &'static [&'static str],
    /// Other words `wrela doc` finds it by: `run` is `[T]`.
    aliases: &'static [&'static str],
    heading: &'static str,
    /// What the family is, for `wrela doc builtins`' list.
    summary: &'static str,
    signature: &'static str,
    takes: Takes,
}

/// Which built-in functions take a type as their receiver (`x.f()`), by the type's kind.
#[derive(Clone, Copy, PartialEq)]
enum Takes {
    Float,
    FloatVector,
    Integer,
    IntegerVector,
    Sequence,
    Nothing,
}

const SCALARS: &str = "### 4.1 Scalars and conversions";
const VECTORS: &str = "### 4.2 Vectors and matrices";

const TYPES: &[Builtin] = &[
    Builtin {
        names: &["bool"],
        aliases: &[],
        heading: SCALARS,
        summary: "true or false",
        signature: "true, false; a == b, !a, a && b, a || b",
        takes: Takes::Nothing,
    },
    Builtin {
        names: &["u32", "i32", "u8", "i8", "u16", "i16", "u64", "i64"],
        aliases: &["integers"],
        heading: SCALARS,
        summary: "the integers, 8 to 64 bits, signed and unsigned",
        signature: "7, u32(x), i32(x); + - * / % (overflow traps on the CPU), & | ^ << >>",
        takes: Takes::Integer,
    },
    Builtin {
        names: &["f32", "f64"],
        aliases: &["floats"],
        heading: SCALARS,
        summary: "the floats",
        signature: "1.5, 2e3, 15cm (a unit suffix, in SI), f32(n); + - * / %",
        takes: Takes::Float,
    },
    Builtin {
        names: &["vec3", "vec2", "vec4"],
        aliases: &["vectors"],
        heading: VECTORS,
        summary: "vectors of `f32`",
        signature: "vec3(x, y, z), vec3(v.xy, z), vec3(s); v.x, v.zyx, c.rgb, v[i]; + - * / by a vector or an f32",
        takes: Takes::FloatVector,
    },
    Builtin {
        names: &["vec3d", "vec2d", "vec4d"],
        aliases: &[],
        heading: VECTORS,
        summary: "vectors of `f64`, CPU only",
        signature: "vec3d(x, y, z), vec3d(v); v.x, v.zyx, v[i] (CPU only)",
        takes: Takes::FloatVector,
    },
    Builtin {
        names: &["vec3u", "vec2u", "vec4u", "vec3i", "vec2i", "vec4i"],
        aliases: &[],
        heading: VECTORS,
        summary: "vectors of `u32` and `i32`",
        signature: "vec3u(x, y, z), vec3u(p / cell); v.x, v.zyx, v[i]; arithmetic, bit operators, shifts",
        takes: Takes::IntegerVector,
    },
    Builtin {
        names: &[
            "mat4", "mat2", "mat3", "mat2x3", "mat2x4", "mat3x2", "mat3x4", "mat4x2", "mat4x3",
        ],
        aliases: &["matrices"],
        heading: VECTORS,
        summary: "matrices of `f32`, columns of rows",
        signature: "mat3(c0, c1, c2) from column vectors; m[i] is a column; M * v, v * M, A * B",
        takes: Takes::Nothing,
    },
    Builtin {
        names: &["str"],
        aliases: &[],
        heading: "### 4.6 Text and bytes",
        summary: "a borrowed run of UTF-8",
        signature: "\"text\" passes as a str; s.len() in bytes",
        takes: Takes::Nothing,
    },
    Builtin {
        names: &["[T]"],
        aliases: &["run", "[]"],
        heading: "### 4.4 Runs and tuples",
        summary: "a run: a borrowed view of elements",
        signature: "xs: [T], mut [T]; xs[i], xs.len(), for x in xs, for mut x in xs; an array or a Vec passes as one",
        takes: Takes::Sequence,
    },
    Builtin {
        names: &["[T; N]"],
        aliases: &["array"],
        heading: "### 4.3 Arrays",
        summary: "an array of N elements",
        signature: "[a, b, c], [x; N], [a, ..fill]; xs[i], xs.len(), for x in xs",
        takes: Takes::Sequence,
    },
];

/// The built-in functions called as methods of a type of kind `takes`, as their calls: those
/// sema takes on the kind's first type (`f32` for the floats), so the list is the checker's.
fn methods_of(takes: Takes) -> Vec<String> {
    // Called as functions, not methods, though they can be: the derivatives are a fragment
    // shader's.
    const NOT_METHODS: &[&str] = &["dpdx", "dpdy", "fwidth"];
    let ty = match takes {
        Takes::Float => "f32",
        Takes::FloatVector => "vec3",
        Takes::Integer => "u32",
        Takes::IntegerVector => "vec3u",
        Takes::Sequence => "[f32]",
        Takes::Nothing => return Vec::new(),
    };
    wrela_driver::builtin_methods(ty)
        .into_iter()
        .filter(|(name, _)| !NOT_METHODS.contains(name))
        .map(|(name, arity)| crate::doc::builtin_call(name, arity, true))
        .collect()
}

/// The text of the spec's section under `heading`, up to the next heading of any level.
fn section(heading: &str) -> (String, usize) {
    let spec = crate::export::LANGUAGE;
    let Some(at) = spec.find(&format!("\n{heading}\n")) else { return (String::new(), 0) };
    let line = 2 + spec[..at].matches('\n').count();
    let body = &spec[at + heading.len() + 2..];
    let end = body.find("\n#").unwrap_or(body.len());
    (body[..end].trim().to_string(), line)
}

/// Adds the built-in types' module and entries to `index`. Called before impls are attached,
/// so std's `impl<T> [T]` and `impl str` list their methods under them.
pub(crate) fn add(index: &mut Index) {
    index.modules.push(Module {
        path: vec![MODULE.to_string()],
        summary: "The built-in types and functions: in scope in every program, with no import."
            .to_string(),
        intro: "The built-in types and functions: in scope in every program, with no import (language.md §4, §11.4). `builtins` is only where the reference lists them; no program names it."
            .to_string(),
    });
    for t in TYPES {
        let (doc, line) = section(t.heading);
        let section_number = t.heading.trim_start_matches('#').trim();
        let methods = methods_of(t.takes);
        let mut signature = t.signature.to_string();
        if !methods.is_empty() {
            signature.push_str(&format!("\n\nbuilt-in methods: {}", methods.join(", ")));
        }
        let names =
            if t.names.len() > 1 { format!("{}\n\n", t.names.join(", ")) } else { String::new() };
        index.entries.push(Entry {
            path: vec![MODULE.to_string(), t.names[0].to_string()],
            kind: "type",
            signature: format!("{names}{signature}"),
            doc: format!("From language.md §{section_number}:\n\n{doc}"),
            at: format!("language.md:{line}"),
            source: "spec/language.md".to_string(),
            line,
            methods: Vec::new(),
            implements: Vec::new(),
        });
    }
}

/// Each family's page name and summary, for `wrela doc builtins`.
pub(crate) fn summaries() -> impl Iterator<Item = (&'static str, &'static str)> {
    TYPES.iter().map(|t| (t.names[0], t.summary))
}

/// The page that `query` names when it's another name of a built-in type: `u8` is on `u32`'s
/// page, and `run` is `[T]`.
pub(crate) fn resolve_alias(query: &str) -> Option<&'static str> {
    TYPES
        .iter()
        .find(|t| t.names[1..].contains(&query) || t.aliases.contains(&query))
        .map(|t| t.names[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_type_has_its_spec_section() {
        for t in TYPES {
            let (doc, line) = section(t.heading);
            assert!(!doc.is_empty() && line > 0, "no section `{}` in the spec", t.heading);
        }
    }

    #[test]
    fn methods_go_to_the_kinds_they_take() {
        assert!(methods_of(Takes::Sequence).contains(&"x.len()".to_string()));
        assert!(methods_of(Takes::Float).contains(&"x.sqrt()".to_string()));
        assert!(!methods_of(Takes::Float).contains(&"x.count_ones()".to_string()));
        assert!(methods_of(Takes::Integer).contains(&"x.count_ones()".to_string()));
        assert!(!methods_of(Takes::Integer).contains(&"x.sqrt()".to_string()));
        assert!(methods_of(Takes::Nothing).is_empty());
        // Only what sema takes on the type: no vector function on a scalar, no free function.
        for not in ["x.cross(y)", "x.normalize()", "x.bitcast_u32()", "x.select(y, z)"] {
            assert!(!methods_of(Takes::Float).contains(&not.to_string()), "{not}");
        }
        assert!(methods_of(Takes::FloatVector).contains(&"x.cross(y)".to_string()));
        assert!(methods_of(Takes::FloatVector).contains(&"x.normalize()".to_string()));
    }
}
