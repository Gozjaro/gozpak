//! The source stage: get `[source]` material into the work tree.
//!
//! - `urls[]` — remote files: `curl -fsSL --retry 2 -o` into the source
//!   cache (reused when already cached and the sha256 matches), verified
//!   with [`gozpak_core::sha256`] (the M1 implementation, never
//!   re-implemented), extracted with the system `tar`
//!   (`tar.xz`/`tar.gz`/`tar.bz2`, `--strip-components=1`).
//!   Scheme-less URLs are local files relative to the recipe directory.
//!   A TOML file source without a declared sha256 is rejected (v1.2:
//!   required for non-git sources).
//! - `git` — `git clone --depth 1` plus checkout of the pinned ref;
//!   an unpinned ref is a hard error (reproducibility).
//! - `patches[]` — applied in recipe order via `patch -p1`.
//!
//! Variable markers in URLs use the same dialect as the M1 `checksum`
//! command: `$VERSION`-style for TOML recipes, bare-token style for
//! legacy ones (see `gozpak_recipe::subst`).

use std::path::{Path, PathBuf};

use gozpak_core::sha256;
use gozpak_recipe::subst::{cache_path_for, Subst};
use gozpak_recipe::{Recipe, RecipeKind, Source};

use crate::proc;
use crate::BuildError;

/// Per-package scratch layout (the shell `create_tmp_dirs` equivalents),
/// created under `base` (= `<proc>/<pid>`):
///
/// ```text
/// base/<pkg>/extract   unpacked source tree (git checkouts land here too)
/// base/<pkg>/build     out-of-tree build dir (meson/cmake)
/// base/<pkg>/dest      DESTDIR staging prefix
/// base/<pkg>/tmp       intermediates (rootfs tar, gpkg stage, scripts)
/// ```
#[derive(Debug, Clone)]
pub struct Work {
    pub root: PathBuf,
    pub extract: PathBuf,
    pub build: PathBuf,
    pub dest: PathBuf,
    pub tmp: PathBuf,
}

impl Work {
    /// Create `root/<pkg>/{extract,build,dest,tmp}` (fresh dirs).
    pub fn create(base: &Path, pkg: &str) -> Result<Self, BuildError> {
        let root = base.join(pkg);
        let extract = root.join("extract");
        let build = root.join("build");
        let dest = root.join("dest");
        let tmp = root.join("tmp");
        std::fs::create_dir_all(&extract)?;
        std::fs::create_dir_all(&build)?;
        std::fs::create_dir_all(&dest)?;
        std::fs::create_dir_all(&tmp)?;
        Ok(Self {
            root,
            extract,
            build,
            dest,
            tmp,
        })
    }
}

/// Source extensions the system `tar` extracts; anything else is a
/// plain file that is kept as-is (build scripts reference it directly).
const TARBALL_SUFFIXES: [&str; 6] = [".tar.xz", ".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".tar"];

/// Stage all of the recipe's sources; returns the directory the build
/// runs in: the git checkout (git-only recipes), the unpacked tree
/// (file sources), or the empty extract dir (source-less recipes).
pub fn stage(recipe: &Recipe, work: &Work, cache: &Path) -> Result<PathBuf, BuildError> {
    let subst = Subst::from_recipe(&recipe.name, &recipe.version, recipe.epoch);
    let pkg = recipe.name.as_str();

    let mut has_files = false;
    let mut git_dirs: Vec<PathBuf> = Vec::new();

    for src in &recipe.sources {
        match src {
            Source::Files { files } => {
                for f in files {
                    let url = match recipe.kind {
                        RecipeKind::Toml => subst.apply(&f.url),
                        RecipeKind::Legacy => subst.apply_shell(&f.url),
                    };
                    let local = !url.contains("://");
                    if local && recipe.kind == RecipeKind::Toml {
                        // v1.2: file sources carry a mandatory sha256,
                        // local ones included.
                        if f.sha256.is_none() {
                            return Err(BuildError::MissingSha256 {
                                pkg: pkg.to_string(),
                                url: url.clone(),
                            });
                        }
                    }
                    let file = if local {
                        let p = if Path::new(&url).is_absolute() {
                            PathBuf::from(&url)
                        } else {
                            recipe.dir.join(&url)
                        };
                        if !p.is_file() {
                            return Err(BuildError::MissingLocal {
                                pkg: pkg.to_string(),
                                path: p,
                            });
                        }
                        verify(pkg, &p, f.sha256.as_deref())?;
                        p
                    } else {
                        fetch(pkg, cache, &url, f.sha256.as_deref())?
                    };
                    if is_tarball(&file) {
                        proc::run(
                            pkg,
                            &work.extract,
                            "tar",
                            &[
                                "xf",
                                file.to_str().ok_or_else(|| BuildError::Io(std::io::Error::other(
                                    format!("non-UTF8 path: {}", file.display()),
                                )))?,
                                "-C",
                                work.extract.to_str().unwrap(),
                                "--strip-components=1",
                            ],
                            &[],
                        )?;
                    } else {
                        // Non-archive file source (a prebuilt binary, a
                        // patch used directly, …): place it in the source
                        // directory under its file name so `raw` build
                        // steps can reference it by that name.
                        let target = work.extract.join(
                            file.file_name().ok_or_else(|| {
                                BuildError::Io(std::io::Error::other(format!(
                                    "non-UTF8 name: {}",
                                    file.display()
                                )))
                            })?,
                        );
                        std::fs::copy(&file, &target)?;
                    }
                    has_files = true;
                }
            }
            Source::Git { repo, pin } => {
                let pin = pin
                    .as_deref()
                    .filter(|p| !p.trim().is_empty())
                    .ok_or(BuildError::UnpinnedGitRef {
                        pkg: pkg.to_string(),
                        repo: repo.clone(),
                    })?;
                let name = repo
                    .rsplit('/')
                    .next()
                    .unwrap_or(repo)
                    .trim_end_matches(['/', '.'])
                    .trim_end_matches(".git");
                let git_dir = work.extract.join(name);
                if git_dir.exists() {
                    std::fs::remove_dir_all(&git_dir)?;
                }
                proc::run(
                    pkg,
                    &work.extract,
                    "git",
                    &["clone", "--depth", "1", repo, name],
                    &[],
                )?;
                // A pinned commit/tag is only in a depth-1 clone when it
                // is the remote's default HEAD; fetch it explicitly if not.
                if proc::run(pkg, &git_dir, "git", &["checkout", pin], &[]).is_err() {
                    proc::run(
                        pkg,
                        &git_dir,
                        "git",
                        &["fetch", "--depth", "1", "origin", pin],
                        &[],
                    )?;
                    proc::run(pkg, &git_dir, "git", &["checkout", pin], &[])?;
                }
                git_dirs.push(git_dir);
            }
            // `Source::Local` entries are the [source].patches (TOML) or
            // local source files (legacy); they apply/resolve after all
            // primary sources are in place, so they are handled below.
            Source::Local { .. } => {}
        }
    }

    // Patches: in recipe order, against the tree the build will use.
    let target = if git_dirs.is_empty() {
        work.extract.clone()
    } else {
        git_dirs[0].clone()
    };
    if recipe.kind == RecipeKind::Toml {
        apply_patches(pkg, &recipe_patch_files(recipe), &target)?;
    }

    if has_files {
        Ok(work.extract.clone())
    } else {
        match git_dirs.first() {
            Some(d) => Ok(d.clone()),
            None => Ok(work.extract.clone()), // no sources at all
        }
    }
}

/// The `[source].patches` list, in recipe order, resolved against the
/// recipe directory (TOML recipes only).
fn recipe_patch_files(recipe: &Recipe) -> Vec<PathBuf> {
    recipe
        .sources
        .iter()
        .filter_map(|s| match s {
            Source::Local { path } => {
                let p = if path.is_absolute() {
                    path.clone()
                } else {
                    recipe.dir.join(path)
                };
                Some(p)
            }
            _ => None,
        })
        .collect()
}

fn apply_patches(pkg: &str, patches: &[PathBuf], target: &Path) -> Result<(), BuildError> {
    for patch in patches {
        if !patch.is_file() {
            return Err(BuildError::MissingLocal {
                pkg: pkg.to_string(),
                path: patch.clone(),
            });
        }
        let file = std::fs::File::open(patch)?;
        proc::run_with_stdin(
            pkg,
            target,
            "patch",
            &["-p1", "--forward", "-s"],
            std::process::Stdio::from(file),
        )?;
    }
    Ok(())
}

/// Verify `path` against the declared sha256 (local file sources).
fn verify(pkg: &str, path: &Path, declared: Option<&str>) -> Result<(), BuildError> {
    let Some(expected) = declared else { return Ok(()) };
    let got = sha256::hash_file(path).map_err(BuildError::Io)?;
    if got != expected.to_lowercase() {
        return Err(BuildError::Sha256Mismatch {
            pkg: pkg.to_string(),
            url: path.display().to_string(),
            expected: expected.to_string(),
            got,
        });
    }
    Ok(())
}

/// Download (unless cached and verified) + sha256-verify one remote URL.
fn fetch(
    pkg: &str,
    cache: &Path,
    url: &str,
    declared: Option<&str>,
) -> Result<PathBuf, BuildError> {
    let Some(expected) = declared else {
        return Err(BuildError::MissingSha256 {
            pkg: pkg.to_string(),
            url: url.to_string(),
        });
    };
    let dest = cache_path_for(pkg, cache, url, None);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Reuse a cached copy only when its hash already matches; otherwise
    // re-download (the cache may hold a stale or corrupted file).
    let cached_ok = dest.is_file() && sha256_file_ok(&dest, expected);
    if !cached_ok {
        proc::run(
            pkg,
            dest.parent().unwrap_or_else(|| Path::new(".")),
            "curl",
            &[
                "-fsSL",
                "--retry",
                "2",
                "-o",
                &dest.display().to_string(),
                url,
            ],
            &[],
        )?;
    }

    let got = sha256::hash_file(&dest).map_err(BuildError::Io)?;
    if got != expected.to_lowercase() {
        std::fs::remove_file(&dest).ok();
        return Err(BuildError::Sha256Mismatch {
            pkg: pkg.to_string(),
            url: url.to_string(),
            expected: expected.to_string(),
            got,
        });
    }
    Ok(dest)
}

fn sha256_file_ok(path: &Path, expected: &str) -> bool {
    sha256::hash_file(path)
        .map(|h| h == expected.to_lowercase())
        .unwrap_or(false)
}

fn is_tarball(p: &Path) -> bool {
    let Some(name) = p.file_name() else {
        return false;
    };
    let name = name.to_string_lossy();
    TARBALL_SUFFIXES.iter().any(|s| name.ends_with(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn temp(name: &str) -> PathBuf {
        let pid = std::process::id();
        let d = std::env::temp_dir().join(format!("gozpak-build-{pid}-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A minimal TOML recipe dir with one local file source.
    fn test_recipe(dir: &Path, local_file: &Path, declared: Option<&str>) -> Recipe {
        std::fs::create_dir_all(dir).unwrap();
        let sum_owned = match declared {
            Some(s) => s.to_string(),
            None => sha256::hash_file(local_file).unwrap(),
        };
        let sum = sum_owned.as_str();
        std::fs::write(
            dir.join("recipe.toml"),
            format!(
                "[package]\nname = \"t\"\nversion = \"1.0\"\nrelease = 1\n\n[source]\nurls = [ {{ url = \"{f}\", sha256 = \"{sum}\" }} ]\n",
                f = local_file.display()
            ),
        )
        .unwrap();
        Recipe::load(dir).unwrap()
    }

    fn make_tarball(dir: &Path, name: &str) -> PathBuf {
        // tar -czf <out> -C <dir> top — one top-level dir, so
        // --strip-components=1 is well-defined.
        let out = dir.join(name);
        Command::new("tar")
            .args([
                "-czf",
                &out.to_string_lossy(),
                "-C",
                &dir.to_string_lossy(),
                "top",
            ])
            .status()
            .unwrap()
            .success()
            .then(|| out)
            .expect("make tarball")
    }

    #[test]
    fn sha256_mismatch_rejects() {
        let t = temp("sha-mismatch");
        let file = t.join("data.txt");
        std::fs::write(&file, b"hello\n").unwrap();
        let dir = t.join("recipe");
        let recipe = test_recipe(&dir, &file, Some("0000000000000000000000000000000000000000000000000000000000000000"));
        let work = Work::create(&t, "p").unwrap();
        match stage(&recipe, &work, &t.join("cache")).unwrap_err() {
            BuildError::Sha256Mismatch { expected, got, .. } => {
                assert!(expected.starts_with("0000"));
                assert_eq!(got, sha256::hash_hex(b"hello\n"));
            }
            other => panic!("expected Sha256Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn missing_sha256_rejects_toml_file_source() {
        let t = temp("sha-missing");
        let file = t.join("data.txt");
        std::fs::write(&file, b"hello\n").unwrap();
        let dir = t.join("recipe");
        // No sha256 in the entry: build the recipe by hand.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("recipe.toml"),
            format!(
                "[package]\nname = \"t\"\nversion = \"1.0\"\nrelease = 1\n\n[source]\nurls = [ {{ url = \"{f}\" }} ]\n",
                f = file.display()
            ),
        )
        .unwrap();
        // M1 only *warns* about the missing checksum (read-only world);
        // M2a's builder enforces it.
        let recipe = Recipe::load(&dir).unwrap();
        let work = Work::create(&t, "p").unwrap();
        match stage(&recipe, &work, &t.join("cache")).unwrap_err() {
            BuildError::MissingSha256 { .. } => {}
            other => panic!("expected MissingSha256, got {other:?}"),
        }
    }

    #[test]
    fn tarball_extracts_with_top_dir_stripped() {
        let t = temp("extract");
        let src_dir = t.join("s");
        std::fs::create_dir_all(src_dir.join("top/sub")).unwrap();
        std::fs::write(src_dir.join("top/file.txt"), b"content\n").unwrap();
        let tb = make_tarball(&src_dir, "data.tar.gz");
        let dir = t.join("recipe");
        let recipe = test_recipe(&dir, &tb, None);
        let work = Work::create(&t, "p").unwrap();
        let srcdir = stage(&recipe, &work, &t.join("cache")).unwrap();
        assert!(srcdir.join("file.txt").is_file(), "top-level dir stripped");
    }

    #[test]
    fn non_archive_file_source_lands_in_extract() {
        let t = temp("plain-file");
        let file = t.join("tool.bin");
        std::fs::write(&file, b"\x7fELF-fake-binary").unwrap();
        let dir = t.join("recipe");
        let recipe = test_recipe(&dir, &file, None);
        let work = Work::create(&t, "p").unwrap();
        let srcdir = stage(&recipe, &work, &t.join("cache")).unwrap();
        let staged = srcdir.join("tool.bin");
        assert!(staged.is_file(), "plain file kept as-is in the source dir");
        assert_eq!(
            std::fs::read(&staged).unwrap(),
            b"\x7fELF-fake-binary"
        );
    }

    #[test]
    fn missing_local_source_is_error() {
        let t = temp("missing-local");
        let dir = t.join("recipe");
        let recipe = test_recipe(&dir, &t.join("nope.tar.gz"), Some("0000000000000000000000000000000000000000000000000000000000000000"));
        let work = Work::create(&t, "p").unwrap();
        match stage(&recipe, &work, &t.join("cache")) {
            Err(BuildError::MissingLocal { path, .. }) => {
                assert_eq!(path, t.join("nope.tar.gz"));
            }
            other => panic!("expected MissingLocal, got {other:?}"),
        }
    }

    #[test]
    fn patches_apply_in_order() {
        let t = temp("patched");
        let src_dir = t.join("s");
        std::fs::create_dir_all(src_dir.join("top")).unwrap();
        std::fs::write(src_dir.join("top/file.txt"), b"line one\nline two\n").unwrap();
        let tb = make_tarball(&src_dir, "data.tar.gz");

        let dir = t.join("recipe");
        std::fs::create_dir_all(dir.join("patches")).unwrap();
        std::fs::write(
            dir.join("patches/fix.patch"),
            "--- a/file.txt\n+++ b/file.txt\n@@ -1,2 +1,2 @@\n line one\n-line two\n+line two patched\n",
        )
        .unwrap();
        let sum = sha256::hash_file(&tb).unwrap();
        std::fs::write(
            dir.join("recipe.toml"),
            format!(
                "[package]\nname = \"t\"\nversion = \"1.0\"\nrelease = 1\n\n[source]\nurls = [ {{ url = \"{f}\", sha256 = \"{sum}\" }} ]\npatches = [\"patches/fix.patch\"]\n",
                f = tb.display()
            ),
        )
        .unwrap();
        let recipe = Recipe::load(&dir).unwrap();
        let work = Work::create(&t, "p").unwrap();
        let srcdir = stage(&recipe, &work, &t.join("cache")).unwrap();
        let patched = std::fs::read_to_string(srcdir.join("file.txt")).unwrap();
        assert_eq!(patched, "line one\nline two patched\n");
    }

    #[test]
    fn unpinned_git_ref_rejected_at_load() {
        let t = temp("git-unpinned");
        let dir = t.join("recipe");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("recipe.toml"),
            "[package]\nname = \"g\"\nversion = \"1\"\nrelease = 1\n\n[source]\ngit = \"git+https://example.com/o/r#main\"\n",
        )
        .unwrap();
        // The M1 validator already rejects branch-like refs; the builder
        // adds a second guard for legacy unpinned git+ lines.
        match Recipe::load(&dir) {
            Err(e) => assert!(e.to_string().contains("pin"), "{e}"),
            Ok(_) => panic!("unpinned git ref must not load"),
        }
    }
}
