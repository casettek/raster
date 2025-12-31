use raster_core::{Result, manifest::Manifest};
use raster_backend::{Backend, NativeBackend};
use std::path::PathBuf;

/// Orchestrates the build process for a Raster project.
pub struct Builder {
    backend: Box<dyn Backend>,
    _output_dir: PathBuf,
}

impl Builder {
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            backend: Box::new(NativeBackend::new()),
            _output_dir: output_dir,
        }
    }

    pub fn with_backend(mut self, backend: Box<dyn Backend>) -> Self {
        self.backend = backend;
        self
    }

    /// Build all tiles and schemas from a manifest.
    pub fn build(&self, _manifest: &Manifest) -> Result<BuildOutput> {
        // TODO: Implement build orchestration
        // - Compile each tile
        // - Generate schemas
        // - Write artifacts to output_dir
        Ok(BuildOutput {
            tiles_compiled: 0,
            schemas_generated: 0,
        })
    }
}

#[derive(Debug)]
pub struct BuildOutput {
    pub tiles_compiled: usize,
    pub schemas_generated: usize,
}
