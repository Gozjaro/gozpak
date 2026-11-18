# gozpak recipe format (26.11, single-file TOML) — v1.2

Supersedes the kiss-style 3-file recipe (`build` + `version` + `depends` + `sources`).
v1.1 folds in the review pass (2026-11-17): version constraints + epoch + vercmp,
multi-file sources, hook safety, config backup, raw-shell semantics.
Legacy 3-file remains readable (`Recipe::load_legacy`) until the M3 migration tool.

A recipe is a directory containing one `recipe.toml` (TOML 1.0) plus optional
`patches/` and local source files.

## Schema

```toml
[package]
name     = "openssl"            # required, [a-z0-9-]+
version  = "3.5.1"             # required
release  = 1                   # required, bump when the recipe itself changes
epoch    = 0                   # optional; raises precedence when upstream changes
                                 # versioning schemes (e.g. 1.x -> 0.9); compared FIRST
arch     = "x86_64"            # default. "any" ONLY for arch-independent packages
                                 # (data, docs, wallpapers) — must be written explicitly
desc     = "..."
license  = "Apache-2.0"
url      = "https://www.openssl.org/"
# dependency-solver fields. Constraint syntax is pacman-style:
#   "name"          satisfies any version
#   "zlib>=1.3"    op in { =, <, >, <=, >= }
depends        = ["zlib>=1.3", "pcre2"]
build_depends  = ["cmake>=3.16", "make"]
provides       = []            # virtual packages, same syntax ("libssl=3")
conflicts      = []            # "name" or "name<ver"
replaces       = []            # "name" — this package supersedes it

[source]
# multiple files: array of { url, sha256 }; sha256 REQUIRED for non-git sources
urls = [
    { url = "https://www.openssl.org/source/openssl-$VERSION.tar.xz", sha256 = "..." },
]
# archives (tar.xz/tar.gz/tgz/tar.bz2/tbz2/tar) are unpacked (top-level dir
# stripped); any other file (a prebuilt binary, a patch used directly) is
# placed in the source directory as-is under its file name, so `raw`
# build steps can reference it by that name:
# urls = [ { url = "https://…/gozpak-x86_64", sha256 = "…" } ]
# per-entry opt-out: set strip_topdir = false on an archive entry to keep
# its top-level dir in place (e.g. a source tree + a boot JDK shipped as
# two tarballs whose build steps reference the top dirs by name):
# urls = [
#     { url = "…/src.tar.gz", sha256 = "…", strip_topdir = false },
#     { url = "…/bootjdk.tar.gz", sha256 = "…", strip_topdir = false },
# ]
# OR a git source — pinned ref is MANDATORY (commit hash or tag, no branches):
# git = "git+https://github.com/org/repo#d41d8cd9"
patches = ["patches/fix-build.patch"]   # applied in order, relative to recipe dir

[build]
system = "autotools"          # "raw" | "autotools" | "meson" | "cmake" | "make"
# system = "raw": steps is ONE shell script — a single `sh -eu` process,
#   working dir = unpacked source tree. cd/export/variables PERSIST across
#   lines. The staging prefix is provided as the `DESTDIR` ENV VAR (plus
#   `JOBS`, `RECIPE_DIR` = the recipe's own directory, and the subst names
#   VERSION/EPOCH/...); NO positional args are passed. `CFLAGS`/`CPPFLAGS`/
#   `LDFLAGS` are always exported (LFS-standard defaults
#   `-march=x86-64 -O2 -pipe` / empty / empty, host env wins), so recipes
#   may reference them under `set -u`. Legacy LFS `build` files used
#   positional `$1` — migrated recipes rewrite it as `$DESTDIR`
#   (mechanical, one sed); the builder does not re-introduce the
#   positional convention.
steps = [
    "./configure --prefix=/usr --shared",
    "make -j$JOBS",
    "make DESTDIR=$DESTDIR install",
]
# system = "meson"/"cmake"/"autotools"/"make": builder drives the standard flow:
meson_args     = ["-Dssl=true", "-Dtests=false"]
cmake_args     = ["-DCMAKE_INSTALL_PREFIX=/usr"]
configure_args = ["--prefix=/usr"]
make_target    = "install"

[install]
# post-install hooks: REGISTRY NAMES ONLY, no free-form shell.
# Registry: ldconfig, update-mime-database, gdk-pixbuf-query-loaders,
#           desktop-database, fontconfig, shared-mime-info
# Each hook runs ONCE per transaction (install 50 libs -> ldconfig runs once).
# Optional path filter so a hook only fires when relevant files land:
post = [
    "ldconfig",
    { hook = "update-mime-database", match = "/usr/share/mime/**" },
]
files  = "auto"
backup = ["etc/sysctl.conf"]   # user-modified configs: on upgrade the packaged
                                 # copy becomes <file>.pkgnew, the live file is
                                 # kept, and the CLI warns

[meta]                        # optional, free-form
maintainer = "..."
repo = "main"
notes = "..."
```

## Variable substitution

In `[source].urls[].url`/`sha256` and `[build].*`:

| Variable | Value |
|----------|-------|
| `$VERSION`  | full version |
| `$RELEASE`  | release number |
| `$MAJOR`/`$MINOR`/`$PATCH` | version components |
| `$EPOCH`    | epoch |
| `$PACKAGE`  | package name |
| `$DESTDIR`  | staging dir (build/install phase) |
| `$JOBS`     | parallelism (build phase) |

## Version comparison

Pacman `vercmp` semantics, three keys compared in order:
`epoch` (numeric) → `version` (numeric chunks; release tags rc/a/b/alpha sort
BELOW the final release, e.g. `3.5.1rc1 < 3.5.1`) → `release` (numeric).
The solver and the index both use this comparator; the constraint operators
above apply to this ordering.

## Binary packages (.gpkg)

```
<name>-<epoch?>:<version>-<release>.gpkg      # tar.zst, pacman-style name
├── manifest.toml        # serialized [package] + [install] + arch +
│                        # sha256(rootfs), file list, repo, built-by
└── rootfs/              # staged install tree
```

- Filename is pacman-style: `openssl-3.5.1-1.gpkg`, with epoch `libfoo-1:2.0-1.gpkg`
  (the `epoch:` token only when epoch > 0). The `:` makes epoch unambiguous;
  package names keep their dashes — **the filename is NEVER parsed back;
  the index / manifest.toml are the source of truth** (a name with dashes
  would make any split-on-dash parser lie).
- `.gpkg` is **tar.zst** (fast extract, smaller). Consequence: `zstd` + libzstd
  enter the 26.11 base system; the repo crate may use the `zstd` crate (M2 only).

## Repository indexes

Two indexes, same fields, different containers:

1. **Source recipe repo** (`gozjaro-pkgs`): text `repo.index` (existing
   contrib/gozpak-repo-index format), line extended to:
   `name version release epoch arch sha256 url depends provides conflicts`
   — dependency lists space-free, multiple entries joined with `|`
   (constraint syntax contains no spaces). Empty list = `-`.
2. **Binary repo** (GitHub Releases, #94): `index.json` + `index.json.sig`
   (ed25519, pubkey embedded in gozpak — MANDATORY; per-package
   `.gpkg.sig` optional, off by default).
   One JSON object per package:
   `{ name, version, release, epoch, arch, sha256, size, url,
     depends[], provides[], conflicts[], replaces[] }`.

Rule: **the index carries the solver's input** (depends/provides/conflicts
above). The dependency solver MUST NOT need to download .gpkg manifests to
solve — manifests are read at install time only.

## Legacy (kiss 3-file) — read-only until M3

`build` (sh, `$1`=DESTDIR, `$2`=version), `version` (`<ver> <rel>`),
`depends` (one per line, trailing ` make` = build-only), `sources` (URLs +
`git+`), `meta` (key=value). `Recipe::load` prefers `recipe.toml`; a legacy
dir loads as `RecipeKind::Legacy` and the CLI prints a one-line legacy warning.

Migration `gozpak migrate` (M3): `build` script → `[build] system="raw"` steps
(verbatim, single-shell semantics preserved); ` make` suffix in `depends`
splits into `build_depends`; `sources` URLs → `[source].urls` with sha256
computed from the fetched files; `git+` requires a pinned ref (branch → tag
resolution, else FAIL with a hint).
