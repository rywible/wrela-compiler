//! `wrela primer [area]`: the language area by area, as its conformance suite shows it (see
//! `wrela_driver::primer`). With no area, the areas; with one, its rules, each with a program the
//! suite accepts and the lines it rejects. Exit status: 0, or 2 for an area there isn't.

use std::process::ExitCode;

pub fn run(args: &[String]) -> ExitCode {
    if args.len() > 1 || args.first().is_some_and(|a| a.starts_with('-')) {
        return crate::usage();
    }
    match wrela_driver::primer::primer(args.first().map(String::as_str)) {
        Some(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        None => {
            let names: Vec<String> =
                wrela_driver::primer::areas().into_iter().map(|(a, _)| a).collect();
            eprintln!("error: no area `{}` (there are {})", args[0], names.join(", "));
            ExitCode::from(2)
        }
    }
}
