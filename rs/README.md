# gozpak (Rust) — M2b

Rust rewrite of the Gozjaro `gozpak` package manager. Tracked as
[gozjaro-bootstrap#93](https://github.com/Gozjaro/gozjaro-bootstrap/issues/93)
(milestone 26.11). This is the **active** implementation; the shell
`gozpak` v6.0.0 in the repository root is deprecated.

Current phase: **M2b** — build, binary repositories (sync / fetch / get) and
install are implemented; `remove`, `update`, `upgrade`, `download` and
`alternatives` are still stubs (see [Commands](#commands) and the
[roadmap](#m2m3-roadmap)).

## Workspace layout

```
rs/
├── Cargo.toml              # workspace (edition 2021, MSRV 1.85, v6.0.0)
└── crates/
    ├── gozpak-core/        # versions + vercmp, Pkg, dependencies, InstalledDb, paths, SHA-256
    ├── gozpak-recipe/      # recipe parsing: recipe.toml (primary) + legacy 3-file (read-only)
    ├── gozpak-repo/        # index.json, repo URLs / repos.conf, download, ed25519 signing
    ├── gozpak-build/       # source stage, build systems, .gpkg packing, index-build
    ├── gozpak-install/     # .gpkg unpack + verify, deps/conflicts, backups, hooks, DB record
    └── gozpak-cli/         # the `gozpak` binary (lib + thin main)
```

External crates (all pinned, MSRV-safe): `toml` + `serde` (recipes and
`manifest.toml`), `serde_json` (`index.json`), `zstd` (the `.gpkg`
container) and `ed25519-dalek` + `rand_core` (repository signing).
`gozpak-core` itself has no external dependencies; SHA-256 is a small
in-house FIPS 180-4 implementation.

## Commands

| command (alias) | status |
|---|---|
| `version` (v) | implemented — prints `6.0.0` (parity with the shell) |
| `list` (l) | implemented — installed packages from `$GOZPAK_ROOT/var/db/gozpak/installed` |
| `search` (s) | implemented — recipe dirs under `GOZPAK_PATH` (+ installed DB root), shell-glob queries |
| `checksum` (c) | implemented — hashes locally-resolvable sources |
| `build` (b), `repo-build` | implemented — `recipe.toml` → `.gpkg` in `<cache>/gozpak/bin`; `-b` = build only, no pack |
| `sync` (S) | implemented — fetch `index.json` + `index.json.sig`, verify against the embedded ed25519 key |
| `list-remote` (L) | implemented — packages in the synced indices |
| `fetch` (f) | implemented — download `.gpkg` files to the cache, no install |
| `get` (g) / `install` (i) | implemented — dependency closure (incl. `provides`) in topological order, unpack + tree-hash verify, deps/conflicts gate, config backups, hooks, DB record |
| `index-build`, `sign`, `keygen` | implemented — packaging-host tools for publishing a signed repository |
| `remove`/`download`/`update`/`upgrade`/`alternatives` | stubs — log `not implemented`, exit 1 |
| unknown word | extension fallback — locates `gozpak-<word>` on `$PATH`, reports it, does not exec it yet |

Global `-y`/`--yes` flag is parsed (recorded as `Ctx::yes`). Output style
mirrors the shell `log`/`war`/`die` lines (`-> pkg detail` / `WARNING ...` /
`ERROR ...`, all on stderr; no TTY color yet).

### Environment

* `GOZPAK_ROOT` — system root (default `/`); the installed DB lives at
  `$GOZPAK_ROOT/var/db/gozpak/installed`, repo config at
  `$GOZPAK_ROOT/etc/gozpak/repos.conf`.
* `GOZPAK_PATH` — colon-separated recipe search path (`search`/`checksum`/`build`).
* `GOZPAK_REPOS` — colon-separated repo URLs (takes priority over
  `repos.conf`); used by `sync`.
* `GOZPAK_TMPDIR` — per-run staging dir (default `<cache>/gozpak/proc`).
* `GOZPAK_SIGN_KEY` — path to the 32-byte ed25519 seed, for `sign`.
* `XDG_CACHE_HOME` — cache base (`~/.cache` by default) for `gozpak/repos`,
  `gozpak/bin` and `gozpak/sources`.

### No-argument parity detail

Like the shell version, `search`/`checksum` with no package arguments use
the current directory's basename as the package name and prepend its parent
directory to `GOZPAK_PATH` — so `gozpak c` works from inside a recipe dir.

## Recipe formats

`Recipe::load(dir)` prefers the **new primary format**, falling back to the
legacy layout, and records which one it used (`Recipe::kind`):

* **`recipe.toml`** (new primary, parsed via the `toml` crate) — full
  schema in [docs/recipe-format.md](../docs/recipe-format.md) (v1.2):
  `[package]` (name/version/release/epoch/arch/depends/build_depends/
  provides/conflicts/replaces), `[source]` (`urls = [{ url, sha256 }]` or a
  pinned `git` ref, `patches`), `[build]` (`system = raw|autotools|meson|cmake|make`),
  `[install]` (hooks, config `backup`), `[meta]`.
* **Legacy 3-file** (`version`, `build`, `depends`, `sources`, `meta`,
  `checksums`) — kept via `Recipe::load_legacy(dir)`; it will power the M3
  migration tool. Legacy recipes can be read but not built. Shell-style bare-token substitution (`acl-VERSION.tar.xz`)
  is preserved for legacy source lines; TOML URLs use `$VERSION` style.

Fixtures for both formats live in `crates/gozpak-recipe/tests/fixtures/`:
`toml-openssl` (tarball + patches), `toml-git` (pinned git source),
`acl` and `local-hello` (legacy 3-file).

## Building & testing

```sh
cd rs
cargo build          # → target/debug/gozpak
cargo test           # unit + integration tests across all crates
./target/debug/gozpak version   # → 6.0.0
```

## M2/M3 roadmap

* **Done (M2a/M2b):** `build`/`repo-build` (`recipe.toml` → `.gpkg`),
  `sync`/`list-remote`/`fetch`/`get`/`install` with signed indices and
  dependency resolution, `index-build`/`sign`/`keygen`.
* **M2 — remaining:** `remove` (read the per-package file list from the
  installed DB, keep modified config files, refuse when reverse
  dependencies exist), `update`/`upgrade` (via repo diffing), `download`,
  `alternatives`, extension execution, TTY colors.
* **M3 — migration & publishing:** `gozpak-recipe` migration tool that
  converts legacy 3-file recipes to `recipe.toml` (using
  `Recipe::load_legacy`), `repo-push`, per-repository signing keys.
