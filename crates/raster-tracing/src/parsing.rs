use raster_core::trace::TraceItem;

pub fn parse(output: std::process::Output) -> Vec<TraceItem> {
    // let stdout = String::from_utf8_lossy(&output.stdout);

    let trace_items: Vec<TraceItem> = Vec::new();

    // for line in stdout.lines() {
    //     let trace_item = line.map(serde_json::from_str).unwrap();
    //     trace_items.push(trace_item);
    // }
    //
    trace_items
}
