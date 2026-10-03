//! The grammar as an executable spec (D-104): spec/grammar.ebnf is what the hand-written parser
//! is held to, by test.
//!
//! - [`ebnf`] reads spec/grammar.ebnf and rejects malformed grammars; [`cfg`] lowers it to plain
//!   productions.
//! - [`earley`] is the oracle: an Earley parser for the grammar over the compiler's own token
//!   stream (GT_CLOSE splitting included), reporting acceptance, ambiguity and the parse tree.
//! - [`canon`] and [`agree`] define and check agreement between the oracle and the hand-written
//!   parser: same verdict, a unique derivation, and the same structure.
//! - [`generate`] derives random programs from the grammar and renders them as source;
//!   [`differential`] runs the agreement and formatter round-trip checks over many of them.
//! - [`gbnf`] exports the grammar as a character-level GBNF grammar (spec/wrela.gbnf) for
//!   constrained generation; [`sampler`] reads it back, samples it and recognizes strings.
//!
//! The binaries: `differential` (the AC run, 10⁶ programs by default), `export-gbnf` (writes
//! spec/wrela.gbnf) and `gbnf-sample` (checks programs sampled from it).
//!
//! [`testing`] holds helpers for tests, shared with the end-to-end tests (`wrela-tests`).

pub mod agree;
pub mod canon;
pub mod cfg;
pub mod differential;
pub mod earley;
pub mod ebnf;
pub mod gbnf;
pub mod generate;
pub mod rng;
pub mod sampler;
pub mod testing;
