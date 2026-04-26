mod git;
mod routes;
mod vault;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

use crate::git::SshConfig;

/// HTTP server that exposes a git-managed Obsidian vault for editing over a web UI.
///
/// Each save (PUT/POST/DELETE) results in a single git commit in the local repository.
/// When the vault is given as an SSH URL, the server clones into the user's cache
/// directory, pulls --ff-only on startup, and pushes after every commit.
#[derive(Debug, Parser)]
#[command(name = "obsidian-web-server", version, about)]
struct Args {
    /// Path to a local git repository, or an SSH URL (`ssh://...` or `user@host:path`)
    /// of a remote repository to clone.
    vault: String,

    /// Name to use as the git author/committer for commits made by the server.
    #[arg(short = 'n', long)]
    git_user_name: String,

    /// Email to use as the git author/committer for commits made by the server.
    #[arg(short = 'e', long)]
    git_user_email: String,

    /// Path to an SSH private key file. Required when `<VAULT>` is an SSH URL,
    /// rejected when it is a local path. The key must be unencrypted; passphrase
    /// prompts will fail because we run ssh in BatchMode.
    #[arg(short = 'i', long)]
    identity_file: Option<PathBuf>,

    /// Host/IP address to bind the HTTP server to.
    #[arg(long, default_value = "0.0.0.0")]
    host: IpAddr,

    /// Port to bind the HTTP server to.
    #[arg(long, default_value_t = 8080)]
    port: u16,
}

/// How the user told us to find the vault.
#[derive(Debug)]
enum VaultSource {
    /// A local on-disk path that already exists and is a git repo.
    LocalPath(PathBuf),
    /// An SSH URL we should clone (or reuse a clone of) into the cache dir.
    Ssh { url: String, identity: PathBuf },
}

#[derive(Clone)]
pub struct AppState {
    pub vault: Arc<vault::Vault>,
    pub git_user_name: Arc<str>,
    pub git_user_email: Arc<str>,
    /// `Some` iff the vault was cloned from an SSH URL. Pushes use this identity.
    pub ssh: Option<Arc<SshConfig>>,
    /// Serializes all mutation handlers (write+commit+push as one unit).
    pub write_lock: Arc<Mutex<()>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                EnvFilter::new("info,tower_http=info,obsidian_web_server=info")
            }),
        )
        .init();

    let args = Args::parse();

    if args.git_user_name.trim().is_empty() {
        bail!("--git-user-name must not be empty");
    }
    if args.git_user_email.trim().is_empty() {
        bail!("--git-user-email must not be empty");
    }

    let source = classify_vault_arg(&args.vault, args.identity_file.clone())?;

    let (vault_path, ssh) = match source {
        VaultSource::LocalPath(p) => {
            let canon = p
                .canonicalize()
                .with_context(|| format!("vault path does not exist: {}", p.display()))?;
            if !canon.is_dir() {
                bail!("vault path is not a directory: {}", canon.display());
            }
            if !canon.join(".git").exists() {
                bail!(
                    "vault path is not a git repository (missing .git): {}",
                    canon.display()
                );
            }
            (canon, None)
        }
        VaultSource::Ssh { url, identity } => {
            validate_identity_file(&identity)?;
            let ssh = SshConfig {
                identity_file: identity,
            };
            let dir = ensure_remote_clone(&url, &ssh)
                .await
                .with_context(|| format!("failed to prepare clone of {url}"))?;
            (dir, Some(Arc::new(ssh)))
        }
    };

    let vault = Arc::new(vault::Vault::new(vault_path));
    let state = AppState {
        vault,
        git_user_name: Arc::from(args.git_user_name.as_str()),
        git_user_email: Arc::from(args.git_user_email.as_str()),
        ssh,
        write_lock: Arc::new(Mutex::new(())),
    };

    let app = routes::router(state.clone());
    let addr = std::net::SocketAddr::new(args.host, args.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;

    tracing::info!(
        "obsidian-web-server listening on http://{} for vault {} (ssh: {})",
        addr,
        state.vault.root().display(),
        state.ssh.is_some()
    );

    axum::serve(listener, app).await.context("server crashed")?;
    Ok(())
}

/// Decide whether the positional argument is a local path or an SSH URL.
///
/// Rules:
/// - `ssh://...` → SSH URL.
/// - `http://...` / `https://...` → rejected (only SSH is supported).
/// - Contains `:` before any `/` AND the part before the colon looks like a
///   `[user@]host` token → SSH URL (scp-style).
/// - Otherwise → local path.
fn classify_vault_arg(arg: &str, identity: Option<PathBuf>) -> Result<VaultSource> {
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        bail!("vault argument is empty");
    }

    let lowered = trimmed.to_ascii_lowercase();
    if lowered.starts_with("http://") || lowered.starts_with("https://") {
        bail!(
            "HTTPS git URLs are not supported; use an SSH URL (ssh://... or user@host:path) instead"
        );
    }

    let is_ssh = if lowered.starts_with("ssh://") {
        true
    } else {
        // scp-style: "[user@]host:path" with no slash before the first colon.
        match trimmed.find(':') {
            Some(colon) => {
                let before = &trimmed[..colon];
                let after = &trimmed[colon + 1..];
                !before.is_empty()
                    && !before.contains('/')
                    && !before.contains('\\')
                    && is_scp_host(before)
                    && !after.is_empty()
            }
            None => false,
        }
    };

    if is_ssh {
        let identity = identity.ok_or_else(|| {
            anyhow!("--identity-file is required when <VAULT> is an SSH URL ({trimmed})")
        })?;
        Ok(VaultSource::Ssh {
            url: trimmed.to_string(),
            identity,
        })
    } else {
        if identity.is_some() {
            bail!("--identity-file is only valid with an SSH URL; got local path {trimmed}");
        }
        Ok(VaultSource::LocalPath(PathBuf::from(trimmed)))
    }
}

/// Cheap heuristic: does the part before the colon look like `[user@]host`?
/// Hosts/users use letters, digits, dot, dash, underscore. We accept exactly one `@`.
fn is_scp_host(s: &str) -> bool {
    let parts: Vec<&str> = s.splitn(2, '@').collect();
    let host = parts.last().copied().unwrap_or("");
    if host.is_empty() {
        return false;
    }
    let valid = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_');
    if parts.len() == 2 {
        let user = parts[0];
        if user.is_empty() || !user.chars().all(valid) {
            return false;
        }
    }
    host.chars().all(valid)
}

fn validate_identity_file(path: &Path) -> Result<()> {
    let meta = std::fs::metadata(path)
        .with_context(|| format!("identity file not accessible: {}", path.display()))?;
    if !meta.is_file() {
        bail!("identity file is not a regular file: {}", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            tracing::warn!(
                "identity file {} has loose permissions ({:o}); ssh may refuse to use it",
                path.display(),
                mode
            );
        }
    }
    Ok(())
}

/// Ensure a usable working clone of `url` exists in the cache directory.
///
/// - Resolves cache root to `$XDG_CACHE_HOME/obsidian-web-server` or
///   `$HOME/.cache/obsidian-web-server`.
/// - Per-repo subdirectory is named by a 16-hex-char hash of the normalized URL.
/// - If the subdir already contains a `.git` whose `remote.origin.url` matches,
///   reuse it; otherwise wipe and re-clone.
/// - Always runs `git pull --ff-only` after; **fatal** on failure.
async fn ensure_remote_clone(url: &str, ssh: &SshConfig) -> Result<PathBuf> {
    let cache_root = cache_root().context("could not determine cache directory")?;
    tokio::fs::create_dir_all(&cache_root)
        .await
        .with_context(|| format!("failed to create cache dir {}", cache_root.display()))?;

    let dir_name = url_cache_key(url);
    let dest = cache_root.join(&dir_name);

    let needs_clone = match dest.join(".git").try_exists() {
        Ok(true) => match git::remote_origin_url(&dest).await {
            Ok(Some(existing)) if normalize_url(&existing) == normalize_url(url) => {
                tracing::info!("reusing existing clone at {} for {}", dest.display(), url);
                false
            }
            Ok(Some(existing)) => {
                tracing::warn!(
                    "cache dir {} points at a different remote ({}); wiping and re-cloning",
                    dest.display(),
                    existing
                );
                true
            }
            Ok(None) => {
                tracing::warn!(
                    "cache dir {} has no remote.origin.url; wiping and re-cloning",
                    dest.display()
                );
                true
            }
            Err(e) => {
                tracing::warn!(
                    "could not read remote.origin.url from {}: {e}; wiping and re-cloning",
                    dest.display()
                );
                true
            }
        },
        Ok(false) => true,
        Err(e) => {
            return Err(anyhow!(
                "could not check existence of {}: {e}",
                dest.join(".git").display()
            ));
        }
    };

    if needs_clone {
        if dest.exists() {
            tokio::fs::remove_dir_all(&dest)
                .await
                .with_context(|| format!("failed to remove stale {}", dest.display()))?;
        }
        tracing::info!("cloning {} into {}", url, dest.display());
        git::clone(url, ssh, &dest)
            .await
            .with_context(|| format!("git clone {url} -> {} failed", dest.display()))?;
    }

    // Fast-forward to the latest upstream state. Fatal on failure.
    let repo = git::GitRepo {
        root: &dest,
        user_name: "", // unused for pull
        user_email: "",
        ssh: Some(ssh),
    };
    repo.pull_ff_only()
        .await
        .with_context(|| format!("git pull --ff-only failed in {}", dest.display()))?;

    Ok(dest)
}

/// Resolve the cache root: `$XDG_CACHE_HOME/obsidian-web-server`, falling back
/// to `$HOME/.cache/obsidian-web-server`.
fn cache_root() -> Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("obsidian-web-server"));
    }
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .ok_or_else(|| anyhow!("neither $XDG_CACHE_HOME nor $HOME is set"))?;
    Ok(PathBuf::from(home)
        .join(".cache")
        .join("obsidian-web-server"))
}

/// Stable 16-hex-char hash of the normalized URL for use as a cache subdir name.
fn url_cache_key(url: &str) -> String {
    let mut h = DefaultHasher::new();
    normalize_url(url).hash(&mut h);
    let high = h.finish();
    // Mix in a second hash for slightly more entropy in the visible name.
    let mut h2 = DefaultHasher::new();
    (normalize_url(url), 0xa5a5_a5a5u32).hash(&mut h2);
    let low = h2.finish();
    format!("{:08x}{:08x}", high as u32, low as u32)
}

/// Normalize a URL for comparison/hashing: lowercase, strip trailing `.git`,
/// strip trailing `/`.
fn normalize_url(url: &str) -> String {
    let mut s = url.trim().to_ascii_lowercase();
    while s.ends_with('/') {
        s.pop();
    }
    if let Some(stripped) = s.strip_suffix(".git") {
        s = stripped.to_string();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_local_path() {
        let v = classify_vault_arg("/tmp/foo", None).unwrap();
        assert!(matches!(v, VaultSource::LocalPath(_)));
    }

    #[test]
    fn classify_relative_path() {
        let v = classify_vault_arg("./foo/bar", None).unwrap();
        assert!(matches!(v, VaultSource::LocalPath(_)));
    }

    #[test]
    fn classify_ssh_scheme() {
        let v = classify_vault_arg(
            "ssh://git@example.com/owner/repo.git",
            Some(PathBuf::from("/tmp/key")),
        )
        .unwrap();
        assert!(matches!(v, VaultSource::Ssh { .. }));
    }

    #[test]
    fn classify_scp_style() {
        let v = classify_vault_arg(
            "git@github.com:owner/repo.git",
            Some(PathBuf::from("/tmp/key")),
        )
        .unwrap();
        assert!(matches!(v, VaultSource::Ssh { .. }));
    }

    #[test]
    fn classify_https_rejected() {
        let err = classify_vault_arg("https://github.com/owner/repo.git", None).unwrap_err();
        assert!(err.to_string().contains("HTTPS"));
    }

    #[test]
    fn classify_ssh_requires_identity() {
        let err = classify_vault_arg("git@github.com:owner/repo.git", None).unwrap_err();
        assert!(err.to_string().contains("--identity-file"));
    }

    #[test]
    fn classify_local_rejects_identity() {
        let err = classify_vault_arg("/tmp/foo", Some(PathBuf::from("/tmp/key"))).unwrap_err();
        assert!(err.to_string().contains("--identity-file"));
    }

    #[test]
    fn normalize_strips_dot_git_and_trailing_slash() {
        assert_eq!(
            normalize_url("git@github.com:owner/repo.git"),
            normalize_url("git@github.com:owner/repo")
        );
        assert_eq!(
            normalize_url("ssh://git@example.com/owner/repo/"),
            normalize_url("ssh://git@example.com/owner/repo")
        );
    }
}
