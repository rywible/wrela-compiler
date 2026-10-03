//! The CPU state hash (AC7): FNV-1a 64 over every byte a program submits, in order.

pub const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Hashes submitted batches incrementally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateHash(u64);

impl Default for StateHash {
    fn default() -> Self {
        StateHash(FNV_OFFSET)
    }
}

impl StateHash {
    pub fn new() -> StateHash {
        StateHash::default()
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }

    pub fn value(&self) -> u64 {
        self.0
    }

    /// Sixteen lowercase hex digits, as both hosts print it.
    pub fn hex(&self) -> String {
        hex(self.0)
    }
}

/// A hash value as both hosts print it: sixteen lowercase hex digits.
pub fn hex(value: u64) -> String {
    format!("{value:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        // FNV-1a 64 reference values.
        let h = |s: &[u8]| {
            let mut x = StateHash::new();
            x.update(s);
            x.hex()
        };
        assert_eq!(h(b""), "cbf29ce484222325");
        assert_eq!(h(b"a"), "af63dc4c8601ec8c");
        assert_eq!(h(b"foobar"), "85944171f73967e8");
    }

    #[test]
    fn incremental_equals_whole() {
        let mut a = StateHash::new();
        a.update(b"foo");
        a.update(b"bar");
        let mut b = StateHash::new();
        b.update(b"foobar");
        assert_eq!(a, b);
    }
}
