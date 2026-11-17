//! `gozpak-repo`: remote-repository support (M1: read-side only).
//!
//! - [`index`] — text `repo.index` parse/serialize. v1.2 line:
//!   `name version release epoch arch sha256 url depends provides
//!   conflicts` (dependency lists are `|`-joined constraint strings, `-`
//!   when empty); legacy 7-field reference lines still parse with
//!   defaults.
//! - [`binary`] — binary-repo `index.json` parse (JSON array of
//!   [`binary::BinaryEntry`]); ed25519 signature verification is M2.
//! - [`gpkg`] — `.gpkg` tarball filename convention
//!   (`<name>-<epoch?>:<version>-<release>.gpkg`); filenames are built,
//!   never parsed back (index/manifest are the source of truth).
//! - [`urls`] — repository URL configuration (`GOZPAK_REPOS` env var +
//!   `/etc/gozpak/repos.conf`), plus the filesystem-safe URL id used for
//!   the index cache layout.
//!
//! M1 has no network: nothing here downloads. The fetch side
//! ([`remote`], M2b) downloads via the system `curl`; ed25519
//! index signing/verification lives in [`signing`].

pub mod binary;
pub mod gpkg;
pub mod index;
pub mod remote;
pub mod signing;
pub mod urls;

pub use binary::{BinaryEntry, BinaryIndexError, parse_index_json};
pub use gpkg::gpkg_filename;
pub use index::{split_dep_list, IndexEntry, RepoIndex, RepoIndexError};
pub use remote::{
    cached_repos, fetch_from, fetch_pkg, pkg_location, resolve_base, sync_repo, RepoBase,
    RemoteError,
};
pub use signing::{
    EMBEDDED_PUBKEY, hex_key, hex_sig, keygen, read_key_file, sign, to_hex, verify,
    write_key_file, SigningError,
};
pub use urls::{configured_repos, parse_conf, parse_env_repos, load_conf, url_id};
