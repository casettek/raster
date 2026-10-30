//! `replay-profile.json`: the zkVM cost of a run's tiles, measured by replaying
//! recorded calls in the RISC Zero executor without proving.
//!
//! One entry per tile the CFS declares, executed or not. Per tile it keeps the
//! size histogram of every recorded call and the measurements of the calls
//! that were replayed — the largest by input + output bytes. Maxima, medians,
//! totals and cycle ranks are derived when rendering, not stored.
//! See `docs/proposals/tile-cost-estimator.md` §5.

use std::collections::BTreeMap;

use raster_core::cfs::CfsCoordinates;
use raster_core::{Error, Result};
use serde::{Deserialize, Serialize};

pub const REPLAY_PROFILE_KIND: &str = "replay-profile";
pub const REPLAY_PROFILE_VERSION: u32 = 4;
pub const REPLAY_PROFILE_FILE: &str = "replay-profile.json";

/// Width of the cost bar, in terminal cells.
const BAR_WIDTH: usize = 24;
/// Width of a size-histogram bar, in terminal cells.
const HISTOGRAM_WIDTH: usize = 20;
/// Tile names longer than this are cut, so one long name cannot push every row.
const MAX_NAME_WIDTH: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayProfile {
    pub kind: String,
    pub version: u32,
    pub run_id: String,
    /// How many calls per tile were selected, largest by input + output bytes.
    pub calls_per_tile: usize,
    /// Every selected call was replayed. False when a replay failed.
    pub complete: bool,
    pub failure: Option<ReplayFailure>,
    /// Every tile the CFS declares, keyed by tile id.
    pub tiles: BTreeMap<String, TileReplay>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayFailure {
    pub tile: String,
    pub exec_index: u64,
    pub coordinates: CfsCoordinates,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileReplay {
    /// Every recorded call of this tile in the run.
    pub calls_recorded: u64,
    /// Input + output bytes over every recorded call, in power-of-two buckets.
    pub size_histogram: Vec<SizeBucket>,
    /// Image id of the guest that was measured, once the tile was prepared.
    pub image_id: Option<String>,
    /// The tile's image id in `Raster.lock`, when the project has one.
    pub lock_image_id: Option<String>,
    /// Replayed calls, in replay order.
    pub replayed: Vec<ReplayedCall>,
}

/// Calls whose input + output bytes are in `(up_to_bytes / 2, up_to_bytes]`;
/// `up_to_bytes == 0` counts empty calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeBucket {
    pub up_to_bytes: u64,
    pub calls: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayedCall {
    pub exec_index: u64,
    pub coordinates: CfsCoordinates,
    pub input_bytes: u64,
    pub output_bytes: u64,
    /// 1 for the tile's largest call by input + output bytes, among all its
    /// recorded calls.
    pub byte_rank: u64,
    pub user_cycles: u64,
    pub segments: u64,
    /// `Σ 2^po2` over the call's segments.
    pub padded_cycles: u64,
    /// Executor wall time on the measuring machine.
    pub exec_ns: u64,
}

impl ReplayProfile {
    pub fn new(
        run_id: impl Into<String>,
        tile_ids: impl IntoIterator<Item = String>,
        calls_per_tile: usize,
    ) -> Self {
        Self {
            kind: REPLAY_PROFILE_KIND.into(),
            version: REPLAY_PROFILE_VERSION,
            run_id: run_id.into(),
            calls_per_tile,
            complete: false,
            failure: None,
            tiles: tile_ids
                .into_iter()
                .map(|id| (id, TileReplay::default()))
                .collect(),
        }
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let profile: Self = serde_json::from_slice(bytes)
            .map_err(|error| Error::Other(format!("Not a replay profile: {error}")))?;
        if profile.kind != REPLAY_PROFILE_KIND || profile.version != REPLAY_PROFILE_VERSION {
            return Err(Error::Other(format!(
                "Expected a {REPLAY_PROFILE_KIND} version {REPLAY_PROFILE_VERSION}, found {} \
                 version {}",
                profile.kind, profile.version
            )));
        }
        Ok(profile)
    }

    pub fn to_json(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(self)?)
    }

    pub fn to_text(&self) -> String {
        let rows = self.rows();
        let name_width = rows
            .iter()
            .map(|row| display_name(row.0).chars().count())
            .max()
            .unwrap_or(0)
            .max("tile".len());
        let top = rows
            .iter()
            .filter_map(|row| row.2.map(|call| call.user_cycles))
            .max()
            .unwrap_or(0);
        let replayed: usize = self.tiles.values().map(|tile| tile.replayed.len()).sum();
        let recorded: u64 = self.tiles.values().map(|tile| tile.calls_recorded).sum();

        let mut out = vec![
            "Replay profile — RISC Zero executor, no proving".to_string(),
            format!("  Run: {}", self.run_id),
            if self.complete {
                "  Status: complete".to_string()
            } else {
                "  Status: PARTIAL — stopped at the failure below".to_string()
            },
            format!(
                "  Calls: top {} per tile by input + output bytes; {} of {} replayed",
                self.calls_per_tile,
                count(replayed as u64),
                count(recorded)
            ),
            "  Cycles are guest user cycles, including the tile wrapper (input decode and hash, \
             output encode)."
                .to_string(),
            String::new(),
            format!(
                "{:<name_width$}  {:<37}  {:>3} {:>7} {:>6} {:>6}",
                "tile", "heaviest call, user cycles (% of max)", "seg", "padded", "exec", "calls"
            ),
        ];
        for (id, tile, heaviest) in &rows {
            let name = display_name(id);
            match heaviest {
                Some(call) => out.push(format!(
                    "{name:<name_width$}  {:<BAR_WIDTH$} {:>6} {:>4}%  {:>3} {:>7} {:>6} {:>6}",
                    bar(call.user_cycles, top, BAR_WIDTH),
                    cycles(call.user_cycles),
                    percent(call.user_cycles, top),
                    call.segments,
                    cycles(call.padded_cycles),
                    duration(call.exec_ns),
                    count(tile.calls_recorded),
                )),
                None => out.push(format!(
                    "{name:<name_width$}  {:<37}  {:>3} {:>7} {:>6} {:>6}",
                    if tile.calls_recorded == 0 {
                        "not executed"
                    } else {
                        "not replayed"
                    },
                    "",
                    "",
                    "",
                    count(tile.calls_recorded),
                )),
            }
        }

        for (id, tile, heaviest) in &rows {
            if tile.calls_recorded == 0 {
                continue;
            }
            out.push(String::new());
            out.extend(tile_details(id, tile, *heaviest));
        }

        if let Some(failure) = &self.failure {
            out.push(String::new());
            out.push(format!(
                "FAILED: {} at {:?} (exec {}): {}",
                failure.tile, failure.coordinates.0, failure.exec_index, failure.message
            ));
        }
        out.join("\n")
    }

    /// Tiles in display order: replayed tiles by their heaviest call's cycles,
    /// most expensive first; then tiles with no replayed call, by name.
    fn rows(&self) -> Vec<(&str, &TileReplay, Option<&ReplayedCall>)> {
        let mut rows: Vec<_> = self
            .tiles
            .iter()
            .map(|(id, tile)| (id.as_str(), tile, tile.heaviest()))
            .collect();
        rows.sort_by(|left, right| {
            let key = |row: &(&str, &TileReplay, Option<&ReplayedCall>)| {
                std::cmp::Reverse(row.2.map(|call| call.user_cycles))
            };
            key(left).cmp(&key(right)).then(left.0.cmp(right.0))
        });
        rows
    }
}

impl TileReplay {
    /// Count one recorded call of `in_out_bytes` input + output bytes.
    pub fn record_size(&mut self, in_out_bytes: u64) {
        self.calls_recorded += 1;
        let up_to_bytes = size_bucket(in_out_bytes);
        match self
            .size_histogram
            .binary_search_by_key(&up_to_bytes, |bucket| bucket.up_to_bytes)
        {
            Ok(index) => self.size_histogram[index].calls += 1,
            Err(index) => self.size_histogram.insert(
                index,
                SizeBucket {
                    up_to_bytes,
                    calls: 1,
                },
            ),
        }
    }

    /// The replayed call with the most user cycles; the earliest on a tie.
    pub fn heaviest(&self) -> Option<&ReplayedCall> {
        self.replayed.iter().reduce(|best, call| {
            if call.user_cycles > best.user_cycles {
                call
            } else {
                best
            }
        })
    }

    /// 1 for the replayed call with the most user cycles, counting ties as
    /// one rank below every strictly heavier call.
    fn cycle_rank(&self, call: &ReplayedCall) -> usize {
        1 + self
            .replayed
            .iter()
            .filter(|other| other.user_cycles > call.user_cycles)
            .count()
    }
}

/// The power-of-two bucket a call of `bytes` input + output bytes falls in.
pub fn size_bucket(bytes: u64) -> u64 {
    if bytes == 0 {
        0
    } else {
        bytes.next_power_of_two()
    }
}

fn tile_details(id: &str, tile: &TileReplay, heaviest: Option<&ReplayedCall>) -> Vec<String> {
    let mut lines = vec![format!(
        "{id} — {} {}, {} replayed{}",
        count(tile.calls_recorded),
        if tile.calls_recorded == 1 {
            "call"
        } else {
            "calls"
        },
        tile.replayed.len(),
        image_note(tile)
    )];

    let widest = tile
        .size_histogram
        .iter()
        .map(|bucket| bucket.calls)
        .max()
        .unwrap_or(0);
    for (index, bucket) in tile.size_histogram.iter().enumerate() {
        lines.push(format!(
            "  {:<13} {:<10} {:<HISTOGRAM_WIDTH$} {:>6}",
            if index == 0 { "in+out bytes" } else { "" },
            bucket_label(bucket.up_to_bytes),
            bar(bucket.calls, widest, HISTOGRAM_WIDTH),
            count(bucket.calls),
        ));
    }

    if !tile.replayed.is_empty() {
        let mut stats = |label: &str, value: fn(&ReplayedCall) -> u64, show: fn(u64) -> String| {
            let mut values: Vec<u64> = tile.replayed.iter().map(value).collect();
            values.sort_unstable();
            lines.push(format!(
                "  {label:<13} max {:<9} p50 {:<9} total {}",
                show(*values.last().unwrap()),
                show(values[(values.len() - 1) / 2]),
                show(values.iter().sum()),
            ));
        };
        stats("user cycles", |call| call.user_cycles, cycles);
        stats("segments", |call| call.segments, count);
        stats("padded", |call| call.padded_cycles, cycles);
    }
    if let Some(call) = heaviest {
        lines.push(format!(
            "  {:<13} {:?} (exec {}), {} in+out, byte rank {}, cycle rank {}",
            "heaviest",
            call.coordinates.0,
            call.exec_index,
            bytes(call.input_bytes + call.output_bytes),
            call.byte_rank,
            tile.cycle_rank(call),
        ));
    }
    lines
}

fn image_note(tile: &TileReplay) -> String {
    let Some(image) = &tile.image_id else {
        return String::new();
    };
    let short = |id: &str| id.chars().take(12).collect::<String>();
    match &tile.lock_image_id {
        None => format!(", image {}…", short(image)),
        Some(lock) if lock == image => format!(", image {}… (matches Raster.lock)", short(image)),
        Some(lock) => format!(
            ", image {}… (Raster.lock has {}…)",
            short(image),
            short(lock)
        ),
    }
}

fn display_name(id: &str) -> String {
    if id.chars().count() <= MAX_NAME_WIDTH {
        id.to_string()
    } else {
        let mut cut: String = id.chars().take(MAX_NAME_WIDTH - 1).collect();
        cut.push('…');
        cut
    }
}

/// `value / top` as a run of block characters `width` cells long at most,
/// in eighths of a cell.
fn bar(value: u64, top: u64, width: usize) -> String {
    const PARTIAL: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    if top == 0 {
        return String::new();
    }
    let eighths = (value as u128 * width as u128 * 8 / top as u128) as usize;
    let mut out = "█".repeat(eighths / 8);
    out.push_str(PARTIAL[eighths % 8]);
    out
}

fn percent(value: u64, top: u64) -> u64 {
    if top == 0 {
        0
    } else {
        ((value as u128 * 100 + top as u128 / 2) / top as u128) as u64
    }
}

fn cycles(value: u64) -> String {
    if value >= 1_000_000_000 {
        format!("{:.2}G", value as f64 / 1e9)
    } else if value >= 1_000_000 {
        format!("{:.2}M", value as f64 / 1e6)
    } else if value >= 1_000 {
        format!("{}k", (value + 500) / 1_000)
    } else {
        value.to_string()
    }
}

fn bytes(value: u64) -> String {
    if value >= 1 << 20 {
        format!("{:.1} MiB", value as f64 / (1u64 << 20) as f64)
    } else if value >= 1 << 10 {
        format!("{:.1} KiB", value as f64 / (1u64 << 10) as f64)
    } else {
        format!("{value} B")
    }
}

fn bucket_label(up_to_bytes: u64) -> String {
    if up_to_bytes == 0 {
        return "0 B".into();
    }
    let unit = |shift: u32, suffix: &str| format!("≤ {} {suffix}", up_to_bytes >> shift);
    if up_to_bytes >= 1 << 20 {
        unit(20, "MiB")
    } else if up_to_bytes >= 1 << 10 {
        unit(10, "KiB")
    } else {
        format!("≤ {up_to_bytes} B")
    }
}

fn duration(ns: u64) -> String {
    if ns >= 1_000_000_000 {
        format!("{:.2}s", ns as f64 / 1e9)
    } else {
        format!("{}ms", (ns + 500_000) / 1_000_000)
    }
}

fn count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(
        exec_index: u64,
        coords: &[i32],
        in_out: (u64, u64),
        byte_rank: u64,
        user_cycles: u64,
        segments: &[u32],
        exec_ns: u64,
    ) -> ReplayedCall {
        ReplayedCall {
            exec_index,
            coordinates: CfsCoordinates(coords.to_vec()),
            input_bytes: in_out.0,
            output_bytes: in_out.1,
            byte_rank,
            user_cycles,
            segments: segments.len() as u64,
            padded_cycles: segments.iter().map(|po2| 1u64 << po2).sum(),
            exec_ns,
        }
    }

    /// Four rendering cases: a multi-call tile replayed in part, a single-call
    /// tile, a tile the program never called (with a name over the width
    /// cap), and a called tile whose replay never ran.
    fn fixture() -> ReplayProfile {
        let mut profile = ReplayProfile::new(
            "run-42",
            [
                "mac_weight_page".to_string(),
                "zero_accum".to_string(),
                "assert_every_layer_is_complete_before_finalizing".to_string(),
                "finish_layer".to_string(),
            ],
            10,
        );
        let mac = profile.tiles.get_mut("mac_weight_page").unwrap();
        for bytes in [3_000, 3_000, 97_000, 97_000, 98_000] {
            mac.record_size(bytes);
        }
        mac.image_id = Some("1a2b3c4d5e6f7a8b".into());
        mac.lock_image_id = Some("1a2b3c4d5e6f7a8b".into());
        mac.replayed = vec![
            call(
                10,
                &[15, 7, 1],
                (97_500, 500),
                1,
                6_705_160,
                &[20, 20, 20, 20, 20, 20, 19, 18],
                127_400_000,
            ),
            call(
                11,
                &[15, 7, 2],
                (96_500, 500),
                2,
                3_300_178,
                &[20, 20, 20, 19],
                74_500_000,
            ),
            call(3, &[15, 1, 1], (2_900, 100), 4, 185_111, &[18], 18_000_000),
        ];
        let zero = profile.tiles.get_mut("zero_accum").unwrap();
        zero.record_size(64);
        zero.image_id = Some("ffeeddccbbaa9988".into());
        zero.lock_image_id = Some("0011223344556677".into());
        zero.replayed = vec![call(2, &[3], (60, 4), 1, 57_776, &[17], 17_200_000)];
        let finish = profile.tiles.get_mut("finish_layer").unwrap();
        finish.record_size(5_000);
        finish.record_size(0);
        profile.failure = Some(ReplayFailure {
            tile: "finish_layer".into(),
            exec_index: 40,
            coordinates: CfsCoordinates(vec![16, 2]),
            message: "Replay output mismatch (recorded 8 bytes, guest 9 bytes)".into(),
        });
        profile
    }

    const FIXTURE_TEXT: &str = "\
Replay profile — RISC Zero executor, no proving
  Run: run-42
  Status: PARTIAL — stopped at the failure below
  Calls: top 10 per tile by input + output bytes; 4 of 8 replayed
  Cycles are guest user cycles, including the tile wrapper (input decode and hash, output encode).

tile                              heaviest call, user cycles (% of max)  seg  padded   exec  calls
mac_weight_page                   ████████████████████████  6.71M  100%    8   7.08M  127ms      5
zero_accum                        ▏                           58k    1%    1    131k   17ms      1
assert_every_layer_is_complete_…  not executed                                                   0
finish_layer                      not replayed                                                   2

mac_weight_page — 5 calls, 3 replayed, image 1a2b3c4d5e6f… (matches Raster.lock)
  in+out bytes  ≤ 4 KiB    █████████████▎            2
                ≤ 128 KiB  ████████████████████      3
  user cycles   max 6.71M     p50 3.30M     total 10.19M
  segments      max 8         p50 4         total 13
  padded        max 7.08M     p50 3.67M     total 11.01M
  heaviest      [15, 7, 1] (exec 10), 95.7 KiB in+out, byte rank 1, cycle rank 1

zero_accum — 1 call, 1 replayed, image ffeeddccbbaa… (Raster.lock has 001122334455…)
  in+out bytes  ≤ 64 B     ████████████████████      1
  user cycles   max 58k       p50 58k       total 58k
  segments      max 1         p50 1         total 1
  padded        max 131k      p50 131k      total 131k
  heaviest      [3] (exec 2), 64 B in+out, byte rank 1, cycle rank 1

finish_layer — 2 calls, 0 replayed
  in+out bytes  0 B        ████████████████████      1
                ≤ 8 KiB    ████████████████████      1

FAILED: finish_layer at [16, 2] (exec 40): Replay output mismatch (recorded 8 bytes, guest 9 bytes)";

    #[test]
    fn renders_the_bar_view_histograms_and_failure() {
        let text = fixture().to_text();
        assert_eq!(text, FIXTURE_TEXT, "\n{text}");
    }

    #[test]
    fn a_complete_profile_says_so_and_has_no_failure_line() {
        let mut profile = fixture();
        profile.failure = None;
        profile.complete = true;
        let text = profile.to_text();
        assert!(text.contains("  Status: complete\n"));
        assert!(!text.contains("FAILED"));
    }

    #[test]
    fn rows_put_the_most_expensive_tile_first_and_unreplayed_tiles_last_by_name() {
        let profile = fixture();
        let order: Vec<&str> = profile.rows().iter().map(|row| row.0).collect();
        assert_eq!(
            order,
            [
                "mac_weight_page",
                "zero_accum",
                "assert_every_layer_is_complete_before_finalizing",
                "finish_layer",
            ]
        );
    }

    #[test]
    fn heaviest_and_cycle_rank_handle_ties() {
        let mut tile = TileReplay::default();
        assert!(tile.heaviest().is_none());
        tile.replayed = vec![
            call(1, &[1], (1, 0), 2, 500, &[16], 0),
            call(2, &[2], (2, 0), 1, 900, &[16], 0),
            call(3, &[3], (1, 0), 3, 900, &[16], 0),
        ];
        // The earliest of two equal maxima.
        assert_eq!(tile.heaviest().unwrap().exec_index, 2);
        assert_eq!(tile.cycle_rank(&tile.replayed[1]), 1);
        assert_eq!(tile.cycle_rank(&tile.replayed[2]), 1);
        assert_eq!(tile.cycle_rank(&tile.replayed[0]), 3);
    }

    #[test]
    fn sizes_fall_in_power_of_two_buckets() {
        for (bytes, bucket) in [(0, 0), (1, 1), (2, 2), (3, 4), (1024, 1024), (1025, 2048)] {
            assert_eq!(size_bucket(bytes), bucket, "{bytes} bytes");
        }
        let mut tile = TileReplay::default();
        for bytes in [3_000, 0, 5_000, 2_500, 0] {
            tile.record_size(bytes);
        }
        assert_eq!(tile.calls_recorded, 5);
        assert_eq!(
            tile.size_histogram,
            [
                SizeBucket {
                    up_to_bytes: 0,
                    calls: 2
                },
                SizeBucket {
                    up_to_bytes: 4096,
                    calls: 2
                },
                SizeBucket {
                    up_to_bytes: 8192,
                    calls: 1
                },
            ]
        );
    }

    #[test]
    fn bars_are_drawn_in_eighths_of_a_cell() {
        assert_eq!(bar(5, 0, 8), "");
        assert_eq!(bar(0, 10, 8), "");
        assert_eq!(bar(10, 10, 3), "███");
        assert_eq!(bar(1, 16, 2), "▏");
        assert_eq!(bar(9, 16, 2), "█▏");
    }

    #[test]
    fn numbers_are_formatted_for_reading() {
        assert_eq!(count(0), "0");
        assert_eq!(count(1_119), "1,119");
        assert_eq!(count(1_000_000), "1,000,000");
        assert_eq!(cycles(999), "999");
        assert_eq!(cycles(30_644), "31k");
        assert_eq!(cycles(6_705_160), "6.71M");
        assert_eq!(cycles(2_500_000_000), "2.50G");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(99_840), "97.5 KiB");
        assert_eq!(duration(16_600_000), "17ms");
        assert_eq!(duration(2_340_000_000), "2.34s");
        assert_eq!(percent(1, 3), 33);
        assert_eq!(percent(2, 3), 67);
    }

    #[test]
    fn json_round_trips_and_rejects_other_reports() {
        let profile = fixture();
        let json = profile.to_json().unwrap();
        assert_eq!(ReplayProfile::from_json(&json).unwrap(), profile);

        let mut value: serde_json::Value = serde_json::from_slice(&json).unwrap();
        value["version"] = 3.into();
        let error = ReplayProfile::from_json(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(error.to_string().contains("version 3"), "{error}");

        value["version"] = REPLAY_PROFILE_VERSION.into();
        value["kind"] = "zkvm-profile".into();
        assert!(ReplayProfile::from_json(&serde_json::to_vec(&value).unwrap()).is_err());

        assert!(ReplayProfile::from_json(b"{}").is_err());
    }
}
