//! Run command: build and execute the user program as a whole.

use crate::BackendType;
use raster_core::trace::AuditResult;
use raster_core::{Error, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Run the user program with the specified backend.
pub fn run(
    backend_type: BackendType,
    input: Option<&str>,
    commit: Option<&str>,
    audit: Option<&str>,
) -> Result<()> {
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
    cmd.current_dir(&project_path);

    if let Some(input_json) = input {
        cmd.args(["--input", input_json]);
    }

    if let Some(commit_path) = commit {
        cmd.args(["--commit", commit_path]);
    } else if let Some(audit_path) = audit {
        cmd.args(["--audit", audit_path]);
    }

    // Execute the binary and capture output
    let output = cmd
        .output()
        .map_err(|e| Error::Other(format!("Failed to execute binary: {}", e)))?;

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        println!("stderr: {}", stderr);

        return Err(Error::Other(format!("Program exited with code {}: {}", code, stderr)));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Separate trace items, audit results, and regular program output
    let mut trace_items: Vec<serde_json::Value> = Vec::new();
    let mut audit_result: Option<AuditResult> = None;
    let mut program_output: Vec<&str> = Vec::new();

    for line in stdout.lines() {
        if let Some(json_str) = line.strip_prefix("RASTER_TRACE:") {
            if let Ok(trace_item) = serde_json::from_str::<serde_json::Value>(json_str) {
                trace_items.push(trace_item);
            }
        } else if let Some(json_str) = line.strip_prefix("RASTER_AUDIT:") {
            if let Ok(result) = serde_json::from_str::<AuditResult>(json_str) {
                audit_result = Some(result);
            }
        } else {
            program_output.push(line);
        }
    }

    // Print program output first
    if !program_output.is_empty() {
        println!("Output:");
        for line in &program_output {
            println!("  {}", line);
        }
        println!();
    }

    // Print trace items as pretty JSON
    if !trace_items.is_empty() {
        println!("Trace ({} tile executions):", trace_items.len());
        for trace_item in &trace_items {
            if let Ok(pretty) = serde_json::to_string_pretty(&trace_item) {
                // Indent each line of the pretty JSON
                for line in pretty.lines() {
                    println!("  {}", line);
                }
                println!();
            }
        }
    }

    // Handle audit result if present
    if let Some(result) = audit_result {
        if result.success {
            println!(
                "Audit verification passed ({} values verified).",
                result.verified_count
            );
        } else {
            println!("Audit verification FAILED!");
            if let Some(ref diff) = result.diff {
                println!("  Divergence detected at trace index: {}", diff.index);
                if !diff.frontier.is_empty() {
                    // Display frontier as hex for debugging/replay purposes
                    let frontier_hex: String = diff.frontier.iter()
                        .map(|b| format!("{:02x}", b))
                        .collect();
                    println!("  Frontier (hex): {}", frontier_hex);
                }
            }
            println!(
                "  Values verified before failure: {}",
                result.verified_count
            );

            // Display trace window for debugging context
            if !result.trace_window.is_empty() {
                println!();
                println!(
                    "Trace window ({} items leading up to divergence):",
                    result.trace_window.len()
                );
                for (i, item) in result.trace_window.iter().enumerate() {
                    println!();
                    println!("  [{}] {}", i, item.fn_name);
                    if let Some(ref desc) = item.desc {
                        println!("      Description: {}", desc);
                    }
                    if !item.inputs.is_empty() {
                        println!("      Inputs:");
                        for input in &item.inputs {
                            println!("        - {}: {}", input.name, input.ty);
                        }
                    }
                    if let Some(ref output_type) = item.output_type {
                        println!("      Output type: {}", output_type);
                    }
                    println!("      Input data (base64): {}", item.input_data);
                    println!("      Output data (base64): {}", item.output_data);
                }
            }

            return Err(Error::Other("Audit verification failed".into()));
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
