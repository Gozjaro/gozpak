//! `gozpak-cli` (M1): the `gozpak` binary and its command layer.
//!
//! M1 is read-only parity with the shell version:
//!
//! | command        | M1/M2a status                               |
//! |----------------|---------------------------------------------|
//! | `version` (v)  | implemented                                 |
//! | `list` (l)     | implemented (installed DB)                  |
//! | `search` (s)   | implemented (recipes under `GOZPAK_PATH`)   |
//! | `checksum` (c) | implemented (local + cached sources only)   |
//! | `build` / `repo-build` | M2a: stage + build + pack `.gpkg` |
//! | install/get/remove/download/fetch/sync/list-remote/update/upgrade/alternatives | stubbed — "not implemented in M1" |
//! | unknown word   | extension fallback stub (`gozpak-<word>`)   |
//!
//! Environment honored: `GOZPAK_ROOT`, `GOZPAK_PATH`, `GOZPAK_REPOS`
//! (repo URL config is parsed by `gozpak-repo`; M1 commands don't use
//! it yet — that lands with `sync`/`get` in M2).

pub mod args;
pub mod cmd;
pub mod ext;
pub mod log;

use std::env;
use std::path::PathBuf;

use gozpak_core::{paths, InstalledDb};

/// Execution context; the process environment is read exactly once here so
/// the rest of the code stays testable.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// `GOZPAK_ROOT` (default `"/"`).
    pub root: PathBuf,
    /// Recipe search paths from `GOZPAK_PATH` (in order).
    pub recipe_paths: Vec<PathBuf>,
    /// The installed package database.
    pub db: InstalledDb,
    /// True when `-y`/`--yes` was given (shell: `GOZPAK_PROMPT=0`).
    pub yes: bool,
}

impl Ctx {
    /// Build from the process environment.
    pub fn from_env(yes: bool) -> Self {
        let root: PathBuf = env::var("GOZPAK_ROOT")
            .unwrap_or_else(|_| "/".to_string())
            .into();
        let recipe_paths = env::var("GOZPAK_PATH")
            .unwrap_or_default()
            .split(':')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        let db = InstalledDb::new(paths::installed_db_root(&root));
        Self {
            root,
            recipe_paths,
            db,
            yes,
        }
    }
}

/// Dispatch one invocation. `argv` is everything after the program name.
/// Returns the process exit code.
pub fn run(argv: &[String]) -> i32 {
    let parsed = args::parse(argv);
    let ctx = Ctx::from_env(parsed.yes);
    dispatch(&ctx, &parsed)
}

/// Convenience for tests: run with an explicit context (no process env).
pub fn run_with(ctx: &Ctx, argv: &[String]) -> i32 {
    let parsed = args::parse(argv);
    dispatch(ctx, &parsed)
}

fn dispatch(ctx: &Ctx, parsed: &args::Parsed) -> i32 {
    match parsed.action {
        Some(args::Action::Version) => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some(args::Action::List) => cmd::list(ctx, &parsed.names),
        Some(args::Action::Search) => cmd::search(ctx, &parsed.names),
        Some(args::Action::Checksum) => cmd::checksum(ctx, &parsed.names),
        // M2a: build + pack. `repo-build` carries the repo metadata;
        // `-b` (repo-build only) skips the pack step.
        Some(args::Action::Build) => cmd::build(ctx, &parsed.names, false, true),
        Some(args::Action::RepoBuild) => {
            cmd::build(ctx, &parsed.names, true, !parsed.build_only)
        }
        Some(args::Action::Stub { name }) => cmd::stub(name, &parsed.names),
        None => {
            if parsed.action_raw.is_empty() {
                cmd::usage();
                0
            } else {
                ext::fallback(&parsed.action_raw, &parsed.names)
            }
        }
    }
}
