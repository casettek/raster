//! Common run interface for native timings and selected-tile replay costs.

use clap::{Args, ValueEnum};
use raster_core::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ProfileMode {
    /// Time all tile invocations during native execution
    Native,
    /// Replay up to 128 calls per tile and measure one transition's overhead, without proving
    #[value(alias = "zkvm")]
    Replay,
}

#[derive(Args, Debug, Default)]
pub struct ProfileOptions {
    /// Profiling type; native enables Raster's Cargo profiling feature
    #[arg(
        long,
        value_enum,
        requires_if("replay", "profile_tiles"),
        requires_if("zkvm", "profile_tiles"),
        conflicts_with = "no_auth"
    )]
    pub profile: Option<ProfileMode>,

    /// Tiles to profile with --profile replay (comma-separated or repeated)
    #[arg(long = "tile", value_name = "TILE", value_delimiter = ',',
        action = clap::ArgAction::Append, requires = "profile")]
    pub profile_tiles: Vec<String>,

    /// Compatibility spelling of --profile replay --tile TILE
    #[arg(long = "zkvm-profile", value_name = "TILE", value_delimiter = ',',
        action = clap::ArgAction::Append, hide = true,
        conflicts_with_all = ["profile", "profile_tiles", "no_auth", "audit"])]
    pub legacy_profile: Vec<String>,
}

impl ProfileOptions {
    /// Validate before project discovery/build, including value-dependent modes.
    pub fn validate(&self, audit: bool) -> Result<()> {
        if !self.profile_tiles.is_empty() && self.profile != Some(ProfileMode::Replay) {
            return Err(Error::Other("--tile requires --profile replay".into()));
        }
        if audit && !self.selected_tiles().is_empty() {
            return Err(Error::Other(
                "--profile replay cannot be used with --audit".into(),
            ));
        }
        Ok(())
    }

    pub fn selected_tiles(&self) -> &[String] {
        if self.profile == Some(ProfileMode::Replay) {
            &self.profile_tiles
        } else {
            &self.legacy_profile
        }
    }

    pub fn native(&self) -> bool {
        self.profile == Some(ProfileMode::Native)
    }

    pub fn build_features(&self, requested: &[String]) -> Vec<String> {
        let mut features = requested.to_vec();
        if self.native()
            && !requested.iter().any(|feature| {
                feature
                    .split(|ch: char| ch == ',' || ch.is_whitespace())
                    .any(|part| part == "raster/profiling")
            })
        {
            // Enable the dependency feature directly, so consumer projects do
            // not have to define a forwarding feature named `profiling`.
            features.push("raster/profiling".into());
        }
        features
    }
}
