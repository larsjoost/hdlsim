//! FST (Fast Signal Trace) wave format — the native binary wave format of
//! GTKWave, readable by `hdlsim wave file.fst -o out.html` and every GTKWave
//! build. This module is the format core: byte primitives, checksums, zlib/gzip
//! containers (stored-block writers, full inflate reader) and [`read_fst`],
//! which parses an `.fst` file into a [`Flat`] + event list. The writer sink
//! lives in `sinks/fst.rs`.
//!
//! Format summary (verified against GTKWave's `fstapi.c`): a file is a
//! sequence of blocks, each `u8 tag + u64be length` where the length includes
//! the 8 length bytes but not the tag. Blocks: header (tag 0, length 329),
//! value-change data (tag 1), blackout (2), geometry (3), hierarchy (4, gzip),
//! skip (255). Multi-byte integers are big-endian, variable-length integers are
//! LEB128 varints, strings are NUL-terminated. Value changes are grouped in one
//! VC block per time section; within it, a bits array (initial values), one
//! record chain per variable, a position (chain) table and a time table.

use hdl_ir::elab::{Flat, FlatSig, Scope};
use hdl_ir::value::{Value, V0, V1, VDASH, VH, VL, VU, VW, VX, VZ};
use hdl_ir::SigKind;
use hdl_sim::events::SimEvent;
use std::collections::BTreeMap;

// ---- byte-level writers (shared with the sink) --------------------------------

/// Append an unsigned LEB128 varint.
pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// Append a big-endian u64.
pub(crate) fn put_u64be(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Append a NUL-terminated string.
pub(crate) fn put_cstr(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

// ---- checksums ------------------------------------------------------------------

/// CRC-32 (IEEE 802.3, reflected, poly 0xEDB88320) — used by gzip.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Adler-32 — used by zlib.
pub fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

// ---- zlib/gzip containers with stored deflate blocks ---------------------------
//
// A "stored" deflate block copies its payload verbatim, so a valid zlib/gzip
// stream can be built without a compressor: header + stored blocks + checksum.
// Every inflater (zlib, gzip, GTKWave's bundled one) reads it.

fn stored_blocks(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 5 * (data.len() / 65535 + 1) + 5);
    let mut chunks = data.chunks(65535).peekable();
    if data.is_empty() {
        // one final empty stored block
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
        return out;
    }
    while let Some(c) = chunks.next() {
        let last = chunks.peek().is_none();
        out.push(if last { 1 } else { 0 });
        let len = c.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(c);
    }
    out
}

/// Wrap `data` in a zlib stream using only stored deflate blocks (level 0).
pub(crate) fn zlib_store(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    out.extend_from_slice(&stored_blocks(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Wrap `data` in a gzip stream using only stored deflate blocks.
pub(crate) fn gzip_store(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1F, 0x8B, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xFF];
    out.extend_from_slice(&stored_blocks(data));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

// ---- inflate (full DEFLATE decoder: stored + fixed + dynamic Huffman) -----------

struct BitIn<'a> {
    d: &'a [u8],
    pos: usize,
    acc: u32,
    n: u32,
}

impl<'a> BitIn<'a> {
    fn new(d: &'a [u8]) -> Self {
        BitIn {
            d,
            pos: 0,
            acc: 0,
            n: 0,
        }
    }
    fn bit(&mut self) -> Result<u32, String> {
        if self.n == 0 {
            self.acc = *self
                .d
                .get(self.pos)
                .ok_or_else(|| "inflate: unexpected end of stream".to_string())?
                as u32;
            self.pos += 1;
            self.n = 8;
        }
        let b = self.acc & 1;
        self.acc >>= 1;
        self.n -= 1;
        Ok(b)
    }
    /// Read `k` bits, LSB first.
    fn bits(&mut self, k: u32) -> Result<u32, String> {
        let mut v = 0u32;
        for i in 0..k {
            v |= self.bit()? << i;
        }
        Ok(v)
    }
    /// Discard bits up to the next byte boundary.
    fn align(&mut self) {
        self.n = 0;
    }
}

/// Canonical Huffman decoder (puff-style count/symbol decode).
struct Huff {
    count: [u16; 16],
    sym: Vec<u16>,
}

impl Huff {
    fn new(lens: &[u8]) -> Result<Huff, String> {
        let mut count = [0u16; 16];
        for &l in lens {
            if l as usize >= 16 {
                return Err("inflate: code length > 15".to_string());
            }
            count[l as usize] += 1;
        }
        count[0] = 0;
        let mut offs = [0u16; 16];
        for i in 1..16 {
            offs[i] = offs[i - 1] + count[i - 1];
        }
        let total = lens.iter().filter(|&&l| l != 0).count();
        let mut sym = vec![0u16; total];
        for (s, &l) in lens.iter().enumerate() {
            if l != 0 {
                sym[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huff { count, sym })
    }

    fn decode(&self, b: &mut BitIn) -> Result<u16, String> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16 {
            code |= b.bit()? as i32;
            let c = self.count[len] as i32;
            if code < first + c {
                let i = index + (code - first);
                return self
                    .sym
                    .get(i as usize)
                    .copied()
                    .ok_or_else(|| "inflate: bad huffman code".to_string());
            }
            index += c;
            first = (first + c) << 1;
            code <<= 1;
        }
        Err("inflate: huffman code too long".to_string())
    }
}

const LEN_BASE: [u32; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u32; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn fixed_tables() -> Result<(Huff, Huff), String> {
    let mut ll = [0u8; 288];
    for (i, l) in ll.iter_mut().enumerate() {
        *l = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let lit = Huff::new(&ll)?;
    let dist = Huff::new(&[5u8; 32])?;
    Ok((lit, dist))
}

fn dynamic_tables(b: &mut BitIn) -> Result<(Huff, Huff), String> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let hlit = b.bits(5)? as usize + 257;
    let hdist = b.bits(5)? as usize + 1;
    let hclen = b.bits(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err("inflate: too many codes".to_string());
    }
    let mut cl_lens = [0u8; 19];
    for i in 0..hclen {
        cl_lens[ORDER[i]] = b.bits(3)? as u8;
    }
    let cl = Huff::new(&cl_lens)?;
    let total = hlit + hdist;
    let mut lens = vec![0u8; total];
    let mut i = 0;
    while i < total {
        let s = cl.decode(b)?;
        match s {
            0..=15 => {
                lens[i] = s as u8;
                i += 1;
            }
            16 => {
                if i == 0 {
                    return Err("inflate: repeat with no previous".to_string());
                }
                let rep = 3 + b.bits(2)? as usize;
                if i + rep > total {
                    return Err("inflate: repeat overflow".to_string());
                }
                let prev = lens[i - 1];
                for _ in 0..rep {
                    lens[i] = prev;
                    i += 1;
                }
            }
            17 => {
                let rep = 3 + b.bits(3)? as usize;
                if i + rep > total {
                    return Err("inflate: repeat overflow".to_string());
                }
                i += rep;
            }
            18 => {
                let rep = 11 + b.bits(7)? as usize;
                if i + rep > total {
                    return Err("inflate: repeat overflow".to_string());
                }
                i += rep;
            }
            _ => return Err("inflate: bad code-length symbol".to_string()),
        }
    }
    if lens[256] == 0 {
        return Err("inflate: missing end-of-block code".to_string());
    }
    Ok((Huff::new(&lens[..hlit])?, Huff::new(&lens[hlit..])?))
}

fn inflate_block(b: &mut BitIn, out: &mut Vec<u8>, lit: &Huff, dist: &Huff) -> Result<(), String> {
    loop {
        let s = lit.decode(b)? as usize;
        if s < 256 {
            out.push(s as u8);
        } else if s == 256 {
            return Ok(());
        } else {
            let li = s - 257;
            if li >= 29 {
                return Err("inflate: bad length symbol".to_string());
            }
            let len = LEN_BASE[li] + b.bits(LEN_EXTRA[li])?;
            let ds = dist.decode(b)? as usize;
            if ds >= 30 {
                return Err("inflate: bad distance symbol".to_string());
            }
            let dd = (DIST_BASE[ds] + b.bits(DIST_EXTRA[ds])?) as usize;
            if dd > out.len() {
                return Err("inflate: distance too far back".to_string());
            }
            let start = out.len() - dd;
            for k in 0..len as usize {
                let v = out[start + k];
                out.push(v);
            }
        }
    }
}

/// Raw DEFLATE decompression. Returns the output and the number of input bytes
/// consumed (the checksum, if any, starts at that offset).
pub fn inflate_raw(d: &[u8]) -> Result<(Vec<u8>, usize), String> {
    let mut b = BitIn::new(d);
    let mut out = Vec::new();
    loop {
        let bfinal = b.bit()?;
        let btype = b.bits(2)?;
        match btype {
            0 => {
                b.align();
                let hdr =
                    b.d.get(b.pos..b.pos + 4)
                        .ok_or_else(|| "inflate: truncated stored block".to_string())?;
                let len = u16::from_le_bytes([hdr[0], hdr[1]]) as usize;
                let nlen = u16::from_le_bytes([hdr[2], hdr[3]]);
                if nlen != !(len as u16) {
                    return Err("inflate: stored block LEN/NLEN mismatch".to_string());
                }
                let data =
                    b.d.get(b.pos + 4..b.pos + 4 + len)
                        .ok_or_else(|| "inflate: truncated stored block".to_string())?;
                out.extend_from_slice(data);
                b.pos += 4 + len;
            }
            1 => {
                let (lit, dist) = fixed_tables()?;
                inflate_block(&mut b, &mut out, &lit, &dist)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut b)?;
                inflate_block(&mut b, &mut out, &lit, &dist)?;
            }
            _ => return Err("inflate: reserved block type".to_string()),
        }
        if bfinal == 1 {
            break;
        }
    }
    Ok((out, b.pos))
}

/// Decompress a zlib stream (RFC 1950), verifying the Adler-32 trailer.
pub fn zlib_decompress(d: &[u8]) -> Result<Vec<u8>, String> {
    if d.len() < 6 {
        return Err("zlib: too short".to_string());
    }
    let cmf = d[0];
    let flg = d[1];
    if cmf & 0x0f != 8 {
        return Err("zlib: not deflate".to_string());
    }
    if ((cmf as u16) * 256 + flg as u16) % 31 != 0 {
        return Err("zlib: bad header check".to_string());
    }
    let (out, used) = inflate_raw(&d[2..])?;
    let tail = d
        .get(2 + used..)
        .ok_or_else(|| "zlib: missing adler32".to_string())?;
    if tail.len() < 4 {
        return Err("zlib: missing adler32".to_string());
    }
    let want = u32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]);
    if adler32(&out) != want {
        return Err("zlib: adler32 mismatch".to_string());
    }
    Ok(out)
}

/// Decompress a gzip stream (RFC 1952), verifying CRC-32 and ISIZE.
pub fn gzip_decompress(d: &[u8]) -> Result<Vec<u8>, String> {
    if d.len() < 18 || d[0] != 0x1F || d[1] != 0x8B || d[2] != 8 {
        return Err("gzip: bad header".to_string());
    }
    let flg = d[3];
    let mut p = 10usize;
    let skip_cstr = |p: &mut usize| -> Result<(), String> {
        while d.get(*p) != Some(&0) {
            *p += 1;
            if *p >= d.len() {
                return Err("gzip: unterminated name/comment".to_string());
            }
        }
        *p += 1;
        Ok(())
    };
    if flg & 0x04 != 0 {
        let xl = u16::from_le_bytes([
            *d.get(p).ok_or("gzip: truncated")?,
            *d.get(p + 1).ok_or("gzip: truncated")?,
        ]) as usize;
        p += 2 + xl;
    }
    if flg & 0x08 != 0 {
        skip_cstr(&mut p)?;
    }
    if flg & 0x10 != 0 {
        skip_cstr(&mut p)?;
    }
    if flg & 0x02 != 0 {
        p += 2;
    }
    if p >= d.len() {
        return Err("gzip: truncated header".to_string());
    }
    let (out, used) = inflate_raw(&d[p..])?;
    let tail = d
        .get(p + used..)
        .ok_or_else(|| "gzip: missing trailer".to_string())?;
    if tail.len() < 8 {
        return Err("gzip: missing trailer".to_string());
    }
    let crc = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    let isize = u32::from_le_bytes([tail[4], tail[5], tail[6], tail[7]]);
    if crc32(&out) != crc {
        return Err("gzip: crc32 mismatch".to_string());
    }
    if isize != out.len() as u32 {
        return Err("gzip: isize mismatch".to_string());
    }
    Ok(out)
}

// ---- FST reader -----------------------------------------------------------------

/// FST var types we care about (GTKWave `fstVarType`).
const VT_REG: u8 = 5;
const VT_REAL: u8 = 3;
const VT_PORT: u8 = 18;

/// FST value-change codes for non-0/1 single-bit values, indexed by `(rcv >> 1) & 7`.
const RCV_STR: &[u8; 8] = b"xzhuwl-?";

fn char_to_bit(c: u8) -> u8 {
    match c {
        b'0' => V0,
        b'1' => V1,
        b'z' | b'Z' => VZ,
        b'h' | b'H' => VH,
        b'u' | b'U' => VU,
        b'w' | b'W' => VW,
        b'l' | b'L' => VL,
        b'-' => VDASH,
        _ => VX,
    }
}

/// MSB-first FST value string (one char per bit) back to a `Value` (LSB-first bits).
fn value_from_chars(chars: &[u8]) -> Value {
    Value {
        bits: chars.iter().rev().map(|&c| char_to_bit(c)).collect(),
    }
}

struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn new(b: &'a [u8]) -> Self {
        Cur { b, p: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.p.checked_add(n).ok_or("fst: overflow")?;
        let s = self.b.get(self.p..end).ok_or("fst: truncated")?;
        self.p = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u64be(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().map_err(|_| "fst: bad u64")?,
        ))
    }
    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        let mut shift = 0u32;
        loop {
            let b = self.u8()?;
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
            if shift > 63 {
                return Err("fst: varint overflow".to_string());
            }
        }
    }
    fn cstr(&mut self) -> Result<String, String> {
        let start = self.p;
        while self.p < self.b.len() && self.b[self.p] != 0 {
            self.p += 1;
        }
        if self.p >= self.b.len() {
            return Err("fst: unterminated string".to_string());
        }
        let s = String::from_utf8_lossy(&self.b[start..self.p]).into_owned();
        self.p += 1;
        Ok(s)
    }
}

struct HierVar {
    full_name: String,
    scope: u32,
    vartype: u8,
    dir: u8,
    len: u64,
    handle: u32, // 1-based; 0 = alias entry
}

/// Parse the (gunzipped) hierarchy section into scopes + vars.
fn parse_hier(data: &[u8]) -> Result<(Vec<Scope>, Vec<HierVar>), String> {
    let mut c = Cur::new(data);
    let mut scopes: Vec<Scope> = Vec::new();
    let mut vars: Vec<HierVar> = Vec::new();
    let mut stack: Vec<u32> = Vec::new();
    let mut next_handle = 0u32;
    let scope_name = |scopes: &Vec<Scope>, id: u32| -> String {
        // dotted path of scope id
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = Some(id);
        while let Some(i) = cur {
            let s = &scopes[i as usize];
            if !s.name.is_empty() {
                parts.push(s.name.as_str());
            }
            cur = s.parent;
        }
        parts.reverse();
        parts.join(".")
    };
    while c.p < data.len() {
        let tag = c.u8()?;
        match tag {
            254 => {
                // FST_ST_VCD_SCOPE: scopetype u8, name cstr, component cstr
                let _styp = c.u8()?;
                let name = c.cstr()?;
                let _comp = c.cstr()?;
                let parent = stack.last().copied();
                let id = scopes.len() as u32;
                scopes.push(Scope { name, parent });
                stack.push(id);
            }
            255 => {
                // upscope
                stack.pop();
            }
            252 => {
                // attr begin: typ u8, subtype u8, name cstr, arg varint
                let _ = c.u8()?;
                let _ = c.u8()?;
                let _ = c.cstr()?;
                let _ = c.varint()?;
            }
            253 => {} // attr end
            t if t < 100 => {
                // var entry: vartype u8, dir u8, name cstr, len varint, alias varint
                let dir = c.u8()?;
                let name = c.cstr()?;
                let len = c.varint()?;
                let alias = c.varint()? as u32;
                let scope = stack.last().copied().unwrap_or(0);
                let path = scope_name(&scopes, scope);
                let full_name = if path.is_empty() {
                    name
                } else {
                    format!("{}.{}", path, name)
                };
                let handle = if alias == 0 {
                    next_handle += 1;
                    next_handle
                } else {
                    alias // aliases an earlier handle; not a new signal
                };
                vars.push(HierVar {
                    full_name,
                    scope,
                    vartype: t,
                    dir,
                    len,
                    handle,
                });
            }
            _ => return Err(format!("fst: unknown hier tag {}", tag)),
        }
    }
    Ok((scopes, vars))
}

/// One decoded value-change record: time-table index delta + value.
struct Rec {
    tdelta: u64,
    val: Value,
}

/// Decode one variable's record chain. `real` selects the FST_VT_VCD_REAL
/// encoding: varint(tdelta<<1 | 1) + 8 raw f64 bytes (endianness `double_le`).
fn decode_records(
    data: &[u8],
    width: usize,
    real: bool,
    double_le: bool,
) -> Result<Vec<Rec>, String> {
    let mut c = Cur::new(data);
    let mut recs = Vec::new();
    while c.p < data.len() {
        let vli = c.varint()?;
        if real {
            if vli & 1 == 0 {
                return Err("fst: packed-binary real record unsupported".to_string());
            }
            let tdelta = vli >> 1;
            let b = c.take(8)?;
            let f = if double_le {
                f64::from_le_bytes(b.try_into().map_err(|_| "fst: bad real")?)
            } else {
                f64::from_be_bytes(b.try_into().map_err(|_| "fst: bad real")?)
            };
            recs.push(Rec {
                tdelta,
                val: Value::from_f64(f),
            });
            continue;
        }
        let tdelta = if width == 1 {
            if vli & 1 == 0 {
                vli >> 2
            } else {
                vli >> 4
            }
        } else {
            vli >> 1
        };
        let chars = if width == 1 {
            let ch = if vli & 1 == 0 {
                (((vli >> 1) & 1) as u8) | b'0'
            } else {
                RCV_STR[((vli >> 1) & 7) as usize]
            };
            vec![ch]
        } else if vli & 1 == 0 {
            // binary-packed: byte j holds chars 8j..8j+7, first char = bit 7
            let nbytes = (width + 7) / 8;
            let bytes = c.take(nbytes)?;
            (0..width)
                .map(|j| (((bytes[j / 8] >> (7 - (j % 8))) & 1) as u8) | b'0')
                .collect()
        } else {
            c.take(width)?.to_vec()
        };
        recs.push(Rec {
            tdelta,
            val: value_from_chars(&chars),
        });
    }
    Ok(recs)
}

/// Read an `.fst` file into a [`Flat`] (scopes + signals with initial values)
/// plus the event stream, ready to replay into any `WaveSink`.
pub fn read_fst(bytes: &[u8]) -> Result<(Flat, Vec<SimEvent>), String> {
    let mut c = Cur::new(bytes);
    if c.u8()? != 0 {
        return Err("not an FST file (first block is not a header)".to_string());
    }
    let hdr_len = c.u64be()?;
    if hdr_len != 329 {
        return Err(format!("unsupported FST header length {}", hdr_len));
    }
    let _start_time = c.u64be()?;
    let end_time = c.u64be()?;
    // Endian test: the 8 bytes of the f64 constant e = 2.718... in native
    // order; we read doubles little-endian iff the file wrote them so.
    let endian = c.take(8)?;
    let double_le = endian == 2.7182818284590452354f64.to_le_bytes();
    let double_be = endian == 2.7182818284590452354f64.to_be_bytes();
    if !double_le && !double_be {
        return Err("fst: bad endian test word".to_string());
    }
    let _mem = c.u64be()?;
    let _num_scopes = c.u64be()?;
    let _num_hier_vars = c.u64be()?;
    let maxhandle = c.u64be()?;
    let _num_vc = c.u64be()?;
    let timescale = c.u8()? as i8;
    let _version = c.take(128)?;
    let _date = c.take(119)?;
    let _filetype = c.u8()?;
    let _timezero = c.u64be()?;

    // Walk the remaining blocks; collect hier, geom and VC payload slices.
    let mut hier_raw: Option<Vec<u8>> = None;
    let mut geom_widths: Option<Vec<usize>> = None;
    let mut geom_reals: Option<Vec<bool>> = None;
    let mut vc_blocks: Vec<(usize, usize)> = Vec::new(); // (payload start, seclen)
    while c.p < bytes.len() {
        let tag = c.u8()?;
        let seclen = c.u64be()? as usize;
        if seclen < 8 {
            return Err("fst: block length < 8".to_string());
        }
        let payload_start = c.p;
        let payload = c.take(seclen - 8)?;
        match tag {
            3 => {
                // geometry: u64 uclen, u64 count, data (zlib if clen != uclen)
                let mut g = Cur::new(payload);
                let uclen = g.u64be()? as usize;
                let count = g.u64be()? as usize;
                let data = g.take(payload.len() - 16)?;
                let raw = if data.len() != uclen {
                    zlib_decompress(data)?
                } else {
                    data.to_vec()
                };
                let mut gc = Cur::new(&raw);
                let mut widths = Vec::with_capacity(count);
                let mut reals = Vec::with_capacity(count);
                for _ in 0..count {
                    let v = gc.varint()?;
                    reals.push(v == 0); // geom encodes reals as zero width
                    widths.push(if v == 0 {
                        8 // real: 8 raw f64 bytes
                    } else if v == 0xFFFF_FFFF {
                        0
                    } else {
                        v as usize
                    });
                }
                geom_widths = Some(widths);
                geom_reals = Some(reals);
            }
            4 => {
                // hierarchy: u64 uclen, gzip data
                let mut h = Cur::new(payload);
                let _uclen = h.u64be()?;
                let gz = h.take(payload.len() - 8)?;
                hier_raw = Some(gzip_decompress(gz)?);
            }
            1 | 5 | 8 => {
                vc_blocks.push((payload_start, seclen));
            }
            _ => {} // blackout, skip, unknown: ignored
        }
    }

    let (mut scopes, hier_vars) = parse_hier(&hier_raw.unwrap_or_default())?;
    if scopes.is_empty() {
        scopes.push(Scope {
            name: String::new(),
            parent: None,
        });
    }

    // Distinct vars in handle order (handle = 1-based index into signals).
    let nhandles = maxhandle as usize;
    let mut by_handle: Vec<Option<&HierVar>> = vec![None; nhandles.max(1)];
    for v in &hier_vars {
        if v.handle >= 1 && (v.handle as usize) <= by_handle.len() {
            if by_handle[v.handle as usize - 1].is_none() {
                by_handle[v.handle as usize - 1] = Some(v);
            }
        }
    }
    let width_of = |h: usize| -> usize {
        geom_widths
            .as_ref()
            .and_then(|w| w.get(h))
            .copied()
            .filter(|&w| w > 0)
            .or_else(|| {
                by_handle
                    .get(h)
                    .and_then(|v| v.map(|v| v.len as usize))
                    .filter(|&l| l > 0)
            })
            .unwrap_or(1)
            .max(1)
    };
    // A handle is real if geom says zero-width or the hier var is VT_REAL.
    let real_of = |h: usize| -> bool {
        geom_reals
            .as_ref()
            .and_then(|r| r.get(h))
            .copied()
            .unwrap_or(false)
            || by_handle
                .get(h)
                .and_then(|v| v.map(|v| v.vartype == VT_REAL))
                .unwrap_or(false)
    };

    // Events + per-handle changes from the VC blocks.
    let mut changes: Vec<Vec<(u64, Value)>> = vec![Vec::new(); nhandles.max(1)]; // (time, new)
    let mut inits: Vec<Option<Value>> = vec![None; nhandles.max(1)];
    let mut times: Vec<u64> = Vec::new();

    for &(pstart, seclen) in &vc_blocks {
        let mut v = Cur {
            b: bytes,
            p: pstart,
        };
        let payload_end = pstart + seclen - 8;
        let _beg_tim = v.u64be()?;
        let _end_tim = v.u64be()?;
        let _memreq = v.u64be()?;
        let uclen = v.varint()? as usize;
        let clen = v.varint()? as usize;
        let frame_count = v.varint()? as usize;
        let bits_raw = v.take(clen)?;
        let bits = if uclen != clen {
            zlib_decompress(bits_raw)?
        } else {
            bits_raw.to_vec()
        };
        // Time table lives at the very end of the block.
        let tsec = payload_end
            .checked_sub(24)
            .ok_or("fst: VC block too small")?;
        let mut t = Cur { b: bytes, p: tsec };
        let t_uclen = t.u64be()? as usize;
        let t_clen = t.u64be()? as usize;
        let t_count = t.u64be()? as usize;
        let tdata_start = tsec
            .checked_sub(t_clen)
            .ok_or("fst: bad time table offset")?;
        let tdata = bytes
            .get(tdata_start..tsec)
            .ok_or("fst: truncated time table")?;
        let traw = if t_uclen != t_clen {
            zlib_decompress(tdata)?
        } else {
            tdata.to_vec()
        };
        let mut tc = Cur::new(&traw);
        let mut blk_times = Vec::with_capacity(t_count);
        let mut acc = 0u64;
        for _ in 0..t_count {
            acc += tc.varint()?;
            blk_times.push(acc);
        }
        times.extend_from_slice(&blk_times);

        // Position (chain) table sits just before the pos_len u64.
        let indx_pntr = tdata_start
            .checked_sub(8)
            .ok_or("fst: bad chain table offset")?;
        let mut ic = Cur {
            b: bytes,
            p: indx_pntr,
        };
        let chain_len = ic.u64be()? as usize;
        let chain_start = indx_pntr
            .checked_sub(chain_len)
            .ok_or("fst: bad chain table length")?;
        let chain_data = bytes
            .get(chain_start..indx_pntr)
            .ok_or("fst: truncated chain table")?;
        let mut cc = Cur::new(chain_data);
        let vc_maxhandle = v.varint()? as usize;
        let packtype = v.u8()?;
        let vc_start = v.p - 1; // absolute offset of the packtype byte
        if packtype != b'Z' && packtype != b' ' {
            return Err(format!("fst: unsupported packtype '{}'", packtype as char));
        }
        // Decode the chain table into per-handle (position, length).
        let mut chain: Vec<u64> = Vec::with_capacity(vc_maxhandle + 1);
        let mut pval = 0u64;
        let mut prev_pos_idx = 0usize;
        let mut lengths: Vec<u64> = Vec::new();
        while cc.p < chain_data.len() {
            let val = cc.varint()?;
            if val == 0 {
                // alias escape (dynamic alias sections); not produced by our writer
                let _alias = cc.varint()?;
                chain.push(0);
                lengths.push(0);
            } else if val & 1 == 1 {
                pval += val >> 1;
                if !chain.is_empty() && prev_pos_idx < chain.len() {
                    lengths[prev_pos_idx] = pval - chain[prev_pos_idx];
                }
                chain.push(pval);
                prev_pos_idx = chain.len() - 1;
                lengths.push(0);
            } else {
                let zeros = (val >> 1) as usize;
                for _ in 0..zeros {
                    chain.push(0);
                    lengths.push(0);
                }
            }
        }
        // Sentinel: the records region ends where the position table begins;
        // chain positions are relative to the packtype byte (vc_start).
        let records_end = chain_start as u64 - vc_start as u64;
        if !chain.is_empty() && prev_pos_idx < chain.len() {
            lengths[prev_pos_idx] = records_end - chain[prev_pos_idx];
        }

        // Initial values from the bits array, split per handle (real = 8 raw
        // f64 bytes).
        let mut bc = Cur::new(&bits);
        for h in 0..frame_count {
            let slot = inits.get_mut(h);
            if real_of(h) {
                if let (Ok(b), Some(slot)) = (bc.take(8), slot) {
                    let f = if double_le {
                        f64::from_le_bytes(b.try_into().map_err(|_| "fst: bad real")?)
                    } else {
                        f64::from_be_bytes(b.try_into().map_err(|_| "fst: bad real")?)
                    };
                    *slot = Some(Value::from_f64(f));
                }
            } else {
                let w = width_of(h);
                if let (Ok(chars), Some(slot)) = (bc.take(w), slot) {
                    *slot = Some(value_from_chars(chars));
                }
            }
        }

        // Decode each handle's record chain.
        for (h, (&pos, &len)) in chain.iter().zip(lengths.iter()).enumerate() {
            if pos == 0 || h >= changes.len() {
                continue;
            }
            let rstart = vc_start + pos as usize;
            let rdata = bytes
                .get(rstart..rstart + len as usize)
                .ok_or("fst: truncated value changes")?;
            let mut rc = Cur::new(rdata);
            let wrlen = rc.varint()?;
            let body = &rdata[rc.p..];
            let recs_raw = if wrlen == 0 {
                body.to_vec()
            } else {
                if packtype != b'Z' {
                    return Err("fst: compressed records require packtype Z".to_string());
                }
                zlib_decompress(body)?
            };
            let w = width_of(h);
            let recs = decode_records(&recs_raw, w, real_of(h), double_le)?;
            let mut ti = 0u64;
            for rec in recs {
                ti += rec.tdelta;
                let time = blk_times
                    .get(ti as usize)
                    .ok_or("fst: record time index out of range")?;
                changes[h].push((*time, rec.val));
            }
        }
    }

    // Convert times to femtoseconds according to the file's timescale.
    let to_fs = |t: u64| -> u64 {
        let e = timescale as i32 + 15;
        if e >= 0 {
            t * 10u64.pow(e as u32)
        } else {
            t / 10u64.pow((-e) as u32)
        }
    };

    // Build the Flat: one signal per handle, in handle order.
    let mut signals = Vec::with_capacity(nhandles.max(1));
    for h in 0..nhandles.max(1) {
        let v = by_handle.get(h).copied().flatten();
        let w = width_of(h);
        let (name, scope, kind) = match v {
            Some(v) => {
                let kind = match (v.vartype, v.dir) {
                    (VT_REAL, _) => SigKind::Real,
                    (VT_REG, _) => SigKind::Reg,
                    (VT_PORT, 1) => SigKind::Input,
                    (VT_PORT, 2) => SigKind::Output,
                    (VT_PORT, 3) => SigKind::Inout,
                    _ => SigKind::Wire,
                };
                (v.full_name.clone(), v.scope, kind)
            }
            None => (format!("sig{}", h + 1), 0, SigKind::Wire),
        };
        signals.push(FlatSig {
            name,
            width: w,
            kind,
            init: inits.get(h).cloned().flatten(),
            mem: None,
            scope,
            signed: false,
            real_ranges: Vec::new(),
        });
    }

    // Build the event stream: time advances + changes, ordered by time.
    let mut events = Vec::new();
    let mut per_time: BTreeMap<u64, Vec<(u32, Value)>> = BTreeMap::new();
    let mut running: Vec<Value> = signals
        .iter()
        .map(|s| s.init.clone().unwrap_or_else(|| Value::x(s.width)))
        .collect();
    for (h, chs) in changes.iter().enumerate() {
        for (t, new) in chs {
            per_time
                .entry(*t)
                .or_default()
                .push((h as u32, new.clone()));
        }
    }
    for &t in &times {
        events.push(SimEvent::TimeAdvance { time_fs: to_fs(t) });
        if let Some(chs) = per_time.get(&t) {
            for (sig, new) in chs {
                let old = running[*sig as usize].clone();
                events.push(SimEvent::SignalChange {
                    sig: *sig,
                    old,
                    new: new.clone(),
                    time_fs: to_fs(t),
                });
                running[*sig as usize] = new.clone();
            }
        }
    }
    let _ = end_time;

    let flat = Flat {
        signals,
        procs: Vec::new(),
        assigns: Vec::new(),
        functions: Vec::new(),
        scopes,
        top_scope: 0,
    };
    Ok((flat, events))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for v in [0u64, 1, 127, 128, 300, 16384, u32::MAX as u64, u64::MAX] {
            let mut out = Vec::new();
            put_varint(&mut out, v);
            let mut c = Cur::new(&out);
            assert_eq!(c.varint().unwrap(), v);
            assert_eq!(c.p, out.len());
        }
    }

    #[test]
    fn checksum_known_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn zlib_store_roundtrip() {
        for data in [
            &b""[..],
            b"x",
            b"hdlsim fst test",
            &vec![0xABu8; 200_000][..], // crosses the 65535 stored-block boundary
        ] {
            let z = zlib_store(data);
            assert_eq!(zlib_decompress(&z).unwrap(), data);
        }
    }

    #[test]
    fn gzip_store_roundtrip() {
        for data in [&b""[..], b"hello fst", &vec![7u8; 100_000][..]] {
            let g = gzip_store(data);
            assert_eq!(gzip_decompress(&g).unwrap(), data);
        }
    }

    // Real zlib output (python3 zlib.compress level 6, fixed-Huffman block).
    const FIXED_TEXT: &str =
        "hdlsim FST test vector: the quick brown fox jumps over the lazy dog. 0123456789 \
    hdlsim FST test vector: the quick brown fox jumps over the lazy dog. 0123456789 \
    hdlsim FST test vector: the quick brown fox jumps over the lazy dog. 0123456789 \
    hdlsim FST test vector: the quick brown fox jumps over the lazy dog. 0123456789 \
    hdlsim FST test vector: the quick brown fox jumps over the lazy dog. 0123456789 \
    hdlsim FST test vector: the quick brown fox jumps over the lazy dog. 0123456789 \
    hdlsim FST test vector: the quick brown fox jumps over the lazy dog. 0123456789 \
    hdlsim FST test vector: the quick brown fox jumps over the lazy dog. 0123456789 ";
    const FIXED_ZLIB: &[u8] = &[
        120, 156, 203, 72, 201, 41, 206, 204, 85, 112, 11, 14, 81, 40, 73, 45, 46, 81, 40, 75, 77,
        46, 201, 47, 178, 82, 40, 201, 72, 85, 40, 44, 205, 76, 206, 86, 72, 42, 202, 47, 207, 83,
        72, 203, 175, 80, 200, 42, 205, 45, 40, 86, 200, 47, 75, 45, 2, 75, 231, 36, 86, 85, 42,
        164, 228, 167, 235, 41, 24, 24, 26, 25, 155, 152, 154, 153, 91, 88, 42, 100, 140, 154, 55,
        106, 30, 9, 230, 1, 0, 5, 40, 215, 121,
    ];

    // Real gzip output (python3 gzip.compress level 6) of the same text.
    const FIXED_GZIP: &[u8] = &[
        31, 139, 8, 0, 0, 0, 0, 0, 0, 255, 203, 72, 201, 41, 206, 204, 85, 112, 11, 14, 81, 40, 73,
        45, 46, 81, 40, 75, 77, 46, 201, 47, 178, 82, 40, 201, 72, 85, 40, 44, 205, 76, 206, 86,
        72, 42, 202, 47, 207, 83, 72, 203, 175, 80, 200, 42, 205, 45, 40, 86, 200, 47, 75, 45, 2,
        75, 231, 36, 86, 85, 42, 164, 228, 167, 235, 41, 24, 24, 26, 25, 155, 152, 154, 153, 91,
        88, 42, 100, 140, 154, 55, 106, 30, 9, 230, 1, 0, 241, 189, 78, 106, 128, 2, 0, 0,
    ];
    // Real zlib output, level 0 (stored blocks).
    const STORED_TEXT: &[u8] = b"stored block test payload 0123456789";
    const STORED_ZLIB: &[u8] = &[
        120, 1, 1, 36, 0, 219, 255, 115, 116, 111, 114, 101, 100, 32, 98, 108, 111, 99, 107, 32,
        116, 101, 115, 116, 32, 112, 97, 121, 108, 111, 97, 100, 32, 48, 49, 50, 51, 52, 53, 54,
        55, 56, 57, 244, 106, 11, 212,
    ];

    #[test]
    fn inflate_fixed_huffman_vector() {
        assert_eq!(zlib_decompress(FIXED_ZLIB).unwrap(), FIXED_TEXT.as_bytes());
        assert_eq!(gzip_decompress(FIXED_GZIP).unwrap(), FIXED_TEXT.as_bytes());
    }

    #[test]
    fn inflate_stored_vector() {
        assert_eq!(zlib_decompress(STORED_ZLIB).unwrap(), STORED_TEXT);
    }

    // Real zlib output (level 9, dynamic-Huffman block) of 800 lowercase letters
    // (python3 random.seed(7), bytes(randrange(97,123) for _ in range(800))).
    const DYN_TEXT: &str = "kemubcrdlsbqgbcnnchcrnbsdhuusbssmbhbrejnerdsjrvfdssugldrwcsbtgpvrnykosoljhzfwyhcsjqpkxojtcdqnfykepnb\
    vcyrszkkwltpszoccipwvcbxwjusvojwmvlaolftdpbgyjexhmmpcfomrienriwnlvmhecfehvhapsfijaenrltskewqtuvxboyv\
    zrmmmmdpumbgcgofdktbdaserdltacgtmeuiltlpddpoppjcedxkxipwfqagqlewrayqjucwiqlflyhrryqkuhtzzygzhmxzhgqp\
    lxaazipigwtlozxllchdhpgkgpttapulzucvdmzwygpfnzukczxmomxcxffeaesozuettpvlerreaazxudqxenggaigjqhyskirn\
    ebxlovsqnqereqqaoyftayzefeptxdrbkvqqrpzydrbhgibydqoraycoktqtqgwioqrzpqhwqirgoendmokcvhncgvjzdyewuvle\
    ieohxdmpfvhfwnqmknglkcxlakroowamkqtjqcdzhdciibyfiyenvimerqspwkcibzwfnciauczicthcidoakrnitebqwhdfibfg\
    jujqygjoqvfilzaibaaxqrgqphodvunvprmqjwghkgwxuemlbeacuxinfbcvmqvjthwjboffioailkrkhbjglfakmcpiqughqyac\
    icemsbmajjuhcsqyevwztmykxpejxtuebwqunxwzqeqyqszavszwvwuhcabeuldmorbuaurvhpiaozcxqrcvqcxxpizcihxyghxu";

    const DYN_ZLIB: &[u8] = &[
        120, 218, 13, 146, 89, 14, 0, 33, 8, 67, 207, 170, 136, 130, 59, 238, 122, 250, 25, 190, 8,
        41, 244, 165, 33, 96, 154, 26, 154, 137, 93, 139, 211, 144, 51, 16, 180, 172, 187, 161, 57,
        187, 238, 61, 105, 210, 13, 125, 198, 102, 186, 111, 203, 154, 222, 167, 139, 166, 109,
        232, 122, 184, 186, 90, 190, 161, 244, 18, 61, 61, 187, 47, 65, 247, 82, 195, 41, 126, 128,
        145, 108, 111, 192, 154, 245, 130, 219, 250, 11, 97, 199, 81, 251, 43, 0, 92, 247, 2, 125,
        182, 159, 125, 21, 191, 211, 138, 170, 68, 59, 76, 213, 238, 122, 60, 148, 82, 5, 91, 82,
        99, 204, 141, 119, 142, 43, 17, 130, 69, 90, 164, 106, 183, 236, 213, 63, 143, 163, 7, 220,
        50, 230, 58, 186, 220, 245, 90, 250, 203, 212, 153, 180, 3, 87, 172, 9, 67, 27, 213, 127,
        236, 56, 20, 184, 145, 112, 114, 28, 177, 26, 83, 75, 173, 30, 208, 156, 112, 126, 14, 43,
        202, 73, 196, 221, 212, 21, 63, 97, 179, 68, 27, 47, 181, 118, 37, 76, 26, 239, 93, 247,
        40, 157, 71, 78, 106, 60, 74, 61, 174, 236, 246, 136, 229, 157, 24, 129, 12, 85, 23, 92,
        29, 67, 213, 25, 223, 132, 101, 210, 219, 215, 85, 155, 223, 12, 240, 78, 42, 233, 192,
        177, 22, 21, 246, 242, 38, 142, 81, 87, 196, 214, 240, 191, 116, 166, 145, 131, 217, 57,
        197, 206, 11, 221, 30, 184, 101, 212, 39, 150, 213, 37, 11, 54, 20, 81, 229, 218, 161, 238,
        67, 139, 117, 28, 211, 116, 88, 34, 173, 190, 251, 183, 228, 88, 95, 35, 229, 71, 135, 18,
        134, 12, 113, 155, 139, 180, 87, 133, 182, 112, 115, 5, 179, 73, 37, 192, 162, 12, 110,
        249, 103, 46, 238, 249, 219, 51, 22, 58, 38, 85, 187, 200, 238, 44, 41, 100, 23, 3, 156,
        168, 66, 43, 101, 171, 20, 100, 120, 1, 243, 200, 0, 255, 22, 150, 47, 230, 197, 9, 155,
        244, 186, 3, 176, 126, 219, 102, 96, 53, 225, 49, 12, 2, 54, 229, 95, 205, 60, 80, 203, 38,
        99, 89, 91, 231, 167, 151, 235, 124, 145, 101, 57, 62, 197, 90, 169, 35, 237, 15, 145, 138,
        89, 51, 175, 218, 146, 248, 237, 40, 184, 125, 38, 166, 168, 81, 193, 60, 156, 173, 134,
        149, 100, 249, 65, 219, 235, 98, 45, 23, 197, 49, 180, 64, 218, 187, 104, 85, 72, 80, 89,
        166, 35, 185, 10, 24, 48, 117, 157, 148, 247, 243, 127, 60, 185, 184, 246, 27, 233, 134,
        83, 209, 159, 49, 81, 111, 153, 249, 236, 39, 40, 87, 250, 83, 171, 191, 189, 246, 175, 85,
        26, 103, 252, 163, 105, 122, 170, 217, 22, 85, 86, 229, 193, 207, 7, 75, 224, 156, 202, 15,
        152, 206, 117, 116, 230, 7, 82, 255, 85, 20,
    ];

    #[test]
    fn inflate_dynamic_huffman_vector() {
        assert_eq!(zlib_decompress(DYN_ZLIB).unwrap(), DYN_TEXT.as_bytes());
    }

    #[test]
    fn read_fst_rejects_garbage() {
        assert!(read_fst(b"").is_err());
        assert!(read_fst(b"NOPE nope nope").is_err());
        assert!(read_fst(&[0u8; 40]).is_err());
    }
}
