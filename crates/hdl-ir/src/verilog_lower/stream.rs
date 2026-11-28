//! The SV streaming operator's runtime semantics (`{<<W{e}}`, LRM
//! 11.4.14.3): the operand's value re-streamed in `slice`-wide chunks.
//! Shared by the lowerer's const-eval and the simulation kernel, which
//! applies it when it evaluates `Expr::Stream` (the lowerer has no general
//! expression width to reverse statically). `>>` never reaches the IR — it
//! is folded to the operand at lower time.

use crate::value::{Value, VX};

/// Reverse `v`'s `slice`-wide chunks, LSB chunk first (LRM: the stream is
/// unpacked starting from the LSB). If the width is not a multiple of the
/// slice, the top (most-significant) chunk is short: its missing bits read
/// X, mirroring an out-of-range part-select read.
pub fn stream_reverse(v: &Value, slice: u32) -> Value {
    let s = slice.max(1) as usize;
    let n = v.width();
    let chunks = n.div_ceil(s);
    let out: Vec<u8> = (0..n)
        .map(|t| {
            // Target bit t lives in chunk c = t / s at offset r = t % s. The
            // source is the mirrored chunk (n chunks counting the short top
            // one), same offset.
            let src = (chunks - 1 - t / s) * s + t % s;
            if src < n {
                v.bit(src)
            } else {
                VX
            }
        })
        .collect();
    Value {
        bits: out,
        real: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{V0, V1};

    fn vbits(bits: &[u8]) -> Value {
        Value {
            bits: bits.to_vec(),
            real: false,
        }
    }

    #[test]
    fn bit_stream_reverses() {
        // {<<{4'b1010}} = 4'b0101: bit i reads bit (n-1-i).
        let r = stream_reverse(&vbits(&[V0, V1, V0, V1]), 1);
        assert_eq!((r.bit(0), r.bit(1), r.bit(2), r.bit(3)), (V1, V0, V1, V0));
    }

    #[test]
    fn slice_stream_reverses_chunks() {
        // {<<2{4'b0001}}: chunks LSB-first are 01|00; reversed = 00|01 → 0100.
        let r = stream_reverse(&vbits(&[V1, V0, V0, V0]), 2);
        assert_eq!((r.bit(0), r.bit(1), r.bit(2), r.bit(3)), (V0, V0, V1, V0));
    }

    #[test]
    fn short_top_chunk_pads_with_x() {
        // 3 bits, slice 2: chunks LSB-first are [b0,b1] [b2(short)]. Reversed,
        // the short chunk lands first — its second bit has no source → X:
        // [V1,V0,V1] → [V1,VX,V1].
        let r = stream_reverse(&vbits(&[V1, V0, V1]), 2);
        assert_eq!((r.bit(0), r.bit(1), r.bit(2)), (V1, VX, V1));
    }

    #[test]
    fn identity_when_slice_covers_width() {
        let x = vbits(&[V1, V0, V1, V0]);
        let r = stream_reverse(&x, 4);
        assert_eq!(r.bits, x.bits);
    }
}
