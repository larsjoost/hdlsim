//! Reproducer bundles: a minimal ustar TAR writer/reader, std-only.
//!
//! `hdlsim bundle` packs a project (sources, `hdlsim.ini`, library dirs,
//! include dirs) into one `.tar` file that can be attached to a GitHub issue,
//! and `--bundle repro.tar` on `run`/`check`/`debug` extracts it to a temp
//! dir and resolves everything from there — so a bug report is exactly one
//! file and reproduces with a single command.
//!
//! Only the ustar subset we write is required on read (plus GNU `L` longname
//! and pax `x`/`g` skip for robustness with foreign tars). Path traversal
//! (`..` components) is rejected on extract.

use std::path::{Component, Path, PathBuf};

/// One file in a bundle: archive path (always `/`-separated, relative) + bytes.
#[derive(Clone, Debug)]
pub struct BundleEntry {
    pub path: String,
    pub data: Vec<u8>,
}

const BLOCK: usize = 512;

fn octal(field: &mut [u8], value: u64) {
    let s = format!("{:0width$o}", value, width = field.len() - 1);
    field[..s.len()].copy_from_slice(s.as_bytes());
    field[s.len()] = 0;
}

fn put_str(field: &mut [u8], s: &str) {
    let b = s.as_bytes();
    let n = b.len().min(field.len());
    field[..n].copy_from_slice(&b[..n]);
}

/// Split an archive path into (prefix ≤ 155, name ≤ 100) at a `/`.
fn split_name(path: &str) -> Result<(String, String), String> {
    if path.len() <= 100 {
        return Ok((String::new(), path.to_string()));
    }
    let bytes = path.as_bytes();
    for i in (0..path.len()).rev() {
        if bytes[i] == b'/' && i <= 155 && path.len() - i - 1 <= 100 {
            return Ok((path[..i].to_string(), path[i + 1..].to_string()));
        }
    }
    Err(format!("bundle path too long for ustar: '{}'", path))
}

/// Serialize entries as a ustar tar archive.
pub fn write_tar(entries: &[BundleEntry]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for e in entries {
        if e.path.contains("..") || e.path.starts_with('/') {
            return Err(format!("unsafe bundle path '{}'", e.path));
        }
        let (prefix, name) = split_name(&e.path)?;
        let mut h = [0u8; BLOCK];
        put_str(&mut h[0..100], &name);
        put_str(&mut h[100..108], "0000644\0");
        put_str(&mut h[108..116], "0000000\0");
        put_str(&mut h[116..124], "0000000\0");
        octal(&mut h[124..136], e.data.len() as u64);
        octal(&mut h[136..148], 0); // mtime: fixed for reproducible bundles
        h[148..156].fill(b' '); // chksum field counts as spaces while summing
        h[156] = b'0'; // typeflag: regular file
        put_str(&mut h[257..263], "ustar\0");
        put_str(&mut h[263..265], "00");
        put_str(&mut h[265..297], "hdlsim");
        put_str(&mut h[297..329], "hdlsim");
        put_str(&mut h[345..500], &prefix);
        let sum: u64 = h.iter().map(|&b| b as u64).sum();
        let chk = format!("{:06o}\0 ", sum);
        h[148..156].copy_from_slice(chk.as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(&e.data);
        let pad = (BLOCK - (e.data.len() % BLOCK)) % BLOCK;
        out.extend(std::iter::repeat(0u8).take(pad));
    }
    out.extend(std::iter::repeat(0u8).take(2 * BLOCK)); // end-of-archive
    Ok(out)
}

fn get_str(h: &[u8], range: std::ops::Range<usize>) -> String {
    let end = h[range.clone()]
        .iter()
        .position(|&b| b == 0)
        .map(|p| range.start + p)
        .unwrap_or(range.end);
    String::from_utf8_lossy(&h[range.start..end]).to_string()
}

fn get_octal(h: &[u8], range: std::ops::Range<usize>) -> u64 {
    let s = get_str(h, range.clone());
    u64::from_str_radix(s.trim(), 8).unwrap_or(0)
}

/// Parse a tar archive into entries (regular files only; dirs/pax skipped).
pub fn read_tar(bytes: &[u8]) -> Result<Vec<BundleEntry>, String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut longname: Option<String> = None;
    while pos + BLOCK <= bytes.len() {
        let h = &bytes[pos..pos + BLOCK];
        if h.iter().all(|&b| b == 0) {
            break; // end-of-archive marker
        }
        let typeflag = h[156];
        let size = get_octal(h, 124..136) as usize;
        let data_start = pos + BLOCK;
        let data_end = data_start + size;
        if data_end > bytes.len() {
            return Err("truncated tar: file data exceeds archive".to_string());
        }
        let name = if let Some(ln) = longname.take() {
            ln
        } else {
            let prefix = get_str(h, 345..500);
            let name = get_str(h, 0..100);
            if prefix.is_empty() {
                name
            } else {
                format!("{}/{}", prefix, name)
            }
        };
        let data = bytes[data_start..data_end].to_vec();
        pos = data_end + (BLOCK - (size % BLOCK)) % BLOCK;
        match typeflag {
            b'0' | 0 => {
                if name.is_empty() {
                    continue;
                }
                out.push(BundleEntry { path: name, data });
            }
            b'L' => {
                // GNU longname: data is the name of the next entry.
                longname = Some(
                    String::from_utf8_lossy(&data)
                        .trim_end_matches('\0')
                        .to_string(),
                );
            }
            _ => {} // dirs, pax headers, links: skip
        }
    }
    Ok(out)
}

/// Extract a bundle into `dir` (created if needed). Returns the written files.
/// Rejects entries that would escape `dir` (`..`, absolute paths).
pub fn extract_to(bytes: &[u8], dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = read_tar(bytes)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    let mut written = Vec::new();
    for e in entries {
        let rel = Path::new(&e.path);
        if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(format!("unsafe bundle path '{}'", e.path));
        }
        let dest = dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|x| format!("cannot create {}: {}", parent.display(), x))?;
        }
        std::fs::write(&dest, &e.data)
            .map_err(|x| format!("cannot write {}: {}", dest.display(), x))?;
        written.push(dest);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tar_roundtrip() {
        let entries = vec![
            BundleEntry {
                path: "REPRO.md".to_string(),
                data: b"hello bundle".to_vec(),
            },
            BundleEntry {
                path: "src/tb.vhd".to_string(),
                data: vec![0u8; 1000], // spans multiple blocks
            },
            BundleEntry {
                path: "libs/work/a/b/c/".to_string() + &"x".repeat(90), // 106 chars: forces prefix split
                data: b"long name".to_vec(),
            },
        ];
        let tar = write_tar(&entries).unwrap();
        assert_eq!(tar.len() % BLOCK, 0);
        let back = read_tar(&tar).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].path, "REPRO.md");
        assert_eq!(back[0].data, b"hello bundle");
        assert_eq!(back[1].path, "src/tb.vhd");
        assert_eq!(back[1].data.len(), 1000);
        assert_eq!(back[2].data, b"long name");
        assert_eq!(
            back[2].path,
            "libs/work/a/b/c/".to_string() + &"x".repeat(90)
        );
    }

    #[test]
    fn tar_rejects_unsafe_paths() {
        let entries = vec![BundleEntry {
            path: "../evil".to_string(),
            data: vec![],
        }];
        assert!(write_tar(&entries).is_err());
        // And on extract, even a hand-crafted archive with .. is rejected.
        let mut tar = write_tar(&[BundleEntry {
            path: "ok.txt".to_string(),
            data: b"x".to_vec(),
        }])
        .unwrap();
        // splice in a malicious entry by rewriting the name field
        let evil = b"../evil\0";
        tar[..evil.len()].copy_from_slice(evil);
        let sum: u64 = tar[..148].iter().map(|&b| b as u64).sum::<u64>()
            + tar[156..].iter().map(|&b| b as u64).sum::<u64>()
            + 8 * 0x20;
        let chk = format!("{:06o}\0 ", sum);
        tar[148..156].copy_from_slice(chk.as_bytes());
        let dir = std::env::temp_dir().join(format!("hdlsim_bundle_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(extract_to(&tar, &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_writes_files() {
        let entries = vec![
            BundleEntry {
                path: "hdlsim.ini".to_string(),
                data: b"[Library]\nwork = src\n".to_vec(),
            },
            BundleEntry {
                path: "src/tb.vhd".to_string(),
                data: b"entity tb is end entity;\n".to_vec(),
            },
        ];
        let tar = write_tar(&entries).unwrap();
        let dir = std::env::temp_dir().join(format!("hdlsim_bundle_x_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let files = extract_to(&tar, &dir).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(
            std::fs::read_to_string(dir.join("src/tb.vhd")).unwrap(),
            "entity tb is end entity;\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
