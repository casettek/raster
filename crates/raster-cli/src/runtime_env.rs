//! The environment a Raster program is launched with.
//!
//! Every child process the CLI spawns to run a user program goes through this
//! module — `cargo raster run` and each stage of `cargo raster chain run`. It
//! is the only place `raster_runtime`'s env-var constants are written:
//! `raster-runtime` owns the *names* and reads them inside the program, this
//! owns the decision of what to set.
//!
//! Authenticated execution consumes events through a live commitment publisher,
//! a saved trace, or both. Unauthenticated launches clear all inherited event
//! destinations. All child launch configuration is centralized here.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use crate::commands::RunArtifacts;
use crate::TraceFormat;

/// An unauthenticated launch: the run's output directory and nothing else.
///
/// Call [`RuntimeEnv::authenticated`] or [`RuntimeEnv::committing`] for an
/// authenticated run with event processing and optional profiling.
pub(crate) struct RuntimeEnv<'a> {
    output_dir: &'a Path,
}

/// An authenticated launch with a commitment, a saved trace, or both, and
/// optionally a profile. These settings cannot be attached to an
/// unauthenticated launch.
pub(crate) struct AuthenticatedEnv<'a> {
    output_dir: &'a Path,
    trace_path: Option<&'a Path>,
    commitment: Option<(&'a Path, &'a Path, usize)>,
    audit: Option<(&'a Path, &'a Path, &'a Path, &'a str)>,
    trace_format: TraceFormat,
    /// Optional *within* an authenticated run: `cargo raster run` asks for a
    /// profile, chain stages do not.
    profiling: Option<&'a RunArtifacts>,
}

impl<'a> RuntimeEnv<'a> {
    /// Where a program that returns a value writes its output artifact
    /// (`output.bin` / `output.rindex` / `output_manifest.json`). Independent
    /// of the mode: those artifacts are byte-identical either way (§6).
    pub(crate) fn new(output_dir: &'a Path) -> Self {
        Self { output_dir }
    }

    /// Record authenticated events for offline audit/proof processing.
    pub(crate) fn authenticated(
        self,
        trace_path: &'a Path,
        trace_format: TraceFormat,
    ) -> AuthenticatedEnv<'a> {
        AuthenticatedEnv {
            output_dir: self.output_dir,
            trace_path: Some(trace_path),
            commitment: None,
            audit: None,
            trace_format,
            profiling: None,
        }
    }

    pub(crate) fn committing(
        self,
        project: &'a Path,
        pending: &'a Path,
        window: usize,
        trace_path: Option<&'a Path>,
        trace_format: TraceFormat,
    ) -> AuthenticatedEnv<'a> {
        AuthenticatedEnv {
            output_dir: self.output_dir,
            trace_path,
            trace_format,
            commitment: Some((project, pending, window)),
            audit: None,
            profiling: None,
        }
    }

    pub(crate) fn auditing(
        self,
        project: &'a Path,
        disputed: &'a Path,
        pending: &'a Path,
        binding: &'a str,
    ) -> AuthenticatedEnv<'a> {
        AuthenticatedEnv {
            output_dir: self.output_dir,
            trace_path: None,
            commitment: None,
            audit: Some((project, disputed, pending, binding)),
            trace_format: TraceFormat::Binary,
            profiling: None,
        }
    }

    /// Apply to a command about to be spawned.
    pub(crate) fn apply(&self, command: &mut Command) {
        clear_execution_env(command);
        for (name, value) in self.vars() {
            command.env(name, value);
        }
    }

    fn vars(&self) -> Vec<(&'static str, OsString)> {
        base_vars(self.output_dir, false)
    }
}

impl<'a> AuthenticatedEnv<'a> {
    /// Ask for an execution profile and its live stream.
    pub(crate) fn profiling(mut self, artifacts: &'a RunArtifacts) -> Self {
        self.profiling = Some(artifacts);
        self
    }

    /// Apply to a command about to be spawned.
    pub(crate) fn apply(&self, command: &mut Command) {
        clear_execution_env(command);
        for (name, value) in self.vars() {
            command.env(name, value);
        }
    }

    fn vars(&self) -> Vec<(&'static str, OsString)> {
        // This type supports a commitment, a trace, or both; `base_vars`
        // turns that into `RASTER_AUTH=1`.
        let mut vars = base_vars(self.output_dir, true);
        if let Some(path) = self.trace_path {
            vars.push((
                raster_runtime::TRACE_PATH_ENV,
                path.as_os_str().to_os_string(),
            ));
        }
        if let Some((project, disputed, pending, binding)) = self.audit {
            use raster_runtime::tracing::publishers::audit::*;
            vars.extend([
                (AUDIT_PROJECT_ENV, project.as_os_str().to_owned()),
                (AUDIT_PATH_ENV, disputed.as_os_str().to_owned()),
                (AUDIT_OUTPUT_ENV, pending.as_os_str().to_owned()),
                (AUDIT_BINDING_ENV, binding.into()),
            ]);
        }
        if let Some((project, pending, window)) = self.commitment {
            vars.push((
                raster_runtime::tracing::COMMIT_PROJECT_ENV,
                project.as_os_str().to_os_string(),
            ));
            vars.push((
                raster_runtime::tracing::COMMIT_PATH_ENV,
                pending.as_os_str().to_os_string(),
            ));
            vars.push((
                raster_runtime::tracing::COMMIT_WINDOW_ENV,
                window.to_string().into(),
            ));
        }
        vars.push((
            raster_runtime::TRACE_FORMAT_ENV,
            OsString::from(self.trace_format.as_runtime_str()),
        ));

        if let Some(artifacts) = self.profiling {
            vars.push((
                raster_runtime::PROFILE_PATH_ENV,
                artifacts.profile_path.as_os_str().to_os_string(),
            ));
            vars.push((
                raster_runtime::PROFILE_STREAM_PATH_ENV,
                artifacts.profile_stream_path.as_os_str().to_os_string(),
            ));
            vars.push((
                raster_runtime::PROFILE_RUN_ID_ENV,
                OsString::from(&artifacts.run_id),
            ));
        }

        vars
    }
}

/// The two variables every launch sets, whatever the mode.
///
/// `RASTER_AUTH` is written here and nowhere else, always explicitly: a bare
/// `cargo run` on a Raster program defaults to unauthenticated, and the CLI
/// must not inherit that silently. See
/// `docs/proposals/unauthenticated-execution.md` §1.
fn clear_execution_env(command: &mut Command) {
    use raster_runtime::tracing::publishers::audit::*;
    for name in [
        AUDIT_PROJECT_ENV,
        AUDIT_PATH_ENV,
        AUDIT_OUTPUT_ENV,
        AUDIT_BINDING_ENV,
    ] {
        command.env_remove(name);
    }

    for name in [
        raster_runtime::TRACE_PATH_ENV,
        raster_runtime::TRACE_FORMAT_ENV,
        raster_runtime::tracing::COMMIT_PATH_ENV,
        raster_runtime::tracing::COMMIT_PROJECT_ENV,
        raster_runtime::tracing::COMMIT_WINDOW_ENV,
        raster_runtime::PROFILE_PATH_ENV,
        raster_runtime::PROFILE_STREAM_PATH_ENV,
        raster_runtime::PROFILE_RUN_ID_ENV,
    ] {
        command.env_remove(name);
    }
}

fn base_vars(output_dir: &Path, authenticated: bool) -> Vec<(&'static str, OsString)> {
    vec![
        (
            raster_runtime::auth::AUTH_ENV,
            OsString::from(if authenticated { "1" } else { "0" }),
        ),
        (
            raster_runtime::OUTPUT_DIR_ENV,
            output_dir.as_os_str().to_os_string(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    /// Paths only — nothing here spawns a process or touches the run dir.
    fn artifacts() -> RunArtifacts {
        RunArtifacts::new("run-id".to_string(), TraceFormat::Binary)
    }

    fn unauthenticated_vars(env: &RuntimeEnv<'_>) -> HashMap<&'static str, OsString> {
        env.vars().into_iter().collect()
    }

    fn authenticated_vars(env: &AuthenticatedEnv<'_>) -> HashMap<&'static str, OsString> {
        env.vars().into_iter().collect()
    }

    #[test]
    fn live_audit_launch_has_no_trace_or_commitment_destination() {
        use raster_runtime::tracing::publishers::audit::*;
        let artifacts = artifacts();
        let disputed = artifacts.run_dir.join("disputed");
        let pending = artifacts.run_dir.join("pending");
        let env = RuntimeEnv::new(&artifacts.run_dir).auditing(
            &artifacts.run_dir,
            &disputed,
            &pending,
            "attempt",
        );
        let vars = authenticated_vars(&env);
        assert!(vars.contains_key(AUDIT_PATH_ENV));
        assert!(!vars.contains_key(raster_runtime::TRACE_PATH_ENV));
        assert!(!vars.contains_key(raster_runtime::tracing::COMMIT_PATH_ENV));
        let mut command = Command::new("unused");
        RuntimeEnv::new(&artifacts.run_dir).apply(&mut command);
        let vars: HashMap<_, _> = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(|v| v.to_owned())))
            .collect();
        for name in [
            AUDIT_PATH_ENV,
            AUDIT_PROJECT_ENV,
            AUDIT_OUTPUT_ENV,
            AUDIT_BINDING_ENV,
        ] {
            assert_eq!(vars.get(&OsString::from(name)), Some(&None));
        }
    }

    #[test]
    fn unauthenticated_run_sets_no_trace_or_profile_vars() {
        let artifacts = artifacts();
        let vars = unauthenticated_vars(&RuntimeEnv::new(&artifacts.run_dir));

        assert_eq!(
            vars.get(raster_runtime::auth::AUTH_ENV)
                .map(|v| v.as_os_str()),
            Some(OsString::from("0").as_os_str())
        );
        assert_eq!(
            vars.get(raster_runtime::OUTPUT_DIR_ENV)
                .map(|v| v.as_os_str()),
            Some(artifacts.run_dir.as_os_str())
        );
        // §6: no trace publisher is installed, so no empty `trace.bin` is left
        // behind to be mistaken for one that could be audited.
        assert!(!vars.contains_key(raster_runtime::TRACE_PATH_ENV));
        assert!(!vars.contains_key(raster_runtime::TRACE_FORMAT_ENV));
        // The runtime refuses profiling in an unauthenticated run; asking for
        // one here would abort every `--no-auth` run.
        assert!(!vars.contains_key(raster_runtime::PROFILE_PATH_ENV));
        assert!(!vars.contains_key(raster_runtime::PROFILE_STREAM_PATH_ENV));
        assert!(!vars.contains_key(raster_runtime::PROFILE_RUN_ID_ENV));
    }

    #[test]
    fn authenticated_run_sets_the_trace_destination() {
        let artifacts = artifacts();
        let env = RuntimeEnv::new(&artifacts.run_dir)
            .authenticated(&artifacts.trace_path, TraceFormat::Json);
        let vars = authenticated_vars(&env);

        assert_eq!(
            vars.get(raster_runtime::auth::AUTH_ENV)
                .map(|v| v.as_os_str()),
            Some(OsString::from("1").as_os_str())
        );
        assert_eq!(
            vars.get(raster_runtime::TRACE_PATH_ENV)
                .map(|v| v.as_os_str()),
            Some(artifacts.trace_path.as_os_str())
        );
        assert_eq!(
            vars.get(raster_runtime::TRACE_FORMAT_ENV)
                .map(|v| v.as_os_str()),
            Some(OsString::from(TraceFormat::Json.as_runtime_str()).as_os_str())
        );
    }

    #[test]
    fn profiling_is_opt_in_within_an_authenticated_run() {
        let artifacts = artifacts();
        let without = RuntimeEnv::new(&artifacts.run_dir)
            .authenticated(&artifacts.trace_path, TraceFormat::Binary);
        assert!(!authenticated_vars(&without).contains_key(raster_runtime::PROFILE_PATH_ENV));

        let with = RuntimeEnv::new(&artifacts.run_dir)
            .authenticated(&artifacts.trace_path, TraceFormat::Binary)
            .profiling(&artifacts);
        let vars = authenticated_vars(&with);

        assert_eq!(
            vars.get(raster_runtime::PROFILE_PATH_ENV)
                .map(|v| v.as_os_str()),
            Some(artifacts.profile_path.as_os_str())
        );
        assert_eq!(
            vars.get(raster_runtime::PROFILE_STREAM_PATH_ENV)
                .map(|v| v.as_os_str()),
            Some(artifacts.profile_stream_path.as_os_str())
        );
        assert_eq!(
            vars.get(raster_runtime::PROFILE_RUN_ID_ENV)
                .map(|v| v.as_os_str()),
            Some(OsString::from(&artifacts.run_id).as_os_str())
        );
    }
}
