//! `.hdlsim-cache/` — the persistent per-file parse cache.
//!
//! One cache file per source file, named by the hash of its canonical path.
//! File format (little-endian):
//! `magic "HDLCST1\0" | u32 format_version | u64 payload_fingerprint | u64 source_hash | payload`.
//! The payload fingerprint mixes the SyntaxKind table (`hdl_syntax::kinds::fingerprint`,
//! since CST payloads embed kind indices) with the crate version, so parser changes
//! also invalidate entries on upgrade.
//! The payload itself is composed by the caller (hdl-lib): an optional
//! timescale string, the encoded CST, and the encoded diagnostics — see
//! `LibraryManager`. A hit therefore skips lexing+parsing entirely; lowering
//! still runs every session (it is cheap next to parsing and needs the live
//! `SourceDb`).
//!
//! Invalidation is content-addressed: the key includes a hash of the source
//! text, so any edit is automatically a miss. No dependency edges are needed
//! at this level because only parse results (not lowered IR) are cached.
//!
//! Control: enabled by the CLI via [`Cache::from_options`] — `--no-cache`
//! disables, `--cache-dir` overrides the directory (default
//! `.hdlsim-cache/` in the working directory).

use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"HDLCST1\0";
const FORMAT_VERSION: u32 = 2;

/// Fingerprint of everything a cached payload depends on: the SyntaxKind table
/// plus the crate version (parser fixes that do not touch the kind table must
/// still invalidate old entries once a new version ships).
fn payload_fingerprint() -> u64 {
    let mut h = hdl_syntax::kinds::fingerprint();
    for b in concat!("v", env!("CARGO_PKG_VERSION"), "\0").as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// FNV-1a 64-bit hash of the text — the cache key for a file's contents.
pub fn hash64(text: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// Open (creating if needed) a cache directory. None if not writable.
    pub fn open(dir: PathBuf) -> Option<Cache> {
        std::fs::create_dir_all(&dir).ok()?;
        Some(Cache { dir })
    }

    /// Open the cache from explicit options: `no_cache` disables, `dir`
    /// overrides the location (default `.hdlsim-cache/`). The CLI parses
    /// `--no-cache` / `--cache-dir` and passes them here; environment
    /// variables have no spell check, so the cache is not configured
    /// through one.
    pub fn from_options(no_cache: bool, dir: Option<PathBuf>) -> Option<Cache> {
        if no_cache {
            return None;
        }
        Cache::open(dir.unwrap_or_else(|| PathBuf::from(".hdlsim-cache")))
    }

    fn entry(&self, file: &Path) -> PathBuf {
        let key = hash64(
            &std::fs::canonicalize(file)
                .unwrap_or_else(|_| file.to_path_buf())
                .to_string_lossy(),
        );
        self.dir.join(format!("{:016x}.bin", key))
    }

    /// Fetch the payload if the entry exists and matches `source_hash`.
    pub fn get(&self, file: &Path, source_hash: u64) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.entry(file)).ok()?;
        let mut r = Cursor::new(&bytes);
        if r.take(MAGIC.len())? != MAGIC {
            return None;
        }
        if r.u32()? != FORMAT_VERSION {
            return None;
        }
        if r.u64()? != payload_fingerprint() {
            return None;
        }
        if r.u64()? != source_hash {
            return None;
        }
        Some(r.rest().to_vec())
    }

    /// Store a payload (atomic via tmp+rename; failures are silently ignored —
    /// the cache is an optimization, never a correctness requirement).
    pub fn put(&self, file: &Path, source_hash: u64, payload: &[u8]) {
        let mut out = Vec::with_capacity(payload.len() + 32);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&payload_fingerprint().to_le_bytes());
        out.extend_from_slice(&source_hash.to_le_bytes());
        out.extend_from_slice(payload);
        let entry = self.entry(file);
        let tmp = entry.with_extension("tmp");
        if std::fs::write(&tmp, &out).is_ok() {
            let _ = std::fs::rename(&tmp, &entry);
        }
    }
}

// ---- tiny LE writer/reader used for payloads --------------------------------

pub fn w_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

pub fn w_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
    out.extend_from_slice(b);
}

pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8]) -> Cursor<'a> {
        Cursor { buf, pos: 0 }
    }

    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.buf.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }

    pub fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    pub fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    pub fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("hdlsim_cache_test_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn put_get_roundtrip_and_invalidation() {
        let dir = temp_dir("rt");
        let cache = Cache::open(dir.clone()).unwrap();
        let f = Path::new("src/counter.v");
        let payload = b"cst-bytes\0diags".to_vec();
        cache.put(f, hash64("module counter; endmodule"), &payload);
        assert_eq!(
            cache.get(f, hash64("module counter; endmodule")),
            Some(payload.clone())
        );
        // Edited source → different hash → miss.
        assert_eq!(cache.get(f, hash64("module counter; endmodule // x")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_entries_are_rejected() {
        let dir = temp_dir("corrupt");
        let cache = Cache::open(dir.clone()).unwrap();
        let f = Path::new("a.vhd");
        cache.put(f, hash64("x"), b"payload");
        let entry = cache.entry(f);
        let mut bytes = std::fs::read(&entry).unwrap();
        bytes[3] ^= 0xff; // break magic
        std::fs::write(&entry, &bytes).unwrap();
        assert_eq!(cache.get(f, hash64("x")), None);
        // Truncated file → None, no panic.
        let bytes = std::fs::read(&entry).unwrap();
        std::fs::write(&entry, &bytes[..5]).unwrap();
        assert_eq!(cache.get(f, hash64("x")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entries_with_stale_fingerprint_are_rejected() {
        let dir = temp_dir("stale");
        let cache = Cache::open(dir.clone()).unwrap();
        let f = Path::new("a.vhd");
        cache.put(f, hash64("x"), b"payload");
        // Corrupt the fingerprint field (layout: magic 8 | version 4 | fp 8 | hash 8).
        let entry = cache.entry(f);
        let mut bytes = std::fs::read(&entry).unwrap();
        bytes[12] ^= 0xff;
        std::fs::write(&entry, &bytes).unwrap();
        assert_eq!(cache.get(f, hash64("x")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
