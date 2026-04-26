use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use thiserror::Error;
use walkdir::WalkDir;

/// Top-level entries within the vault that should never appear in the file tree
/// or be addressable through the API.
const HIDDEN_TOP_LEVEL: &[&str] = &[".git", ".obsidian", ".trash"];

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("path is empty")]
    Empty,
    #[error("path escapes the vault root")]
    Escape,
    #[error("path is outside the vault: {0}")]
    Outside(String),
    #[error("path refers to a hidden/internal directory: {0}")]
    Hidden(String),
}

/// Owns the on-disk vault root and provides safe path resolution + tree walking.
pub struct Vault {
    root: PathBuf,
}

impl Vault {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a user-supplied relative path to an absolute path, enforcing:
    ///  - the path is non-empty
    ///  - the path uses only Normal components (no `..`, no absolute, no current-dir)
    ///  - the result lives inside the vault root
    ///  - the path does not start in a hidden top-level directory (e.g. `.git/...`)
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, VaultError> {
        let trimmed = rel.trim_start_matches('/').trim();
        if trimmed.is_empty() {
            return Err(VaultError::Empty);
        }

        let candidate = Path::new(trimmed);

        // Reject path traversal / weird components up-front. We only allow plain
        // file/directory name components.
        let mut first_component: Option<String> = None;
        for comp in candidate.components() {
            match comp {
                Component::Normal(part) => {
                    if first_component.is_none() {
                        first_component = Some(part.to_string_lossy().into_owned());
                    }
                }
                Component::CurDir => {}
                _ => return Err(VaultError::Escape),
            }
        }

        if let Some(top) = first_component.as_deref()
            && HIDDEN_TOP_LEVEL.contains(&top)
        {
            return Err(VaultError::Hidden(top.to_string()));
        }

        let joined = self.root.join(candidate);

        // Final safety net: ensure the resulting path is still under root, even after
        // any symlink resolution that may have happened in a parent. We compare against
        // the canonical parent if it exists; otherwise just compare the lexical join.
        let resolved = if joined.exists() {
            joined
                .canonicalize()
                .map_err(|_| VaultError::Outside(rel.to_string()))?
        } else {
            // For not-yet-existing files, canonicalize the deepest existing ancestor and
            // append the remaining components.
            let mut existing = joined.clone();
            let mut tail: Vec<std::ffi::OsString> = Vec::new();
            while !existing.exists() {
                let name = existing.file_name().map(|n| n.to_owned());
                if let Some(name) = name {
                    tail.push(name);
                }
                if !existing.pop() {
                    break;
                }
            }
            let mut canon = existing
                .canonicalize()
                .map_err(|_| VaultError::Outside(rel.to_string()))?;
            for name in tail.iter().rev() {
                canon.push(name);
            }
            canon
        };

        if !resolved.starts_with(&self.root) {
            return Err(VaultError::Outside(rel.to_string()));
        }
        Ok(resolved)
    }

    /// Convert an absolute path inside the vault back to a forward-slash relative path.
    pub fn relative_str(&self, abs: &Path) -> Option<String> {
        abs.strip_prefix(&self.root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    }

    /// Build a recursive tree of the vault, omitting hidden top-level dirs.
    pub fn tree(&self) -> TreeNode {
        let mut root = TreeNode {
            name: String::new(),
            path: String::new(),
            kind: NodeKind::Dir,
            children: Some(Vec::new()),
        };

        // We use WalkDir but with a filter to skip hidden top-level dirs entirely.
        let walker = WalkDir::new(&self.root)
            .min_depth(1)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                if e.depth() == 1
                    && let Some(name) = e.file_name().to_str()
                    && HIDDEN_TOP_LEVEL.contains(&name)
                {
                    return false;
                }
                true
            });

        for entry in walker.flatten() {
            let abs = entry.path();
            let rel = match abs.strip_prefix(&self.root) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            let kind = if entry.file_type().is_dir() {
                NodeKind::Dir
            } else if entry.file_type().is_file() {
                NodeKind::File
            } else {
                continue;
            };
            insert_into_tree(&mut root, &rel, kind);
        }

        root
    }
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Dir,
    File,
}

#[derive(Debug, Serialize)]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub kind: NodeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<TreeNode>>,
}

fn insert_into_tree(root: &mut TreeNode, rel: &str, kind: NodeKind) {
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return;
    }

    let mut current = root;
    for (i, part) in parts.iter().enumerate() {
        let is_last = i == parts.len() - 1;
        let path_so_far = parts[..=i].join("/");

        let children = current.children.get_or_insert_with(Vec::new);

        let existing_idx = children.iter().position(|c| c.name == *part);
        let idx = match existing_idx {
            Some(i) => i,
            None => {
                let node_kind = if is_last { kind } else { NodeKind::Dir };
                let node = TreeNode {
                    name: (*part).to_string(),
                    path: path_so_far,
                    kind: node_kind,
                    children: if matches!(node_kind, NodeKind::Dir) {
                        Some(Vec::new())
                    } else {
                        None
                    },
                };
                children.push(node);
                children.len() - 1
            }
        };
        current = &mut children[idx];
    }
}
