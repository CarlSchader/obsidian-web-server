use std::path::Path;

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
}

/// Outcome of a commit attempt.
#[derive(Debug)]
pub enum CommitResult {
    /// A commit was created with the given short SHA.
    Committed { sha: String },
    /// Nothing was staged, so no commit was created.
    Nothing,
}

pub struct GitRepo<'a> {
    pub root: &'a Path,
    pub user_name: &'a str,
    pub user_email: &'a str,
}

impl<'a> GitRepo<'a> {
    /// Run `git -C <root> <args...>` and return stdout (trimmed). Errors include stderr.
    async fn run(&self, args: &[&str]) -> Result<String, GitError> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(self.root);
        cmd.args(args);
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
