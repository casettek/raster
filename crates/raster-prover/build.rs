// Copyright 2024 RISC Zero, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

/// Second pass of the two-pass guest build (`docs/proposals/transition-split.md`
/// §Image-id pinning): the part guests are built by the `raster-prover-guests`
/// build-dependency, and their image ids are handed to the FRAUD WINDOW guest
/// build as environment variables, which it compiles in. `risc0_build` strips
/// only `CARGO*` and `RUSTUP_TOOLCHAIN` from the guest's environment. (A Docker
/// guest build forwards only `DockerOptions`' variables; pass them there too if
/// these builds move to Docker.)
fn main() {
    use raster_prover_guests as guests;
    for (name, id) in [
        ("RASTER_PART_ID_TRACE", guests::TRACE_GUEST_ID),
        ("RASTER_PART_ID_CONTROL_FLOW", guests::CONTROL_FLOW_GUEST_ID),
        ("RASTER_PART_ID_STORAGE", guests::STORAGE_GUEST_ID),
        ("RASTER_PART_ID_TRANSITION", guests::TRANSITION_GUEST_ID),
        ("RASTER_PART_ID_AUTHORIZATION", guests::AUTHORIZATION_GUEST_ID),
    ] {
        let hex: String = id
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .map(|byte| format!("{byte:02x}"))
            .collect();
        std::env::set_var(name, hex);
    }
    risc0_build::embed_methods();
}
