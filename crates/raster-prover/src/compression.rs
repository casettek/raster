//! Local compression of completed fault receipts. No execution or guest changes.
use crate::{
    trace::{validate_commitment, TraceCommitmentExt},
    TRANSITION_GUEST_ID,
};
use raster_core::{
    transition::{TransitionJournal, TransitionState},
    Error, Result,
};
use risc0_zkvm::{sha::Digestible, InnerReceipt, LocalProver, Prover, ProverOpts, Receipt};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::Path, process::Command, time::Instant};

fn error(message: impl std::fmt::Display) -> Error {
    Error::Other(message.to_string())
}

#[derive(Debug, Serialize)]
pub struct ReceiptSize {
    pub format: &'static str,
    pub sha256: String,
    pub receipt_bytes: usize,
    pub seal_bytes: usize,
}
#[derive(Debug, Serialize)]
pub struct CompressionReport {
    pub version: u32,
    pub verified: bool,
    pub input: ReceiptSize,
    pub output: ReceiptSize,
    pub commitment_sha256: String,
    pub verifier_image_id: String,
    pub program_commitment: String,
    pub disputed_commitment_header_digest: String,
    pub input_verification_ms: f64,
    pub compression_ms: f64,
    pub output_verification_ms: f64,
}
fn format(receipt: &Receipt) -> Result<&'static str> {
    match &receipt.inner {
        InnerReceipt::Composite(_) => Ok("composite"),
        InnerReceipt::Succinct(_) => Ok("succinct"),
        InnerReceipt::Groth16(_) => Ok("groth16"),
        _ => Err(error(
            "Fake/development or unsupported receipts cannot be compressed",
        )),
    }
}
fn size(receipt: &Receipt, bytes: &[u8]) -> Result<ReceiptSize> {
    Ok(ReceiptSize {
        format: format(receipt)?,
        sha256: hex::encode(Sha256::digest(bytes)),
        receipt_bytes: bytes.len(),
        seal_bytes: receipt.seal_size(),
    })
}
fn decode_exact<T: serde::de::DeserializeOwned>(bytes: &[u8], label: &str) -> Result<T> {
    let (value, trailing) =
        postcard::take_from_bytes(bytes).map_err(|e| error(format!("Malformed {label}: {e}")))?;
    if !trailing.is_empty() {
        return Err(error(format!("Malformed {label}: trailing bytes")));
    }
    Ok(value)
}
fn decode(bytes: &[u8]) -> Result<Receipt> {
    decode_exact(bytes, "receipt")
}
fn check_verdict(journal: &TransitionJournal, digest: &[u8]) -> Result<()> {
    if !matches!(journal.current_state, TransitionState::Finished) {
        return Err(error("Receipt does not contain a finished fault verdict"));
    }
    if journal.refuted_trace_commitment != digest {
        return Err(error("Receipt refutes a different disputed commitment"));
    }
    Ok(())
}
fn verify(receipt: &Receipt, digest: &[u8]) -> Result<TransitionJournal> {
    format(receipt)?;
    receipt.verify(TRANSITION_GUEST_ID).map_err(|e| {
        error(format!(
            "Receipt verification failed for this Raster verifier version: {e}"
        ))
    })?;
    let journal: TransitionJournal = receipt
        .journal
        .decode()
        .map_err(|e| error(format!("Invalid fault journal: {e}")))?;
    check_verdict(&journal, digest)?;
    Ok(journal)
}
fn same_claim(input: &Receipt, output: &Receipt) -> Result<()> {
    if input.journal.bytes != output.journal.bytes {
        return Err(error("Compression changed the public journal"));
    }
    let before = input.claim().map_err(error)?.digest();
    let after = output.claim().map_err(error)?.digest();
    if before != after {
        return Err(error("Compression changed the receipt claim"));
    }
    Ok(())
}
pub fn compression_ready() -> Result<()> {
    if ProverOpts::groth16().dev_mode() {
        return Err(error("Compression requires development mode disabled"));
    }
    let output = std::process::Command::new("docker").args(["info", "--format", "{{.ServerVersion}}"])
        .output().map_err(|e| error(format!("Local Groth16 compression requires Docker: {e}. Enable Docker Desktop WSL integration or start the local Docker daemon.")))?;
    if !output.status.success() {
        return Err(error(format!("Local Groth16 compression requires a working Docker daemon. Enable Docker Desktop WSL integration or start Docker. {}",
            String::from_utf8_lossy(&output.stderr))));
    }
    Ok(())
}
fn new_destination(output: &Path) -> Result<()> {
    match fs::symlink_metadata(output) {
        Ok(_) => Err(error(
            "Output path already exists; choose a new output directory",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Compress locally and publish only independently verified results. The progress
/// callback may return an error to cancel before the next phase. CLI signals stop
/// the process normally; no output directory exists during expensive proving.
pub fn compress_fault(
    receipt_path: &Path,
    commitment_path: &Path,
    output: &Path,
    mut progress: impl FnMut(&str) -> Result<()>,
) -> Result<CompressionReport> {
    let options = ProverOpts::groth16();
    if options.dev_mode() {
        return Err(error("Compression requires development mode disabled"));
    }
    new_destination(output)?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(error("Output parent directory must exist"));
    }
    progress("input verification")?;
    let started = Instant::now();
    let commitment_bytes = fs::read(commitment_path).map_err(Error::Io)?;
    let commitment_sha256 = hex::encode(Sha256::digest(&commitment_bytes));
    let commitment: raster_core::trace::TraceCommitment =
        decode_exact(&commitment_bytes, "disputed commitment")?;
    validate_commitment(&commitment).map_err(error)?;
    let digest = commitment.header().digest();
    let bytes = fs::read(receipt_path).map_err(Error::Io)?;
    let input = decode(&bytes)?;
    let input_size = size(&input, &bytes)?;
    drop(bytes);
    let journal = verify(&input, digest.as_slice())?;
    let input_verification_ms = started.elapsed().as_secs_f64() * 1000.0;
    let already_groth16 = matches!(input.inner, InnerReceipt::Groth16(_));
    if !already_groth16 {
        compression_ready()?;
    }
    progress("compression")?;
    let started = Instant::now();
    let compressed = compress_verified(&input, digest.as_slice(), &mut progress)?;
    let compression_ms = started.elapsed().as_secs_f64() * 1000.0;
    progress("output verification")?;
    let started = Instant::now();
    if !matches!(compressed.inner, InnerReceipt::Groth16(_)) {
        return Err(error("Compressor did not return a Groth16 receipt"));
    }
    verify(&compressed, digest.as_slice())?;
    same_claim(&input, &compressed)?;
    let output_verification_ms = started.elapsed().as_secs_f64() * 1000.0;
    let bytes = postcard::to_allocvec(&compressed).map_err(error)?;
    let report = CompressionReport {
        version: 1,
        verified: true,
        input: input_size,
        output: size(&compressed, &bytes)?,
        commitment_sha256,
        verifier_image_id: risc0_zkvm::sha::Digest::from(TRANSITION_GUEST_ID).to_string(),
        program_commitment: hex::encode(&journal.program_commitment),
        disputed_commitment_header_digest: hex::encode(digest),
        input_verification_ms,
        compression_ms,
        output_verification_ms,
    };
    progress("publication")?;
    publish(
        output,
        &bytes,
        &serde_json::to_vec_pretty(&report).map_err(error)?,
    )?;
    Ok(report)
}

/// Finalize a newly proved fault before its caller publishes any receipt.
/// This is shared with saved-receipt compression; no execution is repeated.
pub fn compress_fault_receipt(
    input: &Receipt,
    disputed_header_digest: &[u8],
    mut progress: impl FnMut(&str) -> Result<()>,
) -> Result<Receipt> {
    if ProverOpts::groth16().dev_mode() {
        return Err(error("Compression requires development mode disabled"));
    }
    progress("input verification")?;
    verify(input, disputed_header_digest)?;
    if !matches!(input.inner, InnerReceipt::Groth16(_)) {
        compression_ready()?;
    }
    progress("compression")?;
    let output = compress_verified(input, disputed_header_digest, &mut progress)?;
    progress("output verification")?;
    if !matches!(output.inner, InnerReceipt::Groth16(_)) {
        return Err(error("Compressor did not return a Groth16 receipt"));
    }
    verify(&output, disputed_header_digest)?;
    same_claim(input, &output)?;
    Ok(output)
}

fn compress_verified(
    input: &Receipt,
    digest: &[u8],
    progress: &mut impl FnMut(&str) -> Result<()>,
) -> Result<Receipt> {
    let options = ProverOpts::groth16();
    let already_groth16 = matches!(input.inner, InnerReceipt::Groth16(_));
    let compressed = if already_groth16 {
        input.clone()
    } else {
        // Never use default_prover: environment settings may select a remote service.
        let prover = LocalProver::new("raster-local");
        let succinct = prover
            .compress(&ProverOpts::succinct(), input)
            .map_err(|e| error(format!("Local succinct compression failed: {e:#}")))?;
        verify(&succinct, digest)?;
        same_claim(input, &succinct)?;
        progress("Groth16 wrapping")?;
        prover
            .compress(&options, &succinct)
            .map_err(|e| error(format!("Local Groth16 compression failed: {e:#}")))?
    };
    Ok(compressed)
}

fn publish(output: &Path, receipt: &[u8], report: &[u8]) -> Result<()> {
    publish_with(output, |pending| {
        for (name, bytes) in [
            ("fault.groth16.receipt", receipt),
            ("compression.json", report),
        ] {
            let mut file = fs::File::create(pending.join(name)).map_err(Error::Io)?;
            file.write_all(bytes).map_err(Error::Io)?;
            file.sync_all().map_err(Error::Io)?;
        }
        Ok(())
    })
}
fn publish_with(output: &Path, write: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    new_destination(output)?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let pending = tempfile::Builder::new()
        .prefix(".raster-compression-")
        .tempdir_in(parent)
        .map_err(Error::Io)?;
    write(pending.path())?;
    rename_new(pending.path(), output).map_err(Error::Io)?;
    Ok(())
}

// An existence check followed by ordinary Unix rename can replace an existing
// empty directory. Use the OS no-replace operation to close that race.
fn rename_new(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let source = CString::new(source.as_os_str().as_bytes())?;
        let destination = CString::new(destination.as_os_str().as_bytes())?;
        #[cfg(target_os = "linux")]
        let code = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        let code =
            unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
        if code != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        fs::rename(source, destination)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (source, destination);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Atomic no-replace directory publication is unsupported on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commitment_file::CommitmentFile;
    fn fake(image: [u32; 8], journal: Vec<u8>) -> Receipt {
        let claim = risc0_zkvm::ReceiptClaim::ok(image, journal.clone());
        Receipt::new(
            InnerReceipt::Fake(risc0_zkvm::FakeReceipt::new(claim)),
            journal,
        )
    }
    #[test]
    fn fake_receipts_are_rejected_even_before_crypto_verification() {
        assert!(verify(&fake(TRANSITION_GUEST_ID, vec![]), &[0; 32]).is_err());
    }
    #[test]
    fn default_finalization_rejects_fake_and_cancellation() {
        let receipt = fake(TRANSITION_GUEST_ID, vec![]);
        assert!(compress_fault_receipt(&receipt, &[0; 32], |_| Ok(())).is_err());
        let mut phases = Vec::new();
        assert!(compress_fault_receipt(&receipt, &[0; 32], |phase| {
            phases.push(phase.to_owned());
            Err(error("cancelled"))
        })
        .is_err());
        assert_eq!(phases, vec!["input verification"]);
    }

    #[test]
    fn changed_journal_or_claim_is_rejected() {
        let before = fake([1; 8], vec![1, 2]);
        assert!(same_claim(&before, &before).is_ok());
        assert!(same_claim(&before, &fake([1; 8], vec![1, 3])).is_err());
        assert!(same_claim(&before, &fake([2; 8], vec![1, 2])).is_err());
    }
    #[test]
    fn seal_size_excludes_journal_and_serialization() {
        let receipt = Receipt::new(
            InnerReceipt::Groth16(risc0_zkvm::Groth16Receipt::new(
                vec![1; 256],
                risc0_zkvm::ReceiptClaim::ok([1; 8], vec![2; 1000]).into(),
                risc0_zkvm::sha::Digest::ZERO,
            )),
            vec![2; 1000],
        );
        let bytes = postcard::to_allocvec(&receipt).unwrap();
        let measured = size(&receipt, &bytes).unwrap();
        assert_eq!(measured.seal_bytes, 256);
        assert!(measured.receipt_bytes > 1256);
        assert!(verify(&receipt, &[0; 32]).is_err());
    }
    #[test]
    fn unfinished_and_wrong_commitment_verdicts_are_rejected() {
        use raster_core::{
            fingerprint::{BitPacker, Fingerprint},
            transition::*,
        };
        let frontier = SerializableFrontier {
            position: 0,
            leaf: vec![0; 32],
            ommers: vec![],
        };
        let init = InitTransition {
            init_frontier: frontier.clone(),
            init_storage_frontier: frontier,
            init_storage_root: vec![],
            init_storage_index_root: vec![],
            fingerprint: Fingerprint::from(vec![], BitPacker(1), 0),
        };
        let mut journal = TransitionJournal {
            init_state: init.clone(),
            current_state: TransitionState::Finished,
            transition_image_id: vec![],
            authorization_image_id: vec![],
            input_manifest_commitment: vec![],
            program_commitment: vec![1; 32],
            refuted_trace_commitment: vec![2; 32],
            entrypoint_authorization: EntrypointAuthorization::NotRequired,
            output_authorization: OutputAuthorization::NotRequired,
            window_is_terminal: false,
            final_committed_root: None,
        };
        assert!(check_verdict(&journal, &[2; 32]).is_ok());
        assert!(check_verdict(&journal, &[3; 32]).is_err());
        journal.current_state = TransitionState::Init(init);
        assert!(check_verdict(&journal, &[2; 32]).is_err());
    }
    /// Run against an existing real fault receipt; never generates another proof.
    #[test]
    #[ignore = "requires RASTER_TEST_FAULT_RECEIPT and RASTER_TEST_DISPUTED_COMMITMENT"]
    fn existing_real_receipt_verifies_and_rejects_tampering() {
        let input = std::env::var("RASTER_TEST_FAULT_RECEIPT").unwrap();
        let disputed = std::env::var("RASTER_TEST_DISPUTED_COMMITMENT").unwrap();
        let commitment = CommitmentFile::read_memory(Path::new(&disputed)).unwrap();
        let receipt = decode(&fs::read(input).unwrap()).unwrap();
        let digest = commitment.header().digest();
        verify(&receipt, &digest).unwrap();
        if matches!(receipt.inner, InnerReceipt::Groth16(_)) {
            let mut phases = Vec::new();
            let result = compress_fault_receipt(&receipt, &digest, |phase| {
                phases.push(phase.to_owned());
                Ok(())
            })
            .unwrap();
            assert_eq!(
                postcard::to_allocvec(&receipt).unwrap(),
                postcard::to_allocvec(&result).unwrap()
            );
            assert_eq!(result.seal_size(), 256);
            assert!(!phases.iter().any(|phase| phase == "Groth16 wrapping"));
            assert!(compress_fault_receipt(&receipt, &[0; 32], |_| Ok(())).is_err());
        }
        assert!(receipt.verify([0; 8]).is_err());
        assert!(verify(&receipt, &[0; 32]).is_err());
        let mut tampered = receipt;
        tampered.journal.bytes[0] ^= 1;
        assert!(verify(&tampered, &digest).is_err());
    }
    #[test]
    fn trailing_receipt_bytes_are_rejected() {
        let mut bytes = postcard::to_allocvec(&fake([1; 8], vec![1])).unwrap();
        assert!(decode(&bytes).is_ok());
        bytes.push(0);
        assert!(decode(&bytes).is_err());
        assert!(decode_exact::<u8>(&[1, 2], "disputed commitment").is_err());
    }
    #[test]
    fn malformed_receipt_is_rejected() {
        assert!(decode(b"not a receipt").is_err());
    }
    #[test]
    fn publication_is_complete_and_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let out = root.path().join("result");
        publish(&out, b"receipt", b"report").unwrap();
        assert!(publish(&out, b"changed", b"changed").is_err());
        assert_eq!(
            fs::read(out.join("fault.groth16.receipt")).unwrap(),
            b"receipt"
        );
        assert_eq!(fs::read(out.join("compression.json")).unwrap(), b"report");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[test]
    fn failed_or_cancelled_report_write_removes_partial_output() {
        let root = tempfile::tempdir().unwrap();
        let out = root.path().join("result");
        let result = publish_with(&out, |pending| {
            fs::write(pending.join("fault.groth16.receipt"), b"verified receipt").unwrap();
            Err(error("cancelled or report write failed"))
        });
        assert!(result.is_err());
        assert!(!out.exists());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }
    #[test]
    fn racing_empty_destination_is_preserved() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("pending");
        let dest = root.path().join("existing");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&dest).unwrap();
        fs::write(source.join("proof"), b"proof").unwrap();
        assert!(rename_new(&source, &dest).is_err());
        assert_eq!(fs::read_dir(dest).unwrap().count(), 0);
        assert!(source.join("proof").exists());
    }
    #[test]
    fn cancellation_before_verification_publishes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let out = root.path().join("result");
        let err = compress_fault(Path::new("unused"), Path::new("unused"), &out, |_| {
            Err(error("cancelled"))
        });
        assert!(err.is_err());
        assert!(!out.exists());
    }
    #[test]
    fn failed_publication_preserves_inputs_and_cleans_pending() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input");
        fs::write(&input, b"original").unwrap();
        assert!(publish(&input.join("output"), b"receipt", b"report").is_err());
        assert_eq!(fs::read(&input).unwrap(), b"original");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
