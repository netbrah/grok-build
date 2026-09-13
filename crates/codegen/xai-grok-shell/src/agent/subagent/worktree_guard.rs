//! Worktree-path security guard for subagent isolation (MA-2.6, F16).
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
//! `packages/tools/xai-tool-types/src/task.rs:147-260`
//! (`MAX_SAFE_PATH_SEGMENT_LEN`, `is_windows_reserved_basename`,
//! `is_safe_segment_inner`).
//!
//! Policy: fail-closed. A subagent worktree is only accepted when it is a
//! real (non-symlink) directory under a managed, owner-only base; every path
//! component is non-symlink; parent components are owned by the eUID or root
//! without group/world write (root-owned sticky `/tmp` allowed); and the
//! basename is exactly `subagent-{id}`. Nothing is auto-chmod'd into
//! acceptance.
//!
//! Platform adaptation vs the HY source (Linux-shaped): macOS exposes
//! `/var` and `/tmp` as root-owned symlinks into `/private`. The
//! per-component walks therefore treat a root-owned symlink sitting
//! directly under `/` whose target is inside `/private/` as a transparent
//! system alias (`splice_system_aliases`) instead of rejecting it, and the
//! temp-root base is canonicalized to its real path first. On Linux both
//! are no-ops and behavior matches the source.

use std::path::{Component, Path, PathBuf};

/// Max length of a safe path segment (ids, worktree basenames).
const MAX_SAFE_PATH_SEGMENT_LEN: usize = 128;

/// Windows reserved device basenames — rejected in any path segment.
fn is_windows_reserved_basename(base: &str) -> bool {
    matches!(
        base.to_ascii_uppercase().as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
            | "COM1" | "COM2" | "COM3" | "COM4" | "COM5" | "COM6" | "COM7" | "COM8" | "COM9"
            | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5" | "LPT6" | "LPT7" | "LPT8" | "LPT9"
    )
}

/// Whether `s` is a safe subagent / task id for path joins (worktree dirs,
/// session meta dirs, resume handles). Fail-closed: single segment, length
/// ≤ [`MAX_SAFE_PATH_SEGMENT_LEN`], no separators / NUL / `:` / control
/// chars, no surrounding whitespace (never silently trimmed), no leading
/// `.`/`..`, no trailing dot or space, not a Windows reserved basename,
/// ASCII alnum / `_` / `-` / internal `.` only (historical `task.v1` ids).
pub(crate) fn is_safe_task_id(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_SAFE_PATH_SEGMENT_LEN {
        return false;
    }
    // Explicit IDs must not be silently trimmed — surrounding whitespace is
    // invalid, not normalized away.
    if s != s.trim() {
        return false;
    }
    if s == "." || s == ".." {
        return false;
    }
    // Trailing dot/space are stripped or reserved on Windows; reject them.
    if s.ends_with('.') || s.ends_with(' ') {
        return false;
    }
    if s.contains('/') || s.contains('\\') || s.contains('\0') || s.contains(':') {
        return false;
    }
    if s.chars().any(|c| c.is_control()) {
        return false;
    }
    let basename = s.split('.').next().unwrap_or(s);
    if is_windows_reserved_basename(basename) {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// Identity of a validated worktree directory (canonical path + Unix dev/ino,
/// or mtime on non-Unix) for the pre-spawn TOCTOU recheck.
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
    /// Stat `path` without following symlinks and record identity. Refuses
    /// symlinks and non-directories.
    fn capture(path: &Path) -> Result<Self, String> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| {
            format!(
                "failed to read metadata for worktree '{}': {e}",
                path.display()
            )
        })?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "worktree '{}' became a symbolic link during validation",
                path.display()
            ));
        }
        if !meta.is_dir() {
            return Err(format!("worktree '{}' is not a directory", path.display()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                path: path.to_path_buf(),
                dev: meta.dev(),
                ino: meta.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            let modified_ms = meta.modified().ok().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis())
            });
            Ok(Self {
                path: path.to_path_buf(),
                modified_ms,
            })
        }
    }

    /// Re-stat `path` and ensure it still refers to the same directory
    /// object (path + dev/ino on Unix, mtime elsewhere).
    pub(crate) fn matches_path(&self, path: &Path) -> Result<(), String> {
        let now = Self::capture(path)?;
        if now.path != self.path {
            // Compare via re-canonicalize of the live path.
            let live = dunce::canonicalize(path)
                .map_err(|e| format!("re-canonicalize of '{}' failed: {e}", path.display()))?;
            if live != self.path {
                return Err(format!(
                    "worktree path changed: expected '{}', now '{}'",
                    self.path.display(),
                    live.display()
                ));
            }
        }
        #[cfg(unix)]
        {
            if now.dev != self.dev || now.ino != self.ino {
                return Err(format!(
                    "worktree inode replaced at '{}' (was dev={} ino={}, now dev={} ino={})",
                    self.path.display(),
                    self.dev,
                    self.ino,
                    now.dev,
                    now.ino
                ));
            }
        }
        #[cfg(not(unix))]
        {
            if now.modified_ms != self.modified_ms {
                return Err(format!(
                    "worktree metadata changed at '{}' (possible replacement)",
                    self.path.display()
                ));
            }
        }
        Ok(())
    }
}

/// macOS system-alias resolution: a symlink directly under `/` (e.g. `/var`,
/// `/tmp`) that is root-owned and targets a path inside `/private/`
/// (relative targets resolved against the alias's parent) is a system
/// layout alias, not an attacker-planted link. Returns the resolved target
/// when recognized, `None` otherwise.
#[cfg(unix)]
fn system_alias_target(component: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(component).ok()?;
    if !(meta.file_type().is_symlink() && meta.uid() == 0) {
        return None;
    }
    let target = std::fs::read_link(component).ok()?;
    // macOS uses relative targets (e.g. `/var -> private/var`); resolve
    // against the alias's parent so both relative and absolute forms work.
    let resolved = if target.is_absolute() {
        target
    } else {
        component.parent().map(|parent| parent.join(&target))?
    };
    if resolved.starts_with("/private") {
        return Some(resolved);
    }
    None
}

/// Rewrite `path` so leading system aliases (first component under `/`) are
/// spliced to their real targets before per-component walking. Deep symlinks
/// are left for the callers to reject — only top-level root-owned
/// `/private/` aliases are transparent.
#[cfg(unix)]
fn splice_system_aliases(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    let mut changed = true;
    while changed {
        changed = false;
        let comps: Vec<_> = p.components().collect();
        if comps.len() >= 3 && matches!(comps[0], Component::RootDir) {
            let alias = Path::new(comps[0].as_os_str()).join(comps[1].as_os_str());
            if let Some(target) = system_alias_target(&alias) {
                let mut out = target;
                for c in &comps[2..] {
                    out.push(c.as_os_str());
                }
                p = out;
                changed = true;
            }
        }
    }
    p
}

/// Pure Unix mode check for managed worktree **leaf** bases: owner-only
/// access (no group/world bits). Extracted for unit tests without needing
/// root. File-type bits in the high nibble are ignored.
#[cfg(unix)]
pub(crate) fn unix_mode_is_owner_only(mode: u32) -> bool {
    (mode & 0o077) == 0
}

/// Pure: group/world write bits must be zero (0755 ok; 0775/0777 not).
#[cfg(unix)]
pub(crate) fn unix_mode_no_group_world_write(mode: u32) -> bool {
    (mode & 0o022) == 0
}

/// Pure: sticky bit set (`S_ISVTX` = 0o1000).
#[cfg(unix)]
pub(crate) fn unix_mode_has_sticky(mode: u32) -> bool {
    (mode & 0o1000) != 0
}

/// Pure policy for one parent-chain component (not the leaf base itself).
///
/// Rules:
/// - owner must be `euid` or root (0)
/// - group/world write bits must be 0, **except** root-owned + sticky
///   (classic `/tmp` = `drwxrwxrwt`)
#[cfg(unix)]
pub(crate) fn unix_parent_component_is_safe(owner_uid: u32, mode: u32, euid: u32) -> bool {
    let is_root = owner_uid == 0;
    let is_self = owner_uid == euid;
    if !is_root && !is_self {
        return false;
    }
    if unix_mode_no_group_world_write(mode) {
        return true;
    }
    // Only root-owned sticky may retain world/group write (e.g. /tmp).
    is_root && unix_mode_has_sticky(mode)
}

/// Pure policy for XDG_RUNTIME_DIR (or similar runtime root) itself. Must be
/// owner-only, or at least free of group/world write.
#[cfg(unix)]
pub(crate) fn unix_xdg_runtime_dir_mode_ok(mode: u32) -> bool {
    unix_mode_is_owner_only(mode) || unix_mode_no_group_world_write(mode)
}

/// Walk every existing ancestor of `path` (not including `path` itself if it
/// does not yet exist) and enforce the Unix parent-chain policy.
///
/// Each component must be a non-symlink directory owned by eUID or root,
/// without group/world write unless root-owned + sticky.
#[cfg(unix)]
pub(crate) fn validate_unix_parent_chain(path: &Path) -> Result<(), String> {
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    let euid = unsafe { libc::geteuid() };
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("cannot resolve cwd for relative worktree: {e}"))?
            .join(path)
    };
    #[cfg(unix)]
    let abs = splice_system_aliases(&abs);
    let components: Vec<_> = abs.components().collect();
    // Drop the final component (the leaf); validate parents only.
    let parent_comps = if components.len() > 1 {
        &components[..components.len() - 1]
    } else {
        &components[..]
    };

    let mut acc = PathBuf::new();
    for component in parent_comps {
        acc.push(component.as_os_str());
        match component {
            Component::RootDir | Component::Prefix(_) => continue,
            _ => {}
        }
        let meta = fs::symlink_metadata(&acc)
            .map_err(|e| format!("cannot lstat parent component '{}': {e}", acc.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "parent component '{}' is a symbolic link; refusing",
                acc.display()
            ));
        }
        if !meta.is_dir() {
            return Err(format!(
                "parent component '{}' is not a directory; refusing",
                acc.display()
            ));
        }
        let mode = meta.mode();
        let owner = meta.uid();
        if !unix_parent_component_is_safe(owner, mode, euid) {
            return Err(format!(
                "parent component '{}' owner={} mode={:o} is not a safe parent \
                 (need owner euid/root, no group/world write unless root+sticky)",
                acc.display(),
                owner,
                mode & 0o7777
            ));
        }
    }
    Ok(())
}

/// Non-Unix no-op: ACL policy is degraded (documented in the source).
#[cfg(not(unix))]
pub(crate) fn validate_unix_parent_chain(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// True when `dir` is a usable XDG_RUNTIME_DIR: real dir, owner=euid, safe
/// mode, no symlink components on the path, and parents satisfy the safe
/// parent chain.
#[cfg(unix)]
fn xdg_runtime_dir_is_usable(dir: &Path) -> bool {
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    if dir.as_os_str().is_empty() {
        return false;
    }
    let abs = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(dir),
            Err(_) => return false,
        }
    };
    let meta = match fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return false;
    }
    let euid = unsafe { libc::geteuid() };
    if meta.uid() != euid {
        return false;
    }
    if !unix_xdg_runtime_dir_mode_ok(meta.mode()) {
        return false;
    }
    // Reject symlink ancestors of XDG itself.
    if reject_symlink_components(&abs).is_err() {
        return false;
    }
    // Parents of XDG must be safe (root 0755 / sticky /tmp ok). XDG itself
    // is checked above as euid-owned with no g/w write.
    if validate_unix_parent_chain(&abs).is_err() {
        return false;
    }
    true
}

/// User-private home fallback: `~/.grok/subagent-worktrees` (per-user, not
/// /tmp).
fn subagent_home_worktree_base() -> Option<PathBuf> {
    xai_dirs::home_dir().map(|h| h.join(".grok").join("subagent-worktrees"))
}

/// Per-UID private temp/home root for subagent worktrees.
///
/// Selection order (Unix):
/// 1. `XDG_RUNTIME_DIR/grok-subagent-worktrees-<uid>` only if XDG_RUNTIME_DIR
///    is a real dir, owner=eUID, mode owner-only (or no g/w write), no
///    symlink chain. Insecure XDG → soft-skip (do not Err).
/// 2. `temp_dir()/grok-subagent-worktrees-<uid>` only if the temp **parent**
///    satisfies the safe parent chain (root-owned sticky `/tmp` ok).
/// 3. Else `~/.grok/subagent-worktrees` (user-private home path).
///
/// Never uses a fixed shared name under the temp dir.
///
/// **Windows:** path namespaced by username; ACL not enforced (degraded).
pub(crate) fn subagent_temp_worktree_base() -> PathBuf {
    #[cfg(unix)]
    {
        let uid = unsafe { libc::geteuid() };
        let leaf = format!("grok-subagent-worktrees-{uid}");

        if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
            let p = PathBuf::from(&runtime);
            if xdg_runtime_dir_is_usable(&p) {
                return p.join(&leaf);
            }
            tracing::warn!(
                xdg_runtime_dir = %runtime,
                "XDG_RUNTIME_DIR unsafe for subagent worktrees; falling back"
            );
        }

        // Canonicalize the temp root first: on macOS it is reported under
        // the /var system alias; the managed base must live under the real
        // path.
        let temp_root = dunce::canonicalize(std::env::temp_dir())
            .unwrap_or_else(|_| std::env::temp_dir());
        let temp_candidate = temp_root.join(&leaf);
        // Parent of temp_candidate must be a safe parent chain (e.g. /tmp
        // sticky).
        if validate_unix_parent_chain(&temp_candidate).is_ok() {
            return temp_candidate;
        }
        tracing::warn!(
            temp = %temp_candidate.display(),
            "temp parent chain unsafe for subagent worktrees; using home fallback"
        );

        if let Some(home_base) = subagent_home_worktree_base() {
            return home_base;
        }
        // Last resort: still return the temp path (ensure_real_dir / parent
        // chain will fail closed at use time if unsafe).
        temp_candidate
    }
    #[cfg(not(unix))]
    {
        let user = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "user".into());
        let safe: String = user
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        std::env::temp_dir().join(format!("grok-subagent-worktrees-{safe}"))
    }
}

/// Ensure `path` exists as a real (non-symlink) directory suitable as a
/// managed worktree **leaf** base, and that its parent chain is safe.
///
/// On Unix, **every** use requires for the leaf:
/// - owner == current eUID
/// - mode has no group/world bits (effectively `0o700`)
///
/// And for every parent component:
/// - non-symlink directory
/// - owner eUID or root
/// - no group/world write unless root-owned + sticky (`/tmp`)
///
/// If the directory is newly created, `chmod 0700` **must** succeed (`Err`
/// on failure). Existing leaves with group/world **write** bits are
/// **rejected**; existing euid-owned write-safe leaves (e.g. the app's
/// 0755 `~/.grok/worktrees/<repo>`) are tightened to 0700 — never loosened.
///
/// **Windows:** only non-symlink directory existence; NTFS ACLs degraded.
pub(crate) fn ensure_real_dir(path: &Path) -> Result<(), String> {
    use std::fs;

    // Parent chain first (before create) so we never mkdir under an unsafe
    // parent.
    validate_unix_parent_chain(path)?;

    let created = !path.exists();
    if created {
        // Ensure parents exist only if the parent chain was already
        // validated for the full path; create_dir_all may create
        // intermediate dirs — those are owned by us. Re-validate after.
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "failed to create parent of managed worktree base '{}': {e}",
                    path.display()
                )
            })?;
            // Intermediate parents we just created must be owner-only too if
            // they sit under a sticky temp root (e.g. /tmp/foo created by
            // us).
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }
        fs::create_dir(path)
            .or_else(|_| {
                // Race: another process created it.
                if path.is_dir() {
                    Ok(())
                } else {
                    fs::create_dir_all(path).map(|_| ())
                }
            })
            .map_err(|e| {
                format!(
                    "failed to create managed worktree base '{}': {e}",
                    path.display()
                )
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| {
                format!(
                    "failed to chmod 0700 managed worktree base '{}': {e}",
                    path.display()
                )
            })?;
        }
    }
    let meta = fs::symlink_metadata(path).map_err(|e| {
        format!(
            "managed worktree base '{}' is not accessible: {e}",
            path.display()
        )
    })?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "managed worktree base '{}' is a symbolic link; refusing",
            path.display()
        ));
    }
    if !meta.is_dir() {
        return Err(format!(
            "managed worktree base '{}' is not a directory",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let euid = unsafe { libc::geteuid() };
        if meta.uid() != euid {
            return Err(format!(
                "managed worktree base '{}' owner uid {} != current euid {}; refusing \
                 (will not chown/chmod an existing insecure base)",
                path.display(),
                meta.uid(),
                euid
            ));
        }
        let mode = meta.mode();
        if !unix_mode_no_group_world_write(mode) {
            return Err(format!(
                "managed worktree base '{}' mode {:o} has group/world write bits; \
                 refusing (expected no group/world write; will not auto-chmod)",
                path.display(),
                mode & 0o777
            ));
        }
        if !unix_mode_is_owner_only(mode) {
            // WT adaptation: the app's worktree subsystem creates
            // `~/.grok/worktrees/<repo>` with default 0755 mode. Tightening
            // an existing euid-owned, write-safe base to 0700 is safe (only
            // removes bits); loosening or accepting write-accessible bases
            // stays a refusal.
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| {
                format!(
                    "failed to tighten managed worktree base '{}' to 0700: {e}",
                    path.display()
                )
            })?;
        }
        // Parent chain again after create (intermediates may have appeared).
        validate_unix_parent_chain(path)?;
    }
    let _ = created;
    Ok(())
}

/// Walk every component from the filesystem root (or relative start) to
/// `path` and reject any symlink component. `path` must already exist.
fn reject_symlink_components(path: &Path) -> Result<(), String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("cannot resolve cwd for relative worktree: {e}"))?
            .join(path)
    };
    #[cfg(unix)]
    let abs = splice_system_aliases(&abs);
    let mut acc = PathBuf::new();
    for component in abs.components() {
        acc.push(component.as_os_str());
        match std::fs::symlink_metadata(&acc) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(format!(
                        "path component '{}' is a symbolic link; refusing",
                        acc.display()
                    ));
                }
            }
            Err(e) => {
                return Err(format!(
                    "cannot lstat path component '{}': {e}",
                    acc.display()
                ));
            }
        }
    }
    Ok(())
}

/// True when `child` is a strict descendant of `base` (both already
/// canonical).
fn is_strict_descendant(child: &Path, base: &Path) -> bool {
    child.starts_with(base) && child != base
}

/// Validate a resumed (or freshly created) subagent worktree path.
///
/// Fail-closed (no lexical `starts_with` success path):
/// 1. `dest` must exist, not be a symlink, and be a directory.
/// 2. Every path component to `dest` must be non-symlink.
/// 3. Canonicalize `dest`; must still be a directory.
/// 4. Allowed managed bases must exist (created if needed), not be symlinks,
///    and canonicalize successfully; every base component non-symlink.
/// 5. Canonical dest must be a **strict** descendant of a canonical base.
/// 6. Dest must not equal parent session cwd.
/// 7. Basename must be exactly `subagent-{id}` when `subagent_id` is
///    provided (production naming; no unprefixed legacy directories).
///
/// Returns a [`WorktreeIdentity`] (canonical path + Unix dev/ino) for
/// callers to recheck immediately before child start.
///
/// **TOCTOU honesty:** same-user rename races cannot be fully eliminated
/// without openat/O_NOFOLLOW across platforms; the inode recheck narrows
/// the window but is not a capability-style handle.
pub(crate) fn validate_subagent_worktree_path(
    dest: &Path,
    source_cwd: &Path,
    parent_cwd: &Path,
    subagent_id: Option<&str>,
) -> Result<WorktreeIdentity, String> {
    use std::fs;

    // 1. dest exists, not symlink, is dir.
    match fs::symlink_metadata(dest) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "subagent worktree path '{}' is a symbolic link; refusing to use it",
                    dest.display()
                ));
            }
            if !meta.is_dir() {
                return Err(format!(
                    "subagent worktree path '{}' is not a directory",
                    dest.display()
                ));
            }
        }
        Err(e) => {
            return Err(format!(
                "subagent worktree path '{}' is not accessible: {e}",
                dest.display()
            ));
        }
    }

    // 2. No symlink components on the path to dest (use absolute form when
    // possible).
    let abs_dest = if dest.is_absolute() {
        dest.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("cannot resolve cwd for relative worktree: {e}"))?
            .join(dest)
    };
    reject_symlink_components(&abs_dest)?;

    // 3. Canonicalize dest.
    let canonical = dunce::canonicalize(dest).map_err(|e| {
        format!(
            "failed to canonicalize subagent worktree '{}': {e}",
            dest.display()
        )
    })?;
    if !canonical.is_dir() {
        return Err(format!(
            "canonicalized worktree '{}' is not a directory",
            canonical.display()
        ));
    }
    // Re-check components on the canonical path too.
    reject_symlink_components(&canonical)?;

    // 4–5. Managed bases: create if needed, non-symlink, Unix owner+0700,
    // canonicalize, strict contain. Temp fallback is per-UID private.
    let mut allowed_bases: Vec<PathBuf> = Vec::new();
    if let Ok(base) = crate::session::worktree::worktree_base_dir_for_source(source_cwd) {
        allowed_bases.push(base);
    }
    allowed_bases.push(subagent_temp_worktree_base());

    let mut matched_base: Option<PathBuf> = None;
    let mut base_errors: Vec<String> = Vec::new();
    for base in &allowed_bases {
        if let Err(e) = ensure_real_dir(base) {
            base_errors.push(e);
            continue;
        }
        if let Err(e) = reject_symlink_components(base) {
            base_errors.push(e);
            continue;
        }
        let base_canon = match dunce::canonicalize(base) {
            Ok(p) => p,
            Err(e) => {
                base_errors.push(format!(
                    "failed to canonicalize managed base '{}': {e}",
                    base.display()
                ));
                continue;
            }
        };
        if let Err(e) = reject_symlink_components(&base_canon) {
            base_errors.push(e);
            continue;
        }
        // Re-verify owner+mode on the canonical base path (not just the
        // pre-canonical lexical path).
        if let Err(e) = ensure_real_dir(&base_canon) {
            base_errors.push(e);
            continue;
        }
        if is_strict_descendant(&canonical, &base_canon) {
            matched_base = Some(base_canon);
            break;
        }
    }
    if matched_base.is_none() {
        let detail = if base_errors.is_empty() {
            "not a strict descendant of any managed base".to_string()
        } else {
            format!(
                "not under a valid managed base ({})",
                base_errors.join("; ")
            )
        };
        return Err(format!(
            "subagent worktree '{}' is outside managed worktree bases \
             (expected under ~/.grok/worktrees/... or per-UID temp \
             grok-subagent-worktrees-<uid>): {detail}",
            canonical.display()
        ));
    }

    // 6. Not parent cwd (equality only; containment under parent without a
    // managed base already fails step 5).
    if let Ok(parent_canon) = dunce::canonicalize(parent_cwd)
        && canonical == parent_canon
    {
        return Err(format!(
            "subagent worktree '{}' resolves to the parent session cwd; \
             isolation refused",
            canonical.display()
        ));
    }
    // Silence unused warning when all bases failed before assignment use.
    let _ = matched_base;

    // 7. Strict basename identity: production format is always
    // `subagent-{id}`.
    if let Some(id) = subagent_id {
        if !is_safe_task_id(id) {
            return Err(format!(
                "subagent id {id:?} is not a safe path segment for worktree naming"
            ));
        }
        let expected = format!("subagent-{id}");
        let name = canonical.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name != expected {
            return Err(format!(
                "subagent worktree basename '{name}' must be exactly '{expected}' \
                 (agent metadata must not point at another subagent's directory)"
            ));
        }
    }

    // Capture identity for pre-spawn recheck.
    WorktreeIdentity::capture(&canonical)
}

/// Final pre-spawn check: re-validate path and confirm identity still
/// matches.
pub(crate) fn recheck_worktree_identity(
    expected: &WorktreeIdentity,
    source_cwd: &Path,
    parent_cwd: &Path,
    subagent_id: Option<&str>,
) -> Result<WorktreeIdentity, String> {
    let again =
        validate_subagent_worktree_path(&expected.path, source_cwd, parent_cwd, subagent_id)?;
    expected.matches_path(&again.path)?;
    #[cfg(unix)]
    {
        if again.dev != expected.dev || again.ino != expected.ino {
            return Err(format!(
                "worktree identity changed before spawn at '{}' \
                 (dev/ino {}/{} → {}/{})",
                expected.path.display(),
                expected.dev,
                expected.ino,
                again.dev,
                again.ino
            ));
        }
    }
    Ok(again)
}
