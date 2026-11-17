//! `gozpak-build` (M2a): the build + packaging side of gozpak.
//!
//! Pipeline for one `recipe.toml` (recipe format v1.2):
//!
//! 1. [`source::stage`] — the source stage: fetch `[source].urls` with
//!    `curl -fsSL --retry 2 -o`, verify each sha256 with
//!    [`gozpak_core::sha256`] (never re-implemented here), extract
//!    `tar.xz`/`tar.gz`/`tar.bz2` with the system `tar`, check out
//!    `[source].git` (`git clone --depth 1` + pinned-ref checkout —
//!    unpinned refs are rejected), and apply `[source].patches` in
//!    order via `patch -p1`.
//! 2. [`builder::run`] — the build stage: drive the `[build]` section
//!    (`raw` / `autotools` / `meson` / `cmake` / `make`) with the
//!    `DESTDIR` staging prefix, the subst env table
//!    (VERSION/RELEASE/EPOCH/MAJOR/MINOR/PATCH/IDENT/PACKAGE) and
//!    `JOBS` from [`std::thread::available_parallelism`].
//! 3. [`pack::pack`] — the packaging stage: serialize `manifest.toml`
//!    (`[package]` + `[install]` + `[manifest]`), tar the DESTDIR tree
//!    as `rootfs/`, and compress the container to a `.gpkg` (tar.zst)
//!    with the `zstd` crate. The filename always comes from
//!    [`gozpak_repo::gpkg_filename`] (builder, never parser).
//!
//! Legacy (kiss 3-file) recipes are rejected here: M3's migration tool
//! converts them to `recipe.toml`; building them verbatim is out of
//! scope for M2a.

pub mod builder;
pub mod manifest;
pub mod pack;
mod proc;
pub mod source;

use std::path::PathBuf;

use gozpak_recipe::Recipe;

pub use manifest::Manifest;
pub use pack::Packed;
pub use source::{stage, Work};

/// Options for one build + pack run (all paths, no process env — the
/// CLI layer resolves env to paths; tests pass explicit temp dirs).
#[derive(Debug, Clone)]
pub struct Opts {
    /// Per-run work root (`<proc>/<pid>` in shell terms); per-package
    /// subdirectories are created under it.
    pub work_root: PathBuf,
    /// Download cache (`<cache>/gozpak/sources`); `curl` targets land
    /// here, and cached files are reused when their sha256 matches.
    pub source_cache: PathBuf,
    /// Where the finished `.gpkg` is written (shell `bin_dir`).
    pub out_dir: PathBuf,
    /// `true` for `repo-build`: the manifest carries the repo metadata
    /// (`[manifest].repo` + `[manifest].built_by`). A plain `build`
    /// omits both fields.
    pub repo_meta: bool,
}

/// The result of a full pipeline run.
#[derive(Debug)]
pub struct Report {
    /// The unpacked source tree the build ran in.
    pub src: PathBuf,
    /// The staging prefix (DESTDIR tree).
    pub dest: PathBuf,
    /// The packed package, when `pack` was requested.
    pub packed: Option<Packed>,
}

/// Errors from the build/pack pipeline.
#[derive(Debug)]
pub enum BuildError {
    /// IO failure (file/directory operations, spawned processes).
    Io(std::io::Error),
    /// The recipe could not be loaded (M1 validator error).
    Recipe(gozpak_recipe::RecipeError),
    /// A local source or patch file does not exist.
    MissingLocal { pkg: String, path: PathBuf },
    /// A file source without a declared sha256 (v1.2: required).
    MissingSha256 { pkg: String, url: String },
    /// The downloaded file's sha256 does not match the recipe.
    Sha256Mismatch {
        pkg: String,
        url: String,
        expected: String,
        got: String,
    },
    /// A git source without a pinned ref (never fetchable reproducibly).
    UnpinnedGitRef { pkg: String, repo: String },
    /// The `[build]` section is unusable (unknown system, raw without
    /// steps) — no command was run.
    InvalidBuildSection { pkg: String, message: String },
    /// A spawned command could not even be started (e.g. binary missing).
    Spawn {
        pkg: String,
        command: String,
        source: std::io::Error,
    },
    /// A spawned command exited non-zero.
    CommandFailed {
        pkg: String,
        command: String,
        status: std::process::ExitStatus,
    },
}

impl BuildError {
    /// The package the error belongs to (for log prefixes).
    pub fn pkg(&self) -> &str {
        match self {
            BuildError::MissingLocal { pkg, .. }
            | BuildError::MissingSha256 { pkg, .. }
            | BuildError::Sha256Mismatch { pkg, .. }
            | BuildError::UnpinnedGitRef { pkg, .. }
            | BuildError::InvalidBuildSection { pkg, .. }
            | BuildError::Spawn { pkg, .. }
            | BuildError::CommandFailed { pkg, .. } => pkg,
            BuildError::Io(_) | BuildError::Recipe(_) => "",
        }
    }
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Io(e) => write!(f, "io error: {e}"),
            BuildError::Recipe(e) => write!(f, "invalid recipe: {e}"),
            BuildError::MissingLocal { pkg, path } => write!(
                f,
                "'{pkg}': no local file '{}' (sources and patches resolve against the recipe dir)",
                path.display()
            ),
            BuildError::MissingSha256 { pkg, url } => write!(
                f,
                "'{pkg}': source '{url}' has no sha256 (required for non-git sources, recipe format v1.2)"
            ),
            BuildError::Sha256Mismatch {
                pkg,
                url,
                expected,
                got,
            } => write!(
                f,
                "'{pkg}': sha256 mismatch for {url}: expected {expected}, got {got}"
            ),
            BuildError::UnpinnedGitRef { pkg, repo } => write!(
                f,
                "'{pkg}': git source '{repo}' has no pinned ref; pin a commit hash or a tag (branch names are not buildable)"
            ),
            BuildError::InvalidBuildSection { pkg, message } => {
                write!(f, "'{pkg}': invalid [build] section: {message}")
            }
            BuildError::Spawn {
                pkg,
                command,
                source,
            } => write!(f, "'{pkg}': cannot run {command}: {source}"),
            BuildError::CommandFailed {
                pkg,
                command,
                status,
            } => write!(
                f,
                "'{pkg}': command failed ({status}): {command}"
            ),
        }
    }
}

impl std::error::Error for BuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BuildError::Io(e) => Some(e),
            BuildError::Recipe(e) => Some(e),
            BuildError::Spawn { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<std::io::Error> for BuildError {
    fn from(e: std::io::Error) -> Self {
        BuildError::Io(e)
    }
}

/// Run the full M2a pipeline for one recipe: stage → build → (pack).
///
/// `pack = false` (the `repo-build -b` "build-only" mode) stops after the
/// build stage: the DESTDIR tree is left in place, no `.gpkg` is written.
pub fn pipeline(recipe: &Recipe, opts: &Opts, pack: bool) -> Result<Report, BuildError> {
    if recipe.kind == gozpak_recipe::RecipeKind::Legacy {
        return Err(BuildError::Recipe(
            gozpak_recipe::RecipeError::Toml {
                path: recipe.dir.join("recipe.toml"),
                message: format!(
                    "'{}' is a legacy (kiss 3-file) recipe; M2a builds recipe.toml only — \
                     convert it with `gozpak migrate` (M3)",
                    recipe.name
                ),
            },
        ));
    }

    let work = Work::create(&opts.work_root, &recipe.name)?;
    let src = stage(recipe, &work, &opts.source_cache)?;
    builder::run(recipe, &src, &work)?;

    let packed = if pack {
        Some(pack::pack(recipe, &work.dest, &work.tmp, &opts.out_dir, opts.repo_meta)?)
    } else {
        None
    };

    Ok(Report {
        src,
        dest: work.dest.clone(),
        packed,
    })
}

/// Load a recipe directory and run the pipeline (CLI convenience).
pub fn pipeline_dir(
    dir: &std::path::Path,
    opts: &Opts,
    pack: bool,
) -> Result<(Recipe, Report), BuildError> {
    let recipe = Recipe::load(dir).map_err(BuildError::Recipe)?;
    let report = pipeline(&recipe, opts, pack)?;
    Ok((recipe, report))
}
