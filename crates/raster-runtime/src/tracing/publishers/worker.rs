//! Run a publisher on its own thread, so the program and its instrumentation
//! work at the same time (`docs/proposals/instrumentation-pipeline.md`, stage 1).
//!
//! Events reach the inner publisher in program order through a bounded channel:
//! the program blocks only when the publisher is `capacity` events behind. A
//! publisher that panics stops the program at its next event or at `finish`,
//! with the publisher's own panic, so a failed recorder never lets a run end as
//! if it had succeeded.
//!
//! The publisher hands each event back once it has recorded it, and the program
//! frees it: memory is cheapest to free on the thread that allocated it.
//!
//! A tile event can arrive with its input `data` still to encode (stage 2,
//! option b): this thread encodes it, frees the bytes itself, and hands the
//! cloned arguments back with the event.
use super::Publisher;
use crate::tracing::deferred_input::{take_input_data, DeferredInput};
use raster_core::trace::TraceEvent;
use std::sync::{mpsc, Mutex};
use std::thread::JoinHandle;

/// Events queued between the program and its publisher before the program waits.
pub const QUEUE_ENV: &str = "RASTER_INSTRUMENTATION_QUEUE";
/// `0` keeps the publisher on the program's thread.
pub const THREAD_ENV: &str = "RASTER_INSTRUMENTATION_THREAD";
/// A queued tile event holds clones of its arguments (a whole weight page on a
/// model stage), and the queue is full whenever the publisher is the slower
/// side. At 1024 that cost Iris `prefill_range_l0`, run on real model weights,
/// 150 MB of peak memory; 64 runs as fast there and on hello-tiles.
const DEFAULT_QUEUE: usize = 64;

enum Message {
    Event(TraceEvent),
    Tile(TraceEvent, DeferredInput),
    Finish,
}

/// What goes back to the program's thread to be freed there.
type Spent = (Option<TraceEvent>, Option<DeferredInput>);

pub struct WorkerPublisher {
    sender: Mutex<Option<mpsc::SyncSender<Message>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    /// Events the publisher is done with, to be freed on the program's thread.
    spent: Mutex<mpsc::Receiver<Spent>>,
}

impl WorkerPublisher {
    /// Whether to run the publisher on its own thread: unless turned off.
    pub fn enabled() -> bool {
        std::env::var(THREAD_ENV).map_or(true, |value| value != "0")
    }

    /// [`Self::with_capacity`], with the queue length from [`QUEUE_ENV`].
    pub fn spawn(inner: Box<dyn Publisher>) -> Self {
        let capacity = std::env::var(QUEUE_ENV)
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|capacity| *capacity > 0)
            .unwrap_or(DEFAULT_QUEUE);
        Self::with_capacity(inner, capacity)
    }

    /// Run `inner` on its own thread, `capacity` events behind the program at
    /// most.
    pub fn with_capacity(inner: Box<dyn Publisher>, capacity: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        // Unbounded, but drained at every event the program sends, so it holds
        // about as many events as the queue does.
        let (spend, spent) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("raster-instrumentation".into())
            // The recorder recurses over the program's structure; give it the
            // room the program's own thread has, not a spawned thread's 2 MiB.
            .stack_size(64 << 20)
            .spawn(move || {
                for message in receiver {
                    match message {
                        Message::Event(event) => {
                            if let Some(event) = inner.publish_returning(event) {
                                // Gone only once the program stopped listening:
                                // then this thread frees it.
                                let _ = spend.send((Some(event), None));
                            }
                        }
                        Message::Tile(mut event, mut input) => {
                            input.fill(&mut event);
                            let event = inner.publish_returning(event).map(|mut event| {
                                drop(take_input_data(&mut event));
                                event
                            });
                            let _ = spend.send((event, Some(input)));
                        }
                        Message::Finish => return inner.finish(),
                    }
                }
            })
            .expect("start the instrumentation thread");
        Self {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            spent: Mutex::new(spent),
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
        self.free_spent();
    }

    /// Free, here on the program's thread, the events the publisher is done with.
    fn free_spent(&self) {
        self.spent
            .lock()
            .expect("instrumentation spent events")
            .try_iter()
            .for_each(drop);
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

    fn publish_deferred(&self, event: TraceEvent, input: DeferredInput) {
        self.send(Message::Tile(event, input));
    }

    fn finish(&self) {
        self.send(Message::Finish);
        drop(self.sender.lock().expect("instrumentation sender").take());
        self.join();
        self.free_spent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::deferred_input::{DeferArg, DeferByClone as _, EncodeNow as _};
    use raster_core::trace::{FnCallRecord, FnInput};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Arc;

    /// What the inner publisher saw, in order: each event's tile name and
    /// input bytes, then `"finish"`.
    type Seen = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

    struct Recording {
        seen: Seen,
        /// Panic on the event with this tile name.
        panic_on: Option<&'static str>,
        panic_in_finish: bool,
    }

    impl Publisher for Recording {
        fn publish(&self, event: TraceEvent) {
            self.publish_returning(event);
        }

        fn publish_returning(&self, event: TraceEvent) -> Option<TraceEvent> {
            let TraceEvent::TileExec(record) = &event else {
                panic!("tests publish tile events only")
            };
            if self.panic_on == Some(record.fn_name.as_str()) {
                panic!("recorder failed on {}", record.fn_name);
            }
            let data = record.input.as_ref().map(|input| input.data.clone()).unwrap_or_default();
            self.seen.lock().unwrap().push((record.fn_name.clone(), data));
            Some(event)
        }

        fn finish(&self) {
            if self.panic_in_finish {
                panic!("recorder failed at finish");
            }
            self.seen.lock().unwrap().push(("finish".into(), Vec::new()));
        }
    }

    fn worker(capacity: usize, panic_on: Option<&'static str>, panic_in_finish: bool) -> (WorkerPublisher, Seen) {
        let seen = Seen::default();
        let inner = Recording {
            seen: seen.clone(),
            panic_on,
            panic_in_finish,
        };
        (WorkerPublisher::with_capacity(Box::new(inner), capacity), seen)
    }

    fn tile(name: &str, data: Vec<u8>) -> TraceEvent {
        TraceEvent::TileExec(FnCallRecord {
            fn_name: name.into(),
            input: Some(FnInput {
                data,
                values: Vec::new(),
                args: Vec::new(),
                storage: Default::default(),
            }),
            output: None,
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        })
    }

    fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
        panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|message| message.to_string()))
            .unwrap_or_default()
    }

    /// Every event reaches the publisher once, in program order, and `finish`
    /// runs after the last, whatever the queue length; a queue of one makes
    /// the program wait on nearly every event.
    #[test]
    fn events_arrive_in_order_and_finish_comes_last() {
        for capacity in [1, 64] {
            let (worker, seen) = worker(capacity, None, false);
            for i in 0..2000u32 {
                worker.publish(tile(&format!("t{i}"), i.to_le_bytes().to_vec()));
            }
            worker.finish();
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 2001);
            for (i, (name, data)) in seen[..2000].iter().enumerate() {
                assert_eq!(name, &format!("t{i}"));
                assert_eq!(data, &(i as u32).to_le_bytes().to_vec());
            }
            assert_eq!(seen[2000].0, "finish");
        }
    }

    /// A deferred tile input is encoded before the publisher sees the event,
    /// into the bytes the wrapper's tuple encoding gave.
    #[test]
    fn deferred_inputs_are_encoded_before_the_publisher_sees_them() {
        let (worker, seen) = worker(4, None, false);
        let page = vec![7u8; 300];
        let width = 9u32;
        let parts = vec![
            (&DeferArg(&page)).__raster_input_part(),
            (&DeferArg(&width)).__raster_input_part(),
        ];
        worker.publish_deferred(tile("deferred", Vec::new()), DeferredInput::new(parts));
        worker.publish(tile("plain", vec![1, 2]));
        worker.finish();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, "deferred");
        assert_eq!(seen[0].1, raster_core::postcard::to_allocvec(&(&page, &width)).unwrap());
        assert_eq!(seen[1], ("plain".to_string(), vec![1, 2]));
    }

    /// A publisher that fails stops the program with the publisher's own
    /// panic, at a later event or at `finish`, so a failed recording never
    /// ends as a successful run.
    #[test]
    fn a_failed_publisher_resurfaces_on_the_program_thread() {
        let (worker, seen) = worker(2, Some("t5"), false);
        let result = catch_unwind(AssertUnwindSafe(|| {
            for i in 0..1000 {
                worker.publish(tile(&format!("t{i}"), Vec::new()));
            }
            worker.finish();
        }));
        let message = panic_message(result.expect_err("the program must not finish"));
        assert_eq!(message, "recorder failed on t5");
        assert!(seen.lock().unwrap().iter().all(|(name, _)| name != "finish"));
    }

    #[test]
    fn a_failed_finish_resurfaces_on_the_program_thread() {
        let (worker, _) = worker(2, None, true);
        worker.publish(tile("t0", Vec::new()));
        let result = catch_unwind(AssertUnwindSafe(|| worker.finish()));
        assert_eq!(
            panic_message(result.expect_err("finish must fail")),
            "recorder failed at finish"
        );
    }

    /// Publishing after `finish` is a program error, not a silent drop.
    #[test]
    fn events_after_finish_are_refused() {
        let (worker, _) = worker(2, None, false);
        worker.finish();
        let result = catch_unwind(AssertUnwindSafe(|| worker.publish(tile("late", Vec::new()))));
        assert!(panic_message(result.expect_err("a late event must fail")).contains("stopped"));
    }
}
