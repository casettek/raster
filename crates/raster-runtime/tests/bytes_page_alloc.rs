//! Regression guard for `paged-bytes.md` §12.2.
//!
//! A `BytesPage` used to reach the selection tree by being serialized with the
//! general-purpose `TreeValueSerializer` and pattern-matched afterwards. That
//! expanded the payload into one 56-byte `TreeValue::U8` per byte — 56× the page
//! — built, walked once, and discarded. The fix recognizes the page from its
//! newtype *name*, before the inner value is serialized, and collects the payload
//! into a flat buffer (`raster_core::collections::bytes_page_parts`).
//!
//! Correctness tests cannot see the difference: both spellings produce the same
//! root and the same postcard bytes. Only allocation can, so it is measured here.
//!
//! This lives in its own test binary because a `#[global_allocator]` is
//! process-wide and the peak counter is global — a second test running
//! concurrently would make the measurement meaningless.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingAllocator;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Peak *additional* live bytes while `f` runs.
fn peak_extra_bytes<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let out = f();
    let peak = PEAK.load(Ordering::Relaxed);
    (out, peak.saturating_sub(base))
}

const PAGE_SIZE: usize = 1 << 20; // 1 MiB

#[test]
fn encoding_a_page_does_not_allocate_a_node_per_byte() {
    let region =
        raster_core::Bytes::<{ PAGE_SIZE as u64 }>::paged(vec![0xABu8; PAGE_SIZE]).unwrap();
    assert_eq!(region.pages().len(), 1);

    let (encoded, peak) = peak_extra_bytes(|| raster_runtime::encode_raster_value(&region));
    let (data, _index, _commitment) = encoded.unwrap();
    assert!(data.len() > PAGE_SIZE, "payload should carry the page");

    // The honest floor is a few copies of the payload: the flat `TreeValue`
    // buffer, the assembled `0x0B` payload, and reallocation slack. The old
    // per-byte path peaked at `size_of::<TreeValue>()` = 56 copies, so anything
    // below ~12x separates the two unambiguously without making this a
    // brittle exact-bytes assertion.
    let ratio = peak as f64 / PAGE_SIZE as f64;
    assert!(
        ratio < 12.0,
        "encoding one {PAGE_SIZE}-byte page peaked at {peak} bytes ({ratio:.1}x the page). \
         A per-byte value tree is back — see paged-bytes.md §12.2."
    );
}

/// The draft bridge has the same shape and the same failure mode, and it is
/// reached by every `store_value` of a page-bearing type.
#[test]
fn drafting_a_page_does_not_allocate_a_node_per_byte() {
    let region =
        raster_core::Bytes::<{ PAGE_SIZE as u64 }>::paged(vec![0xCDu8; PAGE_SIZE]).unwrap();

    let (drafted, peak) =
        peak_extra_bytes(|| raster_core::draft::draft_value_from_serialize(&region));
    let drafted = drafted.unwrap();
    let (payload, _root) = raster_core::draft::draft_value_payload_and_root(&drafted).unwrap();
    assert!(payload.len() > PAGE_SIZE);

    let ratio = peak as f64 / PAGE_SIZE as f64;
    assert!(
        ratio < 12.0,
        "drafting one {PAGE_SIZE}-byte page peaked at {peak} bytes ({ratio:.1}x the page). \
         A per-byte value tree is back — see paged-bytes.md §12.2."
    );
}
