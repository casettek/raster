use super::Publisher;
use raster_core::trace::TraceEvent;
use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::PathBuf,
    sync::Mutex,
};

/// Writes in publication order, applying disk backpressure at the publisher.
pub struct BinaryTraceEventPublisher {
    writer: Mutex<BufWriter<File>>,
}
impl BinaryTraceEventPublisher {
    pub fn from_path(path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self {
            writer: Mutex::new(BufWriter::new(File::create(path)?)),
        })
    }
}

impl Publisher for BinaryTraceEventPublisher {
    fn publish(&self, event: TraceEvent) {
        let mut writer = self.writer.lock().expect("trace writer lock");
        let bytes = raster_core::postcard::to_allocvec(&event).expect("encode trace event");
        let len = u32::try_from(bytes.len()).expect("Trace event exceeded 4 GiB frame size");
        writer
            .write_all(&len.to_le_bytes())
            .and_then(|_| writer.write_all(&bytes))
            .expect("write binary trace (check free disk space)");
    }
    fn finish(&self) {
        self.writer
            .lock()
            .expect("trace writer lock")
            .flush()
            .expect("flush binary trace");
    }
}
