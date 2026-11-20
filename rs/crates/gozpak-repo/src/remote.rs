//! The fetch side of a binary repo (M2b).
//!
//! Two URL shapes, per the 26.11 build-system switch
//! (gozjaro-bootstrap#94):
//!
//! - `gozpak sync` uses the **browser channel**: a GitHub repo root
//!   (`https://github.com/<owner>/<repo>`) is resolved through
//!   `releases/latest` (an unauthenticated 302 to `releases/tag/<tag>`;
//!   no API token, no rate-limit surface), and `index.json` +
//!   `index.json.sig` are pulled from the release.
//! - `gozpak get`/`fetch` use the **asset channel**:
//!   `<repo>/releases/download/<tag>/<file>`, served by the GitHub CDN.
//!
//! A `file://` (or plain local) directory is a first-class repo too —
//! the index + signature + `.gpkg` files all live in one directory,
//! which is what the LAN-relay and test flows use.
//!
//! All HTTP transfers shell out to the system `curl` (the M2a source
//! stage does the same) — no HTTP client crate.

use std::path::{Path, PathBuf};

use gozpak_core::sha256;

use crate::binary::parse_index_json;
use crate::signing;
use crate::urls::url_id;

/// A remote-repo error.
#[derive(Debug)]
pub enum RemoteError {
    /// A download could not even be started (no curl, bad args).
    Spawn(std::io::Error),
    /// curl (or a local read) failed.
    Fetch { url: String, detail: String },
    /// The index signature did not verify.
    BadSignature,
    /// The index failed to parse.
    IndexParse(String),
    /// The downloaded `.gpkg` sha256 differs from the index entry.
    Sha256Mismatch {
        url: String,
        expected: String,
        got: String,
    },
    /// IO failure reading/writing the local cache.
    Io(std::io::Error),
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoteError::Spawn(e) => write!(f, "cannot start curl: {e}"),
            RemoteError::Fetch { url, detail } => {
                write!(f, "fetch failed for {url}: {detail}")
            }
            RemoteError::BadSignature => {
                write!(f, "index.json.sig did not verify (repository key mismatch)")
            }
            RemoteError::IndexParse(m) => write!(f, "invalid index.json: {m}"),
            RemoteError::Sha256Mismatch { url, expected, got } => {
                write!(f, "sha256 mismatch for {url}: expected {expected}, got {got}")
            }
            RemoteError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for RemoteError {}

impl From<std::io::Error> for RemoteError {
    fn from(e: std::io::Error) -> Self {
        RemoteError::Io(e)
    }
}

/// Where the repo's files actually live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoBase {
    /// A local directory containing `index.json`, `index.json.sig` and
    /// the `.gpkg` files.
    Local(PathBuf),
    /// The `releases/download/<tag>` asset base (the `get` channel).
    Download { url: String, tag: String },
}

/// Resolve a user-supplied repo URL into a [`RepoBase`].
///
/// - `file:///x` / `/x` → [`RepoBase::Local`].
/// - a GitHub repo root (`https://github.com/o/r[/]`) → the
///   `releases/latest` browser channel: HEAD the URL, follow the 302
///   Location to `releases/tag/<tag>`, and return the
///   `releases/download/<tag>` base.
/// - anything else (a direct `releases/download/...` or a plain
///   mirror) is used verbatim as the download base (the tag, when
///   present in the path, is kept for display).
pub fn resolve_base(url: &str) -> Result<RepoBase, RemoteError> {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.starts_with("file://") || !trimmed.contains("://") {
        let p = trimmed
            .strip_prefix("file://")
            .map(str::to_string)
            .unwrap_or_else(|| trimmed.to_string());
        return Ok(RepoBase::Local(PathBuf::from(p)));
    }
    if let Some(root) = github_repo_root(trimmed) {
        let tag = github_latest_tag(&root)?;
        return Ok(RepoBase::Download {
            url: format!("{root}/releases/download/{tag}"),
            tag,
        });
    }
    // Direct download base (mirror or explicit releases/download/.../).
    let tag = trimmed
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    Ok(RepoBase::Download {
        url: trimmed.to_string(),
        tag,
    })
}

/// `https://github.com/<owner>/<repo>` (host matched case-insensitively,
/// owner/repo kept in their original case) with an optional trailing
/// slash → the repo root. Only a bare two-segment path qualifies: any
/// URL with a further `/releases/...` (or other) suffix is a direct
/// download base, not a repo root to probe for `latest`.
fn github_repo_root(url: &str) -> Option<String> {
    const PREFIX: &str = "https://github.com/";
    let lower = url.to_ascii_lowercase();
    lower.strip_prefix(PREFIX)?; // host must be github.com (any case)
    let rest_original = &url[PREFIX.len()..];
    let segments: Vec<&str> = rest_original
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() != 2 {
        return None;
    }
    Some(format!("https://github.com/{}/{}", segments[0], segments[1]))
}

/// The browser channel: `HEAD <root>/releases/latest` → follow the 302
/// Location (`/releases/tag/<tag>`), percent-decoding the tag.
fn github_latest_tag(root: &str) -> Result<String, RemoteError> {
    let probe = format!("{root}/releases/latest");
    let out = run_curl(&[
        "-s",
        "-I",
        "--max-time",
        "30",
        &probe,
    ])?;
    let location = out
        .lines()
        .find_map(|l| {
            let l = l.trim();
            let lower = l.to_ascii_lowercase();
            let v = lower.strip_prefix("location:")?.trim();
            Some(v.to_string())
        })
        .ok_or_else(|| RemoteError::Fetch {
            url: probe.clone(),
            detail: "no Location header (no release?)".into(),
        })?;
    // Location: /<owner>/<repo>/releases/tag/<tag>  →  last path segment.
    let path = location
        .rsplit('/')
        .next()
        .ok_or_else(|| RemoteError::Fetch {
            url: probe.clone(),
            detail: "unparseable Location".into(),
        })?;
    Ok(percent_decode(path))
}

/// Minimal `%XX` percent-decoding (release tags may carry encoded
/// characters; ordinary tags decode to themselves).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
        {
            out.push(
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap(), 16)
                    .unwrap_or(b' '),
            );
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Fetch one file from a [`RepoBase`] into `dest` (a file path).
pub fn fetch_from(base: &RepoBase, file: &str, dest: &Path) -> Result<(), RemoteError> {
    match base {
        RepoBase::Local(dir) => {
            let src = dir.join(file);
            if !src.is_file() {
                return Err(RemoteError::Fetch {
                    url: src.display().to_string(),
                    detail: "file not in local repo directory".into(),
                });
            }
            std::fs::copy(&src, dest).map(|_| ()).map_err(RemoteError::Io)?;
        }
        RepoBase::Download { url, .. } => {
            // A `file://` (or bare-path) "download" base is local: copy from
            // the filesystem instead of shelling out to curl, which the
            // install target's chroot may not have (offline acceptance).
            if url.starts_with("file://") || url.starts_with('/') {
                let dir = PathBuf::from(url.trim_start_matches("file://"));
                let src = dir.join(file);
                if !src.is_file() {
                    return Err(RemoteError::Fetch {
                        url: src.display().to_string(),
                        detail: "file not in local repo directory".into(),
                    });
                }
                std::fs::copy(&src, dest).map(|_| ()).map_err(RemoteError::Io)?;
                return Ok(());
            }
            let dest_arg = dest.display().to_string();
            let url_arg = format!("{url}/{file}");
            run_curl(&["-fsSL", "--max-time", "300", &url_arg, "-o", &dest_arg])?;
        }
    }
    Ok(())
}

fn run_curl(args: &[&str]) -> Result<String, RemoteError> {
    let out = std::process::Command::new("curl")
        .args(args)
        .output()
        .map_err(RemoteError::Spawn)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let url = args
            .iter()
            .find(|a| a.starts_with("http"))
            .copied()
            .unwrap_or("");
        return Err(RemoteError::Fetch {
            url: url.to_string(),
            detail: stderr.trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Sync one repo: resolve the base, fetch `index.json` +
/// `index.json.sig`, verify the signature, cache the index under
/// `<cache>/<url_id>/index.json`, and return the parsed entries.
///
/// `pubkey` is the 32-byte verification key (hex-decoded by the caller
/// — see [`signing::hex_key`]).
///
/// The cache dir also gains two marker files the `get` channel reads:
/// `url` (the original repo URL, for display) and `base` (the
/// resolved asset base — a directory for local repos, the
/// `releases/download/<tag>` URL for GitHub).
pub fn sync_repo(
    url: &str,
    cache_dir: &Path,
    pubkey: &[u8],
) -> Result<Vec<crate::binary::BinaryEntry>, RemoteError> {
    let base = resolve_base(url)?;
    let id = url_id(url);
    let dir = cache_dir.join(&id);
    std::fs::create_dir_all(&dir)?;
    let idx = dir.join("index.json");
    let sig = dir.join("index.json.sig");

    fetch_from(&base, "index.json", &idx)?;
    fetch_from(&base, "index.json.sig", &sig)?;

    // Verify BEFORE the index is trusted for anything.
    let index_bytes = std::fs::read(&idx)?;
    let sig_text = std::fs::read_to_string(&sig).map_err(RemoteError::Io)?;
    let sig_bytes = signing::hex_sig(&sig_text).map_err(|_| RemoteError::BadSignature)?;
    if signing::verify(pubkey, &index_bytes, &sig_bytes).is_err() {
        return Err(RemoteError::BadSignature);
    }
    let text = String::from_utf8(index_bytes).map_err(|e| {
        RemoteError::IndexParse(format!("index.json is not utf-8: {e}"))
    })?;
    let entries = parse_index_json(&text).map_err(|e| RemoteError::IndexParse(e.to_string()))?;

    std::fs::write(dir.join("url"), url)?;
    match &base {
        RepoBase::Local(p) => std::fs::write(dir.join("base"), p.to_string_lossy().as_ref())?,
        RepoBase::Download { url, .. } => std::fs::write(dir.join("base"), url.as_bytes())?,
    }
    Ok(entries)
}

/// Every synced repo in `cache_dir` (the `get` channel's input): the
/// resolved base plus the cached, signature-verified index.
pub fn cached_repos(cache_dir: &Path) -> Vec<(RepoBase, Vec<crate::binary::BinaryEntry>)> {
    let entries = match std::fs::read_dir(cache_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let idx = dir.join("index.json");
        let ok = match std::fs::read(&idx) {
            Ok(bytes) => match crate::binary::parse_index_json(
                &String::from_utf8_lossy(&bytes),
            ) {
                Ok(v) => v,
                Err(_) => continue,
            },
            Err(_) => continue,
        };
        let base = match std::fs::read_to_string(dir.join("base")) {
            Ok(b) => {
                let b = b.trim().to_string();
                if b.contains("://") {
                    RepoBase::Download {
                        tag: b.rsplit('/').next().unwrap_or_default().to_string(),
                        url: b,
                    }
                } else {
                    RepoBase::Local(std::path::PathBuf::from(b))
                }
            }
            Err(_) => continue,
        };
        out.push((base, ok));
    }
    out
}

/// Where one entry's `.gpkg` lives: bare filenames resolve against the
/// repo the entry came from (a local dir when the index was built for
/// a relay, else a download of the full asset URL).
pub fn pkg_location(
    entry: &crate::binary::BinaryEntry,
    repo_base: &RepoBase,
    bin_dir: &Path,
) -> Result<PathBuf, RemoteError> {
    if entry.url.contains("://") {
        // Full asset URL: strip the filename for the download base.
        let i = entry.url.rfind('/').expect("asset URL has a path");
        let file = entry.url[i + 1..].to_string();
        let base_url = &entry.url[..i];
        let tag = base_url.rsplit('/').next().unwrap_or_default().to_string();
        fetch_pkg(
            &RepoBase::Download {
                url: base_url.to_string(),
                tag,
            },
            &file,
            &entry.sha256,
            bin_dir,
        )
    } else {
        // Bare filename (a locally built index): the file sits in the
        // repo directory itself; verify sha and hand it back.
        let local = match repo_base {
            RepoBase::Local(dir) => dir.join(&entry.url),
            RepoBase::Download { .. } => {
                return Err(RemoteError::Fetch {
                    url: entry.url.clone(),
                    detail: "bare filename entry from a download base".into(),
                })
            }
        };
        if !local.is_file() {
            return Err(RemoteError::Fetch {
                url: local.display().to_string(),
                detail: "not in the local repo directory".into(),
            });
        }
        if sha256::hash_file(&local)? != entry.sha256 {
            return Err(RemoteError::Sha256Mismatch {
                url: local.display().to_string(),
                expected: entry.sha256.clone(),
                got: "local file hash".into(),
            });
        }
        std::fs::create_dir_all(bin_dir)?;
        let dest = bin_dir.join(&entry.url);
        if dest.is_file() && sha256::hash_file(&dest)? == entry.sha256 {
            return Ok(dest);
        }
        std::fs::copy(&local, &dest).map_err(RemoteError::Io)?;
        Ok(dest)
    }
}

/// Download one `.gpkg` from the asset channel and verify its sha256
/// against the index entry. Cached files (correct sha) are reused.
pub fn fetch_pkg(
    base: &RepoBase,
    filename: &str,
    expected_sha256: &str,
    bin_dir: &Path,
) -> Result<PathBuf, RemoteError> {
    std::fs::create_dir_all(bin_dir)?;
    let dest = bin_dir.join(filename);
    if dest.is_file() {
        if sha256::hash_file(&dest)? == expected_sha256 {
            return Ok(dest);
        }
        std::fs::remove_file(&dest)?;
    }
    let part = bin_dir.join(format!("{filename}.part"));
    fetch_from(base, filename, &part)?;
    let got = sha256::hash_file(&part)?;
    if got != expected_sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(RemoteError::Sha256Mismatch {
            url: format!("asset {filename}"),
            expected: expected_sha256.to_string(),
            got,
        });
    }
    std::fs::rename(&part, &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_local_file_urls() {
        let b = resolve_base("file:///srv/repo").unwrap();
        assert_eq!(b, RepoBase::Local(PathBuf::from("/srv/repo")));
        let b = resolve_base("/srv/repo").unwrap();
        assert_eq!(b, RepoBase::Local(PathBuf::from("/srv/repo")));
    }

    #[test]
    fn resolve_direct_download_base() {
        let b = resolve_base(
            "https://github.com/o/r/releases/download/stable",
        )
        .unwrap();
        assert_eq!(
            b,
            RepoBase::Download {
                url: "https://github.com/o/r/releases/download/stable".into(),
                tag: "stable".into(),
            }
        );
    }

    #[test]
    fn fetch_from_file_url_copies_locally_without_curl() {
        let tmp = std::env::temp_dir().join("gozpak-fileurl-test");
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("pkg-1-1.gpkg"), b"payload").unwrap();
        let dest_dir = tmp.join("bin");
        std::fs::create_dir_all(&dest_dir).unwrap();
        let base = RepoBase::Download {
            url: format!("file://{}", repo.display()),
            tag: String::new(),
        };
        fetch_from(&base, "pkg-1-1.gpkg", &dest_dir.join("pkg-1-1.gpkg"))
            .expect("file:// base must copy from the local repo dir");
        assert_eq!(
            std::fs::read(dest_dir.join("pkg-1-1.gpkg")).unwrap(),
            b"payload"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn github_root_is_recognized() {
        assert_eq!(
            github_repo_root("https://github.com/Gozjaro/gozjaro-repo"),
            Some("https://github.com/Gozjaro/gozjaro-repo".to_string())
        );
        assert_eq!(
            github_repo_root("https://github.com/Gozjaro/gozjaro-repo/"),
            Some("https://github.com/Gozjaro/gozjaro-repo".to_string())
        );
        assert_eq!(github_repo_root("https://repo.gozjaro.org/stable"), None);
    }

    #[test]
    fn percent_decode_basics() {
        assert_eq!(percent_decode("26.10"), "26.10");
        assert_eq!(percent_decode("v1.0%2F2"), "v1.0/2");
        assert_eq!(percent_decode("a%41c"), "aAc");
    }
}
