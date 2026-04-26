# AGENTS.md

Compact orientation for agents working on this repo. Read once before editing.

## What this is

Rust + axum HTTP server that serves a single-page UI for editing a
git-managed Obsidian vault. Every save/create/rename/delete results in **one
git commit** in the vault. There is no auth.

The vault may be **either** a local directory or a remote SSH URL that the
server clones into the user's cache directory on startup. In SSH mode the
server also `pull --ff-only`s on startup and `push`es after each commit.

CLI surface (verify with `--help`, do not invent flags):

```
obsidian-web-server <VAULT> \
    -n|--git-user-name <NAME>  (required) \
    -e|--git-user-email <EMAIL> (required) \
    [-i|--identity-file <PATH>] \
    [--host 0.0.0.0] [--port 8080]
```

`<VAULT>` is auto-classified by string shape:
- `ssh://...` or scp-style `user@host:path` → SSH mode (clone-to-cache).
- `http(s)://...` → **rejected** with an explicit error; only SSH URLs are
  supported for remote vaults.
- Anything else → local path; must exist and contain a `.git` directory.

`--identity-file` is **required** when `<VAULT>` is an SSH URL and **rejected**
when it is a local path. The key must be unencrypted; ssh runs with
`BatchMode=yes` so passphrase prompts fail immediately rather than hanging.

## Code map

- `src/main.rs` — CLI parsing (clap derive), vault-source classification
  (`classify_vault_arg`), clone bootstrap (`ensure_remote_clone`), startup
  validation, axum bind.
- `src/vault.rs` — path-traversal-safe relative→absolute resolution; recursive
  tree walking. `HIDDEN_TOP_LEVEL = [".git", ".obsidian", ".trash"]` are
  rejected from API and excluded from the tree.
- `src/git.rs` — async wrapper around the **`git` binary** (shell-out, not
  `git2`). Identity is injected per-command as
  `git -c user.name=… -c user.email=… commit …`; the repo's `.git/config` is
  never modified. SSH calls (`clone`, `pull_ff_only`, `push`) set
  `GIT_SSH_COMMAND` per-invocation; nothing is written to `~/.ssh/config`.
- `src/routes.rs` — axum router, JSON handlers, embedded asset serving.
  Mutation handlers acquire `AppState::write_lock` (a `tokio::sync::Mutex`)
  for the entire write+commit+push sequence; reads stay concurrent.
- `src/assets/{index.html,app.js,style.css}` — vanilla JS UI, embedded into
  the binary at build time via `rust-embed` (`#[folder = "src/assets"]`).
  No bundler, no framework. Edit and rebuild.

UI is **mobile-first**: drawer sidebar with hamburger toggle below 768 px,
two-column layout above. Topbar (`#topbar`) is hidden on desktop via media
query. Don't reintroduce a desktop-only layout that breaks the mobile one.

## Commands

```
cargo check                # fast iteration
cargo build --release      # see host caveat below
cargo fmt && cargo fmt --check
nix build .#default        # full reproducible build via crane
nix flake check
```

Apply rustfmt before committing — there is no CI; the only enforcement is
`craneLib.cargoFmt` in `nix flake check`.

A few unit tests live in `src/main.rs` (vault-arg classification + URL
normalization). Run with `cargo test`. Smoke-test by running the binary against
a temp git repo and curling the API. Mutation responses are JSON shaped as:

```
{ committed: bool,
  sha: Option<String>,
  pushed: Option<bool>,        // omitted in local mode; Some(true|false) in SSH mode
  push_error: Option<String> } // present only when pushed == false
```

Push failure after a successful commit is **non-fatal**: the local commit stands
and the error is surfaced to the client. Pull failure on startup is **fatal**.

## Host-specific build gotcha (real, not stale)

The development host's `rustc` segfaults under parallel codegen at
`opt-level=3` (reproducible across rustc 1.94 and 1.95, in and out of the
nix sandbox). Workarounds already applied:

- `flake.nix` sets `CARGO_BUILD_JOBS = "1"` in `commonArgs` — leave this
  until the host issue is fixed; otherwise `nix build` segfaults mid-build.
- For local `cargo build --release`, prefix with `CARGO_BUILD_JOBS=1` if the
  default invocation crashes in `aho-corasick`/`clap_builder`/etc.
- `Cargo.toml` has **no `lto`** in `[profile.release]` for the same reason.
  Don't add `lto = "thin"` back without re-testing.

Once the host is healthy, the workaround is safe to remove.

## Conventions worth preserving

- **Edition 2024.** Keep crates current with that edition.
- **No `git2` / libgit2.** Shelling out to `git` is intentional; don't
  refactor `git.rs` to use `git2` without discussion (would add system deps
  to the crane build).
- **Path safety lives in `vault.rs`.** Every user-supplied path goes through
  `Vault::resolve`; never join a request path to the vault root directly in
  a handler.
- **Default commit messages** are formatted as `edit: <p>`, `create: <p>`,
  `delete: <p>`, `rename: <a> -> <b>` if the client doesn't supply one.
  Tests/UI rely on these strings.
- **Hidden top-level dirs** (`.git`, `.obsidian`, `.trash`) are filtered in
  *both* the tree walk and `Vault::resolve`. Update `HIDDEN_TOP_LEVEL` in
  one place if you add more.

## Remote sync (SSH mode)

- Cache root: `${XDG_CACHE_HOME:-$HOME/.cache}/obsidian-web-server/`. Per-repo
  subdir is a 16-hex-char hash of the normalized URL (lowercased, trailing
  `.git`/`/` stripped). Existing clones are reused if `remote.origin.url`
  matches the requested URL; otherwise the dir is wiped and re-cloned.
- Startup runs `git pull --ff-only`; failure is fatal (server refuses to
  start) so we never serve a stale/diverged tree.
- After every successful commit, the mutation handler runs `git push`. Push
  failure is **non-fatal** — the commit remains in the local clone and the
  error is surfaced via the response's `pushed: false` / `push_error` fields.
  The next successful push will carry the queued unpushed commits.
- `GIT_SSH_COMMAND` is set per-invocation:
  `ssh -i <identity> -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new -o BatchMode=yes`.
  - `accept-new` is TOFU: first connect writes to `~/.ssh/known_hosts`,
    subsequent mismatches fail. If `~/.ssh` doesn't exist, ssh fails.
  - `BatchMode=yes` makes passphrase prompts a hard error rather than a hang;
    passphrase-protected keys are explicitly unsupported. Use an unencrypted
    key (e.g. one dedicated to the deployment).
- Identity-file paths containing single quotes are rejected up-front (we
  single-quote the path inside `GIT_SSH_COMMAND` and don't escape).

## Out of scope (don't add without asking)

- NixOS module / `nixosModules.default` — explicitly deferred.
- Auth (basic, bearer, etc.) — explicitly deferred.
- HTTPS git URLs — explicitly deferred (SSH only).
- Passphrase-protected keys / ssh-agent integration — explicitly deferred.
- Periodic background pull (only pull-on-start) — explicitly deferred.
- Markdown rendering / preview — explicitly deferred.

## Static assets

`rust-embed` bakes `src/assets/` into the binary at compile time. The flake
filter in `flake.nix` keeps `*.html`, `*.css`, `*.js` files alongside Cargo
sources via a custom `srcFilter`. **If you add a new asset file extension
(e.g. `.svg`, `.ico`), update that filter** or `craneLib.cleanCargoSource`
will strip it from the nix build (cargo build will still see it from the
working tree, masking the bug until the next clean rebuild).
