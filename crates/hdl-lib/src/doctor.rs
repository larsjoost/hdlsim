//! `hdlsim doctor` support: environment sanity checks as data (stdlib
//! location/resolution, project config, `-L` library dirs, `-I` include dirs,
//! parse-cache writability). The CLI turns these into the printed report.

use crate::builtin;
use crate::{parse_library_config, LibraryManager};
use std::path::{Path, PathBuf};

/// Outcome of one check.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

/// One named check with its outcome and a human-readable detail line.
#[derive(Clone, Debug)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

impl Check {
    fn new(name: &'static str, status: Status, detail: String) -> Check {
        Check {
            name,
            status,
            detail,
        }
    }
    fn ok(name: &'static str, detail: String) -> Check {
        Check::new(name, Status::Ok, detail)
    }
    fn warn(name: &'static str, detail: String) -> Check {
        Check::new(name, Status::Warn, detail)
    }
    fn fail(name: &'static str, detail: String) -> Check {
        Check::new(name, Status::Fail, detail)
    }
}

/// Where the parse cache should live: disabled, or a directory.
#[derive(Clone, Debug)]
pub enum CacheSetting {
    Disabled,
    Dir(PathBuf),
}

/// Build the cache setting from explicit options (`--no-cache`,
/// `--cache-dir`); default is `.hdlsim-cache/` in the working directory.
pub fn cache_setting(no_cache: bool, dir: Option<PathBuf>) -> CacheSetting {
    if no_cache {
        return CacheSetting::Disabled;
    }
    CacheSetting::Dir(dir.unwrap_or_else(|| PathBuf::from(".hdlsim-cache")))
}

/// Run all environment checks.
/// - `config`: project config path (e.g. `./hdlsim.ini`), if any
/// - `libs`: `-L name=dir` library mappings
/// - `incdirs`: `-I` include directories
/// - `cache`: where the parse cache lives (see [`cache_setting`])
pub fn checks(
    config: Option<&Path>,
    libs: &[(String, PathBuf)],
    incdirs: &[PathBuf],
    cache: CacheSetting,
) -> Vec<Check> {
    let mut out = Vec::new();

    {
        let custom = std::env::var("HDLSIM_STDLIB").ok().map(PathBuf::from);
        let providers = builtin::stdlib_providers();
        let scanned: Vec<String> = providers
            .iter()
            .flat_map(|p| p.scan().into_iter().map(|(u, _)| u))
            .collect();
        let mut missing: Vec<&str> = Vec::new();
        for unit in ["standard", "std_logic_1164", "numeric_std"] {
            if !scanned.iter().any(|u| u == unit) {
                missing.push(unit);
            }
        }
        if missing.is_empty() {
            let where_ = match &custom {
                Some(d) if d.is_dir() => format!("{} (custom $HDLSIM_STDLIB)", d.display()),
                _ => "embedded in the binary".to_string(),
            };
            out.push(Check::ok(
                "stdlib",
                format!("{} (std, ieee present)", where_),
            ));
        } else {
            out.push(Check::fail(
                "stdlib",
                format!("missing units: {}", missing.join(", ")),
            ));
        }
    }

    let mut mgr = LibraryManager::new();
    mgr.ensure_index();
    let mut unresolved: Vec<&str> = Vec::new();
    for unit in ["standard", "std_logic_1164", "numeric_std"] {
        if mgr.unit_file(unit).is_none() {
            unresolved.push(unit);
        }
    }
    if unresolved.is_empty() {
        out.push(Check::ok(
            "stdlib units",
            "standard, std_logic_1164, numeric_std resolve".to_string(),
        ));
    } else {
        out.push(Check::fail(
            "stdlib units",
            format!("unresolved: {}", unresolved.join(", ")),
        ));
    }

    match config {
        Some(c) => match std::fs::read_to_string(c) {
            Ok(text) => {
                let base = c.parent().unwrap_or_else(|| Path::new("."));
                let mapped = parse_library_config(&text, base);
                let missing: Vec<String> = mapped
                    .iter()
                    .filter(|(_, p)| !p.is_dir())
                    .map(|(n, p)| format!("{}={}", n, p.display()))
                    .collect();
                if missing.is_empty() {
                    out.push(Check::ok(
                        "config",
                        format!("{} ({} libraries)", c.display(), mapped.len()),
                    ));
                } else {
                    out.push(Check::warn(
                        "config",
                        format!("{} maps missing dirs: {}", c.display(), missing.join(", ")),
                    ));
                }
            }
            Err(e) => out.push(Check::fail(
                "config",
                format!("cannot read {}: {}", c.display(), e),
            )),
        },
        None => out.push(Check::ok(
            "config",
            "no hdlsim.ini in the working directory (built-in std/ieee only)".to_string(),
        )),
    }

    for (name, path) in libs {
        if path.is_dir() {
            out.push(Check::ok(
                "library",
                format!("{}={} exists", name, path.display()),
            ));
        } else {
            out.push(Check::warn(
                "library",
                format!("{}={} does not exist", name, path.display()),
            ));
        }
    }

    for d in incdirs {
        if d.is_dir() {
            out.push(Check::ok("incdir", d.display().to_string()));
        } else {
            out.push(Check::warn(
                "incdir",
                format!("{} does not exist", d.display()),
            ));
        }
    }

    match cache {
        CacheSetting::Disabled => {
            out.push(Check::ok("cache", "disabled via --no-cache".to_string()))
        }
        CacheSetting::Dir(dir) => {
            let probe = dir.join(format!(".doctor-probe-{}", std::process::id()));
            let res = std::fs::create_dir_all(&dir).and_then(|_| {
                std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe))
            });
            match res {
                Ok(()) => out.push(Check::ok("cache", format!("{} writable", dir.display()))),
                Err(e) => out.push(Check::warn(
                    "cache",
                    format!("{} not writable: {}", dir.display(), e),
                )),
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn tmpdir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!(
            "hdlsim_doctor_{}_{}_{}",
            std::process::id(),
            tag,
            n
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn stdlib_is_found_and_resolves() {
        let cs = checks(None, &[], &[], CacheSetting::Disabled);
        let stdlib = cs.iter().find(|c| c.name == "stdlib").unwrap();
        assert_eq!(stdlib.status, Status::Ok, "{}", stdlib.detail);
        let units = cs.iter().find(|c| c.name == "stdlib units").unwrap();
        assert_eq!(units.status, Status::Ok, "{}", units.detail);
    }

    #[test]
    fn config_with_missing_library_dir_warns() {
        let d = tmpdir("cfg");
        let cfg = d.join("hdlsim.ini");
        std::fs::write(&cfg, "[Library]\nwork = rtl\nmissing = nope\n").unwrap();
        let cs = checks(Some(&cfg), &[], &[], CacheSetting::Disabled);
        let c = cs.iter().find(|c| c.name == "config").unwrap();
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
        assert!(c.detail.contains("missing="), "{}", c.detail);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_lib_and_incdir_warn_but_cache_dir_ok() {
        let d = tmpdir("dirs");
        let cache = d.join("cache");
        let libs = vec![("work".to_string(), d.join("nope"))];
        let incs = vec![d.join("also-nope")];
        let cs = checks(None, &libs, &incs, CacheSetting::Dir(cache.clone()));
        assert_eq!(
            cs.iter().find(|c| c.name == "library").unwrap().status,
            Status::Warn
        );
        assert_eq!(
            cs.iter().find(|c| c.name == "incdir").unwrap().status,
            Status::Warn
        );
        let cache_check = cs.iter().find(|c| c.name == "cache").unwrap();
        assert_eq!(cache_check.status, Status::Ok, "{}", cache_check.detail);
        assert!(cache.is_dir());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unwritable_cache_warns_not_fails() {
        // A path under a regular file cannot be created as a directory.
        let d = tmpdir("ro");
        let f = d.join("afile");
        std::fs::write(&f, b"x").unwrap();
        let cs = checks(None, &[], &[], CacheSetting::Dir(f.join("cache")));
        let cache_check = cs.iter().find(|c| c.name == "cache").unwrap();
        assert_eq!(cache_check.status, Status::Warn, "{}", cache_check.detail);
        let _ = std::fs::remove_dir_all(&d);
    }
}
