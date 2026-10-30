use std::{fs, process::Command};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cargo-raster"))
}

#[test]
fn compression_help_requires_no_project() {
    let root = tempfile::tempdir().unwrap();
    let output = cli()
        .current_dir(root.path())
        .args(["raster", "proof", "compress", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for flag in ["--receipt", "--commitment", "--output"] {
        assert!(help.contains(flag));
    }
}

#[test]
fn existing_destination_and_inputs_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("input"), b"original").unwrap();
    fs::create_dir(root.path().join("output")).unwrap();
    let output = cli()
        .current_dir(root.path())
        .env_remove("RISC0_DEV_MODE")
        .args([
            "raster",
            "proof",
            "compress",
            "--receipt",
            "input",
            "--commitment",
            "input",
            "--output",
            "output",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already exists"));
    assert_eq!(fs::read(root.path().join("input")).unwrap(), b"original");
    assert_eq!(fs::read_dir(root.path().join("output")).unwrap().count(), 0);
}

#[test]
fn development_mode_is_rejected_without_creating_output() {
    let root = tempfile::tempdir().unwrap();
    let output = cli()
        .current_dir(root.path())
        .env("RISC0_DEV_MODE", "1")
        .args([
            "raster",
            "proof",
            "compress",
            "--receipt",
            "unused",
            "--commitment",
            "unused",
            "--output",
            "output",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("development mode disabled"));
    assert!(!root.path().join("output").exists());
}
