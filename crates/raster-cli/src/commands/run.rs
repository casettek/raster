//! Run command: build and execute the user program as a whole.

use crate::BackendType;
use raster_core::{Error, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Run the user program with the specified backend.
///
/// Unlike `run-tile` which executes individual tiles, this command builds
/// and runs the entire user program.
pub fn run(backend_type: BackendType, input: Option<&str>) -> Result<()> {
    // Only native backend is supported for whole-program execution
    if backend_type != BackendType::Native {
        return Err(Error::Other(
            "Only the native backend is supported for running entire programs. \
             Use 'cargo raster run-tile' to execute individual tiles with the RISC0 backend."
                .into(),
        ));
    }

    let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Extract binary name from Cargo.toml
    let binary_name = extract_binary_name(&project_path)
        .ok_or_else(|| Error::Other("Could not determine binary name from Cargo.toml".into()))?;

    println!("Building project...");

    // Build the project with cargo build --release
    let build_status = Command::new("cargo")
        .current_dir(&project_path)
        .args(["build", "--release"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| Error::Other(format!("Failed to run cargo build: {}", e)))?;

    if !build_status.success() {
        return Err(Error::Other("cargo build failed".into()));
    }

    // Find the target directory using cargo metadata
    let target_dir = find_target_path(&project_path).unwrap_or_else(|| project_path.join("target"));
    let binary_path = target_dir.join("release").join(&binary_name);

    if !binary_path.exists() {
        return Err(Error::Other(format!(
            "Binary not found at: {}",
            binary_path.display()
        )));
    }

    println!();
    println!("Running {}...", binary_name);
    println!();

    // Build command with optional input argument
    let mut cmd = Command::new(&binary_path);
    cmd.current_dir(&project_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if let Some(input_json) = input {
        cmd.args(["--input", input_json]);
    }

    // Execute the binary and stream output
    let output = cmd
        .current_dir(".")
        .output()
        .map_err(|e| Error::Other(format!("Failed to execute binary: {}", e)))?;

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        return Err(Error::Other(format!("Program exited with code {}", code)));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Extract and pretty-print all RASTER_TRACE items
    for line in stdout.clone().lines() {
        if let Some(json_str) = line.strip_prefix("RASTER_TRACE:") {
            match serde_json::from_str::<serde_json::Value>(json_str) {
                Ok(trace_item) => {
                    if let Ok(pretty) = serde_json::to_string_pretty(&trace_item) {
                        println!("{}", pretty);
                    }
                }
                Err(e) => {
                    eprintln!("Failed to parse RASTER_TRACE: {}", e);
                }
            }
        }
    }
    Ok(())
}

/// Find the Cargo target directory for a project.
/// Handles both workspace members and standalone projects.
fn find_target_path(project_path: &std::path::Path) -> Option<PathBuf> {
    // Run cargo metadata to get the target directory
    let output = Command::new("cargo")
        .current_dir(project_path)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let meta: serde_json::Value = serde_json::from_str(&stdout).ok()?;

    meta.get("target_directory")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
}

/// Extract the binary name from a Cargo.toml file.
fn extract_binary_name(project_path: &std::path::Path) -> Option<String> {
    let cargo_toml = std::fs::read_to_string(project_path.join("Cargo.toml")).ok()?;

    // Simple parsing: look for name = "..." in [package] section
    let mut in_package = false;
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed == "[package]" {
            in_package = true;
            continue;
        }
        if trimmed.starts_with('[') {
            in_package = false;
            continue;
        }
        if in_package && trimmed.starts_with("name") {
            if let Some(start) = line.find('"') {
                let rest = &line[start + 1..];
                if let Some(end) = rest.find('"') {
                    return Some(rest[..end].to_string());
                }
            }
        }
    }
    None
}
