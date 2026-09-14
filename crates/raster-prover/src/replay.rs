//! Trace replayer for re-executing tiles with proof generation.

use raster_backend::{Backend, CompilationArtifact, ExecutionMode};
use raster_compiler::tile::TileDiscovery;
use raster_compiler::Project;

use raster_core::draft::TileReplayJournal;
use raster_core::{Error, Result};
use sha2::{Digest, Sha256};

/// A tile prepared once for any number of independent executor-only replays.
/// No receipt is produced or accepted by this profiling path.
pub struct PreparedTileProfile<'a> {
    backend: &'a dyn Backend,
    artifact: Box<dyn CompilationArtifact>,
}

impl PreparedTileProfile<'_> {
    pub fn image_id(&self) -> String {
        self.artifact.artifact_id()
    }

    /// Replay the recorded bytes once and return guest-user cycles, including
    /// wrapper work. Only matching, successfully completed executions count.
    pub fn profile(&self, input: &[u8], expected_output: &[u8]) -> Result<u64> {
        let result =
            self.backend
                .execute_tile(self.artifact.as_ref(), input, ExecutionMode::Estimate)?;
        if result.receipt.is_some() {
            return Err(Error::Other(
                "Profiling unexpectedly generated a receipt".into(),
            ));
        }
        let journal = result
            .journal
            .ok_or_else(|| Error::Other("Profiling requires an executor journal".into()))?;
        let journal: TileReplayJournal = raster_core::postcard::from_bytes(&journal)
            .map_err(|e| Error::Other(format!("Failed to decode replay journal: {e}")))?;
        let expected_commitment: [u8; 32] = Sha256::digest(input).into();
        if journal.input_commitment != expected_commitment {
            return Err(Error::Other("Replay input commitment mismatch".into()));
        }
        if journal.output_bytes != expected_output {
            return Err(Error::Other(format!(
                "Replay output mismatch (recorded {} bytes, guest {} bytes)",
                expected_output.len(),
                journal.output_bytes.len(),
            )));
        }
        result
            .cycles
            .ok_or_else(|| Error::Other("Executor did not report guest cycles".into()))
    }
}

#[derive(Debug, Clone)]
pub struct ReplayResult {
    pub fn_name: String,

    pub receipt: Vec<u8>,

    pub image_id: Vec<u8>,
    pub input: Vec<u8>,
    pub output: Vec<u8>,
    pub replay_journal: TileReplayJournal,
}

/// Result of replaying a trace item.
///
/// Replays trace items on a backend with proof generation.
///
/// `TraceReplayer` takes trace items (typically from `AuditResult.trace_window`)
/// and re-executes them on a specified backend. This allows:
/// - Generating proofs for previously recorded executions
/// - Verifying that replay produces the same outputs
/// - Debugging execution discrepancies
pub struct Replayer<'a> {
    backend: &'a dyn Backend,
    project: &'a Project,
}

impl<'a> Replayer<'a> {
    /// Create a new replayer with the given backend and project context.
    ///
    /// # Arguments
    /// * `backend` - The backend to use for compilation and execution (e.g., Risc0Backend)
    /// * `project` - The project containing tile definitions for lookup
    pub fn new(backend: &'a dyn Backend, project: &'a Project) -> Self {
        Self { backend, project }
    }

    /// The project this replayer resolves tiles against.
    pub fn project(&self) -> &Project {
        self.project
    }

    /// Discover and compile/load a tile once; reuse the returned object for
    /// all of its recorded invocations.
    pub fn prepare_profile(&self, tile_id: &str) -> Result<PreparedTileProfile<'a>> {
        let discovery = TileDiscovery::new(self.project);
        let tile = discovery.get(tile_id).ok_or_else(|| {
            Error::InvalidTileId(format!("Tile '{tile_id}' not found in project"))
        })?;
        let artifact = self
            .backend
            .compile_tile(&tile.to_metadata(), tile.to_content_hash())?;
        Ok(PreparedTileProfile {
            backend: self.backend,
            artifact,
        })
    }

    /// Compile a tile and return its 32-byte image id, without executing it.
    /// Used to assemble the program's tile registry (see program-identity.md).
    pub fn tile_image_id(&self, tile_id: &str) -> Result<[u8; 32]> {
        let discovery = TileDiscovery::new(self.project);
        let tile = discovery.get(tile_id).ok_or_else(|| {
            Error::InvalidTileId(format!("Tile '{}' not found in project", tile_id))
        })?;
        let content_hash = tile.to_content_hash();
        let artifact = self
            .backend
            .compile_tile(&tile.to_metadata(), content_hash)?;
        let bytes = hex::decode(artifact.artifact_id())
            .map_err(|e| Error::Other(format!("Tile image id is not valid hex: {e}")))?;
        bytes
            .try_into()
            .map_err(|_| Error::Other("Tile image id must be 32 bytes".into()))
    }

    /// Replay a single trace item.
    ///
    /// This method:
    /// 1. Decodes the input data from base64
    /// 2. Looks up the tile by function name in the project
    /// 3. Compiles the tile using the backend
    /// 4. Executes the tile with the given execution mode
    /// 5. Optionally compares the output with the recorded output
    ///
    /// # Arguments
    /// * `item` - The trace item to replay
    /// * `mode` - Execution mode (Estimate or Prove)
    ///
    /// # Returns
    /// A `ReplayResult` containing the execution result and optional output comparison.
    pub fn replay(
        &self,
        tile_id: &str,
        input_bytes: &[u8],
        mode: ExecutionMode,
    ) -> Result<ReplayResult> {
        let discovery = TileDiscovery::new(self.project);

        let tile = discovery.get(tile_id).ok_or_else(|| {
            Error::InvalidTileId(format!("Tile '{}' not found in project", tile_id))
        })?;

        let content_hash = tile.to_content_hash();
        let artifact = self
            .backend
            .compile_tile(&tile.to_metadata(), content_hash)?;

        let image_id = artifact.artifact_id();
        let exec_result = self
            .backend
            .execute_tile(artifact.as_ref(), input_bytes, mode)?;

        let image_id = hex::decode(image_id).unwrap();
        let receipt_bytes = exec_result.receipt.clone().ok_or_else(|| {
            Error::Other("Replay requires a proof receipt to recover the replay journal".into())
        })?;
        let receipt: risc0_zkvm::Receipt = raster_core::postcard::from_bytes(&receipt_bytes)
            .map_err(|e| Error::Other(format!("Failed to decode replay receipt: {}", e)))?;
        let replay_journal: TileReplayJournal =
            raster_core::postcard::from_bytes(&receipt.journal.bytes)
                .map_err(|e| Error::Other(format!("Failed to decode replay journal: {}", e)))?;
        Ok(ReplayResult {
            fn_name: tile_id.to_string(),
            receipt: receipt_bytes,
            image_id,
            input: input_bytes.to_vec(),
            output: replay_journal.output_bytes.clone(),
            replay_journal,
        })
    }
}

#[cfg(test)]
mod profiling_tests {
    use super::*;
    use raster_backend::{ArtifactStore, ResourceEstimate, TileExecutionResult};
    use raster_core::tile::TileMetadata;
    use std::any::Any;

    struct Artifact;
    impl CompilationArtifact for Artifact {
        fn id(&self) -> &str {
            "tile"
        }
        fn artifact_id(&self) -> String {
            "ab".repeat(32)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    struct BackendStub {
        result: TileExecutionResult,
        abort: bool,
    }
    impl Backend for BackendStub {
        fn name(&self) -> &'static str {
            "test"
        }
        fn compile_tile(
            &self,
            _: &TileMetadata,
            _: Option<String>,
        ) -> Result<Box<dyn CompilationArtifact>> {
            panic!("a prepared replay must never compile")
        }
        fn execute_tile(
            &self,
            _: &dyn CompilationArtifact,
            input: &[u8],
            mode: ExecutionMode,
        ) -> Result<TileExecutionResult> {
            assert_eq!(input, b"recorded input");
            assert_eq!(mode, ExecutionMode::Estimate);
            if self.abort {
                Err(Error::Other("guest aborted".into()))
            } else {
                Ok(self.result.clone())
            }
        }
        fn artifact_store(&self) -> &dyn ArtifactStore {
            panic!("prepared replay")
        }
        fn estimate_resources(&self, _: &TileMetadata) -> Result<ResourceEstimate> {
            panic!("must measure")
        }
        fn verify_receipt(&self, _: &dyn CompilationArtifact, _: &[u8]) -> Result<bool> {
            panic!("no proof")
        }
    }

    fn backend() -> BackendStub {
        let journal = TileReplayJournal {
            input_commitment: Sha256::digest(b"recorded input").into(),
            output_bytes: b"recorded output".to_vec(),
            draft_transition: None,
            recur: None,
        };
        let mut result = TileExecutionResult::estimate(journal.output_bytes.clone(), 123);
        result.journal = Some(raster_core::postcard::to_allocvec(&journal).unwrap());
        BackendStub {
            result,
            abort: false,
        }
    }

    fn profile(backend: &BackendStub, output: &[u8]) -> Result<u64> {
        PreparedTileProfile {
            backend,
            artifact: Box::new(Artifact),
        }
        .profile(b"recorded input", output)
    }

    #[test]
    fn returns_user_cycles_without_a_receipt_or_recompilation() {
        let backend = backend();
        assert_ne!(backend.result.proof_cycles, Some(123));
        assert_eq!(profile(&backend, b"recorded output").unwrap(), 123);
        assert_eq!(profile(&backend, b"recorded output").unwrap(), 123);
    }

    #[test]
    fn rejects_mismatched_output_and_input_commitment() {
        let mut backend = backend();
        assert!(profile(&backend, b"different output")
            .unwrap_err()
            .to_string()
            .contains("output mismatch"));
        let mut journal: TileReplayJournal =
            raster_core::postcard::from_bytes(backend.result.journal.as_ref().unwrap()).unwrap();
        journal.input_commitment = [0; 32];
        backend.result.journal = Some(raster_core::postcard::to_allocvec(&journal).unwrap());
        assert!(profile(&backend, b"recorded output")
            .unwrap_err()
            .to_string()
            .contains("input commitment mismatch"));
    }

    #[test]
    fn rejects_missing_journal_cycles_receipts_and_guest_failures() {
        let mut missing_journal = backend();
        missing_journal.result.journal = None;
        assert!(profile(&missing_journal, b"recorded output").is_err());
        let mut malformed = backend();
        malformed.result.journal = Some(vec![0]);
        assert!(profile(&malformed, b"recorded output").is_err());
        let mut missing_cycles = backend();
        missing_cycles.result.cycles = None;
        assert!(profile(&missing_cycles, b"recorded output").is_err());
        let mut proved = backend();
        proved.result.receipt = Some(vec![1]);
        assert!(profile(&proved, b"recorded output")
            .unwrap_err()
            .to_string()
            .contains("receipt"));
        let mut aborted = backend();
        aborted.abort = true;
        assert!(profile(&aborted, b"recorded output")
            .unwrap_err()
            .to_string()
            .contains("guest aborted"));
    }
}
