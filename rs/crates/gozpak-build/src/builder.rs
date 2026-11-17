//! The build stage: drive the `[build]` section of a TOML recipe.
//!
//! Every build runs in the unpacked source tree with the same
//! environment the recipe-format v1.2 substitution table describes:
//!
//! - `DESTDIR` — the staging prefix (all installs land under it),
//! - `VERSION`/`RELEASE`/`EPOCH`/`MAJOR`/`MINOR`/`PATCH`/`IDENT`/
//!   `PACKAGE` — the same values as `gozpak_recipe::subst::Subst`,
//! - `JOBS` — `std::thread::available_parallelism()`.
//!
//! Build systems:
//!
//! - `raw` — `steps[]` joined with newlines into ONE script, run as a
//!   single `sh -eu` process (cd/export/variables persist, working
//!   dir = unpacked source tree). This matches how legacy `build`
//!   scripts were sourced, which is what makes the M3 migration
//!   mechanical.
//! - `autotools` — `./configure <configure_args>`, `make -jJOBS`,
//!   `make DESTDIR=$DESTDIR install` (`make_target` overrides
//!   `install`).
//! - `meson` — `meson setup`, `meson compile`, `meson install`
//!   (out-of-tree; DESTDIR as a meson variable).
//! - `cmake` — `cmake -B <builddir>`, `cmake --build`, `cmake
//!   --install` (out-of-tree; DESTDIR as `-DDESTDIR=`).
//! - `make` — `make -jJOBS`, `make DESTDIR=$DESTDIR install`
//!   (`make_target` overrides `install`).

use std::path::Path;
use std::thread;

use gozpak_recipe::subst::Subst;
use gozpak_recipe::Recipe;

use crate::proc;
use crate::BuildError;
use crate::source::Work;

/// Run the recipe's `[build]` section. `src` is the unpacked source tree
/// (the stage's result); `work` provides the out-of-tree build dir
/// (meson/cmake), the DESTDIR staging prefix, and scratch space for the
/// raw script.
pub fn run(recipe: &Recipe, src: &Path, work: &Work) -> Result<(), BuildError> {
    let pkg = recipe.name.as_str();
    let system = recipe
        .build_system
        .as_deref()
        .filter(|s| !s.is_empty());

    let system = match system {
        None => return Ok(()), // no [build] section: staging-only package
        Some(s) => s,
    };

    let env = build_env(recipe, work);
    let envs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();

    match system {
        "raw" => raw(recipe, src, work, &envs),
        "autotools" => autoconf(recipe, src, work, &envs),
        "meson" => meson(recipe, src, work, &envs),
        "cmake" => cmake(recipe, src, work, &envs),
        "make" => plain_make(recipe, src, &work.dest, &envs),
        other => Err(BuildError::InvalidBuildSection {
            pkg: pkg.to_string(),
            message: format!(
                "system '{other}' is not one of raw | autotools | meson | cmake | make"
            ),
        }),
    }
}

/// `system = "raw"`: ONE shell script, a single `sh -eu` process.
///
/// Steps are joined with newlines — a cd or export on line 3 is visible
/// on line 4, exactly like the legacy sourced `build` file. The script
/// is written to a file (not `-c`) so multi-step scripts stay readable
/// in the log and beyond the ARG_MAX limit.
fn raw(recipe: &Recipe, src: &Path, work: &Work, envs: &[(&str, &str)]) -> Result<(), BuildError> {
    let pkg = &recipe.name;
    if recipe.build_steps.is_empty() {
        return Err(BuildError::InvalidBuildSection {
            pkg: pkg.clone(),
            message: "system 'raw' requires a non-empty steps[] list".into(),
        });
    }
    let script = recipe.build_steps.join("\n") + "\n";
    let script_path = work.tmp.join("build.sh");
    std::fs::write(&script_path, script)?;
    proc::run(
        pkg,
        src,
        "sh",
        &["-eu", script_path.to_str().unwrap()],
        envs,
    )
}

fn autoconf(recipe: &Recipe, src: &Path, work: &Work, envs: &[(&str, &str)]) -> Result<(), BuildError> {
    let pkg = &recipe.name;
    let configure: Vec<&str> =
        recipe.configure_args.iter().map(|a| a.as_str()).collect();
    proc::run(pkg, src, "./configure", &configure, envs)?;
    make_step(pkg, src, &work.dest, &recipe.make_target, None, envs)?;
    make_step(pkg, src, &work.dest, &recipe.make_target, Some("install"), envs)?;
    Ok(())
}

fn meson(recipe: &Recipe, src: &Path, work: &Work, envs: &[(&str, &str)]) -> Result<(), BuildError> {
    let pkg = &recipe.name;
    let setup: Vec<&str> = vec![
        "setup",
        work.build.to_str().unwrap(),
        src.to_str().unwrap(),
    ]
    .into_iter()
    .chain(recipe.meson_args.iter().map(|a| a.as_str()))
    .collect();
    proc::run(pkg, src, "meson", &setup, envs)?;
    let jobs = jobs();
    proc::run(
        pkg,
        src,
        "meson",
        &["compile", "-C", work.build.to_str().unwrap(), "-j", &jobs],
        envs,
    )?;
    proc::run(
        pkg,
        src,
        "meson",
        &[
            "install",
            "-C",
            work.build.to_str().unwrap(),
            &format!("DESTDIR={}", work.dest.display()),
        ],
        envs,
    )?;
    Ok(())
}

fn cmake(recipe: &Recipe, src: &Path, work: &Work, envs: &[(&str, &str)]) -> Result<(), BuildError> {
    let pkg = &recipe.name;
    let configure: Vec<&str> = vec![
        "-B",
        work.build.to_str().unwrap(),
        "-S",
        src.to_str().unwrap(),
    ]
    .into_iter()
    .chain(recipe.cmake_args.iter().map(|a| a.as_str()))
    .collect();
    proc::run(pkg, src, "cmake", &configure, envs)?;
    let jobs = jobs();
    proc::run(
        pkg,
        src,
        "cmake",
        &["--build", work.build.to_str().unwrap(), "-j", &jobs],
        envs,
    )?;
    proc::run(
        pkg,
        src,
        "cmake",
        &[
            "--install",
            work.build.to_str().unwrap(),
            &format!("-DDESTDIR={}", work.dest.display()),
        ],
        envs,
    )?;
    Ok(())
}

fn plain_make(
    recipe: &Recipe,
    src: &Path,
    dest: &Path,
    envs: &[(&str, &str)],
) -> Result<(), BuildError> {
    let pkg = &recipe.name;
    make_step(pkg, src, dest, &recipe.make_target, None, envs)?;
    make_step(pkg, src, dest, &recipe.make_target, Some("install"), envs)?;
    Ok(())
}

/// One `make` invocation: `make -jJOBS` (build) or `make DESTDIR=...
/// <target>` (install). `install_target = None` means the default-target
/// build step; `Some(t)` means the install step, where the recipe's
/// `make_target` overrides `t` when present. `DESTDIR` is passed the
/// shell way — as a make variable override on the command line, exactly
/// like `make DESTDIR=$DESTDIR install`.
fn make_step(
    pkg: &str,
    dir: &Path,
    dest: &Path,
    make_target: &Option<String>,
    install_target: Option<&str>,
    envs: &[(&str, &str)],
) -> Result<(), BuildError> {
    let jobs = jobs();
    match install_target {
        None => proc::run(pkg, dir, "make", &["-j", &jobs], envs),
        Some(default) => {
            let target = make_target.as_deref().unwrap_or(default);
            let destdir = format!("DESTDIR={}", dest.display());
            proc::run(pkg, dir, "make", &[&destdir, target], envs)
        }
    }
}

/// `JOBS`: `std::thread::available_parallelism` (the spec's parallelism
/// source), falling back to 1 where the OS cannot answer.
pub fn jobs() -> String {
    thread::available_parallelism()
        .map(|n| n.to_string())
        .unwrap_or_else(|_| "1".into())
}

/// The build-stage environment for one recipe: the DESTDIR staging
/// prefix, the subst table values (the same names the v1.2
/// substitution table uses, so build steps can use `$DESTDIR` /
/// `$VERSION` ...), and JOBS.
pub fn build_env(recipe: &Recipe, work: &Work) -> Vec<(&'static str, String)> {
    let subst = Subst::from_recipe(&recipe.name, &recipe.version, recipe.epoch);
    // Deterministic build PATH: LFS build tools land in /usr/local/bin
    // (book convention), system binaries in /usr/bin:/bin. Build steps
    // must not depend on the invoking shell's PATH.
    vec![
        ("PATH", "/usr/local/bin:/usr/bin:/bin".into()),
        ("DESTDIR", work.dest.to_string_lossy().into_owned()),
        ("VERSION", subst.version.clone()),
        ("RELEASE", subst.release.clone()),
        ("EPOCH", subst.epoch.to_string()),
        ("MAJOR", subst.major.clone()),
        ("MINOR", subst.minor.clone()),
        ("PATCH", subst.patch.clone()),
        ("IDENT", subst.ident.clone()),
        ("PACKAGE", subst.package.clone()),
        ("JOBS", jobs()),
    ]
}
