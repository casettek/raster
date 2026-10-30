//! Audit/prove an already recorded execution using the normal run pipeline.
use super::{
    prove::{prove, read_trace_commitment, verify},
    run::load_trace_from_file,
};
use raster_compiler::{CfsBuilder, Project};
use raster_core::{
    transition::{TransitionJournal, TransitionState},
    Error, Result,
};
use raster_prover::{replay::Replayer, trace::VerificationResult};
use std::{io::Write, path::PathBuf, time::Instant};

pub fn audit(
    project: PathBuf,
    trace: PathBuf,
    input: &str,
    manifest: &str,
    commitment: &str,
    proof_output: PathBuf,
    report: PathBuf,
    format: crate::TraceFormat,
) -> Result<()> {
    let result = audit_inner(
        project,
        trace,
        input,
        manifest,
        commitment,
        proof_output,
        report.clone(),
        format,
    );
    if let Err(error) = &result {
        let failure = serde_json::json!({"version":1,"outcome":"error","error":error.to_string()});
        if let Ok(bytes) = serde_json::to_vec_pretty(&failure) {
            let _ = publish(&report, &bytes);
        }
    }
    result
}

fn audit_inner(
    project: PathBuf,
    trace: PathBuf,
    input: &str,
    manifest: &str,
    commitment: &str,
    proof_output: PathBuf,
    report: PathBuf,
    format: crate::TraceFormat,
) -> Result<()> {
    let started = Instant::now();
    let project = Project::new(project)?;
    let cfs = CfsBuilder::new(&project).build()?;
    println!("RASTER_AUDIT_PROGRESS {{\"phase\":\"audit\"}}");
    let (records, recorder) =
        load_trace_from_file(&trace, format, &cfs, Some(input), Some(manifest))?;
    let disputed = read_trace_commitment(commitment)?;
    if records.len() != disputed.len() {
        return Err(Error::Other(
            "execution and commitment lengths differ".into(),
        ));
    }
    let result = verify(&records, commitment, &cfs)?;
    let audit_ms = started.elapsed().as_secs_f64() * 1000.0;
    let outcome = match result {
        VerificationResult::Ok => {
            serde_json::json!({"version":1,"outcome":"no-fault","audit_duration_ms":audit_ms})
        }
        VerificationResult::Fraud(evidence) => {
            if risc0_zkvm::is_dev_mode() || raster_prover::TRANSITION_GUEST_ELF.is_empty() {
                return Err(Error::Other(
                    "fault proving requires real proving guests and RISC0_DEV_MODE disabled".into(),
                ));
            }
            println!(
                "RASTER_AUDIT_PROGRESS {{\"phase\":\"proving\",\"audit_duration_ms\":{audit_ms}}}"
            );
            raster_prover::compression::compression_ready()?;
            let proving = Instant::now();
            let backend = raster_backend_risc0::Risc0Backend::new(project.output_dir.clone())
                .with_user_crate(project.root_dir.clone());
            let replayer = Replayer::new(&backend, &project);
            let receipt = prove(
                evidence,
                &records,
                &cfs,
                &recorder,
                &replayer,
                Some(manifest),
                &disputed,
            );
            let receipt = finalize_fault(receipt, &disputed.header().digest())?;
            receipt
                .verify(raster_prover::TRANSITION_GUEST_ID)
                .map_err(|e| Error::Other(e.to_string()))?;
            let journal: TransitionJournal = receipt
                .journal
                .decode()
                .map_err(|e| Error::Other(e.to_string()))?;
            if !matches!(journal.current_state, TransitionState::Finished)
                || journal.refuted_trace_commitment != disputed.header().digest().to_vec()
            {
                return Err(Error::Other(
                    "receipt does not prove a fault against the supplied commitment".into(),
                ));
            }
            publish(
                &proof_output,
                &postcard::to_allocvec(&receipt).map_err(|e| Error::Other(e.to_string()))?,
            )?;
            serde_json::json!({"version":1,"outcome":"proved","verified":true,"proof_path":proof_output,"receipt_format":"groth16","proof_seal_bytes":receipt.seal_size(),
                "audit_duration_ms":audit_ms,"proof_duration_ms":proving.elapsed().as_secs_f64()*1000.0})
        }
    };
    publish(
        &report,
        &serde_json::to_vec_pretty(&outcome).map_err(|e| Error::Other(e.to_string()))?,
    )
}
// Keep the outer proving phase stable for Iris while exposing compression details.
fn finalize_fault(receipt: risc0_zkvm::Receipt, digest: &[u8]) -> Result<risc0_zkvm::Receipt> {
    let started = Instant::now();
    let input_seal_bytes = receipt.seal_size();
    let compressed =
        raster_prover::compression::compress_fault_receipt(&receipt, digest, |phase| {
            println!(
                "RASTER_COMPRESSION_PROGRESS {}",
                serde_json::json!({
                    "phase":"proving", "operation":"compression", "compression_phase":phase
                })
            );
            Ok(())
        })?;
    println!(
        "RASTER_COMPRESSION_PROGRESS {}",
        serde_json::json!({
            "phase":"proving", "operation":"compression", "compression_phase":"complete",
            "compression_duration_ms":started.elapsed().as_secs_f64()*1000.0,
            "input_seal_bytes":input_seal_bytes, "output_seal_bytes":compressed.seal_size()
        })
    );
    Ok(compressed)
}

fn publish(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    raster_prover::archive::Archive::current()
        .flush()
        .map_err(Error::Io)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(Error::Io)?;
    file.write_all(bytes).map_err(Error::Io)?;
    file.persist(path).map_err(|e| Error::Io(e.error))?;
    Ok(())
}

/// Recover commitment generation after execution succeeded but trace processing failed.
pub fn commit_recorded(
    project: PathBuf,
    trace: PathBuf,
    input: &str,
    manifest: &str,
    output: PathBuf,
    window: usize,
    format: crate::TraceFormat,
) -> Result<()> {
    let project = Project::new(project)?;
    let cfs = CfsBuilder::new(&project).build()?;
    let config = raster_prover::trace::FraudProofConfig::from_window_size(window)
        .map_err(|e| Error::Other(e.to_string()))?;
    let commitment =
        super::run::commit_trace_file(&trace, format, &cfs, Some(input), Some(manifest), config)?;
    commitment.write(&output).map_err(Error::Io)?;
    println!("Committed {} steps", commitment.len());
    Ok(())
}

/// Execute one native program, stop on divergence, then consume proof-ready evidence.
pub fn run_live(
    project: &Project,
    cfs: &raster_core::cfs::ControlFlowSchema,
    binary: &std::path::Path,
    artifacts: &super::RunArtifacts,
    input: Option<&str>,
    manifest: Option<&str>,
    disputed_path: &str,
    proof_output: Option<&std::path::Path>,
    report: Option<&std::path::Path>,
) -> Result<()> {
    use raster_prover::transition::AuditHandoff;
    use raster_runtime::tracing::publishers::audit::DIVERGENCE_EXIT_CODE;
    use sha2::{Digest, Sha256};
    let disputed_path = std::fs::canonicalize(disputed_path).map_err(Error::Io)?;
    let disputed = raster_prover::commitment_file::CommitmentFile::read_memory(&disputed_path)
        .map_err(Error::Io)?;
    let proof_output = proof_output
        .map(std::path::Path::to_owned)
        .unwrap_or_else(|| artifacts.run_dir.join("fault.receipt"));
    let report = report
        .map(std::path::Path::to_owned)
        .unwrap_or_else(|| artifacts.run_dir.join("audit.json"));
    let resolve_output =
        |path: &std::path::Path| std::fs::canonicalize(path).or_else(|_| std::path::absolute(path));
    if resolve_output(&proof_output).map_err(Error::Io)?
        == resolve_output(&report).map_err(Error::Io)?
    {
        return Err(Error::Other("proof output and report must differ".into()));
    }
    for destination in [&proof_output, &report] {
        let resolved = std::fs::canonicalize(destination)
            .unwrap_or(std::path::absolute(destination).map_err(Error::Io)?);
        if resolved == disputed_path {
            return Err(Error::Other(
                "audit output cannot replace the disputed commitment".into(),
            ));
        }
    }
    let pending = tempfile::Builder::new()
        .prefix(".audit-evidence-")
        .tempfile_in(&artifacts.run_dir)
        .map_err(Error::Io)?
        .into_temp_path();
    let mut hasher = Sha256::new();
    hasher.update(disputed.header().digest());
    hasher.update(pending.to_string_lossy().as_bytes());
    hasher.update(raster_core::bincode::serialize(cfs).map_err(|e| Error::Other(e.to_string()))?);
    for path in [input, manifest].into_iter().flatten() {
        hasher.update(std::fs::read(path).map_err(Error::Io)?);
    }
    let binding = format!("{:x}", hasher.finalize());
    let mut command = std::process::Command::new(binary);
    command.current_dir(&project.root_dir);
    if let Some(input) = input {
        command.args(["--input", input]);
    }
    if let Some(manifest) = manifest {
        command.args(["--input-manifest", manifest]);
    }
    crate::runtime_env::RuntimeEnv::new(&artifacts.run_dir)
        .auditing(&project.root_dir, &disputed_path, &pending, &binding)
        .profiling(artifacts)
        .apply(&mut command);
    println!("RASTER_AUDIT_PROGRESS {{\"phase\":\"audit\"}}");
    let started = Instant::now();
    let status = command.status().map_err(Error::Io)?;
    if !status.success() && status.code() != Some(DIVERGENCE_EXIT_CODE) {
        return Err(Error::Other(format!("native audit failed ({status})")));
    }
    let bytes = std::fs::read(&pending).map_err(Error::Io)?;
    let handoff: AuditHandoff = raster_core::bincode::deserialize(&bytes)
        .map_err(|e| Error::Other(format!("invalid audit handoff: {e}")))?;
    validate_handoff(
        &handoff,
        &binding,
        &disputed.header().digest(),
        disputed.len(),
        status.code(),
    )?;
    drop(bytes);
    drop(pending); // The validated evidence is owned in RAM; retries rerun audit.
    let audit_ms = started.elapsed().as_secs_f64() * 1000.0;
    let Some(prepared) = handoff.fault else {
        if handoff.steps != disputed.len() {
            return Err(Error::Other("incomplete audit".into()));
        }
        publish_memory(
            &report,
            &serde_json::to_vec_pretty(
                &serde_json::json!({"version":1,"outcome":"no-fault","audit_duration_ms":audit_ms}),
            )
            .unwrap(),
        )?;
        println!("Verification Success");
        return Ok(());
    };
    let frontier =
        raster_prover::trace::SerializableFrontier::from_bytes(&prepared.window.frontier)
            .ok_or_else(|| Error::Other("invalid audit frontier".into()))?;
    let window_start =
        usize::try_from(frontier.position).map_err(|e| Error::Other(e.to_string()))?;
    if window_start.checked_add(prepared.window.items.len()) != Some(handoff.steps)
        || prepared.window.items.len() != handoff.steps.min(disputed.window_size())
        || prepared.io.len() != prepared.window.items.len()
    {
        return Err(Error::Other("invalid audit window positions".into()));
    }
    if risc0_zkvm::is_dev_mode() || raster_prover::TRANSITION_GUEST_ELF.is_empty() {
        return Err(Error::Other(
            "fault proving requires real proving guests and RISC0_DEV_MODE disabled".into(),
        ));
    }
    println!(
        "RASTER_AUDIT_PROGRESS {}",
        serde_json::json!({"phase":"proving","audit_duration_ms":audit_ms,"detected_step":handoff.steps-1,"window_start":window_start,"window_end":handoff.steps})
    );
    raster_prover::compression::compression_ready()?;
    let proving = Instant::now();
    let backend = raster_backend_risc0::Risc0Backend::new(project.output_dir.clone())
        .with_user_crate(project.root_dir.clone());
    let replayer = Replayer::new(&backend, project);
    let receipt = super::prove::prove_prepared(prepared, cfs, &replayer, manifest, &disputed);
    let receipt = finalize_fault(receipt, &disputed.header().digest())?;
    receipt
        .verify(raster_prover::TRANSITION_GUEST_ID)
        .map_err(|e| Error::Other(e.to_string()))?;
    let journal: TransitionJournal = receipt
        .journal
        .decode()
        .map_err(|e| Error::Other(e.to_string()))?;
    if !matches!(journal.current_state, TransitionState::Finished)
        || journal.refuted_trace_commitment != disputed.header().digest().to_vec()
    {
        return Err(Error::Other(
            "receipt does not refute supplied commitment".into(),
        ));
    }
    let report_bytes = serde_json::to_vec_pretty(&serde_json::json!({"version":1,"outcome":"proved","verified":true,"proof_path":proof_output,"receipt_format":"groth16","proof_seal_bytes":receipt.seal_size(),
        "audit_duration_ms":audit_ms,"proof_duration_ms":proving.elapsed().as_secs_f64()*1000.0,
        "detected_step":handoff.steps-1,"window_start":window_start,"window_end":handoff.steps,"execution_artifacts":artifacts.run_dir})).unwrap();
    // Prepare both artifacts before publishing either one.
    let proof_bytes = postcard::to_allocvec(&receipt).map_err(|e| Error::Other(e.to_string()))?;
    let report_pending = prepare_output(&report, &report_bytes)?;
    let proof_pending = prepare_output(&proof_output, &proof_bytes)?;
    publish_pair(proof_pending, &proof_output, report_pending, &report)?;
    Ok(())
}
// Keep the prior receipt recoverable until both renames have succeeded.
fn publish_pair(
    proof: tempfile::NamedTempFile,
    proof_path: &std::path::Path,
    report: tempfile::NamedTempFile,
    report_path: &std::path::Path,
) -> Result<()> {
    let previous = match std::fs::read(proof_path) {
        Ok(bytes) => Some(prepare_output(proof_path, &bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(Error::Io(error)),
    };
    proof.persist(proof_path).map_err(|e| Error::Io(e.error))?;
    if let Err(error) = report.persist(report_path) {
        match previous {
            Some(previous) => {
                previous
                    .persist(proof_path)
                    .map_err(|e| Error::Io(e.error))?;
            }
            None => std::fs::remove_file(proof_path).map_err(Error::Io)?,
        }
        return Err(Error::Io(error.error));
    }
    Ok(())
}
fn prepare_output(path: &std::path::Path, bytes: &[u8]) -> Result<tempfile::NamedTempFile> {
    if path.is_dir() {
        return Err(Error::Other("artifact destination is a directory".into()));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut pending = tempfile::NamedTempFile::new_in(parent).map_err(Error::Io)?;
    pending.write_all(bytes).map_err(Error::Io)?;
    Ok(pending)
}
fn publish_memory(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    prepare_output(path, bytes)?
        .persist(path)
        .map_err(|e| Error::Io(e.error))?;
    Ok(())
}

fn validate_handoff(
    handoff: &raster_prover::transition::AuditHandoff,
    binding: &str,
    digest: &[u8],
    len: usize,
    exit_code: Option<i32>,
) -> Result<()> {
    use raster_runtime::tracing::publishers::audit::DIVERGENCE_EXIT_CODE;
    if handoff.version != 1
        || handoff.binding != binding
        || handoff.commitment_digest != digest
        || handoff.steps == 0
        || handoff.steps > len
        || !matches!(exit_code, Some(0) | Some(DIVERGENCE_EXIT_CODE))
        || (handoff.fault.is_some() != (exit_code == Some(DIVERGENCE_EXIT_CODE)))
        || (handoff.fault.is_none() && handoff.steps != len)
    {
        return Err(Error::Other(
            "audit handoff does not match this execution".into(),
        ));
    }
    Ok(())
}
#[cfg(test)]
mod live_tests {
    use super::*;
    #[test]
    fn handoff_rejects_failure_cancellation_wrong_context_and_incomplete_execution() {
        let mut handoff = raster_prover::transition::AuditHandoff {
            version: 1,
            binding: "attempt".into(),
            commitment_digest: vec![7],
            steps: 3,
            fault: None,
        };
        assert!(validate_handoff(&handoff, "attempt", &[7], 3, Some(0)).is_ok());
        for exit in [
            None,
            Some(1),
            Some(raster_runtime::tracing::publishers::audit::DIVERGENCE_EXIT_CODE),
        ] {
            assert!(validate_handoff(&handoff, "attempt", &[7], 3, exit).is_err());
        }
        assert!(validate_handoff(&handoff, "other attempt", &[7], 3, Some(0)).is_err());
        assert!(validate_handoff(&handoff, "attempt", &[8], 3, Some(0)).is_err());
        assert!(validate_handoff(&handoff, "attempt", &[7], 4, Some(0)).is_err());
        handoff.version = 2;
        assert!(validate_handoff(&handoff, "attempt", &[7], 3, Some(0)).is_err());
    }
    #[test]
    fn report_publication_failure_restores_previous_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let receipt = dir.path().join("receipt");
        let report = dir.path().join("report");
        std::fs::write(&receipt, b"previous valid receipt").unwrap();
        let proof_pending = prepare_output(&receipt, b"new verified receipt").unwrap();
        let report_pending = prepare_output(&report, b"new report").unwrap();
        std::fs::create_dir(&report).unwrap();
        assert!(publish_pair(proof_pending, &receipt, report_pending, &report).is_err());
        assert_eq!(std::fs::read(receipt).unwrap(), b"previous valid receipt");
    }
    #[test]
    fn publication_preparation_preserves_existing_receipt_on_error() {
        let dir = tempfile::tempdir().unwrap();
        let receipt = dir.path().join("receipt");
        std::fs::write(&receipt, b"previous valid receipt").unwrap();
        let report = dir.path().join("report-directory");
        std::fs::create_dir(&report).unwrap();
        assert!(prepare_output(&report, b"report").is_err());
        assert_eq!(std::fs::read(receipt).unwrap(), b"previous valid receipt");
    }
}
