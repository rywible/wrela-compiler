//! Writes spec/wrela.gbnf, the GBNF export of spec/grammar.ebnf.
//!
//!     cargo run -p wrela-grammar --bin export-gbnf            # write the file
//!     cargo run -p wrela-grammar --bin export-gbnf -- --check # exit 1 if it's out of date

use wrela_grammar::ebnf::Grammar;
use wrela_grammar::gbnf::export;

fn main() {
    let check = std::env::args().any(|a| a == "--check");
    let text = export(&Grammar::spec()).unwrap_or_else(|e| {
        eprintln!("export-gbnf: {e}");
        std::process::exit(1)
    });
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/wrela.gbnf");
    if check {
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current != text {
            eprintln!(
                "{} is out of date; run `cargo run -p wrela-grammar --bin export-gbnf`",
                path.display()
            );
            std::process::exit(1);
        }
        return;
    }
    if let Err(e) = std::fs::write(&path, &text) {
        eprintln!("export-gbnf: writing {}: {e}", path.display());
        std::process::exit(1);
    }
    eprintln!(
        "wrote {} ({} rules, {} bytes)",
        path.display(),
        text.lines().filter(|l| l.contains("::=")).count(),
        text.len()
    );
}
