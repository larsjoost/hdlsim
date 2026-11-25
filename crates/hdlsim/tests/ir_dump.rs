//! End-to-end test for `hdlsim ir`: the dump must be valid pretty JSON
//! describing the elaborated top — signals (with compact value encodings),
//! processes, and function bodies — and the top must resolve without `--top`
//! when the candidates are unambiguous (including a VHDL entity/arch pair).

use hdlsim::commands::dispatch;
use std::path::PathBuf;
use std::process::Command;

fn tmp_out(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("hdlsim_ir_test_{tag}.json"))
}

#[test]
fn ir_dump_writes_pretty_json_of_the_elaborated_top() {
    // Library use never enables the parse cache (that is CLI-only, --no-cache);
    let ex = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    let out = tmp_out("verilog");
    let _ = std::fs::remove_file(&out);

    let code = dispatch(&[
        "ir".to_string(),
        "--out".to_string(),
        out.display().to_string(),
        format!("{ex}/counter/tb_counter.v"),
    ])
    .unwrap();
    assert_eq!(code, 0);

    let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap())
        .expect("dump must be valid JSON");
    let sigs = j["signals"].as_array().expect("signals array");
    // Top testbench plus the DUT instance: hierarchical names must survive.
    assert!(
        sigs.iter().any(|s| s["name"].as_str() == Some("q"))
            || sigs
                .iter()
                .any(|s| s["name"].as_str().is_some_and(|n| n.ends_with(".q"))),
        "DUT output q missing from {:?}",
        sigs.iter().map(|s| s["name"].clone()).collect::<Vec<_>>()
    );
    // Values use the compact {"w","hex"|"bits"} shape, never bit arrays.
    for s in sigs {
        if let Some(init) = s["init"].as_object() {
            assert!(
                init.contains_key("hex") || init.contains_key("bits"),
                "{init:?}"
            );
            assert!(init["w"].is_u64(), "{init:?}");
        }
    }
    assert!(
        j["procs"].as_array().unwrap().len() > 0,
        "no processes in dump"
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
fn ir_dump_auto_picks_top_for_entity_arch_pair() {
    // Library use never enables the parse cache (that is CLI-only, --no-cache);
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ir_probe.vhd");
    let out = tmp_out("vhdl");
    let _ = std::fs::remove_file(&out);

    // The entity and its architecture compile as two modules; no --top
    // needed because they collapse to one candidate.
    let code = dispatch(&[
        "ir".to_string(),
        "--out".to_string(),
        out.display().to_string(),
        fixture.to_string(),
    ])
    .unwrap();
    assert_eq!(code, 0);

    let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap())
        .expect("dump must be valid JSON");
    let sigs = j["signals"].as_array().unwrap();
    let names: Vec<&str> = sigs.iter().filter_map(|s| s["name"].as_str()).collect();
    for want in ["clk", "cnt", "temp"] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }
    let cnt = sigs.iter().find(|s| s["name"] == "cnt").unwrap();
    assert_eq!(cnt["width"], 8);
    assert_eq!(
        cnt["init"]["hex"], "2a",
        "x\"2A\" init must read back as hex"
    );
    let temp = sigs.iter().find(|s| s["name"] == "temp").unwrap();
    assert_eq!(temp["kind"], "Real");
    assert_eq!(j["procs"].as_array().unwrap().len(), 2);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn ir_dump_unknown_top_is_an_error() {
    // Library use never enables the parse cache (that is CLI-only, --no-cache);
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ir_probe.vhd");
    assert!(dispatch(&[
        "ir".to_string(),
        "--top".to_string(),
        "no_such_unit".to_string(),
        fixture.to_string(),
    ])
    .is_err());
}

#[test]
fn ir_dump_maps_wraps_the_flat_ir_with_lower_maps() {
    // `--dump-maps` must keep the output ONE JSON document: the flat IR
    // under "ir", the lower-side diagnosis maps under "maps" — per-module
    // signal dims (architecture-local signals included, ids module-relative),
    // per-file object dims for constants/generics, and cache provenance.
    // Fixture: the `'length(d)` regression (2-D signal + package constant
    // + generic aggregate default), so every dim source is asserted.
    // Runs the binary (not in-process `dispatch`): global flags are stripped
    // by `apply_global_args` in `main`, which also arms the run-options
    // `OnceLock` — a per-process singleton an integration test must not race.
    let fx = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/vhdl_len_dim_tb.vhd"
    );
    let out = tmp_out("maps");
    let _ = std::fs::remove_file(&out);

    let ok = Command::new(env!("CARGO_BIN_EXE_hdlsim"))
        .args(["ir", "--dump-maps", "--top", "len_dim_tb"])
        .arg("--out")
        .arg(&out)
        .arg(fx)
        .status()
        .expect("hdlsim ir runs");
    assert!(ok.success(), "ir --dump-maps exited {ok:?}");

    let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap())
        .expect("dump must be valid JSON");
    assert!(j["ir"].get("signals").is_some(), "flat IR under 'ir'");
    let maps = &j["maps"];
    assert!(maps["cache"]["no_cache"].is_boolean(), "cache provenance");
    // Architecture signal plane_s: the dims the lowerer folds `'length(d)`
    // from — (0,5) x (0,2) on module-relative signal 0. Before --dump-maps
    // this lived only in a Lower-side map invisible to every dump.
    let arch = &maps["modules"]["len_dim_tb/sim"]["sig_dims"];
    assert_eq!(
        arch["0"],
        serde_json::json!([[0, 5], [0, 2]]),
        "plane_s dims as lowered: {arch}"
    );
    // Generic aggregate default + package constant with an inline decl
    // constraint cross files as obj_dims (dims are the object's, not the
    // unconstrained base type's).
    let files = maps["files"].as_object().expect("files object");
    let dims = files
        .values()
        .map(|f| &f["obj_dims"])
        .find(|d| d.get("g_grid").is_some())
        .expect("obj_dims for the fixture file");
    assert_eq!(
        dims["g_grid"],
        serde_json::json!([[0, 2], [0, 3]]),
        "generic aggregate default dims: {dims}"
    );
    assert_eq!(
        dims["c_origin"],
        serde_json::json!([[0, 2], [0, 3]]),
        "package constant inline-constraint dims: {dims}"
    );
    let _ = std::fs::remove_file(&out);
}
