//! Run a publisher on its own thread, so the program and its instrumentation
//! work at the same time (`docs/proposals/instrumentation-pipeline.md`, stage 1).
//!
//! Events reach the inner publisher in program order through a bounded channel:
//! the program blocks only when the publisher is `capacity` events behind. A
//! publisher that panics stops the program at its next event or at `finish`,
//! with the publisher's own panic, so a failed recorder never lets a run end as
//! if it had succeeded.
use super::Publisher;
use raster_core::trace::TraceEvent;
use std::sync::{mpsc, Mutex};
use std::thread::JoinHandle;

/// Events queued between the program and its publisher before the program waits.
pub const QUEUE_ENV: &str = "RASTER_INSTRUMENTATION_QUEUE";
/// `0` keeps the publisher on the program's thread.
pub const THREAD_ENV: &str = "RASTER_INSTRUMENTATION_THREAD";
const DEFAULT_QUEUE: usize = 1024;

enum Message {
    Event(TraceEvent),
    Finish,
}

pub struct WorkerPublisher {
    sender: Mutex<Option<mpsc::SyncSender<Message>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl WorkerPublisher {
    /// Whether to run the publisher on its own thread: unless turned off.
    pub fn enabled() -> bool {
        std::env::var(THREAD_ENV).map_or(true, |value| value != "0")
    }

    pub fn spawn(inner: Box<dyn Publisher>) -> Self {
        let capacity = std::env::var(QUEUE_ENV)
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|capacity| *capacity > 0)
            .unwrap_or(DEFAULT_QUEUE);
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let worker = std::thread::Builder::new()
            .name("raster-instrumentation".into())
            // The recorder recurses over the program's structure; give it the
            // room the program's own thread has, not a spawned thread's 2 MiB.
            .stack_size(64 << 20)
            .spawn(move || {
                for message in receiver {
                    match message {
                        Message::Event(event) => inner.publish(event),
                        Message::Finish => return inner.finish(),
                    }
                }
            })
            .expect("start the instrumentation thread");
        Self {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
        }
    }

    fn send(&self, message: Message) {
        let sent = self
            .sender
            .lock()
            .expect("instrumentation sender")
            .as_ref()
            .map(|sender| sender.send(message).is_ok());
        if sent != Some(true) {
            self.join();
            panic!("the instrumentation thread stopped before the program finished");
        }
    }

    /// Wait for the thread, re-raising its panic on the program's thread.
    fn join(&self) {
        let worker = self.worker.lock().expect("instrumentation worker").take();
        if let Some(Err(panic)) = worker.map(JoinHandle::join) {
            std::panic::resume_unwind(panic);
        }
    }
}

impl Publisher for WorkerPublisher {
    fn publish(&self, event: TraceEvent) {
        self.send(Message::Event(event));
    }

    fn finish(&self) {
        self.send(Message::Finish);
        drop(self.sender.lock().expect("instrumentation sender").take());
        self.join();
    }
}
