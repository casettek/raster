use std::io::Write;
use std::sync::Mutex;

use raster_core::trace::{TraceInputParam, TraceItem};

use crate::tracing::subscriber::Subscriber;

/// A JSON-formatting subscriber that writes to a writer.
pub struct JsonSubscriber<W: Write + Send> {
    writer: Mutex<W>,
}

impl<W: Write + Send> JsonSubscriber<W> {
    /// Creates a new JSON subscriber that writes to the given writer.
    pub fn new(writer: W) -> Self {
        Self {
            writer: Mutex::new(writer),
        }
    }
}

impl<W: Write + Send + Sync> Subscriber for JsonSubscriber<W> {
    fn on_trace(
        &self,
        function_name: &str,
        desc: Option<&str>,
        input_params: &[(&str, &str)],
        output_type: Option<&str>,
        input: &[u8],
        output: &[u8],
    ) {
        // TODO: should be enum TraceItem which have Tile/Sequence
        let item = TraceItem {
            fn_name: function_name.to_string(),
            desc: desc.map(|s| s.to_string()),
            inputs: input_params
                .iter()
                .map(|(name, ty)| TraceInputParam {
                    name: name.to_string(),
                    ty: ty.to_string(),
                })
                .collect(),
            input_data: input.to_vec(),
            output_type: output_type.map(|s| s.to_string()),
            output_data: output.to_vec(),
        };

        let mut writer_guard = self.writer.lock().expect("Writer mutex poisoned");
        let json_str = serde_json::to_string(&item).expect("Failed to serialize");
        write!(writer_guard, "RASTER_TRACE:{}\n", json_str).expect("Failed to write");
    }

    fn on_complete(&self) {
        let mut writer_guard = self.writer.lock().expect("Writer mutex poisoned");
        writer_guard.flush().expect("Failed to flush writer");
    }
}
