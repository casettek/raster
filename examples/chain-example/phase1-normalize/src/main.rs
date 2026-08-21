use chain_stage_normalize::*;
use raster::prelude::*;

/// Phase 1 entrypoint.
///
/// Binds two committed externals declared as `main` parameters: the raw
/// `Measurements` column and the cutoff. Its return value is the program's
/// authorized output (`output.bin`), which the chain feeds into phase 2 as its
/// `filtered` parameter.
#[sequence]
fn main(readings: Measurements, threshold: u64) -> Filtered {
    let label = select!(String, readings.clone().label);
    let samples = select!(List<u64>, readings.samples);
    let threshold = select!(u64, threshold);

    let filtered = call_recur!(
        tile = keep_above,
        input = samples,
        output = new!(Filtered),
        args = (label, threshold)
    );

    raster::println!("phase1 normalize → {:?}", filtered);
    filtered
}
