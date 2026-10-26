//! Host-specific settings for Cargo builds of native executables.

use std::{ffi::OsString, process::Command};

pub fn configure(command: &mut Command) {
    let target = if cfg!(all(windows, target_env = "msvc")) {
        match std::env::consts::ARCH {
            "x86_64" => Some("x86_64-pc-windows-msvc"),
            "aarch64" => Some("aarch64-pc-windows-msvc"),
            "x86" => Some("i686-pc-windows-msvc"),
            _ => None,
        }
    } else {
        None
    };
    configure_for_target(command, target);
}

fn inherited_env(command: &Command, name: &str) -> Option<OsString> {
    match command.get_envs().find(|(key, _)| *key == name) {
        Some((_, value)) => value.map(OsString::from),
        None => std::env::var_os(name),
    }
}

fn configure_for_target(command: &mut Command, msvc_target: Option<&str>) {
    let Some(target) = msvc_target else { return };
    // cc-rs prefers the hyphenated target key over its underscored alias.
    // Preserve whichever the caller uses, plus the inherited global CXXFLAGS.
    let hyphenated = format!("CXXFLAGS_{target}");
    let underscored = format!("CXXFLAGS_{}", target.replace('-', "_"));
    let (key, mut flags) = match inherited_env(command, &hyphenated) {
        Some(flags) => (hyphenated, flags),
        None => {
            let flags = inherited_env(command, &underscored).unwrap_or_default();
            (underscored, flags)
        }
    };
    // RISC Zero's generated C++ uses designated initializers and std::min/max.
    // Append these so they override the dependency's C++17 default.
    flags.push(" /DNOMINMAX /std:c++20 /EHsc");
    command.env(key, flags);
    // Native replay needs host libraries, not RISC-V proof guests. Respect an
    // explicit override; never change proof-build behavior on macOS/Linux.
    if inherited_env(command, "RISC0_SKIP_BUILD").is_none() {
        command.env("RISC0_SKIP_BUILD", "1");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_preserves_custom_flags_and_adds_required_defaults() {
        let mut command = Command::new("cargo");
        command.env_remove("CXXFLAGS_x86_64-pc-windows-msvc");
        command.env("CXXFLAGS_x86_64_pc_windows_msvc", "/DUSER_FLAG /std:c++17");
        command.env_remove("RISC0_SKIP_BUILD");
        configure_for_target(&mut command, Some("x86_64-pc-windows-msvc"));
        assert_eq!(
            inherited_env(&command, "CXXFLAGS_x86_64_pc_windows_msvc").unwrap(),
            "/DUSER_FLAG /std:c++17 /DNOMINMAX /std:c++20 /EHsc"
        );
        assert_eq!(inherited_env(&command, "RISC0_SKIP_BUILD").unwrap(), "1");
    }

    #[test]
    fn windows_honors_cc_rs_key_precedence_and_guest_override() {
        let mut command = Command::new("cargo");
        command.env("CXXFLAGS_x86_64-pc-windows-msvc", "/DHYPHENATED");
        command.env("CXXFLAGS_x86_64_pc_windows_msvc", "/DUNDERSCORED");
        command.env("RISC0_SKIP_BUILD", "0");
        configure_for_target(&mut command, Some("x86_64-pc-windows-msvc"));
        assert_eq!(
            inherited_env(&command, "CXXFLAGS_x86_64-pc-windows-msvc").unwrap(),
            "/DHYPHENATED /DNOMINMAX /std:c++20 /EHsc"
        );
        assert_eq!(inherited_env(&command, "RISC0_SKIP_BUILD").unwrap(), "0");
    }

    #[test]
    fn non_msvc_hosts_receive_no_environment_overrides() {
        let mut command = Command::new("cargo");
        command.env("CXXFLAGS", "-O2");
        configure_for_target(&mut command, None);
        assert_eq!(command.get_envs().count(), 1);
        assert_eq!(inherited_env(&command, "CXXFLAGS").unwrap(), "-O2");
    }
}
