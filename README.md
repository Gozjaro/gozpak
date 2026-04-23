# gozpak

The package manager for [Gozjaro Linux](https://github.com/Gozjaro).

Forked from [KISS](https://kisslinux.github.io) by Dylan Araps. Rewritten and
extended with online repositories, AI-assisted recipe generation, and batch
build tooling.

## Features

- **Source builds** — build packages from source using simple 3-file recipes (`build`, `depends`, `version`)
- **Binary repos** — fetch and install pre-built packages from remote mirrors (`gozpak sync && gozpak get`)
- **AI integration** — auto-generate recipes, diagnose build failures, check upstream versions
- **Batch build** — tools for maintainers to build, index, and publish package repositories
- **Extended recipes** — optional `meta` file for post-install triggers (ldconfig, depmod, font cache, etc.)
- **Alternatives system** — handle file conflicts between packages gracefully
- **POSIX shell** — single-file package manager, ~2200 lines, no dependencies beyond a C compiler and coreutils

## Quick start

```sh
# Install gozpak
install -Dm755 gozpak /usr/bin/gozpak

# Install extensions (optional)
for f in contrib/gozpak-*; do install -Dm755 "$f" "/usr/bin/$(basename "$f")"; done
```

### Install packages from a repo

```sh
# Configure a repository
echo 'https://repo.gozjaro.org/stable' > /etc/gozpak/repos.conf

# Sync and install
gozpak sync
gozpak get firefox
```

### Build from source

```sh
# Create a recipe
gozpak ai-new zlib 1.3.1 https://zlib.net/zlib-1.3.1.tar.gz

# Review, checksum, build, install
gozpak checksum zlib
gozpak build zlib
gozpak install zlib
```

## Commands

| Command | Short | Description |
|---------|-------|-------------|
| `gozpak sync` | `S` | Sync remote repository indices |
| `gozpak get <pkg>` | `g` | Fetch and install binary packages |
| `gozpak fetch <pkg>` | `f` | Download binary packages (no install) |
| `gozpak build <pkg>` | `b` | Build packages from source |
| `gozpak install <pkg>` | `i` | Install a built package tarball |
| `gozpak remove <pkg>` | `r` | Remove packages |
| `gozpak search <pkg>` | `s` | Search for packages in repositories |
| `gozpak list` | `l` | List installed packages |
| `gozpak list-remote` | `L` | List packages in remote repos |
| `gozpak update` | `u` | Update repos and system |
| `gozpak upgrade` | `U` | Upgrade installed packages |
| `gozpak alternatives` | `a` | List and swap file alternatives |
| `gozpak checksum <pkg>` | `c` | Generate checksums |
| `gozpak download <pkg>` | `d` | Download sources |
| `gozpak version` | `v` | Print version |

### Global flags

| Flag | Description |
|------|-------------|
| `-y`, `--yes` | Skip all confirmation prompts |

## AI extensions

Generate recipes, fix builds, and check versions with AI assistance.
Supports multiple backends: Ollama, llama.cpp, Claude, OpenAI, and generic OAuth.

```sh
# Configure (pick one)
export GOZPAK_AI_BACKEND=ollama                              # local (default)
export GOZPAK_AI_BACKEND=claude GOZPAK_AI_API_KEY=sk-...     # Anthropic
export GOZPAK_AI_BACKEND=openai GOZPAK_AI_API_KEY=sk-...     # OpenAI

# Generate a recipe from a source tarball
gozpak ai-new curl 8.7.1 https://curl.se/download/curl-8.7.1.tar.xz

# Diagnose a build failure
gozpak ai-fix curl

# Check for upstream updates
gozpak ai-update curl
```

See [docs/ai.md](docs/ai.md) for full configuration reference.

## Repository management

Tools for maintainers to build and publish binary package repositories.

```sh
# Batch-build packages
gozpak repo-build -f packages.list -o ./repo

# Generate repository index
gozpak repo-index ./repo

# Push to a mirror
GOZPAK_REPO_PUSH_DEST=user@mirror:/srv/repo gozpak repo-push ./repo
```

See [docs/repos.md](docs/repos.md) for the repo index format and mirror setup.

## Package recipe format

A package recipe is a directory containing:

| File | Required | Description |
|------|----------|-------------|
| `build` | yes | Executable shell script; `$1` = DESTDIR |
| `version` | yes | `<version> <release>` (e.g. `1.3.1 1`) |
| `depends` | no | One dependency per line; `make` suffix for build-only |
| `sources` | no | Source URLs, one per line |
| `checksums` | no | SHA256 checksums for sources |
| `meta` | no | Key=value metadata (type, description, license, etc.) |

### Meta file

Optional metadata for post-install triggers:

```
type=library
description=General-purpose compression library
license=Zlib
maintainer=user@example.com
```

Supported types: `kernel-module`, `font`, `icon-theme`, `desktop`, `mime`, `library`.

See [docs/recipes.md](docs/recipes.md) for details.

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `GOZPAK_PATH` | — | Colon-separated source repo paths |
| `GOZPAK_ROOT` | `/` | System root (for chroot builds) |
| `GOZPAK_REPOS` | — | Colon-separated remote repo URLs |
| `GOZPAK_COMPRESS` | `gz` | Tarball compression (gz, xz, zst, bz2, lz, lzma) |
| `GOZPAK_PROMPT` | `1` | Set to `0` to skip prompts |
| `GOZPAK_FORCE` | `0` | Force install/remove |
| `GOZPAK_STRIP` | `1` | Strip binaries (0 to disable) |
| `GOZPAK_DEBUG` | `0` | Keep build dirs on exit |
| `GOZPAK_KEEPLOG` | `0` | Keep build logs on success |
| `GOZPAK_COLOR` | `1` | Colored output (0 to disable) |
| `GOZPAK_SU` | auto | Privilege escalation tool |
| `GOZPAK_AI_BACKEND` | `ollama` | AI backend (ollama, llamacpp, claude, openai, oauth) |
| `GOZPAK_AI_API_KEY` | — | API key for cloud AI backends |
| `GOZPAK_AI_MODEL` | auto | Model name override |

## License

MIT — see [LICENSE](LICENSE). Original KISS code by Dylan Araps.
