//! Spawned-process helpers shared by the source/builder/pack stages.
//!
//! Every external command is run with inherited stdio (live output, like
//! the shell version) and a checked exit status; a failed command turns
//! into [`BuildError::CommandFailed`] / [`BuildError::Spawn`] carrying
//! the full command line so the log can say exactly what broke.

use std::process::{Command, Stdio};

use crate::BuildError;

/// Run `program args...` in `dir` with inherited stdio, extra `env`
/// variables, and a checked exit status.
pub(crate) fn run(
    pkg: &str,
    dir: &std::path::Path,
    program: &str,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<(), BuildError> {
    let display = format!(
        "{program} {}",
        args.iter().map(|s| shorten(s)).collect::<Vec<_>>().join(" ")
    );
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    match cmd.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(BuildError::CommandFailed {
            pkg: pkg.to_string(),
            command: display,
            status,
        }),
        Err(e) => Err(BuildError::Spawn {
            pkg: pkg.to_string(),
            command: display,
            source: e,
        }),
    }
}

/// Run with stdin from an explicit source (patch application).
pub(crate) fn run_with_stdin(
    pkg: &str,
    dir: &std::path::Path,
    program: &str,
    args: &[&str],
    stdin: Stdio,
) -> Result<(), BuildError> {
    let display = format!(
        "{program} {}",
        args.iter().map(|s| shorten(s)).collect::<Vec<_>>().join(" ")
    );
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(dir).stdin(stdin);
    match cmd.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(BuildError::CommandFailed {
            pkg: pkg.to_string(),
            command: display,
            status,
        }),
        Err(e) => Err(BuildError::Spawn {
            pkg: pkg.to_string(),
            command: display,
            source: e,
        }),
    }
}

/// Quote arguments containing whitespace for log display.
fn shorten(s: &str) -> String {
    if s.contains(' ') {
        format!("'{s}'")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_and_failure() {
        let dir = std::env::temp_dir();
        assert!(run("t", &dir, "sh", &["-c", "true"], &[]).is_ok());
        match run("t", &dir, "sh", &["-c", "false"], &[]).unwrap_err() {
            BuildError::CommandFailed { pkg, command, .. } => {
                assert_eq!(pkg, "t");
                assert!(command.starts_with("sh -c "));
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }

    #[test]
    fn env_is_visible_to_child() {
        let dir = std::env::temp_dir();
        assert!(run(
            "t",
            &dir,
            "sh",
            &["-c", "test -n \"$GOZPAK_TEST_ENV\""],
            &[("GOZPAK_TEST_ENV", "1")],
        )
        .is_ok());
    }

    #[test]
    fn missing_program_is_spawn_error() {
        match run("t", &std::env::temp_dir(), "gozpak-nonexistent-tool-xyz", &[], &[]) {
            Err(BuildError::Spawn { command, .. }) => assert!(command.starts_with("gozpak-")),
            other => panic!("expected Spawn, got {other:?}"),
        }
    }
}
