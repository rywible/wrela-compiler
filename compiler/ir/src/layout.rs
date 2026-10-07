//! Memory layout, by WGSL's rules for host-shareable types (WGSL §14.4), for every type.
//!
//! `GpuData` fixes a type's layout to WGSL's rules everywhere (language.md §6.10), and the CPU
//! uses the same rules for every type, so there's one layout per type and a uniform block's
//! bytes on the CPU are its bytes on the GPU. Types WGSL doesn't have (64-bit and small
//! integers, runs, pointers) get their natural size and alignment; they never cross to the GPU.

use crate::{Scalar, TypeDef, TypeId, Types};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub size: u32,
    pub align: u32,
}

impl Layout {
    /// The distance between consecutive elements of an array of this layout.
    pub fn stride(self) -> u32 {
        round_up(self.align, self.size)
    }

    /// An array of `n` elements of this layout.
    pub fn array(self, n: u32) -> Layout {
        Layout { size: n.saturating_mul(self.stride()), align: self.align }
    }
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

/// WGSL's vector layout (a `vec3` aligned as a `vec4`), for components of `c` bytes.
fn vector_layout(n: u8, c: u32) -> Layout {
    match n {
        2 => Layout { size: 2 * c, align: 2 * c },
        3 => Layout { size: 3 * c, align: 4 * c },
        _ => Layout { size: 4 * c, align: 4 * c },
    }
}

pub fn layout(types: &Types, t: TypeId) -> Layout {
    types.layout(t)
}

/// The layout of a type being interned and its fields' offsets (see [`field_offsets`]), from
/// its parts' layouts.
pub(crate) fn compute(types: &Types, d: &TypeDef) -> (Layout, Box<[u32]>) {
    let layout = match d {
        TypeDef::Scalar(s) | TypeDef::Atomic(s) => scalar_layout(*s),
        // A vector of f64s never reaches the GPU: its natural layout, as an array's.
        TypeDef::Vector(Scalar::F64, n) => scalar_layout(Scalar::F64).array(u32::from(*n)),
        TypeDef::Vector(s, n) => vector_layout(*n, scalar_layout(*s).size),
        // Its columns, as an array.
        TypeDef::Matrix(n) => vector_layout(*n, 4).array(u32::from(*n)),
        TypeDef::Array(e, n) => types.layout(*e).array(*n),
        TypeDef::RuntimeArray(e) => types.layout(*e).array(1),
        TypeDef::Struct { fields, .. } => {
            let (offsets, layout) = pack(types, fields.iter().map(|(_, f)| *f));
            return (layout, offsets.into());
        }
        TypeDef::Enum { variants, .. } => {
            let payloads = variants.iter().filter_map(|(_, p)| p.map(|p| types.layout(p)));
            let (size, align) = payloads.fold((0, 4), |(s, a), l| (s.max(l.size), a.max(l.align)));
            // After its `u32` tag, aligned for the most aligned payload.
            let at = round_up(align, 4);
            let offsets = std::iter::once(0).chain(variants.iter().map(|_| at)).collect();
            return (Layout { size: round_up(align, at.saturating_add(size)), align }, offsets);
        }
        TypeDef::Run(_) => Layout { size: 8, align: 4 },
        TypeDef::Ptr(_) => Layout { size: 4, align: 4 },
    };
    (layout, Box::default())
}

/// The largest type WGSL allows, in bytes (naga's limit, which the WebGPU hosts share).
pub const WGSL_MAX_TYPE_SIZE: u32 = i32::MAX as u32;

/// A type's layout in WGSL: see [`WgslLayouts`].
pub fn wgsl_layout(types: &Types, t: TypeId) -> Layout {
    WgslLayouts::default().get(types, t).0
}

/// Types' layouts in WGSL, which has no unions: [`layout`], but with each enum laid out as the
/// struct the WGSL back end holds it in (its `u32` tag, then every payload). Each type is laid
/// out once, however often it appears.
#[derive(Default)]
pub struct WgslLayouts(HashMap<TypeId, (Layout, Vec<u32>)>);

impl WgslLayouts {
    /// `t`'s layout, and its members' offsets: a struct's fields, or an enum's tag then each
    /// payload (none for other types).
    pub fn get(&mut self, types: &Types, t: TypeId) -> (Layout, &[u32]) {
        if !self.0.contains_key(&t) {
            let out = match types.get(t) {
                TypeDef::Array(e, n) => (self.get(types, *e).0.array(*n), Vec::new()),
                TypeDef::Struct { fields, .. } => {
                    let (offsets, l) =
                        pack_layouts(fields.iter().map(|(_, f)| self.get(types, *f).0));
                    (l, offsets)
                }
                TypeDef::Enum { variants, .. } => {
                    let payloads =
                        variants.iter().filter_map(|(_, p)| Some(self.get(types, (*p)?).0));
                    let (offsets, l) =
                        pack_layouts(std::iter::once(scalar_layout(Scalar::U32)).chain(payloads));
                    (l, offsets)
                }
                _ => (types.layout(t), Vec::new()),
            };
            self.0.insert(t, out);
        }
        let (l, offsets) = &self.0[&t];
        (*l, offsets)
    }
}

/// Fields one after the other, each at the first offset its alignment allows (a struct's
/// layout): each field's offset, and the layout of the whole.
pub fn pack(types: &Types, fields: impl IntoIterator<Item = TypeId>) -> (Vec<u32>, Layout) {
    pack_layouts(fields.into_iter().map(|f| types.layout(f)))
}

/// [`pack`], from the fields' layouts.
pub fn pack_layouts(fields: impl IntoIterator<Item = Layout>) -> (Vec<u32>, Layout) {
    let mut offsets = Vec::new();
    let mut end = 0u32;
    let mut align = 1;
    for l in fields {
        let at = round_up(l.align, end);
        offsets.push(at);
        end = at.saturating_add(l.size);
        align = align.max(l.align);
    }
    (offsets, Layout { size: round_up(align, end.max(1)), align })
}

/// Each field's byte offset in a struct or an enum (whose payloads all start at one offset).
pub fn field_offsets(types: &Types, t: TypeId) -> &[u32] {
    types.field_offsets(t)
}

/// The scalars a value of type `t` holds, in order (fields, then elements, then components;
/// a matrix's columns), with their byte offsets: how an export returns it (each one a WASM
/// result). An enum's, a run's or a pointer's aren't listed.
pub fn scalars(types: &Types, t: TypeId) -> Vec<(u32, Scalar)> {
    fn go(types: &Types, t: TypeId, at: u32, out: &mut Vec<(u32, Scalar)>) {
        match types.get(t) {
            TypeDef::Scalar(s) => out.push((at, *s)),
            TypeDef::Vector(s, n) => {
                let size = scalar_layout(*s).size;
                out.extend((0..u32::from(*n)).map(|c| (at + c * size, *s)));
            }
            TypeDef::Matrix(n) => {
                for c in 0..u32::from(*n) {
                    out.extend(
                        (0..u32::from(*n))
                            .map(|r| (at + c * column_stride(*n) + 4 * r, Scalar::F32)),
                    );
                }
            }
            TypeDef::Array(e, n) => {
                let stride = array_stride(types, *e);
                for i in 0..*n {
                    go(types, *e, at + i * stride, out);
                }
            }
            TypeDef::Struct { fields, .. } => {
                let offsets = field_offsets(types, t);
                for (k, (_, f)) in fields.iter().enumerate() {
                    go(types, *f, at + offsets[k], out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    go(types, t, 0, &mut out);
    out
}

/// The distance between a `matN`'s columns.
pub fn column_stride(n: u8) -> u32 {
    vector_layout(n, 4).stride()
}

/// The distance between consecutive elements of an array of `elem`.
pub fn array_stride(types: &Types, elem: TypeId) -> u32 {
    types.layout(elem).stride()
}

/// Whether a type's layout meets WGSL's extra rules for the uniform address space: arrays have
/// a 16-byte stride, and struct and array members start on 16 bytes (with the member after a
/// struct starting at least a 16-byte-rounded size later).
pub fn uniform_compatible(types: &Types, t: TypeId) -> bool {
    match types.get(t) {
        // A struct there (`gpu_memory`): its tag, and the payload's words in an array of `u32`s
        // or of vectors of them, as wide as the enum's alignment, which needs a 16-byte stride.
        TypeDef::Enum { variants, .. } => {
            variants.iter().all(|(_, p)| p.is_none()) || layout(types, t).align == 16
        }
        // A `bool` is a `u32` there (`gpu_memory`).
        TypeDef::Scalar(s) => s.on_gpu(),
        TypeDef::Vector(s, _) => s.on_gpu(),
        TypeDef::Matrix(_) => true,
        TypeDef::Array(e, _) => {
            array_stride(types, *e).is_multiple_of(16) && uniform_compatible(types, *e)
        }
        TypeDef::Struct { fields, .. } => {
            let offsets = field_offsets(types, t);
            for (i, (_, f)) in fields.iter().enumerate() {
                if !uniform_compatible(types, *f) {
                    return false;
                }
                let nested = matches!(
                    types.get(*f),
                    TypeDef::Struct { .. } | TypeDef::Enum { .. } | TypeDef::Array(..)
                );
                if nested && !offsets[i].is_multiple_of(16) {
                    return false;
                }
                if matches!(types.get(*f), TypeDef::Struct { .. } | TypeDef::Enum { .. })
                    && let Some(&next) = offsets.get(i + 1)
                    && next < offsets[i].saturating_add(round_up(16, layout(types, *f).size))
                {
                    return false;
                }
            }
            true
        }
        TypeDef::RuntimeArray(_) | TypeDef::Run(_) | TypeDef::Ptr(_) | TypeDef::Atomic(_) => false,
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
        assert_eq!(field_offsets(&t, a), [0, 4, 8, 16]);
        // vec3 is 12 bytes with 16-byte alignment: a following f32 packs into its tail.
        let b = t.intern(TypeDef::Struct {
            name: "B".into(),
            fields: vec![("p".into(), v3), ("r".into(), f32)],
        });
        assert_eq!(field_offsets(&t, b), [0, 12]);
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
        assert_eq!(field_offsets(&t, e), [0, 16, 16, 16]);
        assert_eq!(t.field(e, 0), Some(u32t));
        assert_eq!(t.field(e, 1), None);
        assert_eq!(t.field(e, 3), Some(wide));
    }
}
