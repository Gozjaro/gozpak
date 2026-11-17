//! End-to-end M2a tests: the `hello-raw` fixture recipe (a tiny raw
//! build that installs three files into `$DESTDIR`) is staged, built,
//! packed, unpacked, and verified — manifest parses, the file list is
//! exact, and every rootfs file is byte-identical.
//!
//! The fixture lives at `rs/tests/fixtures/` (workspace level, shared
//! with the CLI evidence run), not inside this crate.

use std::path::{Path, PathBuf};

use gozpak_build::pack::rootfs_sha256;
use gozpak_build::{Manifest, Opts, pipeline_dir};
use gozpak_core::sha256;

/// `rs/tests/fixtures` — two levels up from this crate.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .canonicalize()
        .unwrap()
}

fn temp(name: &str) -> PathBuf {
    let pid = std::process::id();
    let d = std::env::temp_dir().join(format!("gozpak-build-{pid}-e2e-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn hello_raw() -> PathBuf {
    fixtures().join("hello-raw")
}

/// Extract a `.gpkg` (tar.zst) into a fresh dir and return it.
fn extract_gpkg(gpkg: &Path, into: &Path) -> PathBuf {
    let bytes = std::fs::read(gpkg).unwrap();
    let raw = zstd::decode_all(&bytes[..]).unwrap();
    let tarball = into.join("decoded.tar");
    std::fs::write(&tarball, &raw).unwrap();
    let out = into.join("extracted");
    std::fs::create_dir_all(&out).unwrap();
    let status = std::process::Command::new("tar")
        .args([
            "xf",
            tarball.to_str().unwrap(),
            "-C",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "tar xf failed");
    out
}

/// What `$JOBS` should be in this test process.
fn jobs() -> String {
    std::thread::available_parallelism()
        .map(|n| n.to_string())
        .unwrap_or_else(|_| "1".into())
}

#[test]
fn repo_build_pipeline_produces_verifiable_gpkg() {
    let t = temp("hello");
    let opts = Opts {
        work_root: t.join("proc"),
        source_cache: t.join("cache").join("gozpak").join("sources"),
        out_dir: t.join("cache").join("gozpak").join("bin"),
        repo_meta: true,
    };
    std::fs::create_dir_all(&opts.work_root).unwrap();
    std::fs::create_dir_all(&opts.source_cache).unwrap();

    let (recipe, report) = pipeline_dir(&hello_raw(), &opts, true).unwrap();
    assert_eq!(recipe.name, "hello-raw");

    let packed = report.packed.as_ref().unwrap();
    let gpkg = &packed.gpkg;
    assert!(gpkg.is_file(), "missing {}", gpkg.display());
    assert_eq!(gpkg.file_name().unwrap(), "hello-raw-1.0.0-1.gpkg");
    // The reported .gpkg sha256 is the file's own hash.
    assert_eq!(sha256::hash_file(gpkg).unwrap(), packed.gpkg_sha256);

    // Exact file list (three files, relative paths, sorted).
    assert_eq!(
        packed.files,
        vec![
            "bin/hello-raw".to_string(),
            "etc/hello-raw.conf".to_string(),
            "share/hello-raw/README.txt".to_string(),
        ]
    );

    let out = extract_gpkg(gpkg, &t);

    // manifest.toml parses.
    let manifest_text = std::fs::read_to_string(out.join("manifest.toml")).unwrap();
    let m: Manifest = toml::from_str(&manifest_text).unwrap();
    assert_eq!(m.package.name, "hello-raw");
    assert_eq!(m.package.version, "1.0.0");
    assert_eq!(m.package.release, "1");
    assert_eq!(m.package.epoch, 0);
    assert_eq!(m.package.arch, "x86_64");
    assert_eq!(m.manifest.files, packed.files);
    // repo-build carries the repo metadata.
    assert_eq!(m.manifest.repo.as_deref(), Some("gozjaro"));

    // built-by: the fixture lives inside the gozpak git repository, so
    // the recipe git hash is available — unless this checkout is not a
    // git repo, in which case it is exactly "local".
    let expected_local = git_head(&hello_raw()).is_none();
    let built_by = m.manifest.built_by.clone().unwrap();
    if expected_local {
        assert_eq!(built_by, "local");
    } else {
        assert_eq!(built_by, git_head(&hello_raw()).unwrap());
    }

    // rootfs files are byte-identical.
    let bin = std::fs::read(out.join("rootfs/bin/hello-raw")).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&bin),
        format!(
            "hello from hello-raw 1.0.0 release 1 epoch 0 jobs {}\n",
            jobs()
        )
    );
    assert_eq!(
        std::fs::read(out.join("rootfs/etc/hello-raw.conf")).unwrap(),
        b"repo=hello-raw\n"
    );
    let fixture_readme = std::fs::read(&report.src.join("README.txt")).unwrap();
    assert_eq!(
        std::fs::read(out.join("rootfs/share/hello-raw/README.txt")).unwrap(),
        fixture_readme
    );

    // rootfs_sha256 is the deterministic tree hash of the rootfs tree.
    // Recompute it from the *extracted* rootfs/ tree — the exact flow
    // the M2b verifier uses (the value must survive the pack → extract
    // round trip; it is not a hash of a transient tarball).
    let extracted_rootfs = out.join("rootfs");
    assert_eq!(
        rootfs_sha256(&extracted_rootfs).unwrap(),
        m.manifest.rootfs_sha256
    );
}

#[test]
fn plain_build_omits_repo_metadata() {
    let t = temp("plain");
    let opts = Opts {
        work_root: t.join("proc"),
        source_cache: t.join("cache").join("gozpak").join("sources"),
        out_dir: t.join("cache").join("gozpak").join("bin"),
        repo_meta: false,
    };
    std::fs::create_dir_all(&opts.work_root).unwrap();
    std::fs::create_dir_all(&opts.source_cache).unwrap();

    let (_, report) = pipeline_dir(&hello_raw(), &opts, true).unwrap();
    let packed = report.packed.as_ref().unwrap();
    let out = extract_gpkg(&packed.gpkg, &t);
    let m: Manifest =
        toml::from_str(&std::fs::read_to_string(out.join("manifest.toml")).unwrap()).unwrap();
    assert!(m.manifest.repo.is_none(), "build has no repo metadata");
    assert!(m.manifest.built_by.is_none(), "build has no built-by");
    assert_eq!(m.manifest.files, packed.files);
}

#[test]
fn build_only_mode_skips_pack() {
    let t = temp("buildonly");
    let opts = Opts {
        work_root: t.join("proc"),
        source_cache: t.join("cache").join("gozpak").join("sources"),
        out_dir: t.join("cache").join("gozpak").join("bin"),
        repo_meta: true,
    };
    std::fs::create_dir_all(&opts.work_root).unwrap();
    std::fs::create_dir_all(&opts.source_cache).unwrap();

    let (_, report) = pipeline_dir(&hello_raw(), &opts, false).unwrap();
    assert!(report.packed.is_none(), "-b must not produce a .gpkg");
    // The DESTDIR tree was still built.
    assert!(report.dest.join("bin/hello-raw").is_file());
    assert!(!opts.out_dir.exists() || opts.out_dir.read_dir().unwrap().next().is_none());
}

/// `git -C <dir> rev-parse HEAD` (the same provenance source the
/// builder uses), `None` when the dir is not inside a git repository.
fn git_head(dir: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C", dir.to_str()?, "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
