//! Hand-rolled argument parsing (no clap — the alias table is small and the
//! shell version parses its args the same way: first word = action, rest =
//! package names, `-y`/`--yes` skippable anywhere).

/// The resolved command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `version` — implemented in M1.
    Version,
    /// `list` — implemented in M1.
    List,
    /// `search` — implemented in M1.
    Search,
    /// `checksum` — implemented in M1.
    Checksum,
    /// `build` — M2a: stage + build + pack, no repo metadata.
    Build,
    /// `repo-build` — M2a: stage + build + pack with repo metadata
    /// (`[manifest].repo` / `built_by`); the `-b` flag (recorded in
    /// [`Parsed::build_only`]) skips the pack step.
    RepoBuild,
    /// `sync` — M2b: fetch + verify (ed25519) + cache repo indices.
    Sync,
    /// `list-remote` — M2b: packages in the cached (verified) indices.
    ListRemote,
    /// `fetch` — M2b: download a `.gpkg` from the verified index, no
    /// install.
    Fetch,
    /// `get` — M2b: fetch + install.
    Get,
    /// `install` — M2b: install `.gpkg`s (from the bin cache or the
    /// verified index) into the target root.
    Install,
    /// `index-build` — M2b (packaging host): `index.json` from a
    /// directory of `.gpkg` files.
    IndexBuild,
    /// `sign` — M2b (packaging host): sign `index.json` →
    /// `index.json.sig` (ed25519, key from `GOZPAK_SIGN_KEY`).
    Sign,
    /// `keygen` — M2b: generate an ed25519 repo keypair (print the
    /// public key; the seed file goes on the packaging host only).
    KeyGen,
    /// A command the shell version has but M2 only stubs out.
    ///
    /// remove, download, update, upgrade, alternatives.
    Stub {
        /// Canonical (full) command name, e.g. "remove".
        name: &'static str,
    },
}

/// Parsed invocation.
#[derive(Debug)]
pub struct Parsed {
    /// The first word as typed (empty string when there are no arguments).
    pub action_raw: String,
    /// `None` when the action is neither known nor an alias — i.e. it is an
    /// extension candidate (`gozpak-<action_raw>`), see `ext::fallback`.
    pub action: Option<Action>,
    /// Package-name arguments (everything after the action, flags
    /// stripped).
    pub names: Vec<String>,
    /// `-y` / `--yes` was given (shell: sets `GOZPAK_PROMPT=0`).
    pub yes: bool,
    /// `repo-build -b` / `--build-only`: build only, skip the pack step.
    pub build_only: bool,
}

/// Single-letter aliases, mirroring the shell `args()` case table
/// (`gozpak [a|b|c|d|f|g|i|l|L|r|s|S|u|U|v]`).
const ALIASES: [(&str, &str); 15] = [
    ("a", "alternatives"),
    ("b", "build"),
    ("c", "checksum"),
    ("d", "download"),
    ("f", "fetch"),
    ("g", "get"),
    ("i", "install"),
    ("l", "list"),
    ("L", "list-remote"),
    ("r", "remove"),
    ("s", "search"),
    ("S", "sync"),
    ("u", "update"),
    ("U", "upgrade"),
    ("v", "version"),
];

/// Parse `argv` (the words after the program name).
pub fn parse(argv: &[String]) -> Parsed {
    let mut yes = false;
    let mut rest: Vec<&String> = Vec::new();
    for a in argv {
        if a == "-y" || a == "--yes" {
            yes = true;
        } else {
            rest.push(a);
        }
    }

    let action_raw = rest.first().cloned().map(String::as_str).unwrap_or("");
    let mut names: Vec<String> = rest.iter().skip(1).map(|s| s.to_string()).collect();
    let action = resolve_action(action_raw);

    // `repo-build` carries its own `-b`/`--build-only` flag (build only,
    // skip the pack step); strip it from the package-name list.
    let mut build_only = false;
    if matches!(action, Some(Action::RepoBuild)) {
        names.retain(|n| {
            if n == "-b" || n == "--build-only" {
                build_only = true;
                false
            } else {
                true
            }
        });
    }

    Parsed {
        action_raw: action_raw.to_string(),
        action,
        names,
        yes,
        build_only,
    }
}

/// Map the typed first word to a canonical [`Action`], or `None` when it is
/// an unknown word (→ extension fallback).
fn resolve_action(raw: &str) -> Option<Action> {
    let canonical = if raw.is_empty() {
        ""
    } else {
        ALIASES.iter()
            .find(|(alias, _)| *alias == raw)
            .map(|(_, c)| *c)
            .unwrap_or(raw)
    };

    Some(match canonical {
        "version" => Action::Version,
        "list" => Action::List,
        "search" => Action::Search,
        "checksum" => Action::Checksum,
        "build" => Action::Build,
        "repo-build" => Action::RepoBuild,
        "sync" => Action::Sync,
        "list-remote" => Action::ListRemote,
        "fetch" => Action::Fetch,
        "get" => Action::Get,
        "install" => Action::Install,
        "index-build" => Action::IndexBuild,
        "sign" => Action::Sign,
        "keygen" => Action::KeyGen,
        "remove" => Action::Stub { name: "remove" },
        "download" => Action::Stub { name: "download" },
        "update" => Action::Stub { name: "update" },
        "upgrade" => Action::Stub { name: "upgrade" },
        "alternatives" => Action::Stub { name: "alternatives" },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> String {
        s.to_string()
    }

    fn is_stub(action: &Option<Action>) -> bool {
        matches!(action, Some(Action::Stub { .. }))
    }

    #[test]
    fn version_alias_and_full() {
        let p = parse(&[v("v")]);
        assert_eq!(p.action, Some(Action::Version));
        let p = parse(&[v("version")]);
        assert_eq!(p.action, Some(Action::Version));
    }

    #[test]
    fn names_collected_after_action() {
        let p = parse(&[v("l"), v("zlib"), v("curl")]);
        assert_eq!(p.action, Some(Action::List));
        assert_eq!(p.names, vec![v("zlib"), v("curl")]);
    }

    #[test]
    fn yes_flag_anywhere_is_stripped() {
        let p = parse(&[v("-y"), v("s"), v("mesa"), v("--yes")]);
        assert!(p.yes);
        assert_eq!(p.action, Some(Action::Search));
        assert_eq!(p.names, vec![v("mesa")]);

        // -y without any other args → no action, yes still recorded.
        let p = parse(&[v("-y")]);
        assert!(p.yes);
        assert!(p.action_raw.is_empty());
    }

    #[test]
    fn stub_commands_resolve_to_stub() {
        assert!(is_stub(&parse(&[v("r")]).action));
        assert!(is_stub(&parse(&[v("U")]).action));
        assert!(is_stub(&parse(&[v("alternatives")]).action));
        assert!(is_stub(&parse(&[v("d")]).action));
    }

    #[test]
    fn unknown_words_are_extension_candidates() {
        let p = parse(&[v("frobnicate"), v("arg1")]);
        assert_eq!(p.action, None);
        assert_eq!(p.action_raw, "frobnicate");
        assert_eq!(p.names, vec![v("arg1")]);
    }

    #[test]
    fn no_args_means_no_action() {
        let p = parse(&[]);
        assert_eq!(p.action, None);
        assert!(p.action_raw.is_empty());
        assert!(p.names.is_empty());
        assert!(!p.yes);
    }

    #[test]
    fn case_sensitive_aliases() {
        // Lowercase s = search; uppercase S = sync (M2b).
        assert_eq!(parse(&[v("s")]).action, Some(Action::Search));
        assert_eq!(parse(&[v("S")]).action, Some(Action::Sync));
    }

    #[test]
    fn build_and_repo_build_resolve_to_actions() {
        assert_eq!(parse(&[v("b")]).action, Some(Action::Build));
        assert_eq!(parse(&[v("build")]).action, Some(Action::Build));
        assert_eq!(parse(&[v("repo-build")]).action, Some(Action::RepoBuild));
    }

    #[test]
    fn repo_build_build_only_flag_is_stripped() {
        let p = parse(&[v("repo-build"), v("-b"), v("zlib")]);
        assert_eq!(p.action, Some(Action::RepoBuild));
        assert!(p.build_only);
        assert_eq!(p.names, vec![v("zlib")]);

        let p = parse(&[v("repo-build"), v("--build-only"), v("zlib")]);
        assert!(p.build_only);
        assert_eq!(p.names, vec![v("zlib")]);

        // Without the flag: build_only stays false, names untouched.
        let p = parse(&[v("repo-build"), v("zlib")]);
        assert!(!p.build_only);
        assert_eq!(p.names, vec![v("zlib")]);
    }

    #[test]
    fn b_flag_on_plain_build_is_just_a_name() {
        // Only repo-build understands -b; a plain build keeps it
        // verbatim in the name list.
        let p = parse(&[v("b"), v("-b"), v("zlib")]);
        assert_eq!(p.action, Some(Action::Build));
        assert!(!p.build_only);
        assert_eq!(p.names, vec![v("-b"), v("zlib")]);
    }
}
