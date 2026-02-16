//! Trace replayer for re-executing tiles with proof generation.

use raster_backend::{Backend, ExecutionMode};
use raster_compiler::tile::TileDiscovery;
use raster_compiler::Project;

use raster_core::trace::TraceItem;
use raster_core::{Error, Result};

#[derive(Debug, Clone)]
pub struct ReplayResult {
    pub fn_name: String,

    pub receipt: Vec<u8>,

    pub image_id: Vec<u8>,
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
pub struct TraceReplayer<'a> {
    backend: &'a dyn Backend,
    project: &'a Project,
}

impl<'a> TraceReplayer<'a> {
    /// Create a new replayer with the given backend and project context.
    ///
    /// # Arguments
    /// * `backend` - The backend to use for compilation and execution (e.g., Risc0Backend)
    /// * `project` - The project containing tile definitions for lookup
    pub fn new(backend: &'a dyn Backend, project: &'a Project) -> Self {
        Self { backend, project }
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
    pub fn replay(&self, item: &TraceItem, mode: ExecutionMode) -> Result<ReplayResult> {
        let discovery = TileDiscovery::new(self.project);
        let tile = discovery.get(&item.fn_name).ok_or_else(|| {
            Error::InvalidTileId(format!("Tile '{}' not found in project", item.fn_name))
        })?;

        // 3. Compile the tile
        let content_hash = tile.to_content_hash();
        let artifact = self
            .backend
            .compile_tile(&tile.to_metadata(), content_hash.as_deref())?;

        let image_id = artifact.artifact_id();
        // 4. Execute with backend
        let exec_result = self
            .backend
            .execute_tile(artifact.as_ref(), &item.input_data, mode)?;

        Ok(ReplayResult {
            fn_name: item.fn_name.clone(),
            receipt: exec_result.receipt.unwrap(),
            image_id,
        })
    }

    /// Replay multiple trace items in sequence.
    ///
    /// This is a convenience method for replaying all items in a trace window.
    /// Stops on first error and returns the results collected so far.
    ///
    /// # Arguments
    /// * `items` - Slice of trace items to replay
    /// * `mode` - Execution mode for all items
    ///
    /// # Returns
    /// A vector of replay results for each successfully replayed item.
    pub fn replay_all(
        &self,
        items: &[TraceItem],
        mode: ExecutionMode,
    ) -> Result<Vec<ReplayResult>> {
        let mut results = Vec::with_capacity(items.len());
        for item in items {
            results.push(self.replay(item, mode)?);
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_replay_result_debug() {
        // Basic test to ensure ReplayResult can be constructed and debug-printed
        let result = ReplayResult {
            fn_name: "test_fn".to_string(),
            receipt: Vec::new(),
            image_id: Vec::new(),
        };
        assert_eq!(result.fn_name, "test_fn");
    }
}

