//! Files a build ships beside the program (`std::io::Shipped`, M6): a computed constant of that
//! type is written as files of the build, under `files/<dir>/`, and the program's copy of it keeps
//! each file's name and size but not its bytes, which it fetches as it needs them. Build-time code
//! read the bytes in place: the constants computed from it, and tests, which see the whole value
//! (a build that's run as tests ships nothing).

use std::collections::BTreeSet;
use wrela_diag::Diagnostic;
use wrela_lower::{BuildData, Value};
use wrela_sema::Checked;
use wrela_sema::defs::Lang;
use wrela_sema::ty::ConstId;

/// The bytes of a `String`'s value (a `Vec<u8>`'s: its bytes, length and capacity).
fn string_of(v: &Value) -> Option<String> {
    let Value::Parts(s) = v else { return None };
    let Value::Parts(vec) = s.first()? else { return None };
    match vec.first()? {
        Value::Bytes(b) => String::from_utf8(b.clone()).ok(),
        Value::Points(p) if p.is_empty() => Some(String::new()),
        _ => None,
    }
}

/// The elements of a `Vec`'s value.
fn elements(v: &Value) -> Option<&[Value]> {
    let Value::Parts(vec) = v else { return None };
    match vec.first()? {
        Value::Points(p) => Some(p),
        Value::Bytes(b) if b.is_empty() => Some(&[]),
        _ => None,
    }
}

/// A `Vec<u8>`'s bytes.
fn bytes_of(v: &Value) -> Option<Vec<u8>> {
    let Value::Parts(vec) = v else { return None };
    match vec.first()? {
        Value::Bytes(b) => Some(b.clone()),
        Value::Points(p) if p.is_empty() => Some(Vec::new()),
        _ => None,
    }
}

/// An empty `Vec`'s value.
fn empty_vec() -> Value {
    let zero = Value::Scalar(wrela_ir::Const::U32(0));
    Value::Parts(vec![Value::Points(Vec::new()), zero.clone(), zero])
}

/// Takes the files out of `data`'s shipped constants: each file's path in the build and its
/// bytes, and the errors (two constants shipping one directory). Each such constant's value keeps
/// its names and sizes, its bytes emptied.
pub fn take(checked: &Checked, data: &mut BuildData) -> (Vec<(String, Vec<u8>)>, Vec<Diagnostic>) {
    let p = &checked.program;
    let mut files = Vec::new();
    let mut diags = Vec::new();
    let mut dirs = BTreeSet::new();
    let shipped: Vec<ConstId> = data
        .values
        .keys()
        .copied()
        .filter(|&c| {
            checked
                .consts
                .get(&c)
                .is_some_and(|(t, _)| p.lang_of_ty(checked.reveal(*t)) == Some(Lang::Shipped))
        })
        .collect();
    for c in shipped {
        let Some(v) = data.values.get(&c) else { continue };
        let Value::Parts(parts) = &**v else { continue };
        let read = || -> Option<(String, Vec<String>, Vec<Vec<u8>>)> {
            let dir = string_of(parts.first()?)?;
            let names =
                elements(parts.get(1)?)?.iter().map(string_of).collect::<Option<Vec<_>>>()?;
            let bytes =
                elements(parts.get(3)?)?.iter().map(bytes_of).collect::<Option<Vec<_>>>()?;
            Some((dir, names, bytes))
        };
        let Some((dir, names, bytes)) = read() else {
            diags.push(Diagnostic::internal(format!(
                "the shipped constant `{}` isn't laid out as std's `Shipped`",
                p.const_(c).name
            )));
            continue;
        };
        if !dirs.insert(dir.clone()) {
            diags.push(
                Diagnostic::new(
                    wrela_diag::codes::E0704,
                    p.const_(c).span,
                    format!(
                        "{} ships into `files/{dir}/`, as another constant does",
                        p.const_(c).shown()
                    ),
                )
                .with_help("give each shipped constant a directory of its own"),
            );
            continue;
        }
        for (name, b) in names.into_iter().zip(bytes) {
            files.push((format!("files/{dir}/{name}"), b));
        }
        let mut stripped = parts.clone();
        stripped[3] = empty_vec();
        data.values.insert(c, std::rc::Rc::new(Value::Parts(stripped)));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    (files, diags)
}
