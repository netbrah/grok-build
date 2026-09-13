//! Worktree-path security guard for subagent isolation (MA-2.5, F16).
//!
//! Source (BEHAVIORAL reference — re-expressed, never copied verbatim):
//! HY @ `45e984f3` `packages/coding-agent/xai-grok-shell/src/agent/subagent/mod.rs:2157-2830`
//! (`WorktreeIdentity`, `unix_mode_is_owner_only`, `unix_mode_no_group_world_write`,
//! `unix_mode_has_sticky`, `unix_parent_component_is_safe`,
//! `unix_xdg_runtime_dir_mode_ok`, `validate_unix_parent_chain`,
//! `xdg_runtime_dir_is_usable`, `subagent_home_worktree_base`,
//! `subagent_temp_worktree_base`, `ensure_real_dir`, `reject_symlink_components`,
//! `is_strict_descendant`, `validate_subagent_worktree_path`,
//! `recheck_worktree_identity`), plus the id-segment policy at
//! `packages/tools/xai-tool-types/src/task.rs:147-260`.
//!
//! RED STATE (commit MA-2.5): signatures and module wiring only. Every
//! security decision below is a stub chosen so the ported 17-test HY cluster
//! (`subagent/tests/mod.rs`) compiles and FAILS. The green state (MA-2.6)
//! fills in the real logic in this file and wires it into
//! `handle_request.rs`; the test file must not change between red and green.

use std::path::{Path, PathBuf};

/// Identity of a validated worktree directory (canonical path + Unix dev/ino)
/// for the pre-spawn TOCTOU recheck.
#[allow(dead_code)] // red state: fields captured by the green-state `capture`
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeIdentity {
    pub path: PathBuf,
    #[cfg(unix)]
    pub dev: u64,
    #[cfg(unix)]
    pub ino: u64,
    #[cfg(not(unix))]
    pub modified_ms: Option<u128>,
}

impl WorktreeIdentity {
    /// Stat `path` (no symlink follow) and record identity.
    #[allow(dead_code)] // red state
    fn capture(path: &Path) -> Result<Self, String> {
        let _ = path;
        Err("worktree guard not yet implemented (MA-2.5 red state)".to_string())
    }

    /// Re-stat `path` and ensure it still refers to the same directory object.
    pub(crate) fn matches_path(&self, path: &Path) -> Result<(), String> {
        let _ = path;
        Err("worktree guard not yet implemented (MA-2.5 red state)".to_string())
    }
}

/// Pure Unix mode check for managed worktree **leaf** bases: owner-only
/// access (no group/world bits). Red-state stub: always `false`.
#[cfg(unix)]
pub(crate) fn unix_mode_is_owner_only(_mode: u32) -> bool {
    false
}

/// Pure: group/world write bits must be zero. Red-state stub: always `false`.
#[cfg(unix)]
pub(crate) fn unix_mode_no_group_world_write(_mode: u32) -> bool {
    false
}

/// Pure: sticky bit set (`S_ISVTX` = 0o1000). Red-state stub: always `false`.
#[cfg(unix)]
pub(crate) fn unix_mode_has_sticky(_mode: u32) -> bool {
    false
}

/// Pure policy for one parent-chain component. Red-state stub: always `false`.
#[cfg(unix)]
pub(crate) fn unix_parent_component_is_safe(_owner_uid: u32, _mode: u32, _euid: u32) -> bool {
    false
}

/// Pure policy for XDG_RUNTIME_DIR itself. Red-state stub: always `false`.
#[cfg(unix)]
pub(crate) fn unix_xdg_runtime_dir_mode_ok(_mode: u32) -> bool {
    false
}

/// Walk every existing ancestor of `path` and enforce the Unix parent-chain
/// policy. Red-state stub: permissive no-op (the group-writable-component
/// test fails on the resulting `Ok`); the green state implements the walk.
pub(crate) fn validate_unix_parent_chain(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// True when `dir` is a usable XDG_RUNTIME_DIR. Red-state stub: always `false`.
#[cfg(unix)]
#[allow(dead_code)] // red state
fn xdg_runtime_dir_is_usable(_dir: &Path) -> bool {
    false
}

/// User-private home fallback: `~/.grok/subagent-worktrees`.
#[allow(dead_code)] // red state: used by the green-state base selection
fn subagent_home_worktree_base() -> Option<PathBuf> {
    xai_dirs::home_dir().map(|h| h.join(".grok").join("subagent-worktrees"))
}

/// Per-UID private temp/home root for subagent worktrees.
///
/// Red-state stub: honors `XDG_RUNTIME_DIR` when set but never produces the
/// per-UID leaf, and otherwise returns the shared fixed name the tests
/// assert must never be used; the green state selects
/// XDG → per-UID temp → home fallback.
pub(crate) fn subagent_temp_worktree_base() -> PathBuf {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR")
        && !runtime.is_empty()
    {
        return PathBuf::from(runtime).join("grok-subagent-worktrees");
    }
    std::env::temp_dir().join("grok-subagent-worktrees")
}

/// Ensure `path` exists as a real owner-only directory with a safe parent
/// chain. Red-state stub: permissive no-op so test base preparation does not
/// panic; the green state implements create + verify (never auto-chmod).
pub(crate) fn ensure_real_dir(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Walk every component to `path` and reject any symlink component.
/// Red-state stub: permissive no-op.
#[allow(dead_code)] // red state
fn reject_symlink_components(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// True when `child` is a strict descendant of `base` (both already
/// canonical). Red-state stub: always `false`.
#[allow(dead_code)] // red state
fn is_strict_descendant(_child: &Path, _base: &Path) -> bool {
    false
}

/// Validate a resumed (or freshly created) subagent worktree path.
///
/// Red-state stub: fails closed with an implementation marker; the green
/// state implements the seven-step fail-closed check from the HY source.
pub(crate) fn validate_subagent_worktree_path(
    _dest: &Path,
    _source_cwd: &Path,
    _parent_cwd: &Path,
    _subagent_id: Option<&str>,
) -> Result<WorktreeIdentity, String> {
    Err("worktree guard not yet implemented (MA-2.5 red state)".to_string())
}

/// Final pre-spawn check: re-validate path and confirm identity still
/// matches. Red-state stub: fails closed.
pub(crate) fn recheck_worktree_identity(
    _expected: &WorktreeIdentity,
    _source_cwd: &Path,
    _parent_cwd: &Path,
    _subagent_id: Option<&str>,
) -> Result<WorktreeIdentity, String> {
    Err("worktree guard not yet implemented (MA-2.5 red state)".to_string())
}
