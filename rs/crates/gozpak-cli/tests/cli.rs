//! End-to-end tests for the `gozpak` binary (M1).
//!
//! Every invocation runs in a hermetic environment (`env_clear` + explicit
//! vars) so the tests never touch the developer's real `~/.cache`,
//! `GOZPAK_*` vars, or installed packages.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_gozpak")
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A unique scratch directory (created; left in place for inspection).
fn scratch(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let p = std::env::temp_dir().join(format!("gozpak-cli-{pid}-{n}-{name}"));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Run the binary hermetically: cleared env + the given vars + args.
fn gozpak(vars: &[(&str, &str)], args: &[&str]) -> std::process::Output {
    let home = scratch("home");
    let mut c = Command::new(bin());
    c.env_clear();
    c.env("HOME", &home);
    for (k, v) in vars {
        c.env(k, v);
    }
    c.args(args);
    c.output().expect("failed to spawn gozpak")
}

fn out_str(s: &[u8]) -> String {
    String::from_utf8_lossy(s).to_string()
}

// ---------------------------------------------------------------------------
// version
// ---------------------------------------------------------------------------

#[test]
fn version_prints_package_version() {
    let o = gozpak(&[], &["version"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(out_str(&o.stdout), "6.0.0\n");
}

#[test]
fn version_alias_and_yes_flag() {
    let o = gozpak(&[], &["v"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(out_str(&o.stdout), "6.0.0\n");

    let o = gozpak(&[], &["-y", "v"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(out_str(&o.stdout), "6.0.0\n");
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

#[test]
fn list_all_from_installed_db() {
    let root = fixtures().join("root");
    let o = gozpak(&[("GOZPAK_ROOT", root.to_str().unwrap())], &["l"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(out_str(&o.stdout), "hello 1.2.3-1\nzlib 1.3.1-1\n");
}

#[test]
fn list_specific_package() {
    let root = fixtures().join("root");
    let o = gozpak(&[("GOZPAK_ROOT", root.to_str().unwrap())], &["list", "zlib"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(out_str(&o.stdout), "zlib 1.3.1-1\n");
}

#[test]
fn list_missing_package_is_error() {
    let root = fixtures().join("root");
    let o = gozpak(&[("GOZPAK_ROOT", root.to_str().unwrap())], &["l", "does-not-exist"]);
    assert_eq!(o.status.code(), Some(1));
    let err = out_str(&o.stderr);
    assert!(err.contains("ERROR"), "stderr: {err}");
    assert!(err.contains("not found"), "stderr: {err}");
}

#[test]
fn list_empty_when_db_root_missing() {
    // GOZPAK_ROOT pointing at a directory without the DB → nothing listed.
    let empty = scratch("empty-root");
    let o = gozpak(&[("GOZPAK_ROOT", empty.to_str().unwrap())], &["l"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(out_str(&o.stdout), "");
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

#[test]
fn search_finds_toml_recipe() {
    let pkgs = fixtures().join("pkgs");
    let o = gozpak(&[("GOZPAK_PATH", pkgs.to_str().unwrap())], &["s", "toml-openssl"]);
    assert_eq!(o.status.code(), Some(0));
    let expected = pkgs.join("toml-openssl").display().to_string();
    assert_eq!(out_str(&o.stdout), format!("{expected}\n"));
}

#[test]
fn search_glob_matches() {
    let pkgs = fixtures().join("pkgs");
    let o = gozpak(&[("GOZPAK_PATH", pkgs.to_str().unwrap())], &["search", "legacy-*"]);
    assert_eq!(o.status.code(), Some(0));
    let expected = pkgs.join("legacy-acl").display().to_string();
    assert_eq!(out_str(&o.stdout), format!("{expected}\n"));
}

#[test]
fn search_no_match_is_error() {
    let pkgs = fixtures().join("pkgs");
    let o = gozpak(&[("GOZPAK_PATH", pkgs.to_str().unwrap())], &["s", "nonexistent"]);
    assert_eq!(o.status.code(), Some(1));
    let err = out_str(&o.stderr);
    assert!(err.contains("ERROR"), "stderr: {err}");
    assert!(err.contains("not found"), "stderr: {err}");
}

#[test]
fn search_without_gozpak_path_finds_nothing() {
    // No GOZPAK_PATH, db root absent → the query cannot match.
    let empty = scratch("empty-root2");
    let o = gozpak(
        &[("GOZPAK_ROOT", empty.to_str().unwrap())],
        &["s", "mesa"],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_str(&o.stderr).contains("not found"));
}

// ---------------------------------------------------------------------------
// checksum
// ---------------------------------------------------------------------------

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let p = entry.unwrap().path();
        let q = dst.join(p.file_name().unwrap());
        if p.is_dir() {
            std::fs::create_dir_all(&q).unwrap();
            copy_tree(&p, &q);
        } else {
            std::fs::copy(&p, &q).unwrap();
        }
    }
}

#[test]
fn checksum_legacy_local_source_writes_file() {
    let pkgs = fixtures().join("pkgs");
    let tmp = scratch("chk-pkgs");
    let tmp_pkgs = tmp.join("pkgs");
    std::fs::create_dir_all(&tmp_pkgs).unwrap();
    copy_tree(&pkgs.join("local-hello"), &tmp_pkgs.join("local-hello"));

    let cache = tmp.join("cache");
    let o = gozpak(
        &[
            ("GOZPAK_PATH", tmp_pkgs.to_str().unwrap()),
            ("XDG_CACHE_HOME", cache.to_str().unwrap()),
        ],
        &["c", "local-hello"],
    );
    assert_eq!(o.status.code(), Some(0), "stderr: {}", out_str(&o.stderr));
    let err = out_str(&o.stderr);
    assert!(err.contains("legacy recipe"), "stderr: {err}");
    assert!(err.contains("Generated checksums"), "stderr: {err}");

    let written = std::fs::read_to_string(tmp_pkgs.join("local-hello/checksums")).unwrap();
    let expected = gozpak_core::sha256::hash_hex(b"gozpak local source file\n");
    assert_eq!(written, expected + "\n");
}

#[test]
fn checksum_toml_recipe_no_network_no_file() {
    let pkgs = fixtures().join("pkgs");
    let tmp = scratch("chk-toml");
    let tmp_pkgs = tmp.join("pkgs");
    std::fs::create_dir_all(&tmp_pkgs).unwrap();
    copy_tree(&pkgs.join("toml-openssl"), &tmp_pkgs.join("toml-openssl"));

    let cache = tmp.join("cache");
    let o = gozpak(
        &[
            ("GOZPAK_PATH", tmp_pkgs.to_str().unwrap()),
            ("XDG_CACHE_HOME", cache.to_str().unwrap()),
        ],
        &["c", "toml-openssl"],
    );
    assert_eq!(o.status.code(), Some(0), "stderr: {}", out_str(&o.stderr));
    // v1.1: TOML recipes keep their checksum in [source].urls[].sha256,
    // not a file.
    assert!(!tmp_pkgs.join("toml-openssl/checksums").exists());
    let err = out_str(&o.stderr);
    assert!(!err.contains("legacy recipe"), "stderr: {err}");
}

#[test]
fn checksum_remote_only_source_without_cache_is_noop() {
    let pkgs = fixtures().join("pkgs");
    let tmp = scratch("chk-remote");
    let tmp_pkgs = tmp.join("pkgs");
    std::fs::create_dir_all(&tmp_pkgs).unwrap();
    copy_tree(&pkgs.join("legacy-acl"), &tmp_pkgs.join("legacy-acl"));

    let cache = tmp.join("cache");
    let o = gozpak(
        &[
            ("GOZPAK_PATH", tmp_pkgs.to_str().unwrap()),
            ("XDG_CACHE_HOME", cache.to_str().unwrap()),
        ],
        &["c", "legacy-acl"],
    );
    assert_eq!(o.status.code(), Some(0), "stderr: {}", out_str(&o.stderr));
    let err = out_str(&o.stderr);
    assert!(err.contains("No sources needing checksums"), "stderr: {err}");
    // The pre-existing fixture checksums file (a "SKIP" marker) is untouched.
    let existing = std::fs::read_to_string(tmp_pkgs.join("legacy-acl/checksums")).unwrap();
    assert_eq!(existing.trim(), "SKIP");
}

// ---------------------------------------------------------------------------
// stubs + extension fallback
// ---------------------------------------------------------------------------

#[test]
fn stub_commands_report_not_implemented() {
    // `b`/`build` left this list in M2a (it is implemented now); the
    // remaining stubs still report "not implemented in M1".
    for action in ["i", "g", "r", "d", "f", "S", "L", "u", "U", "a"] {
        let o = gozpak(&[], &[action, "somepkg"]);
        assert_eq!(o.status.code(), Some(1), "action {action}");
        let err = out_str(&o.stderr);
        assert!(
            err.contains("not implemented in M1"),
            "action {action}: stderr: {err}"
        );
    }
}

#[test]
fn extension_fallback_missing_is_error() {
    let o = gozpak(&[], &["frobnicate", "x"]);
    assert_eq!(o.status.code(), Some(1));
    let err = out_str(&o.stderr);
    assert!(err.contains("gozpak-frobnicate"), "stderr: {err}");
    assert!(err.contains("M2"), "stderr: {err}");
}

#[test]
fn extension_fallback_found_in_path_refused_in_m1() {
    // Put a fake extension on PATH; M1 must still refuse to run it.
    let dir = scratch("extbin");
    let ext = dir.join("gozpak-fakeext");
    std::fs::write(&ext, "#!/bin/sh\necho never\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ext, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut c = Command::new(bin());
    c.env_clear();
    c.env("PATH", &dir);
    c.arg("fakeext").arg("x");
    let o = c.output().expect("spawn");
    assert_eq!(o.status.code(), Some(1), "stderr: {}", out_str(&o.stderr));
    let err = out_str(&o.stderr);
    assert!(err.contains("gozpak-fakeext"), "stderr: {err}");
    assert!(err.contains("not implemented in M1"), "stderr: {err}");
    // The extension itself must not have run.
    assert!(!out_str(&o.stdout).contains("never"));
}

// ---------------------------------------------------------------------------
// M2a: build / repo-build (stage + build + pack)
// ---------------------------------------------------------------------------

/// The workspace-level M2a fixture (`rs/tests/fixtures`), two levels up
/// from this crate.
fn m2a_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .canonicalize()
        .expect("rs/tests/fixtures exists")
}

/// Run a pipeline command hermetically with isolated cache + work dirs.
fn m2a(vars_extra: &[(&str, &str)], args: &[&str]) -> (std::process::Output, PathBuf, PathBuf) {
    let cache = scratch("m2a-cache");
    let tmpdir = scratch("m2a-proc");
    let fixtures = m2a_fixtures();
    let fixtures_str = fixtures.to_str().unwrap();
    let cache_str = cache.to_str().unwrap();
    let tmpdir_str = tmpdir.to_str().unwrap();
    let mut vars: Vec<(&str, &str)> = vec![
        ("GOZPAK_PATH", fixtures_str),
        ("XDG_CACHE_HOME", cache_str),
        ("GOZPAK_TMPDIR", tmpdir_str),
    ];
    vars.extend(vars_extra.iter().copied());
    let o = gozpak(&vars, args);
    (o, cache, tmpdir)
}

#[test]
fn repo_build_fixture_produces_gpkg() {
    let (o, cache, _) = m2a(&[], &["repo-build", "hello-raw"]);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", out_str(&o.stderr));
    let gpkg = cache.join("gozpak/bin/hello-raw-1.0.0-1.gpkg");
    assert!(gpkg.is_file(), "missing {gpkg:?}");
    // stdout: one sha256sum-style line: `<sha256>  <path>`.
    let stdout = out_str(&o.stdout);
    let expected_hash = gozpak_core::sha256::hash_file(&gpkg).unwrap();
    let line = format!("{expected_hash}  {}", gpkg.display());
    assert!(stdout.contains(&line), "stdout: {stdout}");
    // stderr log trail.
    let err = out_str(&o.stderr);
    assert!(err.contains("staging sources"), "stderr: {err}");
    assert!(err.contains("built"), "stderr: {err}");
    assert!(err.contains("packed"), "stderr: {err}");
}

#[test]
fn build_alias_packs_without_repo_metadata() {
    let (o, cache, _) = m2a(&[], &["b", "hello-raw"]);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", out_str(&o.stderr));
    let gpkg = cache.join("gozpak/bin/hello-raw-1.0.0-1.gpkg");
    assert!(gpkg.is_file(), "missing {gpkg:?}");
    let stdout = out_str(&o.stdout);
    let expected_hash = gozpak_core::sha256::hash_file(&gpkg).unwrap();
    assert!(stdout.contains(&expected_hash), "stdout: {stdout}");
}

#[test]
fn repo_build_b_flag_builds_without_packing() {
    let (o, cache, _) = m2a(&[], &["repo-build", "-b", "hello-raw"]);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", out_str(&o.stderr));
    let bin = cache.join("gozpak/bin");
    assert!(
        !bin.exists() || bin.read_dir().unwrap().next().is_none(),
        "-b must not write a .gpkg"
    );
    let err = out_str(&o.stderr);
    assert!(err.contains("build-only"), "stderr: {err}");
}

#[test]
fn repo_build_unknown_package_is_error() {
    let (o, _, _) = m2a(&[], &["repo-build", "no-such-pkg"]);
    assert_eq!(o.status.code(), Some(1));
    let err = out_str(&o.stderr);
    assert!(err.contains("ERROR"), "stderr: {err}");
    assert!(err.contains("not found"), "stderr: {err}");
}

// ---------------------------------------------------------------------------
// usage
// ---------------------------------------------------------------------------

#[test]
fn no_args_prints_usage() {
    let o = gozpak(&[], &[]);
    assert_eq!(o.status.code(), Some(0));
    let err = out_str(&o.stderr);
    assert!(err.contains("alternatives"), "stderr: {err}");
    assert!(err.contains("-y, --yes"), "stderr: {err}");
    assert!(err.contains("M1 note"), "stderr: {err}");
    assert_eq!(out_str(&o.stdout), "");
}
