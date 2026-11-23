//! End-to-end test for `hdlsim deps`: given only the testbench file plus a
//! `-L work=...` library mapping, the transitive dependency list must include
//! the DUT file — for both Verilog and VHDL.

use hdlsim::commands::dispatch;

#[test]
fn deps_lists_transitive_files_verilog_and_vhdl() {
    // Library use never enables the parse cache (that is CLI-only, --no-cache);
    let ex = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");

    // Verilog: only the tb file is given; counter.v must be resolved from -L.
    let mut mgr = hdl_lib::LibraryManager::new();
    mgr.add_library_dir("work", &std::path::PathBuf::from(format!("{}/counter", ex)));
    let mut design = hdl_ir::Design::default();
    mgr.compile_files(
        &mut design,
        &[std::path::PathBuf::from(format!(
            "{}/counter/tb_counter.v",
            ex
        ))],
    );
    hdl_ir::elab::elaborate_with(&mut design, "tb_counter", &mut mgr).unwrap();
    let files: Vec<String> = mgr
        .compiled_files()
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        files.iter().any(|f| f.ends_with("counter.v")),
        "{:?}",
        files
    );
    assert!(
        files.iter().any(|f| f.ends_with("tb_counter.v")),
        "{:?}",
        files
    );

    // VHDL: same shape.
    let mut mgr = hdl_lib::LibraryManager::new();
    mgr.add_library_dir(
        "work",
        &std::path::PathBuf::from(format!("{}/vhdl-counter", ex)),
    );
    let mut design = hdl_ir::Design::default();
    mgr.compile_files(
        &mut design,
        &[std::path::PathBuf::from(format!(
            "{}/vhdl-counter/tb_counter.vhd",
            ex
        ))],
    );
    hdl_ir::elab::elaborate_with(&mut design, "tb_counter", &mut mgr).unwrap();
    let files: Vec<String> = mgr
        .compiled_files()
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        files.iter().any(|f| f.ends_with("counter.vhd")),
        "{:?}",
        files
    );
    assert!(
        files.iter().any(|f| f.ends_with("tb_counter.vhd")),
        "{:?}",
        files
    );

    // CLI: exit code 0 for a good top, 1 for an unknown top.
    let code = dispatch(&[
        "deps".to_string(),
        "--top".to_string(),
        "tb_counter".to_string(),
        "-L".to_string(),
        format!("work={}/counter", ex),
        format!("{}/counter/tb_counter.v", ex),
    ])
    .unwrap();
    assert_eq!(code, 0);
    let code = dispatch(&[
        "deps".to_string(),
        "--top".to_string(),
        "no_such_top".to_string(),
        format!("{}/counter/tb_counter.v", ex),
    ])
    .unwrap();
    assert_eq!(code, 1);
}
