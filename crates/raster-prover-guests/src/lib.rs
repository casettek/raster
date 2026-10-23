//! The fraud-window part guests' ELFs and image ids: the first pass of a
//! two-pass guest build. `raster-prover` re-exports everything here and builds
//! the FRAUD WINDOW aggregator in a second pass, with these ids compiled in
//! (`docs/proposals/transition-split.md` §Image-id pinning).

include!(concat!(env!("OUT_DIR"), "/methods.rs"));
