//! Thin, synchronous, in-process git query layer (FORNX-302).
//!
//! Exists as a standalone crate — not folded into `fornax-adapter-claude` or
//! any other adapter — because of two real workspace constraints:
//!
//! 1. `crates/fornax-daemon/tests/adversarial_daemon_input.rs::
//!    subprocess_surface_is_still_zero_in_production_code` (FORNX-238)
//!    asserts a zero subprocess-spawn surface across every production module
//!    in this workspace, which rules out shelling out to a `git` binary.
//! 2. `docs/contributing/adding-an-adapter.md`'s "Allowed core dependencies"
//!    restricts adapter crates to depending on `fornax-types` only, which
//!    rules out an in-process git library living directly inside an adapter
//!    crate.
//!
//! This crate depends on [`gix`] — a pure-Rust, in-process reimplementation
//! of git (the gitoxide project), not a wrapper around `libgit2` (no FFI to
//! a system library) and not a wrapper around the `git` binary (no
//! subprocess). It exposes a small, synchronous, adapter-callable interface;
//! adapters depend on this crate in addition to `fornax-types` — see
//! `docs/contributing/adding-an-adapter.md`'s narrowly-scoped amendment for
//! this exact exception.
//!
//! No network access, no subprocess spawn, no blocking beyond local disk
//! I/O — safe to call from the local critical path
//! (`docs/adr/0001-architecture-invariants.md`'s "no cloud dependency on the
//! local critical path").

use std::path::Path;

/// The real, host-observed state of a git working tree at the moment this
/// was queried — independent of anything a coding agent claimed happened.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkingTreeStatus {
    /// `false` when `repo_path` (or any of its ancestors) is not inside a
    /// git working tree at all — a clean, non-error outcome, not folded into
    /// [`VcsError`].
    pub is_repo: bool,
    /// `HEAD`'s commit SHA, as a lowercase hex string. `None` for a real
    /// repository that has no commits yet (an "unborn" `HEAD`) — also not an
    /// error.
    pub head_commit: Option<String>,
    /// Paths (repo-root-relative) that differ between `HEAD`'s tree, the
    /// index, and the working tree, or that are untracked — i.e. every path
    /// this git implementation's own status walk considers not clean, the
    /// same set `git status --ignored=no` would report. Empty when the
    /// working tree is clean.
    pub dirty_paths: Vec<String>,
}

impl WorkingTreeStatus {
    fn not_a_repo() -> Self {
        Self {
            is_repo: false,
            head_commit: None,
            dirty_paths: Vec::new(),
        }
    }

    /// True when `path` (given relative to the queried repo root, using `/`
    /// separators — git's own convention, independent of host path
    /// separator) appears among [`Self::dirty_paths`].
    pub fn is_path_dirty(&self, path: &str) -> bool {
        self.dirty_paths.iter().any(|p| p == path)
    }
}

/// Everything that can go wrong querying working-tree state, distinct from
/// the honest, non-error "not a repo" / "no commits yet" outcomes above.
#[derive(Debug, thiserror::Error)]
pub enum VcsError {
    /// Discovery found *something* at or above `repo_path` that looks like a
    /// git directory but could not be opened as a valid repository (e.g. a
    /// corrupt `.git`) — distinct from [`WorkingTreeStatus::is_repo`] being
    /// `false`, which means no git directory was found at all.
    #[error("failed to open git repository: {0}")]
    Open(String),
    /// Repository opened successfully, but the status/tree query itself
    /// failed (e.g. an unreadable object database).
    #[error("failed to query git working-tree status: {0}")]
    Status(String),
}

/// Query `repo_path`'s working-tree status: whether it is (or is inside) a
/// git repository, its `HEAD` commit, and every path considered dirty
/// (uncommitted, unstaged, or untracked) by this git implementation's own
/// status walk.
///
/// Synchronous and local-only: no network access, no subprocess spawn, no
/// long-lived state. Searches upward from `repo_path` for a `.git`
/// directory, matching `git status`'s own behavior when run from a
/// subdirectory of a working tree.
pub fn working_tree_status(repo_path: &Path) -> Result<WorkingTreeStatus, VcsError> {
    let repo = match gix::discover(repo_path) {
        Ok(repo) => repo,
        // Only the "genuinely searched and found nothing" shapes of
        // `discover::upwards::Error` mean "not a repo" — `InaccessibleDirectory`,
        // `CurrentDir`, `CheckTrust`, etc. are real query failures (e.g. a
        // permission-denied directory on the search path) and must not be
        // silently folded into `is_repo: false`.
        Err(gix::discover::Error::Discover(
            gix::discover::upwards::Error::NoGitRepository { .. }
            | gix::discover::upwards::Error::NoGitRepositoryWithinCeiling { .. }
            | gix::discover::upwards::Error::NoGitRepositoryWithinFs { .. }
            | gix::discover::upwards::Error::NoMatchingCeilingDir,
        )) => return Ok(WorkingTreeStatus::not_a_repo()),
        Err(e) => return Err(VcsError::Open(e.to_string())),
    };

    // An unborn `HEAD` (a real repository with zero commits) is a legitimate
    // state, not a failure — reported as `head_commit: None`, matching
    // `WorkingTreeStatus::head_commit`'s documented contract.
    let head_commit = repo.head_id().ok().map(|id| id.to_string());

    let dirty_paths = collect_dirty_paths(&repo)?;

    Ok(WorkingTreeStatus {
        is_repo: true,
        head_commit,
        dirty_paths,
    })
}

fn collect_dirty_paths(repo: &gix::Repository) -> Result<Vec<String>, VcsError> {
    let status = repo
        .status(gix::progress::Discard)
        .map_err(|e| VcsError::Status(e.to_string()))?;
    let iter = status
        .into_iter(Vec::new())
        .map_err(|e| VcsError::Status(e.to_string()))?;

    let mut paths = Vec::new();
    for item in iter {
        let item = item.map_err(|e| VcsError::Status(e.to_string()))?;
        paths.push(item.location().to_string());
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("fornax-vcs-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn reports_not_a_repo_for_a_plain_directory() {
        let dir = temp_dir();
        let status = working_tree_status(&dir).expect("query should not error");
        std::fs::remove_dir_all(&dir).ok();

        assert!(!status.is_repo);
        assert_eq!(status.head_commit, None);
        assert!(status.dirty_paths.is_empty());
    }

    #[test]
    fn reports_a_freshly_initialized_repo_as_a_clean_unborn_repo() {
        let dir = temp_dir();
        gix::init(&dir).expect("gix::init");

        let status = working_tree_status(&dir).expect("query should not error");
        std::fs::remove_dir_all(&dir).ok();

        assert!(status.is_repo);
        assert_eq!(status.head_commit, None);
        assert!(status.dirty_paths.is_empty());
    }

    #[test]
    fn reports_an_untracked_file_as_dirty() {
        let dir = temp_dir();
        gix::init(&dir).expect("gix::init");
        let file = dir.join("claimed.txt");
        std::fs::write(&file, "hello\n").expect("write file");

        let status = working_tree_status(&dir).expect("query should not error");
        std::fs::remove_dir_all(&dir).ok();

        assert!(status.is_repo);
        assert!(status.is_path_dirty("claimed.txt"));
    }

    #[test]
    fn reports_head_commit_after_a_real_commit() {
        let dir = temp_dir();
        // SAFETY-relevant only in the sense that this mutates process-wide
        // environment state; acceptable in a test binary and scoped to
        // values `gix::Repository::commit`'s author/committer lookup reads
        // (`GIT_AUTHOR_*`/`GIT_COMMITTER_*`), matching real git's own
        // fallback order (repo config, then these env vars, then
        // `user.name`/`user.email`).
        std::env::set_var("GIT_AUTHOR_NAME", "Fornax Test");
        std::env::set_var("GIT_AUTHOR_EMAIL", "fornax-test@example.invalid");
        std::env::set_var("GIT_COMMITTER_NAME", "Fornax Test");
        std::env::set_var("GIT_COMMITTER_EMAIL", "fornax-test@example.invalid");

        let repo = gix::init(&dir).expect("gix::init");
        let blob_id = repo
            .write_blob(b"hello\n".as_slice())
            .expect("write blob")
            .detach();
        let tree = gix::objs::Tree {
            entries: vec![gix::objs::tree::Entry {
                mode: gix::objs::tree::EntryKind::Blob.into(),
                filename: "claimed.txt".into(),
                oid: blob_id,
            }],
        };
        let tree_id = repo.write_object(&tree).expect("write tree").detach();
        let commit_id = repo
            .commit(
                "HEAD",
                "initial commit",
                tree_id,
                std::iter::empty::<gix::ObjectId>(),
            )
            .expect("commit");

        let status = working_tree_status(&dir).expect("query should not error");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            status.head_commit.as_deref(),
            Some(commit_id.to_string().as_str())
        );
    }

    #[test]
    fn reports_open_failure_for_a_path_discovery_cannot_even_access() {
        // A path that does not exist as a directory at all (as opposed to a
        // real, existing, non-repo directory — see
        // `reports_not_a_repo_for_a_plain_directory` above) is a genuine
        // access failure during discovery, not "searched and found no
        // repository" — must surface as `VcsError::Open`, not be folded
        // into `is_repo: false`.
        let bogus = std::env::temp_dir().join(format!(
            "fornax-vcs-test-does-not-exist-{}",
            uuid::Uuid::new_v4()
        ));

        let result = working_tree_status(&bogus);

        assert!(matches!(result, Err(VcsError::Open(_))), "{result:?}");
    }
}
