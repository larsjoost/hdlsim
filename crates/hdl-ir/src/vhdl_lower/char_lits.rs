//! std.standard control-character literal constants: `LF`, `CR`, `HT`,
//! `NUL`, ... are enumeration literals of type `character` (VHDL is
//! case-insensitive), registered as 8-bit constant values.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn register_char_lits(&mut self) {
        const CHAR_LITS: &[(&str, u8)] = &[
            ("nul", 0),
            ("soh", 1),
            ("stx", 2),
            ("etx", 3),
            ("eot", 4),
            ("enq", 5),
            ("ack", 6),
            ("bel", 7),
            ("bs", 8),
            ("ht", 9),
            ("lf", 10),
            ("vt", 11),
            ("ff", 12),
            ("cr", 13),
            ("so", 14),
            ("si", 15),
            ("dle", 16),
            ("dc1", 17),
            ("dc2", 18),
            ("dc3", 19),
            ("dc4", 20),
            ("nak", 21),
            ("syn", 22),
            ("etb", 23),
            ("can", 24),
            ("em", 25),
            ("sub", 26),
            ("esc", 27),
            ("fs", 28),
            ("gs", 29),
            ("rs", 30),
            ("us", 31),
            ("del", 127),
        ];
        for (n, c) in CHAR_LITS {
            Rc::make_mut(&mut self.consts).insert(n.to_string(), Value::from_u64(8, *c as u64));
        }
    }
}
