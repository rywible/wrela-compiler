//! Memory layout, by WGSL's rules for host-shareable types (WGSL §14.4), for every type.
//!
//! `GpuData` fixes a type's layout to WGSL's rules everywhere (language.md §6.10), and the CPU
//! uses the same rules for every type, so there's one layout per type and a uniform block's
//! bytes on the CPU are its bytes on the GPU. Types WGSL doesn't have (64-bit and small
//! integers, runs, pointers) get their natural size and alignment; they never cross to the GPU.

use crate::types::{Scalar, TypeDef, TypeId, Types};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub size: u32,
    pub align: u32,
}

/// Sizes saturate at `u32::MAX` rather than overflow: no value that large fits in memory, and a
/// function with one traps on entry, as a stack overflow.
pub fn round_up(align: u32, n: u32) -> u32 {
    n.div_ceil(align).saturating_mul(align)
}

fn scalar_layout(s: Scalar) -> Layout {
    let n = match s {
        Scalar::Bool => 4,
        Scalar::I8 | Scalar::U8 => 1,
        Scalar::I16 | Scalar::U16 => 2,
        Scalar::I32 | Scalar::U32 | Scalar::F32 => 4,
        Scalar::I64 | Scalar::U64 | Scalar::F64 => 8,
    };
    Layout { size: n, align: n }
}

pub fn vector_layout(n: u8) -> Layout {
    match n {
        2 => Layout { size: 8, align: 8 },
        3 => Layout { size: 12, align: 16 },
        _ => Layout { size: 16, align: 16 },
    }
}

pub fn layout(types: &Types, t: TypeId) -> Layout {
    types.layout(t)
}

/// The layout of a type being interned, from its parts' layouts.
pub(crate) fn compute(types: &Types, d: &TypeDef) -> Layout {
    match d {
        TypeDef::Scalar(s) => scalar_layout(*s),
        TypeDef::Vector(n) => vector_layout(*n),
        TypeDef::Matrix(n) => {
            Layout { size: *n as u32 * column_stride(*n), align: vector_layout(*n).align }
        }
        TypeDef::Array(e, n) => {
            let el = types.layout(*e);
            Layout { size: n.saturating_mul(round_up(el.align, el.size)), align: el.align }
        }
        TypeDef::RuntimeArray(e) => {
            let el = types.layout(*e);
            Layout { size: round_up(el.align, el.size), align: el.align }
        }
        TypeDef::Struct { fields, .. } => {
            let mut offset = 0u32;
            let mut align = 1;
            for (_, f) in fields {
                let l = types.layout(*f);
                offset = round_up(l.align, offset).saturating_add(l.size);
                align = align.max(l.align);
            }
            Layout { size: round_up(align, offset.max(1)), align }
        }
        TypeDef::Enum { variants, .. } => {
            let payloads = variants.iter().filter_map(|(_, p)| p.map(|p| types.layout(p)));
            let (size, align) = payloads.fold((0, 4), |(s, a), l| (s.max(l.size), a.max(l.align)));
            Layout { size: round_up(align, enum_payload_offset(align).saturating_add(size)), align }
        }
        TypeDef::Run(_) => Layout { size: 8, align: 4 },
        TypeDef::Ptr(_) => Layout { size: 4, align: 4 },
    }
}

/// Where an enum's payloads start: after its `u32` tag, aligned for the most aligned payload.
fn enum_payload_offset(align: u32) -> u32 {
    round_up(align, 4)
}

/// Each field's byte offset in a struct or an enum (whose payloads all start at one offset).
pub fn field_offsets(types: &Types, t: TypeId) -> Vec<u32> {
    if let TypeDef::Enum { variants, .. } = types.get(t) {
        let at = enum_payload_offset(layout(types, t).align);
        return std::iter::once(0).chain(variants.iter().map(|_| at)).collect();
    }
    let TypeDef::Struct { fields, .. } = types.get(t) else { return Vec::new() };
    let mut out = Vec::with_capacity(fields.len());
    let mut offset = 0;
    for (_, f) in fields {
        let l = layout(types, *f);
        let at = round_up(l.align, offset);
        out.push(at);
        offset = at.saturating_add(l.size);
    }
    out
}

/// The distance between consecutive elements of an array of `elem`.
/// The distance between a `matN`'s columns.
pub fn column_stride(n: u8) -> u32 {
    let col = vector_layout(n);
    round_up(col.align, col.size)
}

pub fn array_stride(types: &Types, elem: TypeId) -> u32 {
    let l = layout(types, elem);
    round_up(l.align, l.size)
}

/// Whether a type's layout meets WGSL's extra rules for the uniform address space: arrays have
/// a 16-byte stride, and struct and array members start on 16 bytes (with the member after a
/// struct starting at least a 16-byte-rounded size later).
pub fn uniform_compatible(types: &Types, t: TypeId) -> bool {
    match types.get(t) {
        // An enum never reaches the GPU.
        TypeDef::Enum { .. } => false,
        TypeDef::Scalar(s) => s.on_gpu() && *s != Scalar::Bool,
        TypeDef::Vector(_) | TypeDef::Matrix(_) => true,
        TypeDef::Array(e, _) => {
            array_stride(types, *e).is_multiple_of(16) && uniform_compatible(types, *e)
        }
        TypeDef::Struct { fields, .. } => {
            let offsets = field_offsets(types, t);
            for (i, (_, f)) in fields.iter().enumerate() {
                if !uniform_compatible(types, *f) {
                    return false;
                }
                let nested = matches!(types.get(*f), TypeDef::Struct { .. } | TypeDef::Array(..));
                if nested && !offsets[i].is_multiple_of(16) {
                    return false;
                }
                if matches!(types.get(*f), TypeDef::Struct { .. })
                    && let Some(&next) = offsets.get(i + 1)
                    && next < offsets[i].saturating_add(round_up(16, layout(types, *f).size))
                {
                    return false;
                }
            }
            true
        }
        TypeDef::RuntimeArray(_) | TypeDef::Run(_) | TypeDef::Ptr(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wgsl_examples() {
        let mut t = Types::default();
        let f32 = t.f32();
        let v3 = t.vector(3);
        let v2 = t.vector(2);
        // WGSL spec example: struct A { u: f32, v: f32, w: vec2<f32>, x: f32 } is 24 bytes.
        let a = t.intern(TypeDef::Struct {
            name: "A".into(),
            fields: vec![("u".into(), f32), ("v".into(), f32), ("w".into(), v2), ("x".into(), f32)],
        });
        assert_eq!(layout(&t, a), Layout { size: 24, align: 8 });
        assert_eq!(field_offsets(&t, a), vec![0, 4, 8, 16]);
        // vec3 is 12 bytes with 16-byte alignment: a following f32 packs into its tail.
        let b = t.intern(TypeDef::Struct {
            name: "B".into(),
            fields: vec![("p".into(), v3), ("r".into(), f32)],
        });
        assert_eq!(field_offsets(&t, b), vec![0, 12]);
        assert_eq!(layout(&t, b).size, 16);
        let m3 = t.intern(TypeDef::Matrix(3));
        assert_eq!(layout(&t, m3), Layout { size: 48, align: 16 });
        let arr = t.intern(TypeDef::Array(f32, 4));
        assert_eq!(layout(&t, arr).size, 16);
        assert!(!uniform_compatible(&t, arr), "an f32 array has a 4-byte stride");
        let v4 = t.vector(4);
        let arr4 = t.intern(TypeDef::Array(v4, 4));
        assert!(uniform_compatible(&t, arr4));
        assert!(uniform_compatible(&t, b));
    }

    #[test]
    fn an_enum_is_its_tag_and_its_largest_payload() {
        let mut t = Types::default();
        let u32t = t.u32();
        let v4 = t.vector(4);
        let small =
            t.intern(TypeDef::Struct { name: "S".into(), fields: vec![("a".into(), u32t)] });
        let wide = t.intern(TypeDef::Struct {
            name: "W".into(),
            fields: vec![("b".into(), v4), ("c".into(), v4), ("d".into(), v4)],
        });
        let e = t.intern(TypeDef::Enum {
            name: "E".into(),
            variants: vec![
                ("Empty".into(), None),
                ("Small".into(), Some(small)),
                ("Wide".into(), Some(wide)),
            ],
        });
        // The payloads overlap after the tag, aligned for the most aligned one: 16 + 48.
        assert_eq!(layout(&t, e), Layout { size: 64, align: 16 });
        assert_eq!(field_offsets(&t, e), vec![0, 16, 16, 16]);
        assert_eq!(t.field(e, 0), Some(u32t));
        assert_eq!(t.field(e, 1), None);
        assert_eq!(t.field(e, 3), Some(wide));
    }
}
