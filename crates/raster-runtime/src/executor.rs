use raster_core::{Result, schema::SequenceSchema, trace::Trace};
use raster_backend::{Backend, NativeBackend};
use crate::tracer::Tracer;

/// Executes tiles according to a sequence schema.
pub struct Executor<T: Tracer> {
    backend: Box<dyn Backend>,
    tracer: T,
}

impl<T: Tracer> Executor<T> {
    pub fn new(tracer: T) -> Self {
        Self {
            backend: Box::new(NativeBackend::new()),
            tracer,
        }
    }

    pub fn with_backend(mut self, backend: Box<dyn Backend>) -> Self {
        self.backend = backend;
        self
    }

    /// Execute a sequence and return the result with optional trace.
    pub fn execute(self, _schema: &SequenceSchema) -> Result<ExecutionResult> {
        // TODO: Implement execution
        // - Load tiles
        // - Execute according to control flow
        // - Record trace events
        // - Return result

        let trace = self.tracer.finalize()?;

        Ok(ExecutionResult {
            output: Vec::new(),
            trace,
        })
    }
}

#[derive(Debug)]
pub struct ExecutionResult {
    pub output: Vec<u8>,
    pub trace: Option<Trace>,
}
