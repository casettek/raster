// A call macro is a *step*: static discovery gives it a control-flow-schema
// item and the runtime gives it a coordinate. Nested in another call's
// arguments it gets no item — discovery captures arguments as text and does not
// recurse — while the expansion still runs it and it still claims a coordinate.
// The mismatch only surfaces under `--commit`/`--audit`, as a coordinate error
// naming neither the call nor its file, so it is rejected here instead.
use raster::prelude::*;

#[tile(kind = iter)]
fn shout(line: String) -> String {
    line.to_uppercase()
}

#[tile(kind = iter)]
fn exclaim(line: String) -> String {
    format!("{}!", line)
}

#[sequence]
fn nested_in_call(line: String) -> String {
    // Should be hoisted: `let shouted = call!(shout, line);`
    call!(exclaim, call!(shout, line))
}

fn main() {}
