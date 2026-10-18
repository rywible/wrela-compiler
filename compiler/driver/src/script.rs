//! `wrela script`: a `std::input::Script` constant's events (language.md §13.2), in runtime/abi
//! `input`'s script format, which the native host plays (`wrela-host --input`). A frame or tick
//! test whose input is such a constant (`@test(frames: n, input: NAME)`) reads its value here
//! too.

use crate::{load, on_compiler_thread, prepare};
use std::path::Path;
use wrela_diag::{Diagnostic, SourceMap, has_errors};
use wrela_lower::{BuildData, Value};
use wrela_sema::Checked;
use wrela_sema::defs::{Lang, PackageKind};
use wrela_sema::thir::ExprKind;
use wrela_sema::ty::ConstId;

/// What `wrela script` found: the program's diagnostics, and the script, if the package has no
/// errors and the name is a `Script` constant (else why not).
pub struct ScriptOutput {
    pub sources: SourceMap,
    pub diagnostics: Vec<Diagnostic>,
    pub script: Result<String, String>,
}

/// The `std::input::Script` constant `name` (`<module>::<NAME>`, or the end of its path) of
/// the package at `root`, computed as `wrela test` computes it, as runtime/abi `input`'s JSON.
pub fn script(root: &Path, name: &str) -> ScriptOutput {
    on_compiler_thread(|| {
        let (sources, mut diagnostics, loaded) = load(root);
        let Some(loaded) = loaded else {
            let script = Err("the package couldn't be read".into());
            crate::split_std_warnings(&sources, &mut diagnostics);
            return ScriptOutput { sources, diagnostics, script };
        };
        let checked = wrela_sema::check_packages(loaded.units, &loaded.packages, &mut diagnostics);
        let errors = |d: &[Diagnostic]| -> Result<(), String> {
            if has_errors(d) { Err("the package has errors".into()) } else { Ok(()) }
        };
        let script = errors(&diagnostics).and_then(|()| named(&checked, name)).and_then(|c| {
            let mut data = BuildData { debug: true, testing: true, ..Default::default() };
            let cache = crate::const_cache::dirs_of(root);
            prepare(
                &checked,
                &sources,
                &loaded.dirs,
                &mut data,
                &mut diagnostics,
                &cache,
                &crate::reach::Reach::anywhere(),
            );
            errors(&diagnostics)?;
            json_of(&checked, &data, c).ok_or_else(|| format!("`{name}` has no computed value"))
        });
        crate::split_std_warnings(&sources, &mut diagnostics);
        ScriptOutput { sources, diagnostics, script }
    })
}

/// The `Script` constant of the package's own modules whose path is `name`, or ends with it.
fn named(checked: &Checked, name: &str) -> Result<ConstId, String> {
    let p = &checked.program;
    let want: Vec<&str> = name.split("::").collect();
    let found: Vec<ConstId> = (0..p.consts.len() as u32)
        .map(ConstId)
        .filter(|&c| {
            let d = p.const_(c);
            if d.default || p.package_of(d.module).kind != PackageKind::Program {
                return false;
            }
            let mut path: Vec<&str> = p.module(d.module).path.iter().map(String::as_str).collect();
            path.push(&d.name);
            path.ends_with(&want)
        })
        .collect();
    let c = match found[..] {
        [c] => c,
        [] => return Err(format!("the package has no constant `{name}`")),
        _ => {
            return Err(format!(
                "{} constants are named `{name}`: name one by its module",
                found.len()
            ));
        }
    };
    match checked.consts.get(&c) {
        Some((t, _)) if p.lang_of_ty(checked.reveal(*t)) == Some(Lang::Script) => Ok(c),
        Some((t, _)) => Err(format!(
            "`{name}` is a `{}`, not a `std::input::Script`",
            p.display_ty(checked.reveal(*t))
        )),
        None => Err(format!("`{name}` has no type: the package has errors")),
    }
}

/// A `Script` constant's computed value, as runtime/abi `input`'s JSON: its first field. A
/// constant that names another (`const REPLAY: Script = BASE`) isn't computed itself: its value
/// is the one it names, through any chain of names.
pub(crate) fn json_of(checked: &Checked, data: &BuildData, c: ConstId) -> Option<String> {
    let mut c = c;
    for _ in 0..=checked.consts.len() {
        if let Some(v) = data.values.get(&c) {
            let Value::Parts(parts) = &**v else { return None };
            return crate::shipped::string_of(parts.first()?);
        }
        let (_, e) = checked.consts.get(&c)?;
        let ExprKind::Const(next) = e.kind else { return None };
        c = next;
    }
    None
}
