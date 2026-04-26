# AGENTS.md

Compact orientation for agents working on this repo. Read once before editing.

## What this is

Rust + axum HTTP server that serves a single-page UI for editing a local
git-managed Obsidian vault. Every save/create/rename/delete results in **one
git commit** in the vault. The server never pulls or pushes — the operator
handles remote sync manually. There is no auth.

CLI surface (verify with `--help`, do not invent flags):

```
obsidian-web-server <VAULT_PATH> \
    -n|--git-user-name <NAME>  (required) \
    -e|--git-user-email <EMAIL> (required) \
    [--host 0.0.0.0] [--port 8080]
```

`VAULT_PATH` must exist locally and contain a `.git` directory. SSH URLs are
**not** supported; this was an explicit scope decision.

## Code map

- `src/main.rs` — CLI parsing (clap derive), startup validation, axum bind.
- `src/vault.rs` — path-traversal-safe relative→absolute resolution; recursive
  tree walking. `HIDDEN_TOP_LEVEL = [".git", ".obsidian", ".trash"]` are
  rejected from API and excluded from the tree.
- `src/git.rs` — async wrapper around the **`git` binary** (shell-out, not
  `git2`). Identity is injected per-command as
  `git -c user.name=… -c user.email=… commit …`; the repo's `.git/config` is
  never modified.
- `src/routes.rs` — axum router, JSON handlers, embedded asset serving.
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

There are no tests. Smoke-test by running the binary against a temp git repo
and curling the API (the `axum` routes return JSON shaped as
`{committed: bool, sha: Option<String>}` for mutations).

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

## Out of scope (don't add without asking)

- NixOS module / `nixosModules.default` — explicitly deferred.
- Auth (basic, bearer, etc.) — explicitly deferred.
- `git pull` / `git push` from the server — explicitly deferred.
- SSH URL handling, clone-to-tmp — explicitly deferred.
- Markdown rendering / preview — explicitly deferred.

## Static assets

`rust-embed` bakes `src/assets/` into the binary at compile time. The flake
filter in `flake.nix` keeps `*.html`, `*.css`, `*.js` files alongside Cargo
sources via a custom `srcFilter`. **If you add a new asset file extension
(e.g. `.svg`, `.ico`), update that filter** or `craneLib.cleanCargoSource`
will strip it from the nix build (cargo build will still see it from the
working tree, masking the bug until the next clean rebuild).
