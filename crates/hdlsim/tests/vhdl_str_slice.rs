//! Regression tests for string/field element and slice addressing in
//! const-evaluated subprograms and runtime variables. The pre-fix lowerer
//! baked stale shape state from earlier subprograms into slice bounds
//! (`warning[W0401]`, or silently corrupted folded bytes), addressed
//! 1-based string formals at raw bit offsets, and lost the low bound of
//! array-typed record fields.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{}", name))
}

fn run(top: &str, file: &str) -> (String, String, bool) {
    let out = hdlsim()
        .args(["run", "--top", top])
        .arg(fixture(file))
        .output()
        .expect("runs");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn string_building_functions_fold_to_exact_ascii() {
    let (stdout, stderr, ok) = run("strpad_tb", "vhdl_strpad_tb.vhd");
    assert!(ok, "expected exit 0\n{stdout}\n{stderr}");
    // A function lowered first (array formal) must not poison the string
    // functions' folds through aliased value slots.
    assert!(stdout.contains("T3=6"), "{stdout}");
    // pad: dynamic local + (others => ' ') + `result(1 to arg'length)`.
    assert!(stdout.contains("P5=[abc  ]"), "{stdout}");
    assert!(stdout.contains("P3=[hi ]"), "{stdout}");
    // Empty actual: 'length is 0, the guarded assign must not run, and the
    // result is all spaces (not one phantom NUL character short).
    assert!(stdout.contains("P0=[    ]"), "{stdout}");
    // Same-name overloads dispatch by arity.
    assert!(stdout.contains("R1=[ab..]"), "{stdout}");
    assert!(stdout.contains("R2=[ab...]"), "{stdout}");
    // Slice read of an unconstrained string formal is 1-based:
    // `arg(1 to 4)` of "abcdef" is "abcd", not "bcde".
    assert!(stdout.contains("H6=[abcd]"), "{stdout}");
    assert!(stdout.contains("H2=[hi..]"), "{stdout}");
    // No X-fallback warning for the folded initializers.
    assert!(!stderr.contains("W0401"), "{stderr}");
}

#[test]
fn field_array_slice_assign_writes_field_relative_bits() {
    let (stdout, stderr, ok) = run("farr_tb", "vhdl_field_arr_tb.vhd");
    assert!(ok, "expected exit 0\n{stdout}\n{stderr}");
    // opts'length=2: fields 0/1 written, field 2 untouched (X != "    "),
    // count comparison holds -> 1000.
    assert!(stdout.contains("S=1000"), "{stdout}");
    // Written fields read back exactly: ident(0)="ab  " (+1),
    // ident(1)="cd  " (+10), untouched field 2 not "    " (no +100),
    // count=2 (+1000) -> 1011.
    assert!(stdout.contains("S2=1011"), "{stdout}");
    assert!(!stderr.contains("W0401"), "{stderr}");
}

#[test]
fn string_and_bitvector_variable_addressing_follows_object_ranges() {
    let (stdout, _stderr, ok) = run("strvar_tb", "vhdl_strvar_tb.vhd");
    assert!(ok, "expected exit 0\n{stdout}");
    // String variable: whole read, slice-assign, slice read, char read,
    // char write, whole assign.
    assert!(stdout.contains("P0=[abcde]"), "{stdout}");
    assert!(stdout.contains("P1=[     ]"), "{stdout}");
    assert!(stdout.contains("P2=[axyze]"), "{stdout}");
    assert!(stdout.contains("P3=[abc  ]"), "{stdout}");
    assert!(stdout.contains("P4=[xyz]"), "{stdout}");
    assert!(stdout.contains("P5=[e]"), "{stdout}");
    assert!(stdout.contains("P6=[qxyze]"), "{stdout}");
    assert!(stdout.contains("P7=[hello]"), "{stdout}");
    // Ascending bit_vector local: elements address MSB-first.
    assert!(stdout.contains("BVS=[11100000]"), "{stdout}");
    assert!(stdout.contains("BVI=[11100001]"), "{stdout}");
    assert!(stdout.contains("STS=[xyzaaaaa]"), "{stdout}");
    // Signals: ascending, descending, and non-zero-low ranges.
    assert!(stdout.contains("SA=[11100000]"), "{stdout}");
    assert!(stdout.contains("SD=[11100000]"), "{stdout}");
    assert!(stdout.contains("NZ=[11100000]"), "{stdout}");
}
