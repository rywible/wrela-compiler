//! `wrela explain <code>`: what a diagnostic code means, a program that has it, and the program
//! fixed (`wrela_driver::explain`).

use std::process::ExitCode;

pub fn run(args: &[String]) -> ExitCode {
    let [code] = args else { return crate::usage() };
    let Some(c) = wrela_diag::codes::Code::lookup(&code.to_uppercase()) else {
        let retired = wrela_diag::codes::RETIRED.iter().find(|(r, _)| r.eq_ignore_ascii_case(code));
        match retired {
            Some((r, why)) => println!("{r} is retired: {why}"),
            None => eprintln!("error: `{code}` isn't a diagnostic code (they look like E0518)"),
        }
        return if retired.is_some() { ExitCode::SUCCESS } else { ExitCode::from(1) };
    };
    println!("{c}: {}\n", c.title());
    let Some(e) = wrela_driver::explain::get(c.as_str()) else {
        println!("(no explanation yet)");
        return ExitCode::SUCCESS;
    };
    println!("{}\n", e.meaning);
    if let Some(why) = &e.untested {
        println!("(no example program: {why})");
        return ExitCode::SUCCESS;
    }
    let show = |title: &str, files: &[(String, String)]| {
        println!("{title}");
        for (path, text) in files {
            if files.len() > 1 {
                println!("\n// {path}");
            }
            print!("\n{}", text);
        }
        println!();
    };
    show(&format!("A program with {c}:"), &e.wrong);
    show("Fixed:", &e.fixed);
    ExitCode::SUCCESS
}
