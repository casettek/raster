use raster_core::{
    trace::{Trace, TraceEvent},
    Result,
};
use std::path::PathBuf;

/// Trait for capturing execution traces.
pub trait Tracer {
    fn record_event(&mut self, event: TraceEvent) -> Result<()>;
    fn finalize(self) -> Result<Option<Trace>>;
}

/// Tracer that writes to a file.
pub struct FileTracer {
    run_id: String,
    _output_path: PathBuf,
    events: Vec<TraceEvent>,
}

impl FileTracer {
    pub fn new(run_id: String, output_path: PathBuf) -> Self {
        Self {
            run_id,
            _output_path: output_path,
            events: Vec::new(),
        }
    }
}

impl Tracer for FileTracer {
    fn record_event(&mut self, event: TraceEvent) -> Result<()> {
        self.events.push(event);
        Ok(())
    }

    fn finalize(self) -> Result<Option<Trace>> {
        // TODO: Write trace to file
        let trace = Trace {
            run_id: self.run_id,
            timestamp: 0, // TODO: Use actual timestamp
            events: self.events,
        };

        Ok(Some(trace))
    }
}

/// No-op tracer for when tracing is disabled.
pub struct NoOpTracer;

impl Tracer for NoOpTracer {
    fn record_event(&mut self, _event: TraceEvent) -> Result<()> {
        Ok(())
    }

    fn finalize(self) -> Result<Option<Trace>> {
        Ok(None)
    }
}
