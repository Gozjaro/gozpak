# Package Recipes

A gozpak package recipe is a directory containing a small set of files that
describe how to build and install software.

## File reference

### `build` (required)

An executable shell script that compiles and installs the package. Receives
the destination directory as `$1` (DESTDIR).

```sh
#!/bin/sh -e
./configure --prefix=/usr
make
make DESTDIR="$1" install
```

The build script runs with these environment variables set:

| Variable | Value |
|----------|-------|
| `$1` (DESTDIR) | Package staging directory |
| `$2` | Package version |
| `AR` | `ar` (or user override) |
| `CC` | `cc` (or user override) |
| `CXX` | `c++` (or user override) |
| `NM` | `nm` (or user override) |
| `RANLIB` | `ranlib` (or user override) |
| `GOPATH` | `$PWD/go` |
| `GOFLAGS` | `-trimpath -modcacherw` |
| `GOZPAK_ROOT` | System root |

### `version` (required)

A single line: `<version> <release>`.

```
1.3.1 1
```

Bump the release number when changing the recipe without a version change.

### `depends` (optional)

One dependency per line. Append ` make` for build-only dependencies.

```
openssl
zlib
cmake make
```

Lines starting with `#` are comments.

### `sources` (optional)

Source URLs, one per line. Supports variable substitution:

| Variable | Value |
|----------|-------|
| `VERSION` | Package version |
| `RELEASE` | Release number |
| `MAJOR` | Major version component |
| `MINOR` | Minor version component |
| `PATCH` | Patch version component |
| `PACKAGE` | Package name |

```
https://example.com/foo-VERSION.tar.gz
patches/fix-build.patch
```

Local paths (relative to the recipe directory) are also supported.
Git sources use the `git+` prefix:

```
git+https://github.com/user/repo#v1.0
```

### `checksums` (optional)

SHA256 checksums, one per source. Generate with `gozpak checksum <pkg>`.
Use `SKIP` to skip verification for a source.

### `meta` (optional)

Key=value metadata for extended features:

```
type=library
description=General-purpose compression library
license=Zlib
maintainer=user@example.com
post-install-cmd=ldconfig
```

#### Supported keys

| Key | Description |
|-----|-------------|
| `type` | Package type — triggers automatic post-install actions |
| `description` | Human-readable description (used in repo index) |
| `license` | SPDX license identifier |
| `maintainer` | Maintainer email |
| `post-install-cmd` | Custom command to run after install |

#### Type triggers

| Type | Post-install action |
|------|---------------------|
| `kernel-module` | `depmod -a` |
| `font` | `fc-cache -f` |
| `icon-theme` | `gtk-update-icon-cache` |
| `desktop` | `update-desktop-database` |
| `mime` | `update-mime-database` |
| `library` | `ldconfig` |

If `type` is not set, no automatic trigger runs. The `post-install-cmd` key
runs regardless of type (if present).

## Creating a recipe

### Manually

```sh
mkdir mypackage && cd mypackage

cat > build <<'EOF'
#!/bin/sh -e
./configure --prefix=/usr
make
make DESTDIR="$1" install
EOF
chmod +x build

echo '1.0.0 1' > version
echo 'https://example.com/mypackage-VERSION.tar.gz' > sources
echo 'zlib' > depends

cd ..
gozpak checksum mypackage
gozpak build mypackage
```

### With AI assistance

```sh
gozpak ai-new mypackage 1.0.0 https://example.com/mypackage-1.0.0.tar.gz
# Review generated files, then:
gozpak checksum mypackage
gozpak build mypackage
```

### With the boilerplate tool

```sh
gozpak new mypackage 1.0.0 https://example.com/mypackage-1.0.0.tar.gz
# Creates a skeleton with empty build script
```
