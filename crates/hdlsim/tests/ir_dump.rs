//! End-to-end test for `hdlsim ir`: the dump must be valid pretty JSON
//! describing the elaborated top — signals (with compact value encodings),
//! processes, and function bodies — and the top must resolve without `--top`
//! when the candidates are unambiguous (including a VHDL entity/arch pair).

use hdlsim::commands::dispatch;
use std::path::PathBuf;

fn tmp_out(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("hdlsim_ir_test_{tag}.json"))
}

#[test]
fn ir_dump_writes_pretty_json_of_the_elaborated_top() {
    std::env::set_var("HDLSIM_NO_CACHE", "1");
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
    std::env::set_var("HDLSIM_NO_CACHE", "1");
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
    std::env::set_var("HDLSIM_NO_CACHE", "1");
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ir_probe.vhd");
    assert!(dispatch(&[
        "ir".to_string(),
        "--top".to_string(),
        "no_such_unit".to_string(),
        fixture.to_string(),
    ])
    .is_err());
}
