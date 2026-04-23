# Repository Management

gozpak supports fetching pre-built binary packages from remote HTTP
repositories. This document covers the repo index format, mirror configuration,
and tools for building and publishing repositories.

## Repository layout

A gozpak repository is an HTTP-accessible directory containing:

```
repo.index                          # package database
curl@8.7.1-1.tar.gz                 # binary package tarballs
zlib@1.3.1-1.tar.gz
openssl@3.3.0-1.tar.gz
...
```

## Index format

`repo.index` is a plain text file with one package per line. Fields are
tab-separated:

```
name	version	release	sha256	size	depends	description
```

| Field | Description |
|-------|-------------|
| `name` | Package name |
| `version` | Version string |
| `release` | Release number |
| `sha256` | SHA256 checksum of the tarball |
| `size` | File size in bytes |
| `depends` | Comma-separated runtime dependencies, or `-` if none |
| `description` | Human-readable description (from `meta` file, may be empty) |

Example:

```
curl	8.7.1	1	a1b2c3d4...	1234567	openssl,zlib	Command-line URL transfer tool
zlib	1.3.1	1	e5f6a7b8...	654321	-	Compression library
```

## User configuration

### Environment variable

```sh
export GOZPAK_REPOS="https://repo.gozjaro.org/stable:https://mirror.example.com/gozjaro"
```

### Config file

`/etc/gozpak/repos.conf` — one URL per line:

```
# Official repository
https://repo.gozjaro.org/stable

# Community mirror
https://mirror.example.com/gozjaro
```

The environment variable takes priority over the config file.

## User commands

```sh
# Sync repository indices
gozpak sync

# Search and install
gozpak list-remote          # list all available packages
gozpak get curl             # fetch + verify + install
gozpak get -y curl openssl  # non-interactive

# Download without installing
gozpak fetch curl
```

Packages are cached in `~/.cache/gozpak/bin/`. Re-running `gozpak get` with
an already-cached and verified tarball skips the download.

Dependencies listed in the repo index are resolved recursively.

## Building a repository

### Step 1: Batch build

```sh
# Build from a list
gozpak repo-build -f packages.list -o ./repo

# Or specify packages directly
gozpak repo-build -o ./repo curl zlib openssl
```

Options:

| Flag | Description |
|------|-------------|
| `-f <file>` | Read package names from file (one per line, `#` comments) |
| `-o <dir>` | Output directory (default: `./repo` or `$GOZPAK_REPO_OUT`) |

The tool runs `gozpak build` for each package and copies the resulting
tarballs from `~/.cache/gozpak/bin/` to the output directory.

### Step 2: Generate index

```sh
gozpak repo-index ./repo
```

This scans all `*.tar.*` files in the directory, extracts metadata from
inside each tarball (depends, description from meta file), computes SHA256
checksums, and writes `repo.index`.

### Step 3: Push to mirror

```sh
# rsync (default)
GOZPAK_REPO_PUSH_DEST=user@mirror:/srv/repo gozpak repo-push ./repo

# scp
GOZPAK_REPO_PUSH_CMD=scp GOZPAK_REPO_PUSH_DEST=user@mirror:/srv/repo gozpak repo-push

# rclone (for S3, GCS, etc.)
GOZPAK_REPO_PUSH_CMD=rclone GOZPAK_REPO_PUSH_DEST=s3:bucket/repo gozpak repo-push
```

| Variable | Default | Description |
|----------|---------|-------------|
| `GOZPAK_REPO_PUSH_CMD` | `rsync` | Push tool (rsync, scp, rclone) |
| `GOZPAK_REPO_PUSH_DEST` | — | Remote destination (required) |
| `GOZPAK_REPO_PUSH_OPTS` | — | Extra flags for the push command |
| `GOZPAK_REPO_OUT` | `./repo` | Default repo directory |

## Hosting

Any HTTP server works. Minimal nginx example:

```nginx
server {
    listen 80;
    server_name repo.gozjaro.org;
    root /srv/repo;
    autoindex on;
}
```

Or use GitHub Releases, S3 static hosting, or any CDN that serves files.
