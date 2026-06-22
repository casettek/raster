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
        let (sender, receiver) = mpsc::sync_channel(4096);
        let join_handle = std::thread::spawn(move || -> std::io::Result<()> {
            let mut writer = BufWriter::new(file);
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

        Ok(Self {
            sender: Mutex::new(Some(sender)),
            join_handle: Mutex::new(Some(join_handle)),
        })
    }
}

impl Publisher for BinaryTraceEventPublisher {
    fn publish(&self, event: TraceEvent) {
        let sender = self
            .sender
            .lock()
            .expect("Trace writer sender mutex poisoned");
        let Some(sender) = sender.as_ref() else {
            panic!("Trace writer has already been shut down");
        };
        sender
            .send(TraceWriterMessage::Event(event))
            .unwrap_or_else(|error| panic!("Failed to queue trace event: {}", error));
    }

    fn finish(&self) {
        let sender = self
            .sender
            .lock()
            .expect("Trace writer sender mutex poisoned")
            .take();
        if let Some(sender) = sender {
            sender
                .send(TraceWriterMessage::Shutdown)
                .unwrap_or_else(|error| panic!("Failed to shut down trace writer: {}", error));
        }

        let join_handle = self
            .join_handle
            .lock()
            .expect("Trace writer join mutex poisoned")
            .take();
        if let Some(join_handle) = join_handle {
            match join_handle.join() {
                Ok(result) => result.unwrap_or_else(|error| {
                    panic!("Failed to flush binary trace writer: {}", error)
                }),
                Err(_) => panic!("Binary trace writer thread panicked"),
            }
        }
    }
}
