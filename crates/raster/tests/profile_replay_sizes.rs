//! `TileProfileRecord.input_bytes` / `output_bytes` — the replay unit size.
//!
//! Timings say *where* a tile spends its budget; they cannot say *how big the
//! unit being budgeted is*. That is the number an author changes when tuning
//! `page_size` or `chunk = N`, so it has to be observable. This test pins that
//! it is, on the case it exists for: a `Bytes<P>` sweep, where `input_bytes`
//! must equal the declared page size on every full page and be short on the last.
//!
//! Requires `--features profiling`; inert otherwise.
#![cfg(feature = "profiling")]

use raster::materialize_auth_return;
use raster::prelude::*;
use raster_runtime::{ExecutionProfile, ProfileRecord, PROFILE_PATH_ENV};
use serde::{Deserialize, Serialize};

const PAGE_SIZE: u64 = 64;

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct Model {
    #[page_size = 64]
    weights: Bytes<64>,
}

#[tile(kind = recur, description = "sum one page")]
fn sum_page(input: RecurInput<BytesPage>, state: RecurState<u64>) -> RecurState<u64> {
    let mut state = state;
    *state += input
        .into_value()
        .as_slice()
        .iter()
        .map(|byte| *byte as u64)
        .sum::<u64>();
    state
}

#[sequence]
fn sweep() -> u64 {
    // 160 bytes at 64/page -> pages of 64, 64, 32.
    let model = Model {
        weights: Bytes::<64>::paged(vec![1u8; 160]).expect("page the fixture"),
    };
    let source = raster::store_value(&model).expect("store model");
    let pages = select!(List<BytesPage>, storage!(Model, source).weights.pages);
    call_recur!(tile = sum_page, input = pages, state = 0u64, args = ())
}

#[test]
fn recur_input_bytes_track_the_page_size() {
    let dir = std::env::temp_dir().join(format!("raster-profile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("profile.json");

    // SAFETY: single-threaded test binary; the profiler reads this once at init.
    unsafe { std::env::set_var(PROFILE_PATH_ENV, &path) };
    raster_runtime::profiling::init_from_env();

    let sum = materialize_auth_return::<u64, _>(__raster_sequence_auth_sweep());
    assert_eq!(sum, 160);

    let written = raster_runtime::profiling::finish().unwrap();
    assert_eq!(written.as_deref(), Some(path.as_path()));

    let profile: ExecutionProfile =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let pages: Vec<_> = profile
        .records
        .iter()
        .filter_map(|record| match record {
            ProfileRecord::Tile(tile) if tile.tile_id == "sum_page" => Some(tile),
            _ => None,
        })
        .collect();

    assert_eq!(pages.len(), 3, "160 bytes / 64 -> 3 pages");

    // The replay unit is the page plus a small postcard envelope (the page's
    // three u64 coordinates, the recur index/len, varint framing). Assert the
    // *shape* — input scales with the page, not with the region — rather than an
    // exact byte count that would break on any envelope change.
    let envelope = pages[0].input_bytes - PAGE_SIZE;
    assert!(envelope < 64, "envelope should be small, got {envelope}");

    assert_eq!(pages[0].input_bytes, PAGE_SIZE + envelope);
    assert_eq!(pages[1].input_bytes, PAGE_SIZE + envelope);
    assert!(
        pages[2].input_bytes < pages[1].input_bytes,
        "final page is short: {} should be < {}",
        pages[2].input_bytes,
        pages[1].input_bytes,
    );

    // A fold's output is just the state, so it stays tiny while the input scales
    // with `page_size` — exactly the asymmetry an author looks for when deciding
    // whether a sweep is input-bound or output-bound. (It is not perfectly
    // constant: the running sum is postcard-varint encoded, so it grows by a byte
    // as it crosses each 7-bit boundary. Assert the shape, not equality.)
    assert!(pages.iter().all(|page| page.output_bytes > 0));
    assert!(
        pages.iter().all(|page| page.output_bytes < 16),
        "fold output should stay tiny beside a {PAGE_SIZE}-byte input, got {:?}",
        pages.iter().map(|p| p.output_bytes).collect::<Vec<_>>(),
    );
    assert!(
        pages[0].input_bytes > 8 * pages[0].output_bytes,
        "this sweep is input-bound: in={} out={}",
        pages[0].input_bytes,
        pages[0].output_bytes,
    );

    std::fs::remove_dir_all(&dir).ok();
}
