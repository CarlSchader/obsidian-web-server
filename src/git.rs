use std::path::{Path, PathBuf};

use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("failed to spawn git: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("git {args:?} exited with status {status}: {stderr}")]
    Failed {
        args: Vec<String>,
        status: String,
        stderr: String,
    },
    #[error("identity file path contains a single quote which is not supported: {0}")]
    BadIdentityPath(String),
}

/// Outcome of a commit attempt.
#[derive(Debug)]
pub enum CommitResult {
    /// A commit was created with the given short SHA.
    Committed { sha: String },
    /// Nothing was staged, so no commit was created.
    Nothing,
}

/// SSH configuration used when the vault was cloned from a remote.
#[derive(Debug, Clone)]
pub struct SshConfig {
    pub identity_file: PathBuf,
}

impl SshConfig {
    /// Build a `GIT_SSH_COMMAND` value invoking `ssh` with the configured identity.
    ///
    /// `BatchMode=yes` means ssh fails immediately rather than prompting for a
    /// passphrase, so passphrase-protected keys are explicitly unsupported.
    /// `accept-new` populates `~/.ssh/known_hosts` on first connect (TOFU).
    pub fn git_ssh_command(&self) -> Result<String, GitError> {
        let path = self.identity_file.to_string_lossy();
        if path.contains('\'') {
            return Err(GitError::BadIdentityPath(path.into_owned()));
        }
        Ok(format!(
            "ssh -i '{}' -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new -o BatchMode=yes",
            path
        ))
    }
}

pub struct GitRepo<'a> {
    pub root: &'a Path,
    pub user_name: &'a str,
    pub user_email: &'a str,
    pub ssh: Option<&'a SshConfig>,
}

impl<'a> GitRepo<'a> {
    /// Run `git -C <root> <args...>` and return stdout (trimmed). Errors include stderr.
    async fn run(&self, args: &[&str]) -> Result<String, GitError> {
        run_git_in(self.root, args, None).await
    }

    /// Like `run`, but also sets `GIT_SSH_COMMAND` from the configured ssh identity.
    /// If no ssh config is set, this is identical to `run`.
    async fn run_with_ssh(&self, args: &[&str]) -> Result<String, GitError> {
        let ssh_env = match self.ssh {
            Some(cfg) => Some(("GIT_SSH_COMMAND".to_string(), cfg.git_ssh_command()?)),
            None => None,
        };
        run_git_in(self.root, args, ssh_env).await
    }

    /// Same as `run` but injects `-c user.name=... -c user.email=...` before the subcommand
    /// so we never mutate the repo's `.git/config`.
    async fn run_with_identity(&self, args: &[&str]) -> Result<String, GitError> {
        let name_arg = format!("user.name={}", self.user_name);
        let email_arg = format!("user.email={}", self.user_email);
        let mut full: Vec<&str> = vec!["-c", &name_arg, "-c", &email_arg];
        full.extend_from_slice(args);
        self.run(&full).await
    }

    /// Stage paths, then commit if anything changed.
    pub async fn add_and_commit(
        &self,
        rel_paths: &[&str],
        message: &str,
    ) -> Result<CommitResult, GitError> {
        let mut args: Vec<&str> = vec!["add", "--"];
        args.extend(rel_paths.iter().copied());
        self.run(&args).await?;
        self.commit_if_staged(message).await
    }

    /// `git rm` the given path, then commit.
    pub async fn rm_and_commit(
        &self,
        rel_path: &str,
        message: &str,
    ) -> Result<CommitResult, GitError> {
        self.run(&["rm", "--", rel_path]).await?;
        self.commit_if_staged(message).await
    }

    /// `git mv` from -> to, then commit.
    pub async fn mv_and_commit(
        &self,
        from: &str,
        to: &str,
        message: &str,
    ) -> Result<CommitResult, GitError> {
        self.run(&["mv", "--", from, to]).await?;
        self.commit_if_staged(message).await
    }

    /// `git push` against the remote configured for the current branch's upstream.
    /// Uses the configured ssh identity. Returns the trimmed stdout on success.
    pub async fn push(&self) -> Result<String, GitError> {
        self.run_with_ssh(&["push"]).await
    }

    /// `git pull --ff-only` against the configured upstream. Uses the configured ssh
    /// identity. Returns the trimmed stdout on success.
    pub async fn pull_ff_only(&self) -> Result<String, GitError> {
        self.run_with_ssh(&["pull", "--ff-only"]).await
    }

    /// Returns Committed if there are staged changes, else Nothing.
    async fn commit_if_staged(&self, message: &str) -> Result<CommitResult, GitError> {
        // `git diff --cached --quiet` exits 0 if nothing staged, 1 if there are staged changes.
        let mut diff = Command::new("git");
        diff.arg("-C").arg(self.root);
        diff.args(["diff", "--cached", "--quiet"]);
        let status = diff.status().await?;
        match status.code() {
            Some(0) => Ok(CommitResult::Nothing),
            Some(1) => {
                self.run_with_identity(&["commit", "-m", message]).await?;
                let sha = self.run(&["rev-parse", "--short", "HEAD"]).await?;
                Ok(CommitResult::Committed { sha })
            }
            _ => Err(GitError::Failed {
                args: vec!["diff".into(), "--cached".into(), "--quiet".into()],
                status: status.to_string(),
                stderr: "unexpected exit code from git diff --cached".to_string(),
            }),
        }
    }
}

/// Low-level: run `git -C <cwd> <args>` with optional extra env var.
async fn run_git_in(
    cwd: &Path,
    args: &[&str],
    extra_env: Option<(String, String)>,
) -> Result<String, GitError> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(cwd);
    cmd.args(args);
    if let Some((k, v)) = extra_env {
        cmd.env(k, v);
    }
    let output = cmd.output().await?;
    if !output.status.success() {
        return Err(GitError::Failed {
            args: args.iter().map(|s| (*s).to_string()).collect(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `git clone <url> <dest>` using the configured ssh identity. `dest` must not exist
/// (git clone creates it). Parent dir of `dest` must exist.
pub async fn clone(url: &str, ssh: &SshConfig, dest: &Path) -> Result<(), GitError> {
    let dest_str = dest.to_string_lossy();
    let args: Vec<&str> = vec!["clone", "--quiet", url, dest_str.as_ref()];
    let mut cmd = Command::new("git");
    cmd.args(&args);
    cmd.env("GIT_SSH_COMMAND", ssh.git_ssh_command()?);
    let output = cmd.output().await?;
    if !output.status.success() {
        return Err(GitError::Failed {
            args: args.iter().map(|s| (*s).to_string()).collect(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Read `remote.origin.url` from the repo at `repo`. Returns `None` if the config
/// key is missing (git exits 1 with no stdout in that case).
pub async fn remote_origin_url(repo: &Path) -> Result<Option<String>, GitError> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .args(["config", "--get", "remote.origin.url"]);
    let output = cmd.output().await?;
    match output.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        )),
        // Exit 1 means "key not set" for `git config --get`.
        Some(1) => Ok(None),
        _ => Err(GitError::Failed {
            args: vec!["config".into(), "--get".into(), "remote.origin.url".into()],
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
    }
}
