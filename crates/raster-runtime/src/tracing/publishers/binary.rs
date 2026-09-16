use raster_core::trace::TraceEvent;

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, SyncSender};
use std::sync::Mutex;
use std::thread::JoinHandle;

use super::Publisher;

enum TraceWriterMessage {
    Event(TraceEvent),
    Shutdown,
}

pub struct BinaryTraceEventPublisher {
    sender: Mutex<Option<SyncSender<TraceWriterMessage>>>,
    join_handle: Mutex<Option<JoinHandle<std::io::Result<()>>>>,
}

impl BinaryTraceEventPublisher {
    pub fn from_path(path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = File::create(path)?;
        Ok(Self::from_writer(BufWriter::new(file)))
    }

    fn from_writer(writer: impl Write + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::sync_channel(4096);
        let join_handle = std::thread::spawn(move || -> std::io::Result<()> {
            let mut writer = writer;
            while let Ok(message) = receiver.recv() {
                match message {
                    TraceWriterMessage::Event(event) => {
                        let bytes = raster_core::postcard::to_allocvec(&event)
                            .map_err(std::io::Error::other)?;
                        let len = u32::try_from(bytes.len()).map_err(|_| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "Trace event exceeded 4 GiB frame size",
                            )
                        })?;
                        writer.write_all(&len.to_le_bytes())?;
                        writer.write_all(&bytes)?;
                    }
                    TraceWriterMessage::Shutdown => break,
                }
            }
            writer.flush()?;
            Ok(())
        });

        Self {
            sender: Mutex::new(Some(sender)),
            join_handle: Mutex::new(Some(join_handle)),
        }
    }

    fn join_writer(&self) {
        let join_handle = self
            .join_handle
            .lock()
            .expect("Trace writer join mutex poisoned")
            .take();
        if let Some(join_handle) = join_handle {
            match join_handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => panic!("Failed to write binary trace: {}", error),
                Err(_) => panic!("Binary trace writer thread panicked"),
            }
        }
    }
}

impl Publisher for BinaryTraceEventPublisher {
    fn publish(&self, event: TraceEvent) {
        let sender_guard = self
            .sender
            .lock()
            .expect("Trace writer sender mutex poisoned");
        let Some(sender) = sender_guard.as_ref() else {
            panic!("Trace writer has already been shut down");
        };
        let result = sender.send(TraceWriterMessage::Event(event));
        drop(sender_guard);
        if let Err(error) = result {
            // A failed write drops the receiver. Recover that I/O error from
            // the worker instead of hiding it behind a disconnected channel.
            self.join_writer();
            panic!("Failed to queue trace event: {}", error);
        }
    }

    fn finish(&self) {
        let sender = self
            .sender
            .lock()
            .expect("Trace writer sender mutex poisoned")
            .take();
        if let Some(sender) = sender {
            // Even if the worker already exited, join it to surface its actual
            // write/flush failure. Shutdown delivery is not the root cause.
            let _ = sender.send(TraceWriterMessage::Shutdown);
        }

        self.join_writer();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::trace::FnCallRecord;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Arc;

    fn event() -> TraceEvent {
        TraceEvent::SequenceStart(FnCallRecord {
            fn_name: "main".into(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: None,
        })
    }

    struct FailingWriter {
        fail_on_flush: bool,
    }

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.fail_on_flush {
                Ok(bytes.len())
            } else {
                Err(std::io::Error::other("simulated disk full"))
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("simulated flush failure"))
        }
    }

    fn panic_message(run: impl FnOnce()) -> String {
        let panic = catch_unwind(AssertUnwindSafe(run)).unwrap_err();
        panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap()
    }

    #[test]
    fn disconnected_sender_reports_original_write_error() {
        let publisher = BinaryTraceEventPublisher::from_writer(FailingWriter {
            fail_on_flush: false,
        });
        let message = panic_message(|| {
            // More than the queue can hold: the failed worker must disconnect
            // a send, without relying on sleeps or scheduling order.
            for _ in 0..4098 {
                publisher.publish(event());
            }
        });
        assert!(
            message.contains("Failed to write binary trace: simulated disk full"),
            "{message}"
        );
        publisher.finish();
    }

    #[test]
    fn finish_preserves_write_and_flush_errors() {
        for (fail_on_flush, expected) in [
            (false, "simulated disk full"),
            (true, "simulated flush failure"),
        ] {
            let publisher = BinaryTraceEventPublisher::from_writer(FailingWriter { fail_on_flush });
            publisher.publish(event());
            let message = panic_message(|| publisher.finish());
            assert!(message.contains(expected), "{message}");
        }
    }

    struct Buffer(Arc<Mutex<Vec<u8>>>);
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn successful_finish_drains_framed_events_and_is_repeatable() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let publisher = BinaryTraceEventPublisher::from_writer(Buffer(bytes.clone()));
        publisher.publish(event());
        publisher.publish(event());
        publisher.finish();
        publisher.finish();
        let bytes = bytes.lock().unwrap();
        let mut remaining = bytes.as_slice();
        for _ in 0..2 {
            let len = u32::from_le_bytes(remaining[..4].try_into().unwrap()) as usize;
            let decoded: TraceEvent =
                raster_core::postcard::from_bytes(&remaining[4..4 + len]).unwrap();
            assert!(matches!(decoded, TraceEvent::SequenceStart(call) if call.fn_name == "main"));
            remaining = &remaining[4 + len..];
        }
        assert!(remaining.is_empty());
    }
}
