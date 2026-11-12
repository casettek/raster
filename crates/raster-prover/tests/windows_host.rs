//! Verify Windows host backend selection without generating a proof.
#![cfg(windows)]

#[test]
fn windows_prover_selection() {
    const MODE: &str = "RASTER_WINDOWS_PROVER_TEST";
    match std::env::var(MODE).as_deref() {
        Ok("local" | "default") => {
            assert_eq!(risc0_zkvm::default_prover().get_name(), "local");
        }
        Ok("actor") => {
            let error = std::panic::catch_unwind(risc0_zkvm::default_prover)
                .err()
                .expect("Windows actor selection must fail explicitly");
            let message = error
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| error.downcast_ref::<String>().map(String::as_str))
                .unwrap_or_default();
            assert!(message.contains("requires Unix sockets"), "{message}");
        }
        _ => {
            // Each selection gets its own environment; don't mutate the test
            // process's global environment or rely on the user's prover setting.
            for mode in ["local", "default", "actor"] {
                let mut child = std::process::Command::new(std::env::current_exe().unwrap());
                child.args(["--exact", "windows_prover_selection"]);
                child.env(MODE, mode);
                child.env_remove("BONSAI_API_URL");
                child.env_remove("BONSAI_API_KEY");
                if mode == "default" {
                    child.env_remove("RISC0_PROVER");
                } else {
                    child.env("RISC0_PROVER", mode);
                }
                assert!(child.status().unwrap().success(), "selection: {mode}");
            }
        }
    }
}
