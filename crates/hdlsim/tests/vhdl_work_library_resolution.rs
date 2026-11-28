//! `use work.unit;` inside a unit compiled into a mapped library must resolve
//! in that library — `work` names the library the unit is analyzed into
//! (LRM 1076-2008 §13.2), not the ini's literal `work` row. A vendor library
//! that happens to declare a same-named unit (and is indexed earlier) must
//! not shadow the design's own copy: the global fallback only runs after the
//! owning library misses.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

/// Layout: `work = .` holds the tb; `vendor/` (indexed first) declares
/// `float_pkg` with the shadowing value; `design/` declares its own
/// `float_pkg` (unit name differs from the file stem, so only a parse scan
/// finds it) plus `consumer_pkg`, which pulls it in via `use work.float_pkg`.
fn setup(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("hdlsim_workres_{}_{}", tag, std::process::id()));
    fs::create_dir_all(base.join("vendor")).expect("create vendor");
    fs::create_dir_all(base.join("design")).expect("create design");
    fs::write(
        base.join("hdlsim.ini"),
        "[Library]\nwork = .\nvendor = vendor\ndesign = design\n",
    )
    .expect("write hdlsim.ini");
    write_libraries(&base);
    write_tb(&base);
    base
}

/// The two same-named `float_pkg` units: the vendor shadow (wrong value,
/// file stem matches the unit name so it is indexed without parsing) and
/// the design's own copy, plus `consumer_pkg` pulling it via `work`.
fn write_libraries(base: &Path) {
    let vendor = base.join("vendor");
    let design = base.join("design");

    // The vendor shadow: same unit name, file stem matches it (indexed by
    // name without parsing), declares the WRONG value.
    fs::write(
        vendor.join("float_pkg.vhd"),
        "package float_pkg is\n\
         \x20 constant C_ZERO : natural := 32;\n\
         end package float_pkg;\n",
    )
    .expect("write vendor pkg");

    // The design's own copy: the unit the `work` reference must find.
    fs::write(
        design.join("float_pkg_pkg.vhd"),
        "package float_pkg is\n\
         \x20 constant C_ZERO : natural := 8;\n\
         end package float_pkg;\n",
    )
    .expect("write design pkg");

    // Compiled into `design`: its `work.float_pkg` reference is in the
    // unit's own library, never in `work` or `vendor`.
    fs::write(
        design.join("consumer_pkg_pkg.vhd"),
        // Root-level context clause (the classic layout the resolver's
        // context-clause pass covers; a clause nested in the package's
        // declarative part lowers without a resolver round-trip).
        "use work.float_pkg.all;\n\
         package consumer_pkg is\n\
         \x20 constant W : positive := C_ZERO;\n\
         end package consumer_pkg;\n",
    )
    .expect("write consumer pkg");
}

/// The testbench in `work = .`: demands the design library's value.
fn write_tb(base: &Path) {
    fs::write(
        base.join("tb.vhd"),
        "library design;\n\
         use design.consumer_pkg.all;\n\
         entity tb is\n\
         end entity;\n\
         architecture sim of tb is\n\
         begin\n\
         \x20 process begin\n\
         \x20   report \"W=\" & integer'image(W);\n\
         \x20   assert W = 8\n\
         \x20     report \"work.float_pkg resolved to the wrong library: W=\" & integer'image(W)\n\
         \x20     severity failure;\n\
         \x20   wait;\n\
         \x20 end process;\n\
         end architecture;\n",
    )
    .expect("write tb");
}

fn run(work: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_hdlsim"))
        .current_dir(work)
        .arg("--no-cache")
        .args([
            "run",
            "--top",
            "tb",
            "tb.vhd",
            "--max-time",
            "1us",
            "--no-color",
        ])
        .output()
        .expect("hdlsim runs");
    (
        out.status.success(),
        // The transcript (report notes, failure lines) is stdout; diagnostics
        // are stderr. The value under test is reported, so capture both.
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn work_reference_resolves_in_the_owning_library() {
    let dir = setup("owner");
    let (ok, err) = run(&dir);
    assert!(ok, "the design's own float_pkg must win: {}", err);
    assert!(err.contains("W=8"), "expected the design's value: {}", err);
    let _ = fs::remove_dir_all(&dir);
}
