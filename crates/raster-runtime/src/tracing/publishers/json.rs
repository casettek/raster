use raster_core::trace::TraceEvent;

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use super::Publisher;

pub struct JsonTraceEventPublisher<W: Write + Send> {
    writer: Mutex<W>,
    prefix: Option<&'static str>,
}

impl<W: Write + Send> JsonTraceEventPublisher<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer: Mutex::new(writer),
            prefix: None,
        }
    }

    pub fn with_prefix(writer: W, prefix: &'static str) -> Self {
        Self {
            writer: Mutex::new(writer),
            prefix: Some(prefix),
        }
    }
}

impl JsonTraceEventPublisher<BufWriter<File>> {
    pub fn from_path(path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = File::create(path)?;
        Ok(Self::new(BufWriter::new(file)))
    }
}

impl<W: Write + Send + Sync> Publisher for JsonTraceEventPublisher<W> {
    fn publish(&self, event: TraceEvent) {
        let json_str = serde_json::to_string(&event).expect("Failed to serialize trace event");
        let mut writer_guard = self.writer.lock().expect("Writer mutex poisoned");
        if let Some(prefix) = self.prefix {
            write!(writer_guard, "{prefix}").expect("Failed to write trace event prefix");
        }
        writeln!(writer_guard, "{json_str}").expect("Failed to write trace event");
    }

    fn finish(&self) {
        let mut writer_guard = self.writer.lock().expect("Writer mutex poisoned");
        writer_guard.flush().expect("Failed to flush writer");
    }
}

pub type TraceEventPublisher<W> = JsonTraceEventPublisher<W>;

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::trace::{FnCallRecord, TraceEvent};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn test_event() -> TraceEvent {
        TraceEvent::SequenceStart(FnCallRecord {
            fn_name: "main".into(),
            input: None,
            output: None,
            draft_transition_witness: None,
        })
    }

    #[test]
    fn json_publisher_writes_ndjson_without_prefix_by_default() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let publisher = JsonTraceEventPublisher::new(SharedBuffer(Arc::clone(&bytes)));

        publisher.publish(test_event());
        publisher.finish();

        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(output.starts_with(r#"{"SequenceStart""#));
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn json_publisher_can_prefix_lines_for_stdout_debugging() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let publisher =
            JsonTraceEventPublisher::with_prefix(SharedBuffer(Arc::clone(&bytes)), "[trace-event]");

        publisher.publish(test_event());
        publisher.finish();

        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(output.starts_with(r#"[trace-event]{"SequenceStart""#));
    }
}
