//! M1 command implementations: version, list, search, checksum, usage,
//! and the not-implemented stubs. M2a adds `build`/`repo-build`
//! (stage + build + pack via `gozpak-build`). M2b adds the binary-repo
//! side: `sync`/`list-remote`/`fetch`/`get`/`install` (verified
//! index + `.gpkg` install) and the packaging-host tools
//! `index-build`/`sign`/`keygen`.

use std::path::{Path, PathBuf};

use gozpak_core::{paths, sha256};
use gozpak_recipe::{subst, Recipe, RecipeKind, Source};
use gozpak_repo::RemoteError;

use crate::log::{die, log_line, war};
use crate::Ctx;

/// Print the usage block (shell `args()` no-arg help).
pub fn usage() {
    log_line("gozpak [a|b|c|d|f|g|i|l|L|r|s|S|u|U|v] [pkg]...", "");
    log_line("alternatives List and swap alternatives", "");
    log_line("build        Build and pack packages (no repo metadata)", "");
    log_line("checksum     Generate checksums", "");
    log_line("download     Download sources", "");
    log_line("fetch        Download binary packages (no install)", "");
    log_line("get          Fetch and install binary packages", "");
    log_line("index-build  Build index.json from a directory of .gpkg files", "");
    log_line("install      Install packages", "");
    log_line("keygen       Generate an ed25519 repository keypair", "");
    log_line("list         List installed packages", "");
    log_line("list-remote  List packages in remote repositories", "");
    log_line("remove       Remove packages", "");
    log_line("repo-build [-b] pkg...   Build and pack .gpkg packages (repo metadata)", "");
    log_line("search       Search for packages", "");
    log_line("sign         Sign index.json (index.json.sig, ed25519)", "");
    log_line("sync         Sync remote repository indices (signature-verified)", "");
    log_line("update       Update the system and repositories", "");
    log_line("upgrade      Update the system", "");
    log_line("version      Package manager version", "");
    log_line("", "");
    log_line("Global flags:", "");
    log_line("  -y, --yes  Skip all confirmation prompts", "");
    log_line("  -b         (repo-build only) build only, skip the pack step", "");
    log_line("", "");
    log_line(
        "M2b note: sync/list-remote/fetch/get/install/index-build/sign/\
         keygen are implemented; remove/download/update/upgrade/alternatives are stubs.",
        "",
    );
}

/// `l`/`list` — list installed packages (or specific ones) from the DB.
pub fn list(ctx: &Ctx, names: &[String]) -> i32 {
    if names.is_empty() {
        for pkg in ctx.db.list() {
            println!("{} {}", pkg.name, pkg.version.display());
        }
        return 0;
    }
    for name in names {
        match ctx.db.version_of(name) {
            Some(v) => println!("{name} {}", v.display()),
            None => die(name, "not found"),
        }
    }
    0
}

/// `s`/`search` — glob recipe directories under GOZPAK_PATH (+ the
/// installed DB root, like the shell's `_pkg_find` which appends
/// `$sys_db`). With no arguments the shell version searches the current
/// directory's basename instead (see [`pwd_fallback_names`]).
pub fn search(ctx: &Ctx, names: &[String]) -> i32 {
    let (queries, extra_base) = pwd_fallback(names);
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(p) = extra_base {
        bases.push(p);
    }
    bases.extend(ctx.recipe_paths.iter().cloned());
    bases.push(ctx.db.root().to_path_buf());

    for q in &queries {
        let hits = Recipe::search(q, &bases);
        if hits.is_empty() {
            die(q, "not found");
        }
        for h in &hits {
            println!("{}", h.display());
        }
    }
    0
}

/// `c`/`checksum` — generate/verify checksums for a recipe's sources.
///
/// M1 has no network: only sources resolvable on disk are hashable —
/// local files (relative to the recipe dir) and previously cached
/// downloads under the source cache. Git sources are never checksummed
/// (same as the shell version).
pub fn checksum(ctx: &Ctx, names: &[String]) -> i32 {
    let (queries, extra_base) = pwd_fallback(names);
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(p) = extra_base {
        bases.push(p);
    }
    bases.extend(ctx.recipe_paths.iter().cloned());

    for q in &queries {
        let dir = match Recipe::find(q, &bases) {
            Some(d) => d,
            None => die(q, "not found"),
        };
        let recipe = match Recipe::load(&dir) {
            Ok(r) => r,
            Err(e) => die(q, &format!("invalid recipe: {e}")),
        };
        if recipe.kind == RecipeKind::Legacy {
            log_line(&recipe.name, "using legacy recipe format (recipe.toml is preferred)");
        }
        run_checksum(&recipe, ctx);
    }
    0
}

/// `b`/`build` and `repo-build` — the M2a build pipeline:
/// stage sources, run the `[build]` section, pack a `.gpkg` (tar.zst).
///
/// - `repo_meta = true` (`repo-build`): the manifest carries the repo
///   metadata (`[manifest].repo` + `built_by`).
/// - `pack = false` (`repo-build -b`): build only — the DESTDIR tree
///   is left in the work dir, no `.gpkg` is written.
///
/// Recipes load from `GOZPAK_PATH` (plus the current directory's
/// parent, like every other command). The finished `.gpkg` lands in
/// the binary cache (`<cache>/gozpak/bin`); stdout gets one
/// sha256sum-style line `<sha256>  <path>` per packed package.
pub fn build(ctx: &Ctx, names: &[String], repo_meta: bool, pack: bool) -> i32 {
    let (queries, extra_base) = pwd_fallback(names);
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(p) = extra_base {
        bases.push(p);
    }
    bases.extend(ctx.recipe_paths.iter().cloned());

    for q in &queries {
        let dir = match Recipe::find(q, &bases) {
            Some(d) => d,
            None => die(q, "not found"),
        };
        let recipe = match Recipe::load(&dir) {
            Ok(r) => r,
            Err(e) => die(q, &format!("invalid recipe: {e}")),
        };
        if recipe.kind == RecipeKind::Legacy {
            die(
                &recipe.name,
                "legacy recipe cannot be built in M2a (recipe.toml only; \
                 migration is M3)",
            );
        }
        for w in &recipe.warnings {
            war(&recipe.name, w);
        }

        let opts = gozpak_build::Opts {
            work_root: proc_root().join(std::process::id().to_string()),
            source_cache: paths::source_cache_dir(),
            out_dir: paths::bin_cache_dir(),
            repo_meta,
        };

        log_line(&recipe.name, "staging sources");
        let report = match gozpak_build::pipeline(&recipe, &opts, pack) {
            Ok(r) => r,
            Err(e) => die(&recipe.name, &e.to_string()),
        };
        log_line(&recipe.name, "built");

        match report.packed {
            Some(packed) => {
                log_line(&recipe.name, &format!("packed {}", packed.gpkg.display()));
                println!("{}  {}", packed.gpkg_sha256, packed.gpkg.display());
            }
            None => {
                log_line(
                    &recipe.name,
                    &format!(
                        "build-only: DESTDIR at {} (no .gpkg)",
                        report.dest.display()
                    ),
                );
            }
        }
    }
    0
}

/// The per-run process temp root (shell `create_tmp_dirs` `proc`):
/// `$GOZPAK_TMPDIR` when set, else `<cache>/gozpak/proc`.
fn proc_root() -> PathBuf {
    if let Ok(v) = std::env::var("GOZPAK_TMPDIR") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    let root = paths::source_cache_dir()
        .parent()
        .map(|p| p.join("proc"))
        .unwrap_or_else(|| PathBuf::from("/tmp/gozpak/proc"));
    gc_stale_proc(&root);
    root
}

/// Reclaim scratch dirs left by dead runs so sequential builds don't pile
/// up on small volumes (kernel builds peak ~23G against a 40G partition).
/// Each run gets a unique `proc/<pid>`; anything modified more than 4h
/// ago is safe — a whole build fits inside that window, so nothing a
/// concurrent live build is using is ever touched.
fn gc_stale_proc(root: &Path) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let Ok(md) = e.metadata() else {
            continue;
        };
        let Ok(mtime) = md.modified() else {
            continue;
        };
        let stale = mtime
            .elapsed()
            .map(|d| d > std::time::Duration::from_secs(4 * 3600))
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

// ---------------------------------------------------------------------------
// M2b: the binary-repo side.
// ---------------------------------------------------------------------------

/// The embedded repo public key, decoded (the M2b trust root; a
/// per-repo key override is a follow-up, not part of the spec).
fn repo_pubkey() -> Result<Vec<u8>, RemoteError> {
    gozpak_repo::signing::hex_key(gozpak_repo::EMBEDDED_PUBKEY, "embedded public key")
        .map_err(|_| RemoteError::IndexParse("corrupt embedded public key".into()))
}

/// `S`/`sync` — fetch + ed25519-verify + cache the index of every
/// configured repo (or the explicit URLs given as arguments).
pub fn sync(ctx: &Ctx, urls: &[String]) -> i32 {
    let urls = if urls.is_empty() {
        gozpak_repo::urls::configured_repos(&ctx.root)
    } else {
        urls.to_vec()
    };
    if urls.is_empty() {
        die("sync", "no repositories configured (GOZPAK_REPOS or $GOZPAK_ROOT/etc/gozpak/repos.conf)");
    }
    let pubkey = match repo_pubkey() {
        Ok(k) => k,
        Err(e) => die("sync", &e.to_string()),
    };
    let cache = paths::repo_cache_dir();
    let mut ok = 0usize;
    for url in &urls {
        match gozpak_repo::sync_repo(url, &cache, &pubkey) {
            Ok(entries) => {
                ok += 1;
                log_line(url, &format!("{} packages (index verified)", entries.len()));
            }
            Err(e) => {
                // One bad repo must not poison the others.
                war(url, &e.to_string());
            }
        }
    }
    if ok == 0 {
        return 1;
    }
    0
}

/// All synced repos: their resolved bases plus the cached,
/// signature-verified index entries.
fn merged_index() -> (Vec<gozpak_repo::RepoBase>, Vec<gozpak_repo::BinaryEntry>) {
    let mut bases = Vec::new();
    let mut entries = Vec::new();
    for (base, e) in gozpak_repo::cached_repos(&paths::repo_cache_dir()) {
        bases.push(base);
        entries.extend(e);
    }
    (bases, entries)
}

/// `L`/`list-remote` — the packages in the cached (verified) indices.
/// With arguments, only those names are shown; unknown ones die.
/// The same package may be served by more than one synced channel
/// (LAN mirror + GitHub fallback of the same `stable` repo): dedupe
/// identical (name, version, release) entries so each shows once,
/// while genuinely different versions across channels still both
/// appear. Output is deterministic: sorted by name, then version.
pub fn list_remote(_ctx: &Ctx, names: &[String]) -> i32 {
    let (_, entries) = merged_index();
    if entries.is_empty() {
        die("list-remote", "no synced repositories (run 'gozpak sync' first)");
    }
    let mut shown: Vec<(String, u64, String, String, String)> = Vec::new();
    for e in entries.iter().filter(|e| names.is_empty() || names.contains(&e.name)) {
        let rel = e.release.to_string();
        let vr = gozpak_core::VersionRel::new(e.version.clone(), rel.clone());
        shown.push((e.name.clone(), e.epoch, e.version.clone(), rel, vr.display()));
    }
    shown.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| {
            gozpak_core::pkgver_cmp(
                &format!("{}:{}-{}", a.1, a.2, a.3),
                &format!("{}:{}-{}", b.1, b.2, b.3),
            )
        })
    });
    shown.dedup();
    for (name, _, _, _, disp) in &shown {
        println!("{} {}", name, disp);
    }
    if !names.is_empty() {
        let known: Vec<&String> = entries.iter().map(|e| &e.name).collect();
        for n in names {
            if !known.iter().any(|k| *k == n) {
                die(n, "not found in any synced repository");
            }
        }
    }
    0
}

/// Pick, per requested name, the highest (epoch, version, release)
/// entry across all cached repos.
fn resolve_wanted(
    names: &[String],
    entries: &[gozpak_repo::BinaryEntry],
) -> Vec<gozpak_repo::BinaryEntry> {
    use gozpak_repo::BinaryEntry;
    let mut out = Vec::new();
    for n in names {
        let candidates: Vec<&BinaryEntry> = entries.iter().filter(|e| &e.name == n).collect();
        if candidates.is_empty() {
            die(n, "not found in any synced repository");
        }
        let best = candidates.iter().max_by(|a, b| {
            gozpak_core::pkgver_cmp(
                &format!("{}:{}-{}", a.epoch, a.version, a.release),
                &format!("{}:{}-{}", b.epoch, b.version, b.release),
            )
        });
        out.push(best.cloned().expect("candidates non-empty").clone());
    }
    out
}

/// Locate one entry's `.gpkg`: full-URL entries download through the
/// asset channel; bare-filename entries are read from the synced local
/// (relay) repo. Either way the result lands in the bin cache with its
/// sha256 checked against the verified index.
fn ensure_pkg(_ctx: &Ctx, entry: &gozpak_repo::BinaryEntry) -> PathBuf {
    let (bases, _) = merged_index();
    // A bare-filename entry needs the local repo it was indexed from;
    // a full-URL entry ignores the base (its own URL is the source).
    let local_base = bases
        .iter()
        .find(|b| matches!(b, gozpak_repo::RepoBase::Local(_)));
    let base = if entry.url.contains("://") {
        gozpak_repo::RepoBase::Download {
            url: entry.url.clone(),
            tag: String::new(),
        }
    } else {
        match local_base {
            Some(b) => b.clone(),
            None => die(&entry.name, "bare-filename index entry but no local repo was synced"),
        }
    };
    match gozpak_repo::pkg_location(entry, &base, &paths::bin_cache_dir()) {
        Ok(p) => p,
        Err(e) => die(&entry.name, &e.to_string()),
    }
}

/// `f`/`fetch` — download the wanted packages into the bin cache
/// (sha256-verified against the index). No install.
pub fn fetch(ctx: &Ctx, names: &[String]) -> i32 {
    let (_, entries) = merged_index();
    if entries.is_empty() {
        die("fetch", "no synced repositories (run 'gozpak sync' first)");
    }
    for e in resolve_wanted(names, &entries) {
        let p = ensure_pkg(ctx, &e);
        let sum = sha256::hash_file(&p).unwrap_or_default();
        println!("{sum}  {}", p.display());
    }
    0
}

/// `i`/`install` and `g`/`get` — one install transaction:
///
/// 1. resolve + locate every wanted package (download on demand),
/// 2. dependency check against the installed DB + the transaction,
/// 3. unpack + verify (tree hash) each package,
/// 4. install into `ctx.root` (config `.pkgnew` backup),
/// 5. run the transaction's deduplicated post-hooks,
/// 6. record the packages in the installed DB.
///
/// `get` is `install` with the download step guaranteed first — the
/// same code path (`ensure_pkg` reuses cached files).
pub fn install(ctx: &Ctx, names: &[String]) -> i32 {
    use gozpak_install::{
        check_deps, installed_list, record_install, run_hooks, transaction_hooks, unpack_gpkg,
        verify_staged,
    };

    let (_, entries) = merged_index();
    if entries.is_empty() {
        die("install", "no synced repositories (run 'gozpak sync' first)");
    }
    let wanted = resolve_wanted(names, &entries);

    // Dependency gate BEFORE touching the system.
    let db = &ctx.db;
    let installed = installed_list(db);
    if let Err(e) = check_deps(&wanted, &installed) {
        die(&wanted[0].name, &e.to_string());
    }

    let tmp = proc_root();
    let _ = std::fs::create_dir_all(&tmp);
    let staging = tmp.join(format!("install-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&staging);

    // 1+3: locate, unpack, verify.
    let mut staged = Vec::new();
    for e in &wanted {
        let pkg = ensure_pkg(ctx, e);
        let s =
            unpack_gpkg(&pkg, &staging).unwrap_or_else(|err| die(&e.name, &err.to_string()));
        verify_staged(&s).unwrap_or_else(|err| die(&e.name, &err.to_string()));
        log_line(
            &e.name,
            &format!("verified {} ({} KB)", pkg.file_name().unwrap().to_string_lossy(), e.size.div_euclid(1024)),
        );
        staged.push(s);
    }

    // 4: install the trees.
    let mut pkgnew_all = Vec::new();
    for s in &staged {
        match gozpak_install::install_staged(&ctx.root, s) {
            Ok(pn) => pkgnew_all.extend(pn),
            Err(e) => die(s.name(), &format!("install failed: {e}")),
        }
    }
    for p in &pkgnew_all {
        war("config", &format!("saved {} (the package copy is now live)", p.display()));
    }

    // 5: the transaction's hooks (deduplicated across packages).
    let hook_pkgs: Vec<(gozpak_build::Manifest, &Vec<String>)> = staged
        .iter()
        .map(|s| (s.manifest.clone(), &s.manifest.manifest.files))
        .collect();
    let hooks = transaction_hooks(&hook_pkgs);
    for outcome in run_hooks(&ctx.root, &hooks) {
        match outcome {
            gozpak_install::HookOutcome::Ran { command } => log_line("hook", &format!("ran {command}")),
            gozpak_install::HookOutcome::Skipped { name, reason } => {
                war("hook", &format!("{name} skipped ({reason})"))
            }
            gozpak_install::HookOutcome::Failed { command, status } => {
                war("hook", &format!("{command} failed ({status})"))
            }
        }
    }

    // 6: the installed DB.
    for s in &staged {
        let p = &s.manifest.package;
        record_install(db, &p.name, &p.version, &p.release, &s.manifest.manifest.files)
            .unwrap_or_else(|e| die(&p.name, &format!("db record: {e}")));
        log_line(&p.name, &format!("installed {}-{}", p.version, p.release));
    }
    0
}

/// `index-build <dir> [base-url]` — packaging host: the `index.json`
/// for a directory of finished `.gpkg` packages.
pub fn index_build(_ctx: &Ctx, names: &[String]) -> i32 {
    let Some(dir) = names.first() else {
        die("index-build", "usage: gozpak index-build <dir> [base-url]");
    };
    let base_url = names.get(1).map(String::as_str).unwrap_or("");
    let entries = match gozpak_build::build_index(std::path::Path::new(dir), base_url) {
        Ok(e) => e,
        Err(e) => die(dir, &e.to_string()),
    };
    match gozpak_build::write_index(std::path::Path::new(dir), &entries) {
        Ok(p) => {
            println!("{}  {} packages", entries.len(), p.display());
        }
        Err(e) => die(dir, &e.to_string()),
    }
    0
}

/// `sign <dir>` — packaging host: `index.json` → `index.json.sig`
/// (ed25519; the seed comes from the `GOZPAK_SIGN_KEY` env var, the
/// path of the 32-byte seed file on the packaging host).
pub fn sign(_ctx: &Ctx, names: &[String]) -> i32 {
    let Some(dir) = names.first() else {
        die("sign", "usage: gozpak sign <dir> (GOZPAK_SIGN_KEY=<seed file>)");
    };
    let key_path = match std::env::var("GOZPAK_SIGN_KEY") {
        Ok(p) => p,
        Err(_) => die("sign", "GOZPAK_SIGN_KEY (path to the 32-byte seed) is not set"),
    };
    let seed = match gozpak_repo::signing::read_key_file(
        std::path::Path::new(&key_path),
        "signing key",
    ) {
        Ok(k) => k,
        Err(e) => die("sign", &e.to_string()),
    };
    match gozpak_build::sign_index(std::path::Path::new(dir), &seed) {
        Ok(p) => println!(
            "signed {} -> {}",
            std::path::Path::new(dir).join("index.json").display(),
            p.display()
        ),
        Err(e) => die(dir, &e.to_string()),
    }
    0
}

/// `keygen [out]` — generate the repository keypair. The seed file is
/// written to `out` (default `./gozjaro-repo.key`); the public key is
/// printed for embedding in `gozpak_repo::EMBEDDED_PUBKEY`.
pub fn keygen(_ctx: &Ctx, names: &[String]) -> i32 {
    let out = names.first().map(String::as_str).unwrap_or("./gozjaro-repo.key");
    let (seed, pubkey) = gozpak_repo::signing::keygen();
    if let Err(e) = gozpak_repo::signing::write_key_file(std::path::Path::new(out), &seed) {
        die("keygen", &e.to_string());
    }
    let pub_hex = gozpak_repo::signing::to_hex(&pubkey);
    println!("public key: {pub_hex}");
    println!("seed file:  {out} (0600 — keep it on the packaging host only)");
    0
}

/// `pkg_order`-free stub for M2/M3 commands. Mirrors the shell log style
/// and exits 1.
pub fn stub(name: &str, names: &[String]) -> i32 {
    if names.is_empty() {
        log_line(name, &format!("'gozpak {name}' not implemented in M1"));
    } else {
        for n in names {
            log_line(n, &format!("'gozpak {name}' not implemented in M1"));
        }
    }
    1
}

/// Shell parity: when a non-`list` command gets zero package arguments,
/// use the basename of the current directory as the package name and add
/// its parent to the search path (this is how `gozpak c` works from
/// inside a recipe directory).
fn pwd_fallback(names: &[String]) -> (Vec<String>, Option<PathBuf>) {
    if !names.is_empty() {
        return (names.to_vec(), None);
    }
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(_) => return (Vec::new(), None),
    };
    let name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = cwd.parent().map(|p| p.to_path_buf());
    (vec![name], parent)
}

fn run_checksum(recipe: &Recipe, _ctx: &Ctx) {
    log_line(&recipe.name, "Reading sources");

    let cache = paths::source_cache_dir();
    let subst_ctx = subst::Subst::from_recipe(&recipe.name, &recipe.version, recipe.epoch);

    // Hash every source that resolves to a file on disk.
    let mut resolvable: Vec<(Source, String)> = Vec::new();
    for src in &recipe.sources {
        match src {
            Source::Local { path } => {
                let p = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    recipe.dir.join(path)
                };
                if let Ok(s) = sha256::hash_file(&p) {
                    resolvable.push((src.clone(), s));
                }
            }
            Source::Git { .. } => {
                // The shell version never checksums git sources.
                continue;
            }
            Source::Files { files } => {
                for f in files {
                    // v1.1: url keeps markers until use time; legacy lines
                    // carry bare markers, TOML urls use $MARKER.
                    let substituted = match recipe.kind {
                        RecipeKind::Legacy => subst_ctx.apply_shell(&f.url),
                        RecipeKind::Toml => subst_ctx.apply(&f.url),
                    };
                    let cached =
                        subst::cache_path_for(&recipe.name, &cache, &substituted, f.dest.as_deref());
                    if !cached.is_file() {
                        continue; // not downloaded yet; M2 handles this
                    }
                    let computed = match sha256::hash_file(&cached) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    if let Some(declared) = &f.sha256 {
                        if declared != &computed {
                            war(
                                &recipe.name,
                                &format!(
                                    "checksum mismatch for {}: expected {declared}, got {computed}",
                                    cached.display()
                                ),
                            );
                        } else {
                            log_line(&recipe.name, &format!("verified {}", cached.display()));
                        }
                    }
                    resolvable.push((src.clone(), computed));
                }
            }
        }
    }

    if resolvable.is_empty() {
        log_line(&recipe.name, "No sources needing checksums");
        return;
    }

    // Where the checksums land depends on the on-disk format:
    match recipe.kind {
        RecipeKind::Legacy => {
            // Shell behavior: write the `checksums` file.
            let file = paths::recipe_checksums_file(&recipe.dir);
            let body: Vec<_> = resolvable.iter().map(|(_, s)| s.as_str()).collect();
            if let Err(e) = std::fs::write(&file, body.join("\n") + "\n") {
                war(&recipe.name, &format!("failed to write {}: {e}", file.display()));
            } else {
                log_line(&recipe.name, "Generated checksums");
            }
        }
        RecipeKind::Toml => {
            // v1.1: declared checksums live in [source].urls[].sha256 and
            // are verified above when a cached tarball was found; M1 does
            // not rewrite recipe.toml (read-only), and local patch files
            // have no declared checksum.
            log_line(
                &recipe.name,
                "checksums for source URLs belong in [source].urls[].sha256",
            );
        }
    }
}
