//! M1 command implementations: version, list, search, checksum, usage,
//! and the not-implemented stubs. M2a adds `build`/`repo-build`
//! (stage + build + pack via `gozpak-build`).

use std::path::PathBuf;

use gozpak_core::{paths, sha256};
use gozpak_recipe::{subst, Recipe, RecipeKind, Source};

use crate::log::{die, log_line, war};
use crate::Ctx;

/// Print the usage block (shell `args()` no-arg help).
pub fn usage() {
    log_line("gozpak [a|b|c|d|f|g|i|l|L|r|s|S|u|U|v] [pkg]...", "");
    log_line("alternatives List and swap alternatives", "");
    log_line("build        Build and pack packages (no repo metadata)", "");
    log_line("checksum     Generate checksums", "");
    log_line("download     Download sources", "");
    log_line("fetch        Download binary packages (no install)", "");
    log_line("get          Fetch and install binary packages", "");
    log_line("install      Install packages", "");
    log_line("list         List installed packages", "");
    log_line("list-remote  List packages in remote repositories", "");
    log_line("remove       Remove packages", "");
    log_line("repo-build [-b] pkg...   Build and pack .gpkg packages (repo metadata)", "");
    log_line("search       Search for packages", "");
    log_line("sync         Sync remote repository indices", "");
    log_line("update       Update the system and repositories", "");
    log_line("upgrade      Update the system", "");
    log_line("version      Package manager version", "");
    log_line("", "");
    log_line("Global flags:", "");
    log_line("  -y, --yes  Skip all confirmation prompts", "");
    log_line("  -b         (repo-build only) build only, skip the pack step", "");
    log_line("", "");
    log_line(
        "M1 note: version, list, search and checksum are the read \
         commands; build and repo-build are functional in M2a (stage, \
         build, pack .gpkg tar.zst); all other commands are still stubs.",
        "",
    );
}

/// `l`/`list` — list installed packages (or specific ones) from the DB.
pub fn list(ctx: &Ctx, names: &[String]) -> i32 {
    if names.is_empty() {
        for pkg in ctx.db.list() {
            println!("{} {}", pkg.name, pkg.version.display());
        }
        return 0;
    }
    for name in names {
        match ctx.db.version_of(name) {
            Some(v) => println!("{name} {}", v.display()),
            None => die(name, "not found"),
        }
    }
    0
}

/// `s`/`search` — glob recipe directories under GOZPAK_PATH (+ the
/// installed DB root, like the shell's `_pkg_find` which appends
/// `$sys_db`). With no arguments the shell version searches the current
/// directory's basename instead (see [`pwd_fallback_names`]).
pub fn search(ctx: &Ctx, names: &[String]) -> i32 {
    let (queries, extra_base) = pwd_fallback(names);
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(p) = extra_base {
        bases.push(p);
    }
    bases.extend(ctx.recipe_paths.iter().cloned());
    bases.push(ctx.db.root().to_path_buf());

    for q in &queries {
        let hits = Recipe::search(q, &bases);
        if hits.is_empty() {
            die(q, "not found");
        }
        for h in &hits {
            println!("{}", h.display());
        }
    }
    0
}

/// `c`/`checksum` — generate/verify checksums for a recipe's sources.
///
/// M1 has no network: only sources resolvable on disk are hashable —
/// local files (relative to the recipe dir) and previously cached
/// downloads under the source cache. Git sources are never checksummed
/// (same as the shell version).
pub fn checksum(ctx: &Ctx, names: &[String]) -> i32 {
    let (queries, extra_base) = pwd_fallback(names);
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(p) = extra_base {
        bases.push(p);
    }
    bases.extend(ctx.recipe_paths.iter().cloned());

    for q in &queries {
        let dir = match Recipe::find(q, &bases) {
            Some(d) => d,
            None => die(q, "not found"),
        };
        let recipe = match Recipe::load(&dir) {
            Ok(r) => r,
            Err(e) => die(q, &format!("invalid recipe: {e}")),
        };
        if recipe.kind == RecipeKind::Legacy {
            log_line(&recipe.name, "using legacy recipe format (recipe.toml is preferred)");
        }
        run_checksum(&recipe, ctx);
    }
    0
}

/// `b`/`build` and `repo-build` — the M2a build pipeline:
/// stage sources, run the `[build]` section, pack a `.gpkg` (tar.zst).
///
/// - `repo_meta = true` (`repo-build`): the manifest carries the repo
///   metadata (`[manifest].repo` + `built_by`).
/// - `pack = false` (`repo-build -b`): build only — the DESTDIR tree
///   is left in the work dir, no `.gpkg` is written.
///
/// Recipes load from `GOZPAK_PATH` (plus the current directory's
/// parent, like every other command). The finished `.gpkg` lands in
/// the binary cache (`<cache>/gozpak/bin`); stdout gets one
/// sha256sum-style line `<sha256>  <path>` per packed package.
pub fn build(ctx: &Ctx, names: &[String], repo_meta: bool, pack: bool) -> i32 {
    let (queries, extra_base) = pwd_fallback(names);
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(p) = extra_base {
        bases.push(p);
    }
    bases.extend(ctx.recipe_paths.iter().cloned());

    for q in &queries {
        let dir = match Recipe::find(q, &bases) {
            Some(d) => d,
            None => die(q, "not found"),
        };
        let recipe = match Recipe::load(&dir) {
            Ok(r) => r,
            Err(e) => die(q, &format!("invalid recipe: {e}")),
        };
        if recipe.kind == RecipeKind::Legacy {
            die(
                &recipe.name,
                "legacy recipe cannot be built in M2a (recipe.toml only; \
                 migration is M3)",
            );
        }
        for w in &recipe.warnings {
            war(&recipe.name, w);
        }

        let opts = gozpak_build::Opts {
            work_root: proc_root().join(std::process::id().to_string()),
            source_cache: paths::source_cache_dir(),
            out_dir: paths::bin_cache_dir(),
            repo_meta,
        };

        log_line(&recipe.name, "staging sources");
        let report = match gozpak_build::pipeline(&recipe, &opts, pack) {
            Ok(r) => r,
            Err(e) => die(&recipe.name, &e.to_string()),
        };
        log_line(&recipe.name, "built");

        match report.packed {
            Some(packed) => {
                log_line(&recipe.name, &format!("packed {}", packed.gpkg.display()));
                println!("{}  {}", packed.gpkg_sha256, packed.gpkg.display());
            }
            None => {
                log_line(
                    &recipe.name,
                    &format!(
                        "build-only: DESTDIR at {} (no .gpkg)",
                        report.dest.display()
                    ),
                );
            }
        }
    }
    0
}

/// The per-run process temp root (shell `create_tmp_dirs` `proc`):
/// `$GOZPAK_TMPDIR` when set, else `<cache>/gozpak/proc`.
fn proc_root() -> PathBuf {
    if let Ok(v) = std::env::var("GOZPAK_TMPDIR") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    paths::source_cache_dir()
        .parent()
        .map(|p| p.join("proc"))
        .unwrap_or_else(|| PathBuf::from("/tmp/gozpak/proc"))
}

/// `pkg_order`-free stub for M2/M3 commands. Mirrors the shell log style
/// and exits 1.
pub fn stub(name: &str, names: &[String]) -> i32 {
    if names.is_empty() {
        log_line(name, &format!("'gozpak {name}' not implemented in M1"));
    } else {
        for n in names {
            log_line(n, &format!("'gozpak {name}' not implemented in M1"));
        }
    }
    1
}

/// Shell parity: when a non-`list` command gets zero package arguments,
/// use the basename of the current directory as the package name and add
/// its parent to the search path (this is how `gozpak c` works from
/// inside a recipe directory).
fn pwd_fallback(names: &[String]) -> (Vec<String>, Option<PathBuf>) {
    if !names.is_empty() {
        return (names.to_vec(), None);
    }
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(_) => return (Vec::new(), None),
    };
    let name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = cwd.parent().map(|p| p.to_path_buf());
    (vec![name], parent)
}

fn run_checksum(recipe: &Recipe, _ctx: &Ctx) {
    log_line(&recipe.name, "Reading sources");

    let cache = paths::source_cache_dir();
    let subst_ctx = subst::Subst::from_recipe(&recipe.name, &recipe.version, recipe.epoch);

    // Hash every source that resolves to a file on disk.
    let mut resolvable: Vec<(Source, String)> = Vec::new();
    for src in &recipe.sources {
        match src {
            Source::Local { path } => {
                let p = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    recipe.dir.join(path)
                };
                if let Ok(s) = sha256::hash_file(&p) {
                    resolvable.push((src.clone(), s));
                }
            }
            Source::Git { .. } => {
                // The shell version never checksums git sources.
                continue;
            }
            Source::Files { files } => {
                for f in files {
                    // v1.1: url keeps markers until use time; legacy lines
                    // carry bare markers, TOML urls use $MARKER.
                    let substituted = match recipe.kind {
                        RecipeKind::Legacy => subst_ctx.apply_shell(&f.url),
                        RecipeKind::Toml => subst_ctx.apply(&f.url),
                    };
                    let cached =
                        subst::cache_path_for(&recipe.name, &cache, &substituted, f.dest.as_deref());
                    if !cached.is_file() {
                        continue; // not downloaded yet; M2 handles this
                    }
                    let computed = match sha256::hash_file(&cached) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    if let Some(declared) = &f.sha256 {
                        if declared != &computed {
                            war(
                                &recipe.name,
                                &format!(
                                    "checksum mismatch for {}: expected {declared}, got {computed}",
                                    cached.display()
                                ),
                            );
                        } else {
                            log_line(&recipe.name, &format!("verified {}", cached.display()));
                        }
                    }
                    resolvable.push((src.clone(), computed));
                }
            }
        }
    }

    if resolvable.is_empty() {
        log_line(&recipe.name, "No sources needing checksums");
        return;
    }

    // Where the checksums land depends on the on-disk format:
    match recipe.kind {
        RecipeKind::Legacy => {
            // Shell behavior: write the `checksums` file.
            let file = paths::recipe_checksums_file(&recipe.dir);
            let body: Vec<_> = resolvable.iter().map(|(_, s)| s.as_str()).collect();
            if let Err(e) = std::fs::write(&file, body.join("\n") + "\n") {
                war(&recipe.name, &format!("failed to write {}: {e}", file.display()));
            } else {
                log_line(&recipe.name, "Generated checksums");
            }
        }
        RecipeKind::Toml => {
            // v1.1: declared checksums live in [source].urls[].sha256 and
            // are verified above when a cached tarball was found; M1 does
            // not rewrite recipe.toml (read-only), and local patch files
            // have no declared checksum.
            log_line(
                &recipe.name,
                "checksums for source URLs belong in [source].urls[].sha256",
            );
        }
    }
}
