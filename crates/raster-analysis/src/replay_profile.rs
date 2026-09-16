//! Compact, executor-only profiles of explicitly selected tiles.

use std::collections::BTreeMap;

use raster_core::{cfs::CfsCoordinates, Error, Result};
use serde::{Deserialize, Serialize};

pub const REPLAY_PROFILE_KIND: &str = "replay-profile";
pub const REPLAY_PROFILE_VERSION: u32 = 3;
/// Fixed profiling budget per selected tile ID, across all call sites.
pub const REPLAY_INVOCATION_LIMIT: u64 = 128;
pub const LEGACY_ZKVM_PROFILE_KIND: &str = "zkvm-profile";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvocationLocation {
    pub exec_index: u64,
    pub coordinates: CfsCoordinates,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplayTileProfile {
    /// All recorded invocations, including those beyond the profiling limit.
    pub invocations: u64,
    pub profiled_invocations: u64,
    pub image_id: Option<String>,
    pub total_guest_cycles: u64,
    pub max_guest_cycles: Option<u64>,
    pub heaviest_invocation: Option<InvocationLocation>,
    #[serde(default)]
    pub transition_overhead: Option<TransitionOverhead>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionOverhead {
    pub guest_cycles: u64,
    pub invocation: InvocationLocation,
    pub transition_image_id: String,
    /// This describes modeled continuation state, not a proven fault window.
    pub context: String,
    pub context_version: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileFailurePhase {
    #[default]
    Replay,
    Authorization,
    TransitionOverhead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayProfileFailure {
    #[serde(default)]
    pub phase: ProfileFailurePhase,
    pub tile: String,
    pub invocation: InvocationLocation,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayProfile {
    pub kind: String,
    pub version: u32,
    pub run_id: String,
    /// Whether the capped replays and (in v3) each executed tile's overhead
    /// measurement finished successfully.
    pub complete: bool,
    /// None for version 1 reports, which replayed every recorded invocation.
    #[serde(default)]
    pub invocation_limit: Option<u64>,
    pub tiles: BTreeMap<String, ReplayTileProfile>,
    pub total_guest_cycles: u64,
    pub failure: Option<ReplayProfileFailure>,
}

impl ReplayProfile {
    pub fn new(run_id: String, tiles: impl IntoIterator<Item = String>) -> Self {
        Self {
            kind: REPLAY_PROFILE_KIND.into(),
            version: REPLAY_PROFILE_VERSION,
            run_id,
            complete: false,
            invocation_limit: Some(REPLAY_INVOCATION_LIMIT),
            tiles: tiles
                .into_iter()
                .map(|tile| (tile, ReplayTileProfile::default()))
                .collect(),
            total_guest_cycles: 0,
            failure: None,
        }
    }

    pub fn validate_format(&self) -> Result<()> {
        let supported = match (self.kind.as_str(), self.version) {
            (REPLAY_PROFILE_KIND | LEGACY_ZKVM_PROFILE_KIND, 1) => self.invocation_limit.is_none(),
            (REPLAY_PROFILE_KIND, 2 | REPLAY_PROFILE_VERSION) => {
                self.invocation_limit == Some(REPLAY_INVOCATION_LIMIT)
            }
            _ => false,
        };
        if !supported {
            return Err(Error::Other(format!(
                "Unsupported replay profile format: kind '{}', version {}",
                self.kind, self.version,
            )));
        }
        Ok(())
    }

    /// Retain only totals and the first heaviest invocation on a tie.
    pub fn record(
        &mut self,
        tile: &str,
        invocation: InvocationLocation,
        cycles: u64,
    ) -> Result<()> {
        let stats = self
            .tiles
            .get_mut(tile)
            .ok_or_else(|| Error::InvalidTileId(tile.into()))?;
        let total = self
            .total_guest_cycles
            .checked_add(cycles)
            .ok_or_else(|| Error::Other("Guest cycle total overflow".into()))?;
        let tile_total = stats
            .total_guest_cycles
            .checked_add(cycles)
            .ok_or_else(|| Error::Other("Tile cycle total overflow".into()))?;
        stats.profiled_invocations += 1;
        stats.total_guest_cycles = tile_total;
        if stats
            .max_guest_cycles
            .is_none_or(|maximum| cycles > maximum)
        {
            stats.max_guest_cycles = Some(cycles);
            stats.heaviest_invocation = Some(invocation);
        }
        self.total_guest_cycles = total;
        Ok(())
    }

    pub fn to_text(&self) -> String {
        let state = if self.complete { "complete" } else { "PARTIAL" };
        let mut lines = vec![
            "Profile Summary".into(),
            "  Type: replay (RISC Zero, execution only, no proving)".into(),
            format!("  Run: {}", self.run_id),
            format!("  Status: {state}"),
            "  Guest cycles include tile wrapper work; totals and maxima cover profiled invocations only.".into(),
        ];
        if let Some(limit) = self.invocation_limit {
            lines.push(format!(
                "  Limit: first {limit} invocations per tile ID, across all call sites."
            ));
        }
        lines.push("  Calls: successfully profiled / recorded invocations.".into());
        if self.version >= 3 {
            lines.push("  Transition overhead: one representative continuation step; separate from replay totals, excludes window setup and proving.".into());
        }
        if !self.complete {
            lines.push("  Partial totals include only successfully profiled invocations.".into());
        }
        let mut tiles: Vec<_> = self.tiles.iter().collect();
        tiles.sort_by(|(left_id, left), (right_id, right)| {
            right
                .max_guest_cycles
                .cmp(&left.max_guest_cycles)
                .then_with(|| left_id.cmp(right_id))
        });
        lines.push("  Hot Tiles (by maximum measured guest cycles)".into());
        for (tile, stats) in tiles {
            let location = stats
                .heaviest_invocation
                .as_ref()
                .map(|location| format!("{:?}", location.coordinates.0))
                .unwrap_or_else(|| {
                    if stats.invocations == 0 {
                        "not executed"
                    } else {
                        "not profiled"
                    }
                    .into()
                });
            let maximum = stats
                .max_guest_cycles
                .map(|cycles| cycles.to_string())
                .unwrap_or_else(|| "—".into());
            let total = if stats.profiled_invocations == 0 {
                "—".into()
            } else {
                stats.total_guest_cycles.to_string()
            };
            let capped = self
                .invocation_limit
                .filter(|limit| stats.invocations > *limit)
                .map(|limit| format!(", capped ({} beyond limit)", stats.invocations - limit))
                .unwrap_or_default();
            lines.push(format!(
                "    {tile}: max {maximum}, total {total} guest cycles, calls {}, heaviest {location}{capped}",
                format!("{}/{}", stats.profiled_invocations, stats.invocations),
            ));
            if stats.invocations > 0 {
                lines.push(match &stats.transition_overhead {
                    Some(overhead) => format!(
                        "      Sample transition overhead: {} guest cycles at {:?} (representative)",
                        overhead.guest_cycles, overhead.invocation.coordinates.0,
                    ),
                    None => "      Sample transition overhead: not measured".into(),
                });
            }
        }
        lines.push(format!(
            "  {}profiled-invocation guest cycles: {}",
            if self.complete { "Total " } else { "Partial " },
            self.total_guest_cycles,
        ));
        if let Some(failure) = &self.failure {
            lines.push(format!(
                "  FAILED ({:?}): {} at {:?} (execution {}): {}",
                failure.phase,
                failure.tile,
                failure.invocation.coordinates.0,
                failure.invocation.exec_index,
                failure.message,
            ));
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(index: u32) -> InvocationLocation {
        InvocationLocation {
            exec_index: index as u64,
            coordinates: CfsCoordinates(vec![index]),
        }
    }

    #[test]
    fn overhead_is_separate_and_old_reports_do_not_invent_it() {
        let mut profile = ReplayProfile::new("new".into(), ["tile".into()]);
        profile.tiles.get_mut("tile").unwrap().invocations = 2;
        profile.record("tile", location(0), 40).unwrap();
        profile.record("tile", location(1), 50).unwrap();
        profile.tiles.get_mut("tile").unwrap().transition_overhead = Some(TransitionOverhead {
            guest_cycles: 1000,
            invocation: location(0),
            transition_image_id: "image".into(),
            context: "representative-continuation".into(),
            context_version: 1,
        });
        profile.complete = true;
        let restored: ReplayProfile =
            serde_json::from_slice(&serde_json::to_vec(&profile).unwrap()).unwrap();
        restored.validate_format().unwrap();
        assert_eq!(restored.total_guest_cycles, 90);
        assert_eq!(restored.tiles["tile"].max_guest_cycles, Some(50));
        assert!(restored
            .to_text()
            .contains("Sample transition overhead: 1000 guest cycles at [0]"));
        let mut old = serde_json::to_value(profile).unwrap();
        old["version"] = 2.into();
        old["tiles"]["tile"]
            .as_object_mut()
            .unwrap()
            .remove("transition_overhead");
        let old: ReplayProfile = serde_json::from_value(old).unwrap();
        old.validate_format().unwrap();
        assert!(old.tiles["tile"].transition_overhead.is_none());
        assert!(old
            .to_text()
            .contains("Sample transition overhead: not measured"));
    }

    #[test]
    fn aggregates_and_renders_heaviest_first_with_deterministic_ties() {
        let mut profile = ReplayProfile::new("run-1".into(), ["b", "a", "cold"].map(String::from));
        profile.tiles.get_mut("a").unwrap().invocations = 2;
        profile.tiles.get_mut("b").unwrap().invocations = 1;
        profile.record("b", location(0), 50).unwrap();
        profile.record("a", location(1), 50).unwrap();
        profile.record("a", location(2), 20).unwrap();
        profile.complete = true;
        assert_eq!(profile.total_guest_cycles, 120);
        assert_eq!(profile.tiles["a"].total_guest_cycles, 70);
        assert_eq!(profile.tiles["a"].heaviest_invocation, Some(location(1)));
        let text = profile.to_text();
        assert!(text.find("    a:").unwrap() < text.find("    b:").unwrap());
        assert!(text.contains("not executed"));
        assert!(text.contains("Total profiled-invocation guest cycles: 120"));
        let restored: ReplayProfile =
            serde_json::from_str(&serde_json::to_string(&profile).unwrap()).unwrap();
        restored.validate_format().unwrap();
        assert_eq!(restored.to_text(), text);
    }

    #[test]
    fn zero_cycles_are_measured_and_partial_totals_are_explicit() {
        let mut profile = ReplayProfile::new("run-2".into(), ["tile".into()]);
        profile.tiles.get_mut("tile").unwrap().invocations = 3;
        profile.record("tile", location(0), 0).unwrap();
        profile.failure = Some(ReplayProfileFailure {
            phase: ProfileFailurePhase::Replay,
            tile: "tile".into(),
            invocation: location(1),
            message: "guest aborted".into(),
        });
        assert_eq!(profile.tiles["tile"].max_guest_cycles, Some(0));
        let text = profile.to_text();
        assert!(text.contains("PARTIAL"));
        assert!(text.contains("1/3"));
        assert!(text.contains("guest aborted"));
        assert!(!text.contains("not executed"));
        profile.version += 1;
        assert!(profile.validate_format().is_err());
    }

    #[test]
    fn completed_capped_report_labels_measured_scope_and_round_trips_limit() {
        let mut profile = ReplayProfile::new("capped-run".into(), ["tile".into()]);
        profile.tiles.get_mut("tile").unwrap().invocations = 1000;
        for index in 0..128 {
            profile.record("tile", location(index), 10).unwrap();
        }
        profile.complete = true;
        let text = profile.to_text();
        assert!(text.contains("Status: complete"));
        assert!(!text.contains("PARTIAL"));
        assert!(text.contains("Limit: first 128 invocations per tile ID"));
        assert!(text.contains("calls 128/1000"));
        assert!(text.contains("capped (872 beyond limit)"));
        assert!(text.contains("totals and maxima cover profiled invocations only"));
        assert!(text.contains("Total profiled-invocation guest cycles: 1280"));
        let decoded: ReplayProfile =
            serde_json::from_slice(&serde_json::to_vec(&profile).unwrap()).unwrap();
        decoded.validate_format().unwrap();
        assert_eq!(decoded.version, 3);
        assert_eq!(decoded.invocation_limit, Some(128));
        assert_eq!(decoded.to_text(), text);
    }

    #[test]
    fn version_one_reports_remain_uncapped_and_version_two_requires_a_limit() {
        let mut value =
            serde_json::to_value(ReplayProfile::new("old-run".into(), ["tile".into()])).unwrap();
        value["version"] = 1.into();
        value.as_object_mut().unwrap().remove("invocation_limit");
        for kind in [REPLAY_PROFILE_KIND, LEGACY_ZKVM_PROFILE_KIND] {
            value["kind"] = kind.into();
            let old: ReplayProfile = serde_json::from_value(value.clone()).unwrap();
            old.validate_format().unwrap();
            assert_eq!(old.invocation_limit, None);
            assert!(!old.to_text().contains("Limit: first"));
        }
        value["kind"] = REPLAY_PROFILE_KIND.into();
        value["version"] = 2.into();
        let missing_limit: ReplayProfile = serde_json::from_value(value).unwrap();
        assert!(missing_limit.validate_format().is_err());
    }
}
