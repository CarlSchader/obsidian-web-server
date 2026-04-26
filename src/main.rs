mod git;
mod routes;
mod vault;

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tracing_subscriber::EnvFilter;

/// HTTP server that exposes a git-managed Obsidian vault for editing over a web UI.
///
/// Each save (PUT/POST/DELETE) results in a single git commit in the local repository.
/// Pulling and pushing are intentionally not performed by the server; the operator is
/// expected to manage remote sync manually.
#[derive(Debug, Parser)]
#[command(name = "obsidian-web-server", version, about)]
struct Args {
    /// Path to the local git repository containing the Obsidian vault.
    vault_path: PathBuf,

    /// Name to use as the git author/committer for commits made by the server.
    #[arg(short = 'n', long)]
    git_user_name: String,

    /// Email to use as the git author/committer for commits made by the server.
    #[arg(short = 'e', long)]
    git_user_email: String,

    /// Host/IP address to bind the HTTP server to.
    #[arg(long, default_value = "0.0.0.0")]
    host: IpAddr,

    /// Port to bind the HTTP server to.
    #[arg(long, default_value_t = 8080)]
    port: u16,
}

#[derive(Clone)]
pub struct AppState {
    pub vault: Arc<vault::Vault>,
    pub git_user_name: Arc<str>,
    pub git_user_email: Arc<str>,
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

    let vault_path = args
        .vault_path
        .canonicalize()
        .with_context(|| format!("vault path does not exist: {}", args.vault_path.display()))?;

    if !vault_path.is_dir() {
        bail!("vault path is not a directory: {}", vault_path.display());
    }
    if !vault_path.join(".git").exists() {
        bail!(
            "vault path is not a git repository (missing .git): {}",
            vault_path.display()
        );
    }
    if args.git_user_name.trim().is_empty() {
        bail!("--git-user-name must not be empty");
    }
    if args.git_user_email.trim().is_empty() {
        bail!("--git-user-email must not be empty");
    }

    let vault = Arc::new(vault::Vault::new(vault_path));
    let state = AppState {
        vault,
        git_user_name: Arc::from(args.git_user_name.as_str()),
        git_user_email: Arc::from(args.git_user_email.as_str()),
    };

    let app = routes::router(state.clone());
    let addr = std::net::SocketAddr::new(args.host, args.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;

    tracing::info!(
        "obsidian-web-server listening on http://{} for vault {}",
        addr,
        state.vault.root().display()
    );

    axum::serve(listener, app).await.context("server crashed")?;
    Ok(())
}
