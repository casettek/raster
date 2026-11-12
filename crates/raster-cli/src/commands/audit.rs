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
    // Before the error-report fallback below: a refused audit writes nothing.
    let mut inputs = vec![
        ("trace", trace.clone()),
        ("commitment", std::path::PathBuf::from(commitment)),
    ];
    inputs.extend(input_files(Some(input), Some(manifest)));
    check_outputs(
        &inputs,
        &[("--proof-output", &proof_output), ("--report", &report)],
    )?;
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
    // Lengths that differ are a fault `verify` proves, not an error.
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

/// Refuse outputs that would replace an input or each other. Call it before
/// anything is written, an error report included, so a refused command
/// leaves every file as it was.
fn check_outputs(
    inputs: &[(&str, std::path::PathBuf)],
    outputs: &[(&str, &std::path::Path)],
) -> Result<()> {
    let outputs = outputs
        .iter()
        .map(|(label, path)| Ok((*label, resolve_path(path).map_err(Error::Io)?)))
        .collect::<Result<Vec<_>>>()?;
    for (at, (label, path)) in outputs.iter().enumerate() {
        if let Some((other, _)) = outputs[..at]
            .iter()
            .find(|(_, other)| same_file(path, other))
        {
            return Err(Error::Other(format!(
                "{other} and {label} must be different files ({})",
                path.display()
            )));
        }
        for (input, input_path) in inputs {
            if same_file(path, &resolve_path(input_path).map_err(Error::Io)?) {
                return Err(Error::Other(format!(
                    "{label} would replace the {input} ({})",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

/// A path as the filesystem resolves it, for one that may not exist yet: its
/// parent's canonical path, so `./x`, `dir/../x` and a symlinked directory
/// all name the same file.
fn resolve_path(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    if let Ok(path) = std::fs::canonicalize(path) {
        return Ok(path);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    match (std::fs::canonicalize(parent), path.file_name()) {
        (Ok(parent), Some(name)) => Ok(parent.join(name)),
        _ => std::path::absolute(path),
    }
}

/// Two resolved paths naming one file, a hard link included.
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    if a == b {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return a.dev() == b.dev() && a.ino() == b.ino();
        }
    }
    false
}

/// The files an audit reads its inputs from: the input document and the
/// manifest when they are files rather than inline JSON, and the data files
/// the document names. An unreadable document names none here; the audit
/// reports it.
fn input_files(
    input: Option<&str>,
    manifest: Option<&str>,
) -> Vec<(&'static str, std::path::PathBuf)> {
    let mut files: Vec<_> = [(input, "input"), (manifest, "input manifest")]
        .into_iter()
        .filter_map(|(raw, label)| {
            raw.filter(|raw| std::path::Path::new(raw).is_file())
                .map(|raw| (label, std::path::PathBuf::from(raw)))
        })
        .collect();
    let entries = crate::utils::authorization::read_json_source(input, "input")
        .ok()
        .filter(|bytes| !bytes.is_empty())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    if let Some(entries) = entries {
        files.extend(
            input_document_files(input, &entries)
                .into_iter()
                .map(|path| ("input data file", path)),
        );
    }
    files
}

/// The data files an input document names, resolved as the program resolves
/// them: next to the document when it is a file.
fn input_document_files(
    raw: Option<&str>,
    entries: &raster_core::input::InputDocument,
) -> Vec<std::path::PathBuf> {
    let base = raw
        .filter(|s| std::path::Path::new(s).is_file())
        .and_then(|s| std::path::Path::new(s).parent())
        .unwrap_or(std::path::Path::new("."));
    let mut files = Vec::new();
    for entry in entries.values() {
        files.push(base.join(entry.path()));
        if let Some(index) = entry.index_path() {
            files.push(base.join(index));
        }
    }
    files
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
    let mut inputs = vec![("trace", trace.clone())];
    inputs.extend(input_files(Some(input), Some(manifest)));
    check_outputs(&inputs, &[("--output", &output)])?;
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

/// Audit once with a rolling evidence window, then estimate or prove.
pub fn run_live(
    project: &Project,
    cfs: &raster_core::cfs::ControlFlowSchema,
    binary: &std::path::Path,
    artifacts: &super::RunArtifacts,
    input: Option<&str>,
    manifest: Option<&str>,
    disputed_path: &str,
    estimate: Option<Option<usize>>,
    proof_output: Option<&std::path::Path>,
    report: Option<&std::path::Path>,
) -> Result<()> {
    use raster_prover::transition::AuditHandoff;
    use sha2::{Digest, Sha256};
    let disputed_path = std::fs::canonicalize(disputed_path).map_err(Error::Io)?;
    let disputed = raster_prover::commitment_file::CommitmentFile::read_memory(&disputed_path)
        .map_err(Error::Io)?;
    let proof_output = proof_output
        .map(std::path::Path::to_owned)
        .unwrap_or_else(|| artifacts.run_dir.join("fault.receipt"));
    let report = report.map(std::path::Path::to_owned).unwrap_or_else(|| {
        artifacts.run_dir.join(if estimate.is_some() {
            "fault-estimate.json"
        } else {
            "audit.json"
        })
    });
    let mut inputs = vec![("disputed commitment", disputed_path.clone())];
    inputs.extend(input_files(input, manifest));
    check_outputs(
        &inputs,
        &[("proof output", &proof_output), ("report", &report)],
    )?;
    let mut cancellation = AuditCancellation::install()?;
    let mut failure_report = None;
    let result = (|| -> Result<()> {
        let started = Instant::now();
        let pending = tempfile::Builder::new()
            .prefix(".audit-evidence-")
            .tempfile_in(&artifacts.run_dir)
            .map_err(Error::Io)?
            .into_temp_path();
        let mut hasher = Sha256::new();
        hasher.update(disputed.header().digest());
        hasher.update(pending.to_string_lossy().as_bytes());
        hasher
            .update(raster_core::bincode::serialize(cfs).map_err(|e| Error::Other(e.to_string()))?);
        let binding = format!("{:x}", hasher.finalize());
        let context = audit_context(project, binary, cfs, input, manifest, &disputed_path)?;
        let binding = format!("{binding}:{context}");
        println!("RASTER_AUDIT_PROGRESS {{\"phase\":\"audit\"}}");
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
        let mut child = command.spawn().map_err(Error::Io)?;
        let status = match cancellation.wait(&mut child) {
            Ok(status) => status,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Io(error));
            }
        };
        if audit_context(project, binary, cfs, input, manifest, &disputed_path)? != context {
            return Err(Error::Other(
                "audit context changed during execution".into(),
            ));
        }
        let handoff: AuditHandoff =
            raster_core::bincode::deserialize(&std::fs::read(&pending).map_err(Error::Io)?)
                .map_err(|e| Error::Other(format!("invalid audit evidence: {e}")))?;
        validate_handoff(
            &handoff,
            &binding,
            &disputed.header().digest(),
            disputed.len(),
            status.code(),
        )?;
        drop(pending);
        cancellation.finish()?;
        let audit_ms = started.elapsed().as_secs_f64() * 1000.0;
        let Some(prepared) = handoff.fault else {
            if handoff.steps != disputed.len() {
                return Err(Error::Other("incomplete audit".into()));
            }
            let value = if estimate.is_some() {
                serde_json::json!({"version":1,"kind":"fault-window-estimate","outcome":"no-fault","complete":false,"measured_step_count":0,"audit_duration_ms":audit_ms})
            } else {
                serde_json::json!({"version":1,"outcome":"no-fault","audit_duration_ms":audit_ms})
            };
            publish_memory(
                &report,
                &serde_json::to_vec_pretty(&value).map_err(|e| Error::Other(e.to_string()))?,
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
        if let Some(limit) = estimate {
            println!(
                "RASTER_AUDIT_PROGRESS {}",
                serde_json::json!({"phase":"estimating","audit_duration_ms":audit_ms})
            );
            let backend = raster_backend_risc0::Risc0Backend::new(project.output_dir.clone())
                .with_user_crate(project.root_dir.clone());
            let replayer = Replayer::new(&backend, project);
            let inputs = crate::utils::authorization::build_manifested_inputs(manifest)?;
            let program = super::prove::build_program_frame(cfs, &replayer)?;
            let measured = raster_prover::estimate::estimate_window(
                &prepared, &program, &inputs, &disputed, &replayer, limit,
            );
            print!("{}", measured.to_text());
            let mut value =
                serde_json::to_value(&measured).map_err(|e| Error::Other(e.to_string()))?;
            value["audit_duration_ms"] = serde_json::json!(audit_ms);
            value["execution_artifacts"] = serde_json::json!(artifacts.run_dir);
            if let Some(failure) = &measured.error {
                let message = format!(
                    "estimation failed in {} at {:?}: {}",
                    failure.phase, failure.exec_index, failure.message
                );
                failure_report = Some(value);
                return Err(Error::Other(message));
            }
            publish_memory(
                &report,
                &serde_json::to_vec_pretty(&value).map_err(|e| Error::Other(e.to_string()))?,
            )?;
            println!("Fault estimate report: {}", report.display());
            return Ok(());
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
        let proof_bytes =
            postcard::to_allocvec(&receipt).map_err(|e| Error::Other(e.to_string()))?;
        let report_pending = prepare_output(&report, &report_bytes)?;
        let proof_pending = prepare_output(&proof_output, &proof_bytes)?;
        publish_pair(proof_pending, &proof_output, report_pending, &report)?;
        Ok(())
    })();
    cancellation.finish()?;
    if let Err(e) = &result {
        if estimate.is_some() {
            let value = failure_report.unwrap_or_else(|| serde_json::json!({
                "version":1,"kind":"fault-window-estimate","outcome":"error","complete":false,
                "measured_step_count":0,"error":{"phase":"audit/preparation","exec_index":null,"message":e.to_string()}
            }));
            publish_memory(
                &report,
                &serde_json::to_vec_pretty(&value).map_err(|e| Error::Other(e.to_string()))?,
            )?;
        }
    }
    result
}
// Handle cancellation on the parent thread so TempPath cleanup and child reaping
// run before restoring the usual signal exit status. SIGKILL cannot be cleaned up.
#[cfg(unix)]
static AUDIT_CANCELLED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
#[cfg(unix)]
extern "C" fn audit_cancel(signal: libc::c_int) {
    AUDIT_CANCELLED.store(signal, std::sync::atomic::Ordering::SeqCst);
}
struct AuditCancellation {
    #[cfg(unix)]
    previous: Vec<(libc::c_int, libc::sigaction)>,
}
impl AuditCancellation {
    fn install() -> Result<Self> {
        let mut guard = Self {
            #[cfg(unix)]
            previous: Vec::new(),
        };
        #[cfg(unix)]
        {
            AUDIT_CANCELLED.store(0, std::sync::atomic::Ordering::SeqCst);
            for signal in [libc::SIGINT, libc::SIGTERM] {
                // SAFETY: initialized sigaction structs and a signal-safe atomic handler.
                unsafe {
                    let mut action: libc::sigaction = std::mem::zeroed();
                    let mut previous: libc::sigaction = std::mem::zeroed();
                    action.sa_sigaction = audit_cancel as *const () as usize;
                    libc::sigemptyset(&mut action.sa_mask);
                    if libc::sigaction(signal, &action, &mut previous) != 0 {
                        return Err(Error::Io(std::io::Error::last_os_error()));
                    }
                    guard.previous.push((signal, previous));
                }
            }
        }
        Ok(guard)
    }
    fn wait(&self, child: &mut std::process::Child) -> std::io::Result<std::process::ExitStatus> {
        loop {
            #[cfg(unix)]
            if AUDIT_CANCELLED.load(std::sync::atomic::Ordering::SeqCst) != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "audit cancelled",
                ));
            }
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    fn finish(&mut self) -> Result<()> {
        #[cfg(unix)]
        let signal = AUDIT_CANCELLED.load(std::sync::atomic::Ordering::SeqCst);
        #[cfg(unix)]
        for (saved_signal, previous) in self.previous.drain(..).rev() {
            // SAFETY: restoring handlers saved by install().
            unsafe {
                libc::sigaction(saved_signal, &previous, std::ptr::null_mut());
            }
        }
        #[cfg(unix)]
        if signal != 0 {
            // SAFETY: handlers have been restored and private handoffs dropped.
            unsafe {
                libc::raise(signal);
            }
            return Err(Error::Other("audit cancelled".into()));
        }
        Ok(())
    }
}
impl Drop for AuditCancellation {
    fn drop(&mut self) {
        #[cfg(unix)]
        for (signal, previous) in self.previous.iter().rev() {
            // SAFETY: restoring handlers saved by install().
            unsafe {
                libc::sigaction(*signal, previous, std::ptr::null_mut());
            }
        }
    }
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

fn audit_context(
    project: &Project,
    binary: &std::path::Path,
    cfs: &raster_core::cfs::ControlFlowSchema,
    input: Option<&str>,
    manifest: Option<&str>,
    disputed: &std::path::Path,
) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut hash = Sha256::new();
    let expected_cfs =
        raster_core::bincode::serialize(cfs).map_err(|e| Error::Other(e.to_string()))?;
    let current_project = Project::new(project.root_dir.clone())?;
    let current_cfs = CfsBuilder::new(&current_project).build()?;
    if raster_core::bincode::serialize(&current_cfs).map_err(|e| Error::Other(e.to_string()))?
        != expected_cfs
    {
        return Err(Error::Other("audit CFS changed".into()));
    }
    hash.update(expected_cfs);
    let mut files = vec![binary.to_owned(), disputed.to_owned()];
    let lock = project.root_dir.join("Raster.lock");
    hash.update(b"Raster.lock");
    hash.update([u8::from(lock.exists())]);
    if lock.exists() {
        files.push(lock);
    }
    for (raw, label) in [(input, "input"), (manifest, "manifest")] {
        let bytes = crate::utils::authorization::read_json_source(raw, label)?;
        hash.update(label.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
        if label == "input" && !bytes.is_empty() {
            let entries: raster_core::input::InputDocument = serde_json::from_slice(&bytes)
                .map_err(|e| Error::Other(format!("invalid audit input document: {e}")))?;
            files.extend(input_document_files(raw, &entries));
        }
    }
    for path in files {
        let canonical = std::fs::canonicalize(&path).map_err(Error::Io)?;
        let name = canonical.to_string_lossy();
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        let mut file = std::fs::File::open(canonical).map_err(Error::Io)?;
        let mut file_hash = Sha256::new();
        let mut buffer = [0; 65536];
        loop {
            let n = file.read(&mut buffer).map_err(Error::Io)?;
            if n == 0 {
                break;
            }
            file_hash.update(&buffer[..n]);
        }
        hash.update(file_hash.finalize());
    }
    Ok(format!("{:x}", hash.finalize()))
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
    fn context_binds_program_commitment_manifest_and_resolved_input_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname=\"audit-context-test\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[workspace]\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "#[sequence] fn main() {} ").unwrap();
        for file in ["Raster.lock", "native", "commitment", "data", "index"] {
            std::fs::write(root.join(file), file.as_bytes()).unwrap();
        }
        let input = root.join("input.json");
        std::fs::write(
            &input,
            r#"{"value":{"path":"data","index_path":"index","load_preference":"read"}}"#,
        )
        .unwrap();
        let manifest = root.join("manifest.json");
        std::fs::write(&manifest, b"{}").unwrap();
        let project = Project::new(root.to_owned()).unwrap();
        let cfs = CfsBuilder::new(&project).build().unwrap();
        let hash = || {
            audit_context(
                &project,
                &root.join("native"),
                &cfs,
                input.to_str(),
                manifest.to_str(),
                &root.join("commitment"),
            )
        };
        let original = hash().unwrap();
        for file in ["Raster.lock", "native", "commitment", "data", "index"] {
            std::fs::write(root.join(file), b"changed").unwrap();
            assert_ne!(hash().unwrap(), original, "{file}");
            std::fs::write(root.join(file), file.as_bytes()).unwrap();
        }
        assert_eq!(hash().unwrap(), original);
        std::fs::write(&manifest, b"{ \"changed\": true }").unwrap();
        assert_ne!(hash().unwrap(), original);
        std::fs::write(&manifest, b"{}").unwrap();
        let original_input = std::fs::read(&input).unwrap();
        let mut changed_input = original_input.clone();
        changed_input.push(b' ');
        std::fs::write(&input, changed_input).unwrap();
        assert_ne!(hash().unwrap(), original);
        std::fs::write(&input, original_input).unwrap();
        std::fs::remove_file(root.join("Raster.lock")).unwrap();
        assert_ne!(hash().unwrap(), original);
        std::fs::write(root.join("src/main.rs"), "#[sequence] fn other() {} ").unwrap();
        assert!(hash().is_err());
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
    /// An evidence directory: a trace, a commitment, and an input document
    /// naming one data file, each holding its own name.
    fn evidence() -> (tempfile::TempDir, [std::path::PathBuf; 5]) {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        let files = [
            path("trace.bin"),
            path("commit.bin"),
            path("input.json"),
            path("manifest.json"),
            path("data.rastered"),
        ];
        for file in &files[..2] {
            std::fs::write(file, file.file_name().unwrap().as_encoded_bytes()).unwrap();
        }
        std::fs::write(
            &files[2],
            br#"{"data":{"path":"data.rastered","load_preference":"read"}}"#,
        )
        .unwrap();
        std::fs::write(&files[3], b"{}").unwrap();
        std::fs::write(&files[4], b"data").unwrap();
        (dir, files)
    }

    fn contents(files: &[std::path::PathBuf]) -> Vec<Vec<u8>> {
        files
            .iter()
            .map(|file| std::fs::read(file).unwrap())
            .collect()
    }

    fn trace_audit(
        files: &[std::path::PathBuf; 5],
        proof_output: std::path::PathBuf,
        report: std::path::PathBuf,
    ) -> Result<()> {
        let [trace, commitment, input, manifest, _] = files;
        // A project whose CFS builds, so an audit that passes the check fails
        // only later, on the trace, which is not one.
        let project = trace.parent().unwrap().join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname=\"audit-paths-test\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(project.join("src/main.rs"), "#[sequence] fn main() {} ").unwrap();
        audit(
            project,
            trace.clone(),
            input.to_str().unwrap(),
            manifest.to_str().unwrap(),
            commitment.to_str().unwrap(),
            proof_output,
            report,
            crate::TraceFormat::Binary,
        )
    }

    /// Outputs that would replace evidence, or each other, are refused before
    /// anything runs, and the refusal writes no error report over them.
    #[test]
    fn trace_audit_refuses_outputs_that_replace_its_evidence() {
        let (dir, files) = evidence();
        let before = contents(&files);
        let fresh = dir.path().join("fresh.json");
        let receipt = dir.path().join("fault.receipt");
        let spelled = dir.path().join(".").join("trace.bin");
        let linked = dir.path().join("commit-link.bin");
        std::fs::hard_link(&files[1], &linked).unwrap();
        for (proof_output, report, reason) in [
            (receipt.clone(), files[1].clone(), "replace the commitment"),
            (fresh.clone(), fresh.clone(), "must be different files"),
            (receipt.clone(), spelled, "replace the trace"),
            (linked, fresh.clone(), "replace the commitment"),
            (receipt.clone(), files[2].clone(), "replace the input"),
            (
                receipt.clone(),
                files[3].clone(),
                "replace the input manifest",
            ),
            (
                files[4].clone(),
                fresh.clone(),
                "replace the input data file",
            ),
        ] {
            let error = trace_audit(&files, proof_output, report).unwrap_err();
            assert!(error.to_string().contains(reason), "{error}");
            assert_eq!(contents(&files), before, "after: {error}");
            assert!(!fresh.exists() && !receipt.exists(), "after: {error}");
        }
    }

    /// Distinct outputs pass the check; this audit then fails on its trace
    /// and reports that, as before.
    #[test]
    fn trace_audit_with_distinct_outputs_still_reports_its_error() {
        let (dir, files) = evidence();
        let report = dir.path().join("audit.json");
        assert!(trace_audit(&files, dir.path().join("fault.receipt"), report.clone()).is_err());
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
        assert_eq!(report["outcome"], "error");
    }

    #[test]
    fn trace_commit_refuses_to_replace_its_trace() {
        let (_dir, files) = evidence();
        let before = contents(&files);
        let [trace, _, input, manifest, _] = &files;
        let error = commit_recorded(
            "/nonexistent-project".into(),
            trace.clone(),
            input.to_str().unwrap(),
            manifest.to_str().unwrap(),
            trace.clone(),
            32,
            crate::TraceFormat::Binary,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("--output would replace the trace"),
            "{error}"
        );
        assert_eq!(contents(&files), before);
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
