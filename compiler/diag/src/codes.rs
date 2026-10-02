//! The registry of diagnostic codes. A code is declared once here and can only be named through
//! this module, so every code the compiler emits is listed with its title.
//!
//! Ranges: E00xx lexical, E01xx syntax, E02xx names and modules, E03xx types, E04xx traits and
//! generics, E05xx modes, moves, projections and exclusivity, E06xx effects and GPU rules, E07xx
//! derived interpretations and back ends, E09xx not in tier 0, W0xxx warnings.
//!
//! A code's number and meaning never change once released; retired codes stay reserved.

use crate::diagnostic::Severity;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Code(&'static CodeInfo);

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct CodeInfo {
    pub code: &'static str,
    pub title: &'static str,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        self.0.code
    }

    pub fn title(self) -> &'static str {
        self.0.title
    }

    pub fn severity(self) -> Severity {
        if self.0.code.starts_with('W') { Severity::Warning } else { Severity::Error }
    }

    /// Looks a code up by its text, e.g. `"E0001"`.
    pub fn lookup(code: &str) -> Option<Code> {
        ALL.iter().copied().find(|c| c.as_str() == code)
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.code)
    }
}

macro_rules! codes {
    ($($name:ident = $code:literal, $title:literal;)*) => {
        $(
            #[doc = $title]
            pub const $name: Code = Code(&CodeInfo { code: $code, title: $title });
        )*
        /// Every registered code, in declaration order.
        pub const ALL: &[Code] = &[$($name),*];
    };
}

codes! {
    // ---- E00xx: lexical (spec/lexical.md) --------------------------------------------------
    E0001 = "E0001", "a character that can't appear outside a comment";
    E0002 = "E0002", "a block comment (wrela has only `//` comments)";
    E0003 = "E0003", "a number with a type suffix";
    E0004 = "E0004", "a malformed number";
    E0005 = "E0005", "a malformed string literal";
    E0006 = "E0006", "an integer literal too large for its type";

    // ---- E01xx: syntax (spec/grammar.ebnf) -------------------------------------------------
    E0100 = "E0100", "unexpected token";
    E0101 = "E0101", "unclosed bracket";
    E0102 = "E0102", "mismatched closing bracket";
    E0103 = "E0103", "a binary operator at the start of a line";
    E0104 = "E0104", "a missing statement separator";
    E0105 = "E0105", "a positional argument after a named one";
    E0106 = "E0106", "`&` isn't a type or an operator on places";
    E0107 = "E0107", "comparisons don't chain";
    E0108 = "E0108", "an attribute in the wrong place";
    E0109 = "E0109", "a missing parameter type";
    E0110 = "E0110", "an assignment where an expression is expected";
    E0111 = "E0111", "`else` on a new line";

    // ---- E02xx: names and modules ----------------------------------------------------------
    E0200 = "E0200", "an unknown name";
    E0201 = "E0201", "a name defined twice";
    E0202 = "E0202", "an unknown module";
    E0203 = "E0203", "a private item";
    E0204 = "E0204", "an unknown attribute";
    E0205 = "E0205", "a module file that can't be read";
    E0206 = "E0206", "an unknown field";
    E0207 = "E0207", "an unknown method";
    E0208 = "E0208", "a symbolic link to a directory";
    E0209 = "E0209", "an import cycle or ambiguous import";
    E0210 = "E0210", "a private field";
    E0211 = "E0211", "an unknown variant";
    E0212 = "E0212", "a type used as a value, or a value as a type";
    E0213 = "E0213", "mutable global state";

    // ---- E03xx: types ----------------------------------------------------------------------
    E0300 = "E0300", "mismatched types";
    E0301 = "E0301", "wrong number of arguments";
    E0302 = "E0302", "an unknown named argument";
    E0303 = "E0303", "a missing argument";
    E0304 = "E0304", "an argument given twice";
    E0305 = "E0305", "an operator that doesn't apply to these types";
    E0306 = "E0306", "a type that can't be inferred";
    E0307 = "E0307", "a missing struct field";
    E0308 = "E0308", "a struct field given twice";
    E0309 = "E0309", "a non-exhaustive match";
    E0310 = "E0310", "a value that isn't callable";
    E0311 = "E0311", "a field access on a type without fields";
    E0312 = "E0312", "an index into a value that isn't indexable";
    E0313 = "E0313", "a condition that isn't `bool`";
    E0314 = "E0314", "a missing return value";
    E0315 = "E0315", "`break` or `continue` outside a loop";
    E0316 = "E0316", "a constant that isn't a literal value";
    E0317 = "E0317", "an invalid swizzle";
    E0318 = "E0318", "a recursive type";
    E0319 = "E0319", "an invalid conversion";
    E0320 = "E0320", "a pattern that doesn't match the type";
    E0322 = "E0322", "a wrong number of generic arguments";
    E0323 = "E0323", "a vector constructor with the wrong components";
    E0324 = "E0324", "a struct default that isn't a constant";
    E0325 = "E0325", "an array length that isn't a constant";
    E0326 = "E0326", "a CPU-only type in GPU code";
    E0327 = "E0327", "a run type `[T]` outside a parameter";
    E0328 = "E0328", "a constant whose value refers to itself";

    // ---- E04xx: traits and generics --------------------------------------------------------
    E0400 = "E0400", "a type that doesn't implement a trait";
    E0401 = "E0401", "a missing trait item in an impl";
    E0402 = "E0402", "an impl item the trait doesn't declare";
    E0403 = "E0403", "an impl that conflicts with another";
    E0404 = "E0404", "an impl outside the trait's or the type's module tree (orphan rule)";
    E0405 = "E0405", "an impl method whose signature differs from the trait's";
    E0406 = "E0406", "an ambiguous method";
    E0407 = "E0407", "a struct can't be `Copy` or `GpuData` because of a field";
    E0408 = "E0408", "an unknown associated type";
    E0409 = "E0409", "a return type that names a trait but no single type";
    E0410 = "E0410", "a trait used as a value type";
    E0411 = "E0411", "a cyclic supertrait";

    // ---- E05xx: modes, moves, projections, exclusivity, closures -------------------------
    E0500 = "E0500", "a use of a moved value";
    E0501 = "E0501", "a move out of a named place without `take`";
    E0502 = "E0502", "a move out of a projection";
    E0503 = "E0503", "a `mut` argument without `mut` at the call site";
    E0504 = "E0504", "a `mut` marker where the parameter isn't `mut`";
    E0505 = "E0505", "an assignment through a read-only place";
    E0506 = "E0506", "an access that overlaps a live `mut` access";
    E0507 = "E0507", "a write to a place that's borrowed";
    E0508 = "E0508", "a projection that doesn't come from a `borrow` or `mut` parameter";
    E0509 = "E0509", "a projection that escapes";
    E0510 = "E0510", "a closure that escapes";
    E0511 = "E0511", "a `take` of a value that isn't a named place";
    E0512 = "E0512", "a `mut` projection of a read-only place";
    E0513 = "E0513", "overlapping arguments where one is `mut`";
    E0514 = "E0514", "a `.clone()` of a type that isn't `Clone`";
    E0515 = "E0515", "a move inside a loop of a value from outside it";
    E0516 = "E0516", "a use of a possibly moved value";
    E0517 = "E0517", "`var` binding of a named place without `take` or `.clone()`";

    // ---- E06xx: effects and GPU rules ------------------------------------------------------
    E0600 = "E0600", "an effect a context forbids";
    E0601 = "E0601", "a kernel `mut` parameter that isn't safe to share across invocations";
    E0602 = "E0602", "a GPU entry point with an invalid signature";
    E0603 = "E0603", "a dispatch or draw that doesn't match its entry points";
    E0604 = "E0604", "data crossing to the GPU that isn't `GpuData`";
    E0605 = "E0605", "a workgroup size out of range";
    E0606 = "E0606", "an entry point called directly";
    E0607 = "E0607", "a GPU feature used in CPU code";

    // ---- E07xx: derived interpretations and back ends --------------------------------------
    E0700 = "E0700", "a function that can't be derived";
    E0701 = "E0701", "an interval of a loop whose exit depends on the input";
    E0702 = "E0702", "a construct not supported by a back end";
    E0703 = "E0703", "an exported function with an unsupported signature";

    // ---- E09xx: not in tier 0 --------------------------------------------------------------
    E0900 = "E0900", "units are tier 1";
    E0901 = "E0901", "strings are tier 1";
    E0902 = "E0902", "`?` and `Result` are tier 1";
    E0903 = "E0903", "an attribute that is tier 1 or later";
    E0904 = "E0904", "`unsafe` is tier 1";
    E0905 = "E0905", "`dyn` is tier 2";
    E0906 = "E0906", "an evaluated constant initializer is tier 1";
    E0907 = "E0907", "workgroup-shared memory is milestone 2";

    // ---- W0xxx: warnings -------------------------------------------------------------------
    W0001 = "W0001", "an unused local";
    W0002 = "W0002", "unreachable code";
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn codes_are_unique_and_well_formed() {
        let mut seen = HashSet::new();
        for c in ALL {
            assert!(seen.insert(c.as_str()), "{} declared twice", c);
            let s = c.as_str();
            assert_eq!(s.len(), 5, "{s}");
            assert!(s.starts_with('E') || s.starts_with('W'), "{s}");
            assert!(s[1..].bytes().all(|b| b.is_ascii_digit()), "{s}");
            assert!(!c.title().is_empty());
        }
    }

    #[test]
    fn lookup_finds_codes() {
        assert_eq!(Code::lookup("E0001"), Some(E0001));
        assert_eq!(Code::lookup("E9999"), None);
        assert_eq!(W0001.severity(), Severity::Warning);
    }
}
