use raster_core::trace::TraceEvent;

use std::sync::OnceLock;

// TODO: consider adding linkme here
/// The global subscriber instance.
pub(crate) static GLOBAL_PUBLISHER: OnceLock<Box<dyn Publisher>> = OnceLock::new();

use std::io::Write;
use std::sync::Mutex;

use crate::tracing::TRACE_EVENT_PREFIX;

/// A trait for receiving trace events.
pub trait Publisher: Send + Sync {
    fn publish(&self, event: TraceEvent);

    fn finish(&self);
}

pub struct TraceEventPublisher<W: Write + Send> {
    writer: Mutex<W>,
}

impl<W: Write + Send> TraceEventPublisher<W> {
    pub fn new(writer: W) -> Self {
        Self { writer: Mutex::new(writer) }
    }
}

impl<W: Write + Send + Sync> Publisher for TraceEventPublisher<W> {
    fn publish(&self, event: TraceEvent) {
        let json_str = serde_json::to_string(&event).expect("Failed to serialize");
        let mut writer_guard = self.writer.lock().expect("Writer mutex poisoned");
        writeln!(writer_guard, "{}{}", TRACE_EVENT_PREFIX, json_str).expect("Failed to write");
    }

    fn finish(&self) {
        let mut writer_guard = self.writer.lock().expect("Writer mutex poisoned");
        writer_guard.flush().expect("Failed to flush writer");
    }
}
