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

fn main() {
    // embed_methods watches its generated methods.rs; also rebuild the guests
    // when shared verification rules change, so the host and guest cannot drift.
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=../raster-core/src");
    println!("cargo:rerun-if-changed=../raster-core/Cargo.toml");
    for guest in ["authorization", "chain_fraud", "transition"] {
        for source in ["src", "Cargo.toml", "Cargo.lock"] {
            println!("cargo:rerun-if-changed=guests/{guest}/{source}");
        }
    }
    risc0_build::embed_methods();
}
