# gozpak

The package manager for [Gozjaro Linux](https://github.com/Gozjaro).

gozpak started as a fork of [KISS](https://kisslinux.github.io) by Dylan Araps
(a single POSIX shell script). It is now being **rewritten in Rust** in
[`rs/`](rs/), with a single-file `recipe.toml` format, `.gpkg` binary
packages and ed25519-signed repositories.

> **Status:** the Rust version is the active one and is **work in progress**
> (milestone M2b). The shell script `./gozpak` (v6.0.0) is **deprecated** and
> kept only for reference — see [Legacy shell version](#legacy-shell-version).

## Features

- **Rust, small dependency set** — `gozpak-core` has no external
  dependencies (SHA-256 is in-tree); the rest use a few pinned crates
  (`toml`/`serde`, `serde_json`, `zstd`, `ed25519-dalek`)
- **`recipe.toml`** — one TOML file per package: metadata, sources (tarballs,
  pinned git refs, patches), build system (`raw`, `autotools`, `meson`,
  `cmake`, `make`), install hooks and config backups
- **`.gpkg` packages** — `tar.zst` container with a `manifest.toml` and the
  `rootfs/` tree; the tree hash is verified before anything touches the system
- **Signed binary repos** — `index.json` + `index.json.sig` (ed25519), verified
  on every `sync`
- **Dependency resolution** — `install`/`get` pull in the full dependency
  closure (including `provides`) and install it in topological order, with
  version constraints (`zlib>=1.3`) and epoch-aware version comparison
- **Install transactions** — dependency and conflict checks, config-file
  backups, and post-install hooks (run once per transaction)

## Building

Requires Rust 1.85 or newer.

```sh
cd rs
cargo build --release     # → rs/target/release/gozpak
cargo test                # unit + integration tests for all crates

install -Dm755 target/release/gozpak /usr/bin/gozpak
```

## Quick start

### Install packages from a repository

```sh
# Configure a repository (one URL per line, '#' comments)
echo 'https://repo.example.org/stable' > /etc/gozpak/repos.conf

gozpak sync          # fetch + verify the repository index
gozpak get curl      # download and install curl and its dependencies
gozpak list          # show installed packages
```

### Build a package from a recipe

```sh
export GOZPAK_PATH=/path/to/gozjaro-repo/packages

gozpak build zlib    # → .gpkg in ~/.cache/gozpak/bin
gozpak build -b zlib # build only, keep DESTDIR, no .gpkg
```

## Commands

| Command | Short | Status | Description |
|---------|-------|--------|-------------|
| `gozpak sync [url…]` | `S` | ✅ | Fetch and signature-verify repository indices |
| `gozpak get <pkg…>` | `g` | ✅ | Download and install binary packages (with dependencies) |
| `gozpak install <pkg…>` | `i` | ✅ | Install packages (same path as `get`) |
| `gozpak fetch <pkg…>` | `f` | ✅ | Download binary packages without installing |
| `gozpak list [pkg…]` | `l` | ✅ | List installed packages |
| `gozpak list-remote [pkg…]` | `L` | ✅ | List packages in synced repositories |
| `gozpak search <pattern…>` | `s` | ✅ | Search recipes (shell-glob patterns) |
| `gozpak build <pkg…>` | `b` | ✅ | Build and pack `.gpkg` packages |
| `gozpak checksum <pkg…>` | `c` | ✅ | Hash sources for a recipe |
| `gozpak version` | `v` | ✅ | Print version |
| `gozpak remove <pkg…>` | `r` | 🚧 stub | Remove packages |
| `gozpak update` | `u` | 🚧 stub | Update repositories and the system |
| `gozpak upgrade` | `U` | 🚧 stub | Upgrade installed packages |
| `gozpak download <pkg…>` | `d` | 🚧 stub | Download sources |
| `gozpak alternatives` | `a` | 🚧 stub | List and swap file alternatives |

Stubs print `not implemented` and exit with status 1.

### Global flags

| Flag | Description |
|------|-------------|
| `-y`, `--yes` | Skip all confirmation prompts |
| `-b` | (`build` / `repo-build` only) build only, skip the pack step |

## Repository maintenance

Commands for the packaging host:

```sh
gozpak keygen ./gozjaro-repo.key             # new ed25519 keypair (prints the public key)
gozpak repo-build zlib curl                  # build + pack with repo metadata
gozpak index-build ./repo https://repo.example.org/stable   # → ./repo/index.json
GOZPAK_SIGN_KEY=./gozjaro-repo.key gozpak sign ./repo         # → ./repo/index.json.sig
```

The repository public key is currently embedded in the binary
(`gozpak_repo::EMBEDDED_PUBKEY`); per-repository keys are a planned follow-up.
See [docs/repos.md](docs/repos.md) for the index format and mirror setup.

## Recipe format

A recipe is a directory with one `recipe.toml` plus optional `patches/` and
local source files:

```toml
[package]
name    = "zlib"
version = "1.3.1"
release = 1
depends = []

[source]
urls = [
    { url = "https://zlib.net/zlib-$VERSION.tar.gz", sha256 = "..." },
]

[build]
system = "autotools"
```

Full schema: [docs/recipe-format.md](docs/recipe-format.md). Legacy KISS-style
3-file recipes (`build` / `version` / `depends` / `sources`) can still be
read, but not built; a migration tool is planned for M3.

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `GOZPAK_ROOT` | `/` | Target root: installed DB at `$GOZPAK_ROOT/var/db/gozpak/installed`, repo config at `$GOZPAK_ROOT/etc/gozpak/repos.conf` |
| `GOZPAK_PATH` | — | Colon-separated recipe search path |
| `GOZPAK_REPOS` | — | Colon-separated repository URLs (overrides `repos.conf`) |
| `GOZPAK_TMPDIR` | `~/.cache/gozpak/proc` | Per-run build/staging directory |
| `GOZPAK_SIGN_KEY` | — | Path to the 32-byte signing seed (for `sign`) |
| `XDG_CACHE_HOME` | `~/.cache` | Cache base: `gozpak/repos`, `gozpak/bin`, `gozpak/sources` |

`CFLAGS`, `CPPFLAGS` and `LDFLAGS` are passed through to builds.

## Roadmap

- **M2 (in progress)** — `remove` (needs the per-package file list in the
  installed DB), `update`/`upgrade`, `download`, `alternatives`, running
  `gozpak-*` extensions from `$PATH`
- **M3** — legacy recipe migration tool, `repo-push`, per-repository keys

Details and crate layout: [rs/README.md](rs/README.md).

## Legacy shell version

`./gozpak` is the original POSIX shell implementation (v6.0.0, KISS-derived).
It is **deprecated** and no longer used on Gozjaro; it stays in the tree for
reference while the Rust version reaches feature parity. The `contrib/`
extensions (`gozpak-ai-*`, `gozpak-repo-*`, `gozpak-owns`, …) were written for
it; the Rust CLI finds them on `$PATH` but does not run them yet.

## License

MIT — see [LICENSE](LICENSE). Original KISS code by Dylan Araps.
