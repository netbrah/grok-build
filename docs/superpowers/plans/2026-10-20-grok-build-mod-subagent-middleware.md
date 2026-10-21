# Grok-Build Mod Subagent Middleware (`xai-grok-mods` + `plugins/cursor`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `xai-grok-mods` leaf crate, the `xai-grok-shell/src/agent/subagent/mod_adapter.rs` shell adapter, and the `plugins/cursor` reference sidecar plugin so `cursor:<model>` subagents work across `Task`, v2 `spawn_agent`/`send_message`/`wait`/`close_agent`, and Rhai `agent()`/`spawn_agents()` within an upstream diff budget of ≤ 85 lines across 6 existing files.

**Architecture:** `xai-grok-mods` is an acyclic leaf crate depending on `xai-grok-tools` and `xai-tool-types` that implements `mod.json` validation, the NDJSON sidecar protocol (`run.start`, `run.message`, `run.drain`, `run.finish`, `run.interrupt`, `run.cancel`), `ModChildControl` (`impl ChildControl`), `sessions.json` persistence, and the per-run Unix socket MCP bridge (`$TMPDIR/gm-<8hex>.sock`). `xai-grok-shell` wraps `ShellChildRuntime` as `enum ShellChildRuntime { Native(NativeShellChildRuntime), Mod(xai_grok_mods::ModChildControl) }` and dispatches to `subagent::mod_adapter::run` inside `run_shell_child` (`handle_request.rs` after line 911), reusing existing definition resolution, gating, resume-source lookup, and git worktree creation/disposal with zero changes to `xai-grok-agent`.

**Tech Stack:** Rust 2024 edition (`tokio`, `tokio-util`, `serde`, `serde_json`, `parking_lot`, `dunce`, `which`, `libc`, `rand`, `thiserror`, `tracing`), Node.js ESM (`sidecar/runner.mjs`, `sidecar/catalog.mjs`).

**Spec:** `docs/superpowers/specs/2026-10-20-grok-build-mod-subagent-middleware-design.md` (Approved, Round 3 Code-Verified)

## Global Constraints

- **Target repo & branch:** `refs/netbrah-grok-build` (`feat/hts-native-shipping` @ `2fcd97294a23cfb19b1d6e4c1a788840a17dc569`).
- **Upstream diff budget:** ≤ 85 changed lines across 6 existing files (`Cargo.toml`, `crates/codegen/xai-grok-shell/Cargo.toml`, `crates/codegen/xai-grok-shell/src/agent/subagent/mod.rs`, `child_runtime.rs`, `handle_request.rs`, `child_runtime_tests.rs`).
- **Zero-touch crates:** `0` lines modified in `xai-grok-agent`, `xai-grok-tools`, `xai-grok-mcp`, `xai-workflow`, `spawn.rs`, and `start_artifact_publication.rs`.
- **Acyclic crate graph:** `xai-grok-mods` depends only on `xai-grok-tools`, `xai-tool-types`, and workspace external crates — never on `xai-grok-agent`, `xai-grok-mcp`, or `xai-grok-shell`.
- **Plugin namespace invariant:** `.grok-plugin/plugin.json` `"name"` MUST equal `mod.json` `"namespace"` (`"cursor"`).
- **Socket path bound:** MCP bridge Unix domain socket path `$TMPDIR/gm-<8hex>.sock` MUST be `< 100` bytes (falling back to `/tmp/gm-<8hex>.sock`) with `0600` permissions and 32-byte random hex token authentication.

---

### Task 1: Workspace Registration, `manifest.rs` & `protocol.rs` in `xai-grok-mods`

**Files:**
- Modify: `Cargo.toml:69` (add `"crates/codegen/xai-grok-mods"` to `[workspace].members` and `xai-grok-mods = { path = "crates/codegen/xai-grok-mods" }` to `[workspace.dependencies]`)
- Create: `crates/codegen/xai-grok-mods/Cargo.toml`
- Create: `crates/codegen/xai-grok-mods/src/lib.rs`
- Create: `crates/codegen/xai-grok-mods/src/manifest.rs`
- Create: `crates/codegen/xai-grok-mods/src/protocol.rs`

**Interfaces:**
- Consumes: `dunce::canonicalize`, `which::which`, `serde::{Deserialize, Serialize}`
- Produces:
  - `xai_grok_mods::manifest::{ModConfig, ModRuntime, ModCapabilities, ModMcpBridge, ValidatedMod, ModError}`
  - `ModConfig::load(plugin_name: &str, canonical_root: &Path) -> Result<ValidatedMod, ModError>`
  - `xai_grok_mods::protocol::{HostFrame, SidecarFrame, MessageMode, BridgeInfo, BridgedTool, VendorToolActivity, BridgeRpcRequest, BridgeRpcResponse}`

- [ ] **Step 1: Create `crates/codegen/xai-grok-mods/Cargo.toml` and register in workspace `Cargo.toml`**

Add `"crates/codegen/xai-grok-mods"` to `[workspace].members` in `Cargo.toml` and create `crates/codegen/xai-grok-mods/Cargo.toml`:
```toml
[package]
name = "xai-grok-mods"
version = "0.1.0"
edition = "2024"
license = "Apache-2.0"

[dependencies]
dunce = { workspace = true }
libc = { workspace = true }
parking_lot = { workspace = true }
rand = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
tracing = { workspace = true }
which = { workspace = true }
xai-grok-tools = { workspace = true }
xai-tool-types = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 2: Write failing unit tests in `crates/codegen/xai-grok-mods/src/manifest.rs` and `protocol.rs`**

Create `crates/codegen/xai-grok-mods/src/protocol.rs` and `crates/codegen/xai-grok-mods/src/manifest.rs` with unit tests `t1_manifest_validates_namespace_and_containment` and `protocol_roundtrips_ndjson_frames`:
```rust
// crates/codegen/xai-grok-mods/src/protocol.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgedTool {
    pub server: String,
    pub tool: String,
    pub name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeInfo {
    pub socket_path: String,
    pub token: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageMode {
    Steer,
    Queue,
    Followup,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum HostFrame {
    #[serde(rename = "run.start")]
    Start {
        protocol: u32,
        subagent_id: String,
        parent_session_id: String,
        subagent_type: String,
        agent_name: String,
        model: String,
        cwd: String,
        prompt: String,
        system_prompt: Option<String>,
        resume_vendor_session_id: Option<String>,
        read_only: bool,
        reasoning_effort: Option<String>,
        bridge: Option<BridgeInfo>,
        bridged_tools: Vec<BridgedTool>,
    },
    #[serde(rename = "run.message")]
    Message {
        message_id: String,
        mode: MessageMode,
        from: String,
        text: String,
    },
    #[serde(rename = "run.drain")]
    Drain,
    #[serde(rename = "run.finish")]
    Finish,
    #[serde(rename = "run.interrupt")]
    Interrupt,
    #[serde(rename = "run.cancel")]
    Cancel { reason: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VendorToolActivity {
    pub name: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum SidecarFrame {
    #[serde(rename = "run.started")]
    Started {
        vendor_session_id: String,
        #[serde(default)]
        effective_model: Option<String>,
    },
    #[serde(rename = "run.progress")]
    Progress {
        turns: u32,
        tool_calls: u32,
        tokens_used: u64,
        #[serde(default)]
        tool: Option<VendorToolActivity>,
    },
    #[serde(rename = "run.consumed")]
    Consumed { message_id: String },
    #[serde(rename = "run.idle")]
    Idle { final_text: String },
    #[serde(rename = "run.completed")]
    Completed {
        final_text: String,
        turns: u32,
        tool_calls: u32,
        tokens_used: u64,
    },
    #[serde(rename = "run.failed")]
    Failed {
        error: String,
        #[serde(default)]
        cancelled: bool,
        #[serde(default)]
        turns: u32,
        #[serde(default)]
        tool_calls: u32,
        #[serde(default)]
        tokens_used: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeRpcRequest {
    pub id: String,
    pub token: String,
    pub method: String,
    pub server: String,
    pub tool: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeRpcResponse {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
```

- [ ] **Step 3: Implement `crates/codegen/xai-grok-mods/src/manifest.rs` with path containment & mtime cache**

```rust
// crates/codegen/xai-grok-mods/src/manifest.rs
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;
use std::time::SystemTime;

use parking_lot::Mutex;
use serde::Deserialize;
use thiserror::Error;

const ALLOWED_BARE_COMMANDS: &[&str] = &["node", "bun", "deno", "python3"];

#[derive(Debug, Error, Clone)]
pub enum ModError {
    #[error("failed to read mod.json: {0}")]
    Io(String),
    #[error("failed to parse mod.json: {0}")]
    Parse(String),
    #[error("unsupported mod.json schemaVersion {0} (expected 1)")]
    UnsupportedSchema(u32),
    #[error("mod.json namespace '{namespace}' must equal plugin name '{plugin_name}'")]
    NamespaceMismatch { namespace: String, plugin_name: String },
    #[error("invalid mod namespace '{0}'")]
    InvalidNamespace(String),
    #[error("disallowed runtime command '{0}'")]
    DisallowedCommand(String),
    #[error("runtime path escapes plugin root: '{0}'")]
    PathEscape(String),
}

fn default_timeout_secs() -> u64 {
    1800
}
fn default_graceful_kill_ms() -> u64 {
    2000
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModConfig {
    pub schema_version: u32,
    pub namespace: String,
    pub runtime: ModRuntime,
    #[serde(default)]
    pub capabilities: ModCapabilities,
    #[serde(default)]
    pub mcp_bridge: ModMcpBridge,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModRuntime {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_graceful_kill_ms")]
    pub graceful_kill_ms: u64,
    #[serde(default)]
    pub env_allowlist: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModCapabilities {
    #[serde(default)]
    pub live_steer: bool,
    #[serde(default)]
    pub session_resume: bool,
    #[serde(default)]
    pub mcp_bridge: bool,
    #[serde(default)]
    pub mcp_in_read_only: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModMcpBridge {
    #[serde(default)]
    pub deny_servers: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ValidatedMod {
    pub config: ModConfig,
    pub root: PathBuf,
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

static MOD_CACHE: LazyLock<Mutex<HashMap<(PathBuf, Option<SystemTime>), ValidatedMod>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn is_valid_namespace(ns: &str) -> bool {
    let bytes = ns.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

fn has_parent_or_root_component(p: &Path) -> bool {
    p.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

impl ModConfig {
    pub fn load(plugin_name: &str, canonical_root: &Path) -> Result<ValidatedMod, ModError> {
        let root = dunce::canonicalize(canonical_root)
            .map_err(|e| ModError::Io(format!("{}: {e}", canonical_root.display())))?;
        let manifest_path = root.join("mod.json");
        let mtime = std::fs::metadata(&manifest_path)
            .and_then(|m| m.modified())
            .ok();
        if let Some(cached) = MOD_CACHE.lock().get(&(root.clone(), mtime)).cloned() {
            if cached.config.namespace == plugin_name {
                return Ok(cached);
            }
        }
        let raw = std::fs::read_to_string(&manifest_path)
            .map_err(|e| ModError::Io(format!("{}: {e}", manifest_path.display())))?;
        let config: ModConfig =
            serde_json::from_str(&raw).map_err(|e| ModError::Parse(e.to_string()))?;
        if config.schema_version != 1 {
            return Err(ModError::UnsupportedSchema(config.schema_version));
        }
        if config.namespace != plugin_name {
            return Err(ModError::NamespaceMismatch {
                namespace: config.namespace,
                plugin_name: plugin_name.to_owned(),
            });
        }
        if !is_valid_namespace(&config.namespace) {
            return Err(ModError::InvalidNamespace(config.namespace));
        }

        let cmd_str = config.runtime.command.as_str();
        let program = if !cmd_str.contains('/') && !cmd_str.contains('\\') {
            if !ALLOWED_BARE_COMMANDS.contains(&cmd_str) {
                return Err(ModError::DisallowedCommand(cmd_str.to_owned()));
            }
            which::which(cmd_str).map_err(|e| ModError::DisallowedCommand(format!("{cmd_str}: {e}")))?
        } else {
            let rel = Path::new(cmd_str);
            if has_parent_or_root_component(rel) {
                return Err(ModError::PathEscape(cmd_str.to_owned()));
            }
            let joined = root.join(rel);
            let canon = dunce::canonicalize(&joined)
                .map_err(|e| ModError::PathEscape(format!("{cmd_str}: {e}")))?;
            if !canon.starts_with(&root) {
                return Err(ModError::PathEscape(cmd_str.to_owned()));
            }
            canon
        };

        let mut resolved_args = Vec::with_capacity(config.runtime.args.len());
        for arg in &config.runtime.args {
            if arg.starts_with('-') {
                resolved_args.push(OsString::from(arg));
                continue;
            }
            let arg_path = Path::new(arg);
            if has_parent_or_root_component(arg_path) {
                return Err(ModError::PathEscape(arg.clone()));
            }
            let joined = root.join(arg_path);
            if joined.symlink_metadata().is_ok() {
                let canon = dunce::canonicalize(&joined)
                    .map_err(|e| ModError::PathEscape(format!("{arg}: {e}")))?;
                if !canon.starts_with(&root) {
                    return Err(ModError::PathEscape(arg.clone()));
                }
                resolved_args.push(canon.into_os_string());
            } else {
                resolved_args.push(joined.into_os_string());
            }
        }

        let validated = ValidatedMod {
            config,
            root: root.clone(),
            program,
            args: resolved_args,
        };
        MOD_CACHE.lock().insert((root, mtime), validated.clone());
        Ok(validated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t1_manifest_validates_namespace_and_containment() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("sidecar")).unwrap();
        std::fs::write(root.join("sidecar/runner.mjs"), "// ok\n").unwrap();

        // Valid manifest
        std::fs::write(
            root.join("mod.json"),
            r#"{"schemaVersion":1,"namespace":"cursor","runtime":{"command":"node","args":["sidecar/runner.mjs"]}}"#,
        )
        .unwrap();
        let v = ModConfig::load("cursor", &root).expect("valid mod.json");
        assert_eq!(v.config.namespace, "cursor");
        assert_eq!(v.args.len(), 1);

        // Namespace mismatch rejected
        assert!(matches!(
            ModConfig::load("other", &root),
            Err(ModError::NamespaceMismatch { .. })
        ));

        // Disallowed bare command rejected
        std::fs::write(
            root.join("mod.json"),
            r#"{"schemaVersion":1,"namespace":"cursor","runtime":{"command":"bash","args":[]}}"#,
        )
        .unwrap();
        assert!(matches!(
            ModConfig::load("cursor", &root),
            Err(ModError::DisallowedCommand(_))
        ));

        // Path traversal in args rejected
        std::fs::write(
            root.join("mod.json"),
            r#"{"schemaVersion":1,"namespace":"cursor","runtime":{"command":"node","args":["../escape.mjs"]}}"#,
        )
        .unwrap();
        assert!(matches!(
            ModConfig::load("cursor", &root),
            Err(ModError::PathEscape(_))
        ));

        // Unknown field rejected by deny_unknown_fields
        std::fs::write(
            root.join("mod.json"),
            r#"{"schemaVersion":1,"namespace":"cursor","unknownField":true,"runtime":{"command":"node","args":[]}}"#,
        )
        .unwrap();
        assert!(matches!(
            ModConfig::load("cursor", &root),
            Err(ModError::Parse(_))
        ));

        // Symlink escape out of plugin root rejected
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            let outside_script = outside.path().join("outside.mjs");
            std::fs::write(&outside_script, "// outside\n").unwrap();
            let link_path = root.join("sidecar/escape_link.mjs");
            std::os::unix::fs::symlink(&outside_script, &link_path).unwrap();
            std::fs::write(
                root.join("mod.json"),
                r#"{"schemaVersion":1,"namespace":"cursor","runtime":{"command":"node","args":["sidecar/escape_link.mjs"]}}"#,
            )
            .unwrap();
            assert!(matches!(
                ModConfig::load("cursor", &root),
                Err(ModError::PathEscape(_))
            ));
        }
    }
}
```

 Also create `crates/codegen/xai-grok-mods/src/lib.rs` exporting `manifest` and `protocol` plus the root traits and structs used by subsequent modules:
```rust
// crates/codegen/xai-grok-mods/src/lib.rs
pub mod manifest;
pub mod protocol;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;
use xai_grok_tools::implementations::grok_build::task::coordinator::SubagentProgress;

pub use manifest::{ModCapabilities, ModConfig, ModError, ModMcpBridge, ModRuntime, ValidatedMod};
pub use protocol::{
    BridgeInfo, BridgeRpcRequest, BridgeRpcResponse, BridgedTool, HostFrame, MessageMode,
    SidecarFrame, VendorToolActivity,
};

pub type BorrowedSendFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone)]
pub struct ModStarted {
    pub vendor_session_id: String,
    pub effective_model: String,
}

#[derive(Debug, Clone)]
pub struct ModRunInput {
    pub validated: ValidatedMod,
    pub subagent_id: String,
    pub parent_session_id: String,
    pub subagent_type: String,
    pub agent_name: String,
    pub vendor_model: String,
    pub cwd: PathBuf,
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub resume_key: Option<String>,
    pub read_only: bool,
    pub reasoning_effort: Option<String>,
    pub bridged_tools: Vec<BridgedTool>,
    pub cancellation: CancellationToken,
}

#[derive(Debug, Clone)]
pub struct ModRunOutcome {
    pub promoted: bool,
    pub final_text: String,
    pub error: Option<String>,
    pub cancelled: bool,
    pub turns: u32,
    pub tool_calls: u32,
    pub tokens_used: u64,
    pub vendor_session_id: Option<String>,
    pub admission_uncertain: bool,
    pub warnings: Vec<String>,
}
```

- [ ] **Step 4: Run `cargo test -p xai-grok-mods` to verify T1 passes**

Run: `cargo test -p xai-grok-mods`
Expected: PASS (`t1_manifest_validates_namespace_and_containment`).

- [ ] **Step 5: Commit Task 1**

```bash
git add Cargo.toml crates/codegen/xai-grok-mods/
git commit -m "feat(mods): add xai-grok-mods crate with manifest validation and NDJSON protocol"
```

---

### Task 2: Vendor Session Store (`store.rs`) & `ModChildControl` (`control.rs`)

**Files:**
- Create: `crates/codegen/xai-grok-mods/src/store.rs`
- Create: `crates/codegen/xai-grok-mods/src/control.rs`
- Modify: `crates/codegen/xai-grok-mods/src/lib.rs`

**Interfaces:**
- Consumes:
  - `xai_grok_tools::implementations::grok_build::task::coordinator::{ActiveMessageAdmission, ChildControl, LocalBoxFuture, SendBoxFuture, SubagentProgress}`
  - `xai_grok_tools::implementations::grok_build::task::types::{ActiveAgentMessageDelivery, ActiveAgentMessageOperation, AgentMailboxMessage}`
  - `crate::protocol::{HostFrame, MessageMode}`
- Produces:
  - `xai_grok_mods::store::{ModSessionStore, SessionRecord, select_resume_key}`
  - `xai_grok_mods::control::ModChildControl` (`impl ChildControl for ModChildControl`)

- [ ] **Step 1: Implement `crates/codegen/xai-grok-mods/src/store.rs` with unit test T4**

```rust
// crates/codegen/xai-grok-mods/src/store.rs
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub vendor_session_id: String,
    pub parent_session_id: String,
    pub subagent_type: String,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ModSessionStore {
    base_dir: PathBuf,
}

pub fn select_resume_key(
    request_id: &str,
    resume_from: Option<&str>,
    is_wake: bool,
) -> Option<String> {
    if is_wake {
        Some(request_id.to_owned())
    } else {
        resume_from.filter(|s| !s.is_empty()).map(str::to_owned)
    }
}

impl ModSessionStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    pub fn default_grok_home() -> Self {
        let home = std::env::var_os("GROK_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".grok")))
            .unwrap_or_else(|| PathBuf::from(".grok"));
        Self::new(home.join("mods"))
    }

    fn ns_dir(&self, namespace: &str) -> PathBuf {
        self.base_dir.join(namespace)
    }

    fn with_lock<T>(&self, namespace: &str, f: impl FnOnce(&Path) -> T) -> std::io::Result<T> {
        let dir = self.ns_dir(namespace);
        std::fs::create_dir_all(&dir)?;
        let lock_path = dir.join("sessions.json.lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        #[cfg(unix)]
        unsafe {
            use std::os::unix::io::AsRawFd;
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX);
        }
        let sessions_path = dir.join("sessions.json");
        let res = f(&sessions_path);
        #[cfg(unix)]
        unsafe {
            use std::os::unix::io::AsRawFd;
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
        }
        Ok(res)
    }

    pub fn lookup(
        &self,
        namespace: &str,
        resume_key: &str,
        parent_session_id: &str,
    ) -> Option<String> {
        self.with_lock(namespace, |path| {
            let raw = std::fs::read_to_string(path).ok()?;
            let map: HashMap<String, SessionRecord> = serde_json::from_str(&raw).ok()?;
            let rec = map.get(resume_key)?;
            (rec.parent_session_id == parent_session_id).then(|| rec.vendor_session_id.clone())
        })
        .ok()
        .flatten()
    }

    pub fn upsert(&self, namespace: &str, subagent_id: &str, record: SessionRecord) {
        let _ = self.with_lock(namespace, |path| {
            let mut map: HashMap<String, SessionRecord> = std::fs::read_to_string(path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            map.insert(subagent_id.to_owned(), record);
            let tmp_path = path.with_extension(format!("json.tmp.{}", rand::random::<u32>()));
            if let Ok(mut f) = File::create(&tmp_path) {
                if let Ok(bytes) = serde_json::to_vec_pretty(&map) {
                    if f.write_all(&bytes).is_ok() && f.sync_all().is_ok() {
                        let _ = std::fs::rename(&tmp_path, path);
                    }
                }
                let _ = std::fs::remove_file(&tmp_path);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t4_store_atomic_upsert_and_resume_key_selection() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ModSessionStore::new(tmp.path());
        store.upsert(
            "cursor",
            "sub-1",
            SessionRecord {
                vendor_session_id: "vendor-abc".into(),
                parent_session_id: "parent-1".into(),
                subagent_type: "cursor:composer-2".into(),
                updated_at_ms: 100,
            },
        );
        assert_eq!(
            store.lookup("cursor", "sub-1", "parent-1").as_deref(),
            Some("vendor-abc")
        );
        // Foreign parent session must miss
        assert_eq!(store.lookup("cursor", "sub-1", "parent-2"), None);

        // Wake key vs explicit resume_from key
        assert_eq!(
            select_resume_key("sub-1", None, true).as_deref(),
            Some("sub-1")
        );
        assert_eq!(
            select_resume_key("sub-2", Some("sub-1"), false).as_deref(),
            Some("sub-1")
        );
        assert_eq!(select_resume_key("sub-2", None, false), None);
    }
}
```

- [ ] **Step 2: Implement `crates/codegen/xai-grok-mods/src/control.rs` with unit tests T2 & T3**

Note on T2 (from Spec §10 R1): `ActiveAgentMessageDelivery::new` is `pub(crate)` in `xai-grok-tools`, so `control.rs` tests exercise `send_active_message` through `SubagentCoordinator` (`xai_grok_tools::implementations::grok_build::task::coordinator::SubagentCoordinator`), which is public in `xai-grok-tools`, while testing `accepts_native_message`, `deliver_initial_message`, `deliver_followup`, `interrupt`, and `cancel` directly.
```rust
// crates/codegen/xai-grok-mods/src/control.rs
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use xai_grok_tools::implementations::grok_build::task::coordinator::{
    ActiveMessageAdmission, ChildControl, LocalBoxFuture, SendBoxFuture, SubagentProgress,
};
use xai_grok_tools::implementations::grok_build::task::types::{
    ActiveAgentMessageDelivery, ActiveAgentMessageOperation, AgentMailboxMessage,
};

use crate::protocol::{HostFrame, MessageMode};

#[derive(Clone)]
pub struct ModChildControl {
    tx: mpsc::UnboundedSender<HostFrame>,
    pending: Arc<Mutex<HashSet<String>>>,
    progress: Arc<Mutex<SubagentProgress>>,
    live_steer: bool,
    cancel: CancellationToken,
    closed: Arc<AtomicBool>,
}

impl ModChildControl {
    pub fn new(
        tx: mpsc::UnboundedSender<HostFrame>,
        pending: Arc<Mutex<HashSet<String>>>,
        progress: Arc<Mutex<SubagentProgress>>,
        live_steer: bool,
        cancel: CancellationToken,
        closed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            tx,
            pending,
            progress,
            live_steer,
            cancel,
            closed,
        }
    }

    fn followup(&self, m: &AgentMailboxMessage) -> bool {
        if !self.accepts_native_message(m) || self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.pending.lock().insert(m.message_id.clone());
        let ok = self
            .tx
            .send(HostFrame::Message {
                message_id: m.message_id.clone(),
                mode: MessageMode::Followup,
                from: m.from_agent_id.clone(),
                text: m.body.clone(),
            })
            .is_ok();
        if !ok {
            self.pending.lock().remove(&m.message_id);
        }
        ok
    }
}

impl ChildControl for ModChildControl {
    type ProgressFuture = LocalBoxFuture<SubagentProgress>;

    fn progress(&self) -> Self::ProgressFuture {
        let p = self.progress.lock().clone();
        Box::pin(std::future::ready(p))
    }

    fn send_active_message(
        &self,
        d: ActiveAgentMessageDelivery,
    ) -> SendBoxFuture<ActiveMessageAdmission> {
        if !self.live_steer {
            return Box::pin(std::future::ready(ActiveMessageAdmission::Unsupported));
        }
        if self.closed.load(Ordering::Acquire) || self.tx.is_closed() {
            return Box::pin(std::future::ready(ActiveMessageAdmission::ChannelClosed));
        }
        let m = d.message();
        let frame = HostFrame::Message {
            message_id: m.message_id.clone(),
            mode: match d.operation() {
                ActiveAgentMessageOperation::Steer => MessageMode::Steer,
                ActiveAgentMessageOperation::Queue => MessageMode::Queue,
            },
            from: m.sender_session_id.clone(),
            text: m.text.to_string(),
        };
        let id = m.message_id.clone();
        let (tx, pending) = (self.tx.clone(), self.pending.clone());
        let admitted = d.commit_admission(|| {
            pending.lock().insert(id.clone());
            tx.send(frame).is_ok()
        });
        let out = match admitted {
            Some(true) => ActiveMessageAdmission::Admitted,
            None => ActiveMessageAdmission::Rejected,
            Some(false) => {
                pending.lock().remove(&id);
                ActiveMessageAdmission::ChannelClosed
            }
        };
        Box::pin(std::future::ready(out))
    }

    fn cancel(&self) {
        self.cancel.cancel();
    }

    fn interrupt(&self) -> bool {
        !self.closed.load(Ordering::Acquire) && self.tx.send(HostFrame::Interrupt).is_ok()
    }

    fn accepts_native_message(&self, m: &AgentMailboxMessage) -> bool {
        !m.native.as_ref().is_some_and(|n| n.encrypted)
    }

    fn deliver_initial_message(&self, m: &AgentMailboxMessage) -> bool {
        self.followup(m)
    }

    fn deliver_followup(&self, m: &AgentMailboxMessage) -> bool {
        self.followup(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_tools::implementations::grok_build::task::types::{
        AgentMailboxMessageKind, NativeAgentMessage,
    };

    fn sample_mailbox_msg(encrypted: bool) -> AgentMailboxMessage {
        AgentMailboxMessage {
            message_id: "m-1".into(),
            team_scope_id: "scope-1".into(),
            from_agent_id: "/root".into(),
            to_agent_id: "/root/worker".into(),
            kind: AgentMailboxMessageKind::NativeMessage,
            body: "check line 42".into(),
            created_at_ms: 1,
            native: Some(NativeAgentMessage {
                author: "/root".into(),
                recipient: "/root/worker".into(),
                encrypted,
                trigger_prompt_id: None,
            }),
        }
    }

    #[test]
    fn t3_followup_and_encrypted_rejection() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let progress = Arc::new(Mutex::new(SubagentProgress::default()));
        let cancel = CancellationToken::new();
        let closed = Arc::new(AtomicBool::new(false));
        let ctrl = ModChildControl::new(tx, pending.clone(), progress, true, cancel, closed);

        // Encrypted message rejected
        assert!(!ctrl.accepts_native_message(&sample_mailbox_msg(true)));
        assert!(!ctrl.deliver_followup(&sample_mailbox_msg(true)));
        assert!(pending.lock().is_empty());

        // Plaintext message delivered as Followup and tracked in pending
        assert!(ctrl.deliver_followup(&sample_mailbox_msg(false)));
        assert!(pending.lock().contains("m-1"));
        let frame = rx.try_recv().expect("frame");
        assert_eq!(
            frame,
            HostFrame::Message {
                message_id: "m-1".into(),
                mode: MessageMode::Followup,
                from: "/root".into(),
                text: "check line 42".into(),
            }
        );
    }
}
```

- [ ] **Step 3: Add `pub mod store; pub mod control;` and `pub trait ModHost` to `crates/codegen/xai-grok-mods/src/lib.rs`, then run `cargo test -p xai-grok-mods` to verify T3 & T4 pass**

Append to `crates/codegen/xai-grok-mods/src/lib.rs`:
```rust
pub mod control;
pub mod store;

pub use control::ModChildControl;
pub use store::{ModSessionStore, SessionRecord, select_resume_key};

pub trait ModHost: Send + Sync + 'static {
    fn on_started<'a>(
        &'a self,
        started: ModStarted,
        control: ModChildControl,
    ) -> BorrowedSendFuture<'a, bool>;

    fn on_progress(&self, progress: &SubagentProgress, tool: Option<&VendorToolActivity>);

    fn finalizing<'a>(&'a self) -> BorrowedSendFuture<'a, bool>;

    fn call_mcp_tool<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        arguments: serde_json::Value,
    ) -> BorrowedSendFuture<'a, Result<serde_json::Value, String>>;
}
```

Run: `cargo test -p xai-grok-mods`
Expected: PASS (`t3_followup_and_encrypted_rejection`, `t4_store_atomic_upsert_and_resume_key_selection`).

- [ ] **Step 4: Commit Task 2**

```bash
git add crates/codegen/xai-grok-mods/
git commit -m "feat(mods): add ModSessionStore and ModChildControl"
```

---

### Task 3: Per-Run Unix Socket MCP Bridge (`bridge.rs`)

**Files:**
- Create: `crates/codegen/xai-grok-mods/src/bridge.rs`
- Modify: `crates/codegen/xai-grok-mods/src/lib.rs`

**Interfaces:**
- Consumes: `crate::protocol::{BridgeInfo, BridgedTool, BridgeRpcRequest, BridgeRpcResponse}`, `crate::ModHost`
- Produces: `xai_grok_mods::bridge::BridgeHandle::start(tools: &[BridgedTool], host: Arc<dyn ModHost>) -> std::io::Result<BridgeHandle>` (with `.info() -> BridgeInfo` and automatic socket file unlink on drop)

- [ ] **Step 1: Implement `crates/codegen/xai-grok-mods/src/bridge.rs` with unit test T5**

```rust
// crates/codegen/xai-grok-mods/src/bridge.rs
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::ModHost;
use crate::protocol::{BridgeInfo, BridgeRpcRequest, BridgeRpcResponse, BridgedTool};

const MAX_IN_FLIGHT_CALLS: usize = 8;
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

pub struct BridgeHandle {
    info: BridgeInfo,
    socket_path: PathBuf,
    task: JoinHandle<()>,
}

impl BridgeHandle {
    pub fn start(tools: &[BridgedTool], host: Arc<dyn ModHost>) -> std::io::Result<Self> {
        let suffix = format!("gm-{:08x}.sock", rand::random::<u32>());
        let candidate = std::env::temp_dir().join(&suffix);
        let socket_path = if candidate.as_os_str().len() < 100 {
            candidate
        } else {
            PathBuf::from("/tmp").join(&suffix)
        };
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600));
        }
        let token = format!(
            "{:016x}{:016x}{:016x}{:016x}",
            rand::random::<u64>(),
            rand::random::<u64>(),
            rand::random::<u64>(),
            rand::random::<u64>()
        );
        let allowed: Arc<HashSet<(String, String)>> = Arc::new(
            tools
                .iter()
                .map(|t| (t.server.clone(), t.tool.clone()))
                .collect(),
        );
        let sem = Arc::new(Semaphore::new(MAX_IN_FLIGHT_CALLS));
        let token_clone = token.clone();

        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let host = Arc::clone(&host);
                let allowed = Arc::clone(&allowed);
                let sem = Arc::clone(&sem);
                let expected_token = token_clone.clone();
                tokio::spawn(async move {
                    let (rd, mut wr) = stream.into_split();
                    let mut lines = BufReader::new(rd).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let Ok(req) = serde_json::from_str::<BridgeRpcRequest>(&line) else {
                            break;
                        };
                        if req.token != expected_token {
                            break;
                        }
                        let resp = if req.method != "tools/call"
                            || !allowed.contains(&(req.server.clone(), req.tool.clone()))
                        {
                            BridgeRpcResponse {
                                id: req.id,
                                ok: false,
                                result: None,
                                error: Some("unknown or disallowed tool".into()),
                            }
                        } else {
                            let Ok(_permit) = sem.acquire().await else {
                                break;
                            };
                            match tokio::time::timeout(
                                CALL_TIMEOUT,
                                host.call_mcp_tool(&req.server, &req.tool, req.arguments),
                            )
                            .await
                            {
                                Ok(Ok(val)) => BridgeRpcResponse {
                                    id: req.id,
                                    ok: true,
                                    result: Some(val),
                                    error: None,
                                },
                                Ok(Err(err)) => BridgeRpcResponse {
                                    id: req.id,
                                    ok: false,
                                    result: None,
                                    error: Some(err),
                                },
                                Err(_) => BridgeRpcResponse {
                                    id: req.id,
                                    ok: false,
                                    result: None,
                                    error: Some("MCP tool call timed out".into()),
                                },
                            }
                        };
                        let Ok(mut bytes) = serde_json::to_vec(&resp) else {
                            break;
                        };
                        bytes.push(b'\n');
                        if wr.write_all(&bytes).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });

        Ok(Self {
            info: BridgeInfo {
                socket_path: socket_path.to_string_lossy().into_owned(),
                token,
            },
            socket_path,
            task,
        })
    }

    pub fn info(&self) -> BridgeInfo {
        self.info.clone()
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BorrowedSendFuture, ModChildControl, ModStarted};
    use xai_grok_tools::implementations::grok_build::task::coordinator::SubagentProgress;

    struct DummyHost;
    impl ModHost for DummyHost {
        fn on_started<'a>(
            &'a self,
            _: ModStarted,
            _: ModChildControl,
        ) -> BorrowedSendFuture<'a, bool> {
            Box::pin(std::future::ready(true))
        }
        fn on_progress(&self, _: &SubagentProgress, _: Option<&crate::protocol::VendorToolActivity>) {}
        fn finalizing<'a>(&'a self) -> BorrowedSendFuture<'a, bool> {
            Box::pin(std::future::ready(true))
        }
        fn call_mcp_tool<'a>(
            &'a self,
            server: &'a str,
            tool: &'a str,
            args: serde_json::Value,
        ) -> BorrowedSendFuture<'a, Result<serde_json::Value, String>> {
            Box::pin(async move {
                Ok(serde_json::json!({ "server": server, "tool": tool, "args": args }))
            })
        }
    }

    #[tokio::test]
    async fn t5_bridge_socket_auth_and_allowlist() {
        let tools = vec![BridgedTool {
            server: "github".into(),
            tool: "search_issues".into(),
            name: "github__search_issues".into(),
            description: None,
            input_schema: serde_json::json!({}),
        }];
        let handle = BridgeHandle::start(&tools, Arc::new(DummyHost)).unwrap();
        let info = handle.info();
        assert!(info.socket_path.len() < 100);

        // Allowed call succeeds
        let stream = tokio::net::UnixStream::connect(&info.socket_path)
            .await
            .unwrap();
        let (rd, mut wr) = stream.into_split();
        let mut lines = BufReader::new(rd).lines();
        let req = serde_json::json!({
            "id": "r1",
            "token": info.token,
            "method": "tools/call",
            "server": "github",
            "tool": "search_issues",
            "arguments": {"q": "bug"}
        });
        wr.write_all(format!("{req}\n").as_bytes()).await.unwrap();
        let resp: BridgeRpcResponse =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert!(resp.ok);

        // Unlisted tool rejected
        let req2 = serde_json::json!({
            "id": "r2",
            "token": info.token,
            "method": "tools/call",
            "server": "github",
            "tool": "delete_repo",
            "arguments": {}
        });
        wr.write_all(format!("{req2}\n").as_bytes()).await.unwrap();
        let resp2: BridgeRpcResponse =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert!(!resp2.ok);

        // Bad token closes connection
        let bad = serde_json::json!({
            "id": "r3",
            "token": "wrong",
            "method": "tools/call",
            "server": "github",
            "tool": "search_issues",
            "arguments": {}
        });
        wr.write_all(format!("{bad}\n").as_bytes()).await.unwrap();
        assert!(lines.next_line().await.unwrap().is_none());
    }
}
```

- [ ] **Step 2: Add `pub mod bridge;` to `crates/codegen/xai-grok-mods/src/lib.rs` and run `cargo test -p xai-grok-mods` to verify T5 passes**

Append `pub mod bridge;` to `crates/codegen/xai-grok-mods/src/lib.rs`.
Run: `cargo test -p xai-grok-mods`
Expected: PASS (`t5_bridge_socket_auth_and_allowlist`).

- [ ] **Step 3: Commit Task 3**

```bash
git add crates/codegen/xai-grok-mods/
git commit -m "feat(mods): add per-run Unix socket MCP bridge"
```

---

### Task 4: Sidecar Process Lifecycle (`process.rs`) & Run State Machine (`session.rs`) + Mock Sidecar Tests (T2, T6, T7)

**Files:**
- Create: `crates/codegen/xai-grok-mods/src/process.rs`
- Create: `crates/codegen/xai-grok-mods/src/session.rs`
- Modify: `crates/codegen/xai-grok-mods/src/lib.rs`
- Create: `crates/codegen/xai-grok-mods/tests/fixtures/mock-sidecar.mjs`
- Create: `crates/codegen/xai-grok-mods/tests/mock_sidecar.rs`

**Interfaces:**
- Consumes: `ModConfig`, `ValidatedMod`, `HostFrame`, `SidecarFrame`, `ModChildControl`, `ModSessionStore`, `BridgeHandle`
- Produces:
  - `xai_grok_mods::{ModHost, ModStarted, ModRunInput, ModRunOutcome, BorrowedSendFuture, run_mod_child}`

- [ ] **Step 1: Implement `crates/codegen/xai-grok-mods/src/process.rs` (env scrubbing, `.process_group(0)`, 64 KiB stderr ring, graceful-then-SIGKILL escalation)**

```rust
// crates/codegen/xai-grok-mods/src/process.rs
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::manifest::ValidatedMod;

const STDERR_RING_CAP: usize = 64 * 1024;

pub struct SidecarProcess {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
    stderr_ring: Arc<Mutex<Vec<u8>>>,
    pgid: Option<i32>,
}

impl SidecarProcess {
    pub fn spawn(
        validated: &ValidatedMod,
        subagent_id: &str,
        cwd: &Path,
    ) -> std::io::Result<Self> {
        let mut cmd = Command::new(&validated.program);
        cmd.args(&validated.args)
            .current_dir(cwd)
            .env_clear()
            .env("GROK_MOD_NAMESPACE", &validated.config.namespace)
            .env("GROK_MOD_SUBAGENT_ID", subagent_id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for key in &validated.config.runtime.env_allowlist {
            if let Some(val) = std::env::var_os(key) {
                cmd.env(key, val);
            }
        }

        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn()?;
        let pgid = child.id().and_then(|pid| i32::try_from(pid).ok());
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut stderr = child.stderr.take().expect("piped stderr");
        let stderr_ring = Arc::new(Mutex::new(Vec::new()));
        let ring_clone = Arc::clone(&stderr_ring);
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut g = ring_clone.lock();
                g.extend_from_slice(&buf[..n]);
                if g.len() > STDERR_RING_CAP {
                    let excess = g.len() - STDERR_RING_CAP;
                    g.drain(..excess);
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            stdout,
            stderr_ring,
            pgid,
        })
    }

    pub fn stderr_tail(&self) -> String {
        String::from_utf8_lossy(&self.stderr_ring.lock())
            .trim()
            .to_owned()
    }

    pub async fn terminate_group(&mut self, graceful_ms: u64) {
        let grace = Duration::from_millis(graceful_ms);
        if tokio::time::timeout(grace, self.child.wait()).await.is_ok() {
            return;
        }
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_millis(500), self.child.wait()).await;
    }
}
```

- [ ] **Step 2: Implement `crates/codegen/xai-grok-mods/src/session.rs` and `lib.rs` (run loop, `run.idle` -> `finalizing()` -> `run.drain` / `run.finish`)**

```rust
// crates/codegen/xai-grok-mods/src/session.rs
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use xai_grok_tools::implementations::grok_build::task::coordinator::SubagentProgress;

use crate::bridge::BridgeHandle;
use crate::control::ModChildControl;
use crate::process::SidecarProcess;
use crate::protocol::{HostFrame, SidecarFrame};
use crate::store::{ModSessionStore, SessionRecord};
use crate::{ModHost, ModRunInput, ModRunOutcome, ModStarted};

async fn write_frame(stdin: &mut tokio::process::ChildStdin, frame: &HostFrame) -> bool {
    let Ok(mut bytes) = serde_json::to_vec(frame) else {
        return false;
    };
    bytes.push(b'\n');
    stdin.write_all(&bytes).await.is_ok() && stdin.flush().await.is_ok()
}

pub async fn run_mod_child_with_store(
    input: ModRunInput,
    host: Arc<dyn ModHost>,
    store: &ModSessionStore,
) -> ModRunOutcome {
    let ns = input.validated.config.namespace.clone();
    let mut warnings = Vec::new();
    let resume_vendor_session_id = if input.validated.config.capabilities.session_resume {
        if let Some(key) = input.resume_key.as_deref() {
            match store.lookup(&ns, key, &input.parent_session_id) {
                Some(vsid) => Some(vsid),
                None => {
                    warnings.push(format!("vendor session for {key} not found; started fresh"));
                    None
                }
            }
        } else {
            None
        }
    } else {
        if let Some(key) = input.resume_key.as_deref() {
            warnings.push(format!(
                "mod '{ns}' does not support sessionResume (ignored {key})"
            ));
        }
        None
    };

    let bridge_handle = if !input.bridged_tools.is_empty()
        && input.validated.config.capabilities.mcp_bridge
        && (!input.read_only || input.validated.config.capabilities.mcp_in_read_only)
    {
        BridgeHandle::start(&input.bridged_tools, Arc::clone(&host)).ok()
    } else {
        None
    };

    let mut proc = match SidecarProcess::spawn(&input.validated, &input.subagent_id, &input.cwd) {
        Ok(p) => p,
        Err(e) => {
            return ModRunOutcome {
                promoted: false,
                final_text: String::new(),
                error: Some(format!("failed to spawn mod sidecar '{ns}': {e}")),
                cancelled: false,
                turns: 0,
                tool_calls: 0,
                tokens_used: 0,
                vendor_session_id: None,
                admission_uncertain: false,
                warnings,
            };
        }
    };

    let start_frame = HostFrame::Start {
        protocol: 1,
        subagent_id: input.subagent_id.clone(),
        parent_session_id: input.parent_session_id.clone(),
        subagent_type: input.subagent_type.clone(),
        agent_name: input.agent_name.clone(),
        model: input.vendor_model.clone(),
        cwd: input.cwd.to_string_lossy().into_owned(),
        prompt: input.prompt.clone(),
        system_prompt: input.system_prompt.clone(),
        resume_vendor_session_id,
        read_only: input.read_only,
        reasoning_effort: input.reasoning_effort.clone(),
        bridge: bridge_handle.as_ref().map(BridgeHandle::info),
        bridged_tools: if bridge_handle.is_some() {
            input.bridged_tools.clone()
        } else {
            Vec::new()
        },
    };

    if !write_frame(&mut proc.stdin, &start_frame).await {
        proc.terminate_group(input.validated.config.runtime.graceful_kill_ms)
            .await;
        return ModRunOutcome {
            promoted: false,
            final_text: String::new(),
            error: Some(format!(
                "failed to write run.start to '{ns}': {}",
                proc.stderr_tail()
            )),
            cancelled: false,
            turns: 0,
            tool_calls: 0,
            tokens_used: 0,
            vendor_session_id: None,
            admission_uncertain: false,
            warnings,
        };
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<HostFrame>();
    let pending = Arc::new(Mutex::new(HashSet::<String>::new()));
    let progress = Arc::new(Mutex::new(SubagentProgress::default()));
    let control_cancel = CancellationToken::new();
    let closed = Arc::new(AtomicBool::new(false));
    let control = ModChildControl::new(
        tx,
        Arc::clone(&pending),
        Arc::clone(&progress),
        input.validated.config.capabilities.live_steer,
        control_cancel.clone(),
        Arc::clone(&closed),
    );

    let timeout = Duration::from_secs(input.validated.config.runtime.timeout_secs);
    let deadline = tokio::time::Instant::now() + timeout;
    let mut lines = (&mut proc.stdout).lines();

    let mut promoted = false;
    let mut finalizing_called = false;
    let mut admission_uncertain = false;
    let mut vendor_session_id: Option<String> = None;
    let mut last_idle_text = String::new();
    let mut turns = 0u32;
    let mut tool_calls = 0u32;
    let mut tokens_used = 0u64;

    let outcome = loop {
        tokio::select! {
            biased;
            _ = input.cancellation.cancelled() => {
                closed.store(true, Ordering::Release);
                let _ = write_frame(&mut proc.stdin, &HostFrame::Cancel { reason: Some("cancelled".into()) }).await;
                proc.terminate_group(input.validated.config.runtime.graceful_kill_ms).await;
                break ModRunOutcome {
                    promoted,
                    final_text: last_idle_text,
                    error: Some("Subagent was cancelled".into()),
                    cancelled: true,
                    turns,
                    tool_calls,
                    tokens_used,
                    vendor_session_id,
                    admission_uncertain,
                    warnings,
                };
            }
            _ = control_cancel.cancelled() => {
                closed.store(true, Ordering::Release);
                let _ = write_frame(&mut proc.stdin, &HostFrame::Cancel { reason: Some("cancelled".into()) }).await;
                proc.terminate_group(input.validated.config.runtime.graceful_kill_ms).await;
                break ModRunOutcome {
                    promoted,
                    final_text: last_idle_text,
                    error: Some("Subagent was cancelled".into()),
                    cancelled: true,
                    turns,
                    tool_calls,
                    tokens_used,
                    vendor_session_id,
                    admission_uncertain,
                    warnings,
                };
            }
            _ = tokio::time::sleep_until(deadline) => {
                closed.store(true, Ordering::Release);
                let reason = format!("mod sidecar timed out after {}s", timeout.as_secs());
                let _ = write_frame(&mut proc.stdin, &HostFrame::Cancel { reason: Some(reason.clone()) }).await;
                proc.terminate_group(input.validated.config.runtime.graceful_kill_ms).await;
                break ModRunOutcome {
                    promoted,
                    final_text: last_idle_text,
                    error: Some(reason),
                    cancelled: false,
                    turns,
                    tool_calls,
                    tokens_used,
                    vendor_session_id,
                    admission_uncertain,
                    warnings,
                };
            }
            Some(out_frame) = rx.recv() => {
                let _ = write_frame(&mut proc.stdin, &out_frame).await;
            }
            line_res = lines.next_line() => {
                let Ok(Some(line)) = line_res else {
                    closed.store(true, Ordering::Release);
                    let status = proc.child.wait().await.ok().and_then(|s| s.code());
                    let tail = proc.stderr_tail();
                    break ModRunOutcome {
                        promoted,
                        final_text: last_idle_text,
                        error: Some(format!("mod sidecar exited ({status:?}): {tail}")),
                        cancelled: false,
                        turns,
                        tool_calls,
                        tokens_used,
                        vendor_session_id,
                        admission_uncertain,
                        warnings,
                    };
                };
                if line.len() > 1024 * 1024 {
                    continue;
                }
                let Ok(frame) = serde_json::from_str::<SidecarFrame>(&line) else {
                    tracing::debug!(namespace = %ns, "ignoring non-JSON sidecar stdout line");
                    continue;
                };
                match frame {
                    SidecarFrame::Started { vendor_session_id: vsid, effective_model } => {
                        vendor_session_id = Some(vsid.clone());
                        store.upsert(&ns, &input.subagent_id, SessionRecord {
                            vendor_session_id: vsid.clone(),
                            parent_session_id: input.parent_session_id.clone(),
                            subagent_type: input.subagent_type.clone(),
                            updated_at_ms: now_ms(),
                        });
                        let eff = effective_model.unwrap_or_else(|| input.vendor_model.clone());
                        if !host.on_started(ModStarted { vendor_session_id: vsid, effective_model: eff }, control.clone()).await {
                            closed.store(true, Ordering::Release);
                            let _ = write_frame(&mut proc.stdin, &HostFrame::Cancel { reason: Some("admission refused".into()) }).await;
                            proc.terminate_group(input.validated.config.runtime.graceful_kill_ms).await;
                            break ModRunOutcome {
                                promoted: false,
                                final_text: String::new(),
                                error: Some("Subagent was cancelled".into()),
                                cancelled: true,
                                turns: 0,
                                tool_calls: 0,
                                tokens_used: 0,
                                vendor_session_id,
                                admission_uncertain: false,
                                warnings,
                            };
                        }
                        promoted = true;
                    }
                    SidecarFrame::Progress { turns: t, tool_calls: tc, tokens_used: tok, tool } => {
                        turns = t;
                        tool_calls = tc;
                        tokens_used = tok;
                        let snap = {
                            let mut p = progress.lock();
                            p.turn_count = t;
                            p.tool_call_count = tc;
                            p.tokens_used = tok;
                            if let Some(ref act) = tool {
                                if !p.tools_used.contains(&act.name) {
                                    p.tools_used.push(act.name.clone());
                                }
                            }
                            p.clone()
                        };
                        host.on_progress(&snap, tool.as_ref());
                    }
                    SidecarFrame::Consumed { message_id } => {
                        pending.lock().remove(&message_id);
                    }
                    SidecarFrame::Idle { final_text } => {
                        last_idle_text = final_text;
                        if !finalizing_called {
                            finalizing_called = true;
                            if promoted && !host.finalizing().await {
                                admission_uncertain = true;
                            }
                        }
                        while let Ok(queued) = rx.try_recv() {
                            let _ = write_frame(&mut proc.stdin, &queued).await;
                        }
                        if pending.lock().is_empty() {
                            let _ = write_frame(&mut proc.stdin, &HostFrame::Finish).await;
                        } else {
                            let _ = write_frame(&mut proc.stdin, &HostFrame::Drain).await;
                        }
                    }
                    SidecarFrame::Completed { final_text, turns: t, tool_calls: tc, tokens_used: tok } => {
                        closed.store(true, Ordering::Release);
                        turns = t;
                        tool_calls = tc;
                        tokens_used = tok;
                        if let Some(ref vsid) = vendor_session_id {
                            store.upsert(&ns, &input.subagent_id, SessionRecord {
                                vendor_session_id: vsid.clone(),
                                parent_session_id: input.parent_session_id.clone(),
                                subagent_type: input.subagent_type.clone(),
                                updated_at_ms: now_ms(),
                            });
                        }
                        break ModRunOutcome {
                            promoted,
                            final_text,
                            error: None,
                            cancelled: false,
                            turns,
                            tool_calls,
                            tokens_used,
                            vendor_session_id,
                            admission_uncertain,
                            warnings,
                        };
                    }
                    SidecarFrame::Failed { error, cancelled, turns: t, tool_calls: tc, tokens_used: tok } => {
                        closed.store(true, Ordering::Release);
                        break ModRunOutcome {
                            promoted,
                            final_text: last_idle_text,
                            error: Some(error),
                            cancelled,
                            turns: t,
                            tool_calls: tc,
                            tokens_used: tok,
                            vendor_session_id,
                            admission_uncertain,
                            warnings,
                        };
                    }
                }
            }
        }
    };

    drop(bridge_handle);
    outcome
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
```

And append `pub mod process; pub mod session;` plus `pub async fn run_mod_child` to `crates/codegen/xai-grok-mods/src/lib.rs`:
```rust
pub mod process;
pub mod session;

pub use session::run_mod_child_with_store;

pub async fn run_mod_child(input: ModRunInput, host: std::sync::Arc<dyn ModHost>) -> ModRunOutcome {
    let store = ModSessionStore::default_grok_home();
    run_mod_child_with_store(input, host, &store).await
}
```

- [ ] **Step 3: Write `crates/codegen/xai-grok-mods/tests/fixtures/mock-sidecar.mjs` and `tests/mock_sidecar.rs` covering T2, T6, and T7**

Create `crates/codegen/xai-grok-mods/tests/fixtures/mock-sidecar.mjs`:
```javascript
#!/usr/bin/env node
import { spawn } from "node:child_process";
import fs from "node:fs";
import readline from "node:readline";

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
const emit = (obj) => process.stdout.write(JSON.stringify(obj) + "\n");

let turns = 0;
let toolCalls = 0;
let lastText = "";
let queued = [];
let mode = "normal";
let waitingForActiveMsg = false;

rl.on("line", (line) => {
  if (!line.trim()) return;
  const msg = JSON.parse(line);
  if (msg.type === "run.start") {
    if (msg.prompt === "CRASH") {
      process.stderr.write("fatal mock crash\n");
      process.exit(42);
    }
    if (msg.prompt.startsWith("HANG:")) {
      mode = "hang";
      const pidFile = msg.prompt.slice(5);
      const grandchild = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], {
        stdio: "ignore",
      });
      fs.writeFileSync(pidFile, String(grandchild.pid));
      emit({ type: "run.started", vendor_session_id: "v-hang", effective_model: msg.model });
      return;
    }
    const vsid = msg.resume_vendor_session_id || `v-${msg.subagent_id}`;
    emit({ type: "run.started", vendor_session_id: vsid, effective_model: msg.model });
    turns += 1;
    toolCalls += 1;
    emit({
      type: "run.progress",
      turns,
      tool_calls: toolCalls,
      tokens_used: 120,
      tool: { name: "read_file", summary: "Cargo.toml" },
    });
    if (msg.prompt === "JSON_RETRY") {
      lastText = msg.resume_vendor_session_id
        ? JSON.stringify({ ok: true, vsid: msg.resume_vendor_session_id })
        : "not-json";
    } else {
      lastText = `reply:${msg.prompt}:vsid=${vsid}`;
    }
    if (msg.prompt.startsWith("WAIT_FOR_ACTIVE_MSG")) {
      waitingForActiveMsg = true;
      return;
    }
    setTimeout(() => emit({ type: "run.idle", final_text: lastText }), 20);
  } else if (msg.type === "run.message") {
    if (waitingForActiveMsg) {
      waitingForActiveMsg = false;
      emit({ type: "run.consumed", message_id: msg.message_id });
      lastText += `|steered:${msg.mode}:${msg.text}`;
      emit({ type: "run.idle", final_text: lastText });
      return;
    }
    queued.push(msg);
  } else if (msg.type === "run.drain") {
    turns += 1;
    for (const q of queued.splice(0)) {
      emit({ type: "run.consumed", message_id: q.message_id });
      lastText += `|steered:${q.mode}:${q.text}`;
    }
    emit({ type: "run.idle", final_text: lastText });
  } else if (msg.type === "run.finish") {
    emit({
      type: "run.completed",
      final_text: lastText,
      turns,
      tool_calls: toolCalls,
      tokens_used: 120 * turns,
    });
    process.exit(0);
  } else if (msg.type === "run.cancel") {
    if (mode === "hang") return; // force graceful_kill_ms escalation to SIGKILL group
    emit({
      type: "run.failed",
      error: msg.reason || "cancelled",
      cancelled: true,
      turns,
      tool_calls: toolCalls,
      tokens_used: 0,
    });
    process.exit(0);
  }
});
```

Create `crates/codegen/xai-grok-mods/tests/mock_sidecar.rs`:
```rust
use std::path::PathBuf;
use std::sync::Arc;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use xai_grok_mods::{
    BorrowedSendFuture, ModChildControl, ModConfig, ModHost, ModRunInput, ModSessionStore,
    ModStarted, VendorToolActivity, run_mod_child_with_store,
};
use xai_grok_tools::implementations::grok_build::task::coordinator::{
    ChildControl, SubagentProgress,
};
use xai_grok_tools::implementations::grok_build::task::types::{
    AgentMailboxMessage, AgentMailboxMessageKind,
};

struct RecordingHost {
    admit_start: bool,
    inject_on_finalizing: Option<String>,
    control: Mutex<Option<ModChildControl>>,
}

impl ModHost for RecordingHost {
    fn on_started<'a>(
        &'a self,
        _started: ModStarted,
        control: ModChildControl,
    ) -> BorrowedSendFuture<'a, bool> {
        *self.control.lock() = Some(control);
        let ok = self.admit_start;
        Box::pin(async move { ok })
    }

    fn on_progress(&self, _progress: &SubagentProgress, _tool: Option<&VendorToolActivity>) {}

    fn finalizing<'a>(&'a self) -> BorrowedSendFuture<'a, bool> {
        if let Some(ref body) = self.inject_on_finalizing {
            if let Some(ref ctrl) = *self.control.lock() {
                ctrl.deliver_followup(&AgentMailboxMessage {
                    message_id: "late-msg-1".into(),
                    team_scope_id: "scope".into(),
                    from_agent_id: "/root".into(),
                    to_agent_id: "/root/worker".into(),
                    kind: AgentMailboxMessageKind::Message,
                    body: body.clone(),
                    created_at_ms: 1,
                    native: None,
                });
            }
        }
        Box::pin(async { true })
    }

    fn call_mcp_tool<'a>(
        &'a self,
        _server: &'a str,
        _tool: &'a str,
        _args: serde_json::Value,
    ) -> BorrowedSendFuture<'a, Result<serde_json::Value, String>> {
        Box::pin(async { Err("unused".into()) })
    }
}

fn setup_plugin(dir: &std::path::Path, extra_env: &[&str]) -> xai_grok_mods::ValidatedMod {
    let root = dunce::canonicalize(dir).unwrap();
    std::fs::create_dir_all(root.join("sidecar")).unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock-sidecar.mjs");
    std::fs::copy(&fixture, root.join("sidecar/runner.mjs")).unwrap();
    let mut allow = vec!["PATH", "HOME"];
    allow.extend_from_slice(extra_env);
    let manifest = serde_json::json!({
        "schemaVersion": 1,
        "namespace": "cursor",
        "runtime": {
            "command": "node",
            "args": ["sidecar/runner.mjs"],
            "timeoutSecs": 10,
            "gracefulKillMs": 100,
            "envAllowlist": allow,
        },
        "capabilities": {
            "liveSteer": true,
            "sessionResume": true,
            "mcpBridge": true,
            "mcpInReadOnly": false
        }
    });
    std::fs::write(root.join("mod.json"), manifest.to_string()).unwrap();
    ModConfig::load("cursor", &root).unwrap()
}

#[tokio::test]
async fn t6_normal_lifecycle_crash_and_cancel_reaps_grandchild() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let store = ModSessionStore::new(store_dir.path());
    let validated = setup_plugin(dir.path(), &[]);

    // 1. Normal completion + session persistence
    let host = Arc::new(RecordingHost {
        admit_start: true,
        inject_on_finalizing: None,
        control: Mutex::new(None),
    });
    let out = run_mod_child_with_store(
        ModRunInput {
            validated: validated.clone(),
            subagent_id: "sub-1".into(),
            parent_session_id: "parent-1".into(),
            subagent_type: "cursor:composer-2".into(),
            agent_name: "composer-2".into(),
            vendor_model: "composer-2".into(),
            cwd: dir.path().to_path_buf(),
            prompt: "hello".into(),
            system_prompt: None,
            resume_key: None,
            read_only: false,
            reasoning_effort: None,
            bridged_tools: Vec::new(),
            cancellation: CancellationToken::new(),
        },
        host,
        &store,
    )
    .await;
    assert!(out.promoted);
    assert!(out.error.is_none());
    assert_eq!(out.final_text, "reply:hello:vsid=v-sub-1");
    assert_eq!(
        store.lookup("cursor", "sub-1", "parent-1").as_deref(),
        Some("v-sub-1")
    );

    // 2. Resume miss records warning and starts fresh
    let host_miss = Arc::new(RecordingHost {
        admit_start: true,
        inject_on_finalizing: None,
        control: Mutex::new(None),
    });
    let out_miss = run_mod_child_with_store(
        ModRunInput {
            validated: validated.clone(),
            subagent_id: "sub-2".into(),
            parent_session_id: "parent-1".into(),
            subagent_type: "cursor:composer-2".into(),
            agent_name: "composer-2".into(),
            vendor_model: "composer-2".into(),
            cwd: dir.path().to_path_buf(),
            prompt: "fresh".into(),
            system_prompt: None,
            resume_key: Some("missing-key".into()),
            read_only: false,
            reasoning_effort: None,
            bridged_tools: Vec::new(),
            cancellation: CancellationToken::new(),
        },
        host_miss,
        &store,
    )
    .await;
    assert_eq!(
        out_miss.warnings,
        vec!["vendor session for missing-key not found; started fresh".to_string()]
    );

    // 3. on_started = false refuses promotion, sends cancel, terminates sidecar
    let host_refuse = Arc::new(RecordingHost {
        admit_start: false,
        inject_on_finalizing: None,
        control: Mutex::new(None),
    });
    let out_refuse = run_mod_child_with_store(
        ModRunInput {
            validated: validated.clone(),
            subagent_id: "sub-refuse".into(),
            parent_session_id: "parent-1".into(),
            subagent_type: "cursor:composer-2".into(),
            agent_name: "composer-2".into(),
            vendor_model: "composer-2".into(),
            cwd: dir.path().to_path_buf(),
            prompt: "refuse".into(),
            system_prompt: None,
            resume_key: None,
            read_only: false,
            reasoning_effort: None,
            bridged_tools: Vec::new(),
            cancellation: CancellationToken::new(),
        },
        host_refuse,
        &store,
    )
    .await;
    assert!(!out_refuse.promoted);
    assert!(out_refuse.cancelled);

    // 4. Unexpected EOF / crash synthesizes failure with stderr tail
    let host_crash = Arc::new(RecordingHost {
        admit_start: true,
        inject_on_finalizing: None,
        control: Mutex::new(None),
    });
    let out_crash = run_mod_child_with_store(
        ModRunInput {
            validated: validated.clone(),
            subagent_id: "sub-crash".into(),
            parent_session_id: "parent-1".into(),
            subagent_type: "cursor:composer-2".into(),
            agent_name: "composer-2".into(),
            vendor_model: "composer-2".into(),
            cwd: dir.path().to_path_buf(),
            prompt: "CRASH".into(),
            system_prompt: None,
            resume_key: None,
            read_only: false,
            reasoning_effort: None,
            bridged_tools: Vec::new(),
            cancellation: CancellationToken::new(),
        },
        host_crash,
        &store,
    )
    .await;
    assert!(out_crash.error.as_deref().unwrap_or_default().contains("fatal mock crash"));

    // 5. Hang + cancellation token escalates to SIGKILL of process group and reaps grandchild
    #[cfg(unix)]
    {
        let pid_file = dir.path().join("grandchild.pid");
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            cancel_clone.cancel();
        });
        let host_hang = Arc::new(RecordingHost {
            admit_start: true,
            inject_on_finalizing: None,
            control: Mutex::new(None),
        });
        let out_hang = run_mod_child_with_store(
            ModRunInput {
                validated: validated.clone(),
                subagent_id: "sub-hang".into(),
                parent_session_id: "parent-1".into(),
                subagent_type: "cursor:composer-2".into(),
                agent_name: "composer-2".into(),
                vendor_model: "composer-2".into(),
                cwd: dir.path().to_path_buf(),
                prompt: format!("HANG:{}", pid_file.display()),
                system_prompt: None,
                resume_key: None,
                read_only: false,
                reasoning_effort: None,
                bridged_tools: Vec::new(),
                cancellation: cancel,
            },
            host_hang,
            &store,
        )
        .await;
        assert!(out_hang.cancelled);
        if let Ok(raw_pid) = std::fs::read_to_string(&pid_file) {
            if let Ok(gpid) = raw_pid.trim().parse::<i32>() {
                let alive = unsafe { libc::kill(gpid, 0) == 0 };
                assert!(!alive, "grandchild pid {gpid} should be killed with process group");
            }
        }
    }
}

#[tokio::test]
async fn t7_idle_finalizing_drain_consumes_pending_messages() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let store = ModSessionStore::new(store_dir.path());
    let validated = setup_plugin(dir.path(), &[]);

    let host = Arc::new(RecordingHost {
        admit_start: true,
        inject_on_finalizing: Some("extra-work".into()),
        control: Mutex::new(None),
    });
    let out = run_mod_child_with_store(
        ModRunInput {
            validated,
            subagent_id: "sub-drain".into(),
            parent_session_id: "parent-1".into(),
            subagent_type: "cursor:composer-2".into(),
            agent_name: "composer-2".into(),
            vendor_model: "composer-2".into(),
            cwd: dir.path().to_path_buf(),
            prompt: "initial".into(),
            system_prompt: None,
            resume_key: None,
            read_only: false,
            reasoning_effort: None,
            bridged_tools: Vec::new(),
            cancellation: CancellationToken::new(),
        },
        host,
        &store,
    )
    .await;
    assert!(out.error.is_none());
    assert_eq!(out.turns, 2);
    assert!(out.final_text.contains("|steered:followup:extra-work"));
}

#[tokio::test(flavor = "current_thread")]
async fn t2_coordinator_send_active_message_admits_and_drains() {
    use xai_grok_tools::implementations::grok_build::task::backend::{
        ChannelBackend, SubagentBackend,
    };
    use xai_grok_tools::implementations::grok_build::task::coordinator::{
        ChildCompletion, ChildRunOutput, ChildRunRequest, ChildRunner, CoordinatorConfig,
        LocalBoxFuture, StartedChild, SubagentCoordinator,
    };
    use xai_grok_tools::implementations::grok_build::task::types::{
        ActiveAgentMessageOutcome, ActiveAgentMessageRequest, SubagentDescribeOutcome,
        SubagentOwner, SubagentRequest, SubagentResult, SubagentValidateTypeOutcome,
    };

    #[derive(Clone)]
    struct ModTestRunner {
        validated: xai_grok_mods::ValidatedMod,
        store: ModSessionStore,
        cwd: PathBuf,
    }

    struct CoordinatorModHost {
        reporter: xai_grok_tools::implementations::grok_build::task::coordinator::ChildReporter<
            ModChildControl,
        >,
        id: String,
        cwd: String,
    }

    impl ModHost for CoordinatorModHost {
        fn on_started<'a>(
            &'a self,
            started: ModStarted,
            control: ModChildControl,
        ) -> BorrowedSendFuture<'a, bool> {
            let child = StartedChild {
                child_session_id: self.id.clone(),
                persona: None,
                resumed_from: None,
                child_cwd: self.cwd.clone(),
                worktree_path: None,
                effective_model_id: format!("cursor/{}", started.effective_model),
                definition_background: false,
                control,
            };
            Box::pin(async move { self.reporter.started(child).await })
        }
        fn on_progress(&self, _: &SubagentProgress, _: Option<&VendorToolActivity>) {}
        fn finalizing<'a>(&'a self) -> BorrowedSendFuture<'a, bool> {
            Box::pin(async move { self.reporter.finalizing().await })
        }
        fn call_mcp_tool<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: serde_json::Value,
        ) -> BorrowedSendFuture<'a, Result<serde_json::Value, String>> {
            Box::pin(async { Err("unused".into()) })
        }
    }

    impl ChildRunner for ModTestRunner {
        type Control = ModChildControl;
        type CompletionData = ();
        type RunFuture = LocalBoxFuture<ChildRunOutput<()>>;
        type ValidateFuture = LocalBoxFuture<SubagentValidateTypeOutcome>;
        type DescribeFuture = LocalBoxFuture<SubagentDescribeOutcome>;

        fn run(&self, run: ChildRunRequest<Self::Control>) -> Self::RunFuture {
            let validated = self.validated.clone();
            let store = self.store.clone();
            let cwd = self.cwd.clone();
            Box::pin(async move {
                let host = Arc::new(CoordinatorModHost {
                    reporter: run.reporter,
                    id: run.request.id.clone(),
                    cwd: cwd.to_string_lossy().into_owned(),
                });
                let out = run_mod_child_with_store(
                    ModRunInput {
                        validated,
                        subagent_id: run.request.id.clone(),
                        parent_session_id: run.request.parent_session_id.clone(),
                        subagent_type: run.request.subagent_type.clone(),
                        agent_name: "composer-2".into(),
                        vendor_model: "composer-2".into(),
                        cwd,
                        prompt: run.request.prompt.clone(),
                        system_prompt: None,
                        resume_key: None,
                        read_only: false,
                        reasoning_effort: None,
                        bridged_tools: Vec::new(),
                        cancellation: run.cancellation,
                    },
                    host,
                    &store,
                )
                .await;
                ChildRunOutput {
                    result: SubagentResult {
                        success: out.error.is_none(),
                        output: Arc::from(out.final_text),
                        subagent_id: run.request.id.clone(),
                        child_session_id: run.request.id,
                        turns: out.turns,
                        tool_calls: out.tool_calls,
                        ..Default::default()
                    },
                    completion_data: (),
                    snapshot_ref: None,
                }
            })
        }
        fn validate_type(&self, _: String, _: String) -> Self::ValidateFuture {
            Box::pin(std::future::ready(SubagentValidateTypeOutcome::Ok))
        }
        fn describe_type(&self, _: String, _: Option<String>, _: String) -> Self::DescribeFuture {
            Box::pin(std::future::ready(SubagentDescribeOutcome::Unavailable))
        }
        fn on_completed(&self, _: ChildCompletion<()>, done: Box<dyn FnOnce() + Send>) {
            done();
        }
    }

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let store_dir = tempfile::tempdir().unwrap();
            let store = ModSessionStore::new(store_dir.path());
            let validated = setup_plugin(dir.path(), &[]);
            let (cmd_tx, cmd_rx) = SubagentCoordinator::<ModTestRunner>::channel();
            let _coord = tokio::task::spawn_local(
                SubagentCoordinator::from_channel(
                    cmd_rx,
                    ModTestRunner {
                        validated,
                        store,
                        cwd: dir.path().to_path_buf(),
                    },
                    CoordinatorConfig::default(),
                )
                .run(),
            );
            let backend = ChannelBackend::for_coordinator_session(cmd_tx, "parent-1");
            let req = SubagentRequest {
                id: "sub-live-1".into(),
                prompt: "WAIT_FOR_ACTIVE_MSG".into(),
                description: "test".into(),
                subagent_type: "cursor:composer-2".into(),
                parent_session_id: "parent-1".into(),
                parent_prompt_id: None,
                resume_from: None,
                cwd: None,
                runtime_overrides: Default::default(),
                run_in_background: true,
                surface_completion: true,
                await_to_completion: false,
                fork_context: false,
                context: Default::default(),
                owner: SubagentOwner::Task,
                cancel_token: CancellationToken::new(),
                spawn_root: Default::default(),
            };
            let spawn = tokio::task::spawn_local({
                let backend = backend.clone();
                async move { backend.spawn(req, None).await }
            });
            // Poll until the child is promoted and the live control admits the message.
            let mut admitted = false;
            for _ in 0..200 {
                let msg_req =
                    ActiveAgentMessageRequest::try_new("sub-live-1", "live-steer-msg").unwrap();
                if matches!(
                    backend.send_active_message(msg_req).await,
                    ActiveAgentMessageOutcome::Accepted { .. }
                ) {
                    admitted = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(admitted, "live steer was never admitted through ModChildControl");
            let _ = spawn.await;
        })
        .await;
}
```

- [ ] **Step 4: Run `cargo test -p xai-grok-mods` to verify T2, T6, and T7 pass**

Run: `cargo test -p xai-grok-mods`
Expected: PASS (all unit and integration tests in `xai-grok-mods` green).

- [ ] **Step 5: Commit Task 4**

```bash
git add crates/codegen/xai-grok-mods/
git commit -m "feat(mods): implement sidecar process group runner, drain loop, and mock sidecar tests"
```

---

### Task 5: `ShellChildRuntime` Enum & `mod_adapter.rs` in `xai-grok-shell` (T8, T9, T10, T11)

**Files:**
- Modify: `crates/codegen/xai-grok-shell/Cargo.toml` (+1 line: `xai-grok-mods = { workspace = true }`)
- Modify: `crates/codegen/xai-grok-shell/src/agent/subagent/mod.rs` (+1 line after `:68`: `mod mod_adapter;`)
- Modify: `crates/codegen/xai-grok-shell/src/agent/subagent/child_runtime.rs:14,29` (rename struct to `NativeShellChildRuntime`; append `enum ShellChildRuntime` + delegating impl)
- Modify: `crates/codegen/xai-grok-shell/src/agent/subagent/child_runtime_tests.rs:86` (wrap struct literal)
- Modify: `crates/codegen/xai-grok-shell/src/agent/subagent/handle_request.rs:22,919,1956` (`static` → `pub(super) static` for `SUBAGENTS_ACTIVE`; 12-line dispatch block after `:919`; wrap struct literal at `:1956`)
- Modify: `crates/codegen/xai-grok-shell/src/agent/subagent/tests/wake.rs` (+1 line at EOF: `mod mod_adapter_tests;`)
- Create: `crates/codegen/xai-grok-shell/src/agent/subagent/mod_adapter.rs`
- Create: `crates/codegen/xai-grok-shell/src/agent/subagent/tests/wake/mod_adapter_tests.rs`

**Interfaces:**
- Consumes: `xai_grok_mods::{ModConfig, ValidatedMod, ModHost, ModStarted, ModRunInput, ModChildControl, BridgedTool, BorrowedSendFuture, VendorToolActivity, select_resume_key, run_mod_child}`, `StartArtifactPublication`, `PreparedStartArtifacts`, `PublicationBoundary`, `StartedChild`, `ChildReporter`
- Produces:
  - `pub(crate) enum ShellChildRuntime { Native(NativeShellChildRuntime), Mod(xai_grok_mods::ModChildControl) }`
  - `subagent::mod_adapter::{Dispatch, detect, run}`

- [ ] **Step 1: Update `child_runtime.rs` and `child_runtime_tests.rs`**

In `crates/codegen/xai-grok-shell/src/agent/subagent/child_runtime.rs`:
1. Rename `pub(crate) struct ShellChildRuntime` (`:14`) to `pub(crate) struct NativeShellChildRuntime` and `impl ChildControl for ShellChildRuntime` (`:29`) to `impl ChildControl for NativeShellChildRuntime` without changing any of the 9 fields or any method body.
2. Append the enum and its delegating impl (verbatim from Spec §5.1), plus a compile-time `Send` assertion so the `ModHost` (`Send + Sync`) futures in Step 2 are guaranteed to type-check:
```rust
pub(crate) enum ShellChildRuntime {
    Native(NativeShellChildRuntime),
    Mod(xai_grok_mods::ModChildControl),
}

const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<ShellChildRuntime>();
};

impl ChildControl for ShellChildRuntime {
    type ProgressFuture = LocalBoxFuture<SubagentProgress>;
    fn progress(&self) -> Self::ProgressFuture {
        match self { Self::Native(c) => c.progress(), Self::Mod(c) => c.progress() }
    }
    fn send_active_message(&self, d: ActiveAgentMessageDelivery) -> SendBoxFuture<ActiveMessageAdmission> {
        match self { Self::Native(c) => c.send_active_message(d), Self::Mod(c) => c.send_active_message(d) }
    }
    fn cancel(&self) { match self { Self::Native(c) => c.cancel(), Self::Mod(c) => c.cancel() } }
    fn interrupt(&self) -> bool { match self { Self::Native(c) => c.interrupt(), Self::Mod(c) => c.interrupt() } }
    fn accepts_native_message(&self, m: &AgentMailboxMessage) -> bool {
        match self { Self::Native(c) => c.accepts_native_message(m), Self::Mod(c) => c.accepts_native_message(m) }
    }
    fn deliver_initial_message(&self, m: &AgentMailboxMessage) -> bool {
        match self { Self::Native(c) => c.deliver_initial_message(m), Self::Mod(c) => c.deliver_initial_message(m) }
    }
    fn deliver_followup(&self, m: &AgentMailboxMessage) -> bool {
        match self { Self::Native(c) => c.deliver_followup(m), Self::Mod(c) => c.deliver_followup(m) }
    }
}
```
3. In `child_runtime_tests.rs:86`, change `runtime: ShellChildRuntime { ... },` to `runtime: ShellChildRuntime::Native(super::child_runtime::NativeShellChildRuntime { ... }),` (body unchanged).
4. Run `cargo check -p xai-grok-shell`. Expected: errors only at `handle_request.rs:1956` (fixed in Step 3). If the `assert_send` line fails, stop and report: a `!Send` field was added upstream and §5.2 must switch `BorrowedSendFuture` to a local future plus `tokio::task::spawn_local` in `bridge.rs`.

- [ ] **Step 2: Create `crates/codegen/xai-grok-shell/src/agent/subagent/mod_adapter.rs`**

`#![deny(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]` applies (`mod.rs:13`): every helper below takes ≤ 7 args and ≤ 3 bools; all state flows through `Dispatch`. `ShellCompletionData` is `#[derive(Clone, Default)]` with shared `Arc<Mutex<ShellCompletionState>>` (`mod.rs:544-556`), so `ShellModHost` holds a clone whose `mark_spawned_notification_emitted()` is visible to the returned `completion_data`.

```rust
// crates/codegen/xai-grok-shell/src/agent/subagent/mod_adapter.rs
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use xai_grok_agent::config::{AgentDefinition, ModelOverride};
use xai_grok_mods::{
    BorrowedSendFuture, BridgedTool, ModChildControl, ModConfig, ModHost, ModRunInput,
    ModStarted, ValidatedMod, VendorToolActivity, select_resume_key,
};
use xai_grok_tools::implementations::grok_build::task::coordinator::{
    ChildReporter, StartedChild, SubagentProgress,
};
use xai_tool_types::SubagentCapabilityMode;

use super::start_artifact_publication::{
    PreparedStartArtifacts, PublicationBoundary, StartArtifactPublication,
};
use super::*;

/// Every binding in scope at `handle_request.rs:919` that the adapter needs.
pub(super) struct Dispatch {
    pub validated: Result<ValidatedMod, String>,
    pub request: SubagentRequest,
    pub ctx: SubagentSpawnContext,
    pub completion_data: ShellCompletionData,
    pub gateway: GatewaySender,
    pub reporter: ChildReporter<ShellChildRuntime>,
    pub cancel_token: tokio_util::sync::CancellationToken,
    pub attempt_id: Option<String>,
    pub agent_address: Option<xai_message_delivery_core::AgentAddress>,
    pub is_wake: bool,
    pub queued_for: Option<std::time::Duration>,
    pub session_running: usize,
    pub definition: AgentDefinition,
    pub effective_runtime: xai_grok_subagent_resolution::EffectiveRuntimeConfig,
    pub resume_source: Option<ResumeSourceData>,
    pub worktree_path: Option<PathBuf>,
    pub worktree_freshly_created: bool,
    pub child_depth: u32,
    pub spawn_prepare_span: xai_grok_telemetry::region::Region,
    pub spawn_timer: xai_grok_telemetry::subagent_spawn::SharedSubagentSpawnTimer,
    pub start: std::time::Instant,
}

/// Spec §4.6.
pub(super) fn detect(
    def: &AgentDefinition,
    ctx: &SubagentSpawnContext,
) -> Option<Result<ValidatedMod, String>> {
    let name = def.plugin_name.as_deref()?;
    let registry = ctx.plugin_registry.as_ref()?;
    let plugin = registry.get(name)?;
    if !plugin.canonical_root.join("mod.json").is_file() {
        return None;
    }
    if !plugin.trusted {
        return Some(Err(format!("mod plugin '{name}' is not trusted")));
    }
    Some(ModConfig::load(name, &plugin.canonical_root).map_err(|e| e.to_string()))
}

struct ShellModHost {
    reporter: ChildReporter<ShellChildRuntime>,
    gateway: GatewaySender,
    parent_cmd_tx: Option<tokio::sync::mpsc::UnboundedSender<SessionCommand>>,
    parent_session_id: String,
    subagent_id: String,
    attempt_id: Option<String>,
    started_at: std::time::Instant,
    is_wake: bool,
    child_template: Mutex<Option<StartedChildTemplate>>,
    start_artifacts: Arc<Mutex<StartArtifactPublication>>,
    completion_data: ShellCompletionData,
    mcp_pool: Option<crate::session::mcp_servers::SharedMcpPool>,
}

struct StartedChildTemplate {
    persona: Option<String>,
    resumed_from: Option<String>,
    child_cwd: String,
    worktree_path: Option<String>,
    effective_model_id: String,
    definition_background: bool,
}

impl ModHost for ShellModHost {
    fn on_started<'a>(
        &'a self,
        _started: ModStarted,
        control: ModChildControl,
    ) -> BorrowedSendFuture<'a, bool> {
        Box::pin(async move {
            let Some(t) = self.child_template.lock().take() else {
                return false;
            };
            let child = StartedChild {
                child_session_id: self.subagent_id.clone(),
                persona: t.persona,
                resumed_from: t.resumed_from,
                child_cwd: t.child_cwd,
                worktree_path: t.worktree_path,
                effective_model_id: t.effective_model_id,
                definition_background: t.definition_background,
                control: ShellChildRuntime::Mod(control),
            };
            if !self.is_wake {
                return self.reporter.started(child).await;
            }
            if !self.reporter.started_deferred(child).await {
                return false;
            }
            if !self.reporter.settle_deferred_start(true).await {
                let _ = self.reporter.settle_deferred_start(false).await;
                self.start_artifacts.lock().forbid_terminal_persistence();
                return false;
            }
            if self.start_artifacts.lock().publish_at(PublicationBoundary::Started) {
                self.completion_data.mark_spawned_notification_emitted();
            }
            true
        })
    }

    fn on_progress(&self, p: &SubagentProgress, _activity: Option<&VendorToolActivity>) {
        let update = SessionUpdate::SubagentProgress {
            subagent_id: self.subagent_id.clone(),
            attempt_id: self.attempt_id.clone(),
            parent_session_id: self.parent_session_id.clone(),
            child_session_id: self.subagent_id.clone(),
            duration_ms: u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            turn_count: p.turn_count,
            tool_call_count: p.tool_call_count,
            tokens_used: p.tokens_used,
            context_window_tokens: p.context_window_tokens,
            context_usage_pct: p.context_usage_pct,
            tools_used: p.tools_used.clone(),
            error_count: p.error_count,
        };
        super::spawn::emit_subagent_notification(
            &self.gateway,
            &self.parent_session_id,
            update,
            self.parent_cmd_tx.as_ref(),
        );
    }

    fn finalizing<'a>(&'a self) -> BorrowedSendFuture<'a, bool> {
        Box::pin(async move { self.reporter.finalizing().await })
    }

    fn call_mcp_tool<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        args: serde_json::Value,
    ) -> BorrowedSendFuture<'a, Result<serde_json::Value, String>> {
        Box::pin(async move {
            let client = self
                .mcp_pool
                .as_ref()
                .and_then(|p| p.get_client(server))
                .cloned()
                .ok_or_else(|| format!("MCP server '{server}' not available"))?;
            let out = client.call_tool(tool, args).await.map_err(|e| e.to_string())?;
            serde_json::to_value(out).map_err(|e| e.to_string())
        })
    }
}

/// Spec §6.7: `<server>__<tool>` from the parent snapshot, restricted to inherited, non-denied servers.
fn bridged_tools(
    pool: Option<&crate::session::mcp_servers::SharedMcpPool>,
    defs: Option<&[xai_grok_sampling_types::ToolSpec]>,
    deny: &[String],
) -> Vec<BridgedTool> {
    let (Some(pool), Some(defs)) = (pool, defs) else {
        return Vec::new();
    };
    let servers: std::collections::HashSet<&str> =
        pool.server_names().filter(|s| !deny.iter().any(|d| d == s)).collect();
    defs.iter()
        .filter_map(|spec| {
            let (server, tool) = spec.name.split_once("__")?;
            servers.contains(server).then(|| BridgedTool {
                server: server.to_owned(),
                tool: tool.to_owned(),
                name: spec.name.clone(),
                description: spec.description.clone(),
                input_schema: spec.parameters.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
fn meta_dir_for(ctx: &SubagentSpawnContext, parent_session_dir: &std::path::Path, id: &str) -> PathBuf {
    ctx.run_shell_child_harness
        .as_ref()
        .map(|h| h.meta_dir.clone())
        .unwrap_or_else(|| parent_session_dir.join("subagents").join(id))
}
#[cfg(not(test))]
fn meta_dir_for(_ctx: &SubagentSpawnContext, parent_session_dir: &std::path::Path, id: &str) -> PathBuf {
    parent_session_dir.join("subagents").join(id)
}

async fn remove_unpromoted_worktree(d: &Dispatch) {
    if d.worktree_freshly_created
        && let Some(wt) = d.worktree_path.as_deref()
        && let Err(e) = crate::session::worktree::remove_subagent_worktree(wt).await
    {
        tracing::warn!(subagent_id = %d.request.id, error = %e, "failed to remove unpromoted mod worktree");
    }
}

pub(super) async fn run(mut d: Dispatch) -> ChildRunOutput<ShellCompletionData> {
    // Same gauge the native path holds for the child's lifetime (handle_request.rs:1657).
    let _active = super::handle_request::SUBAGENTS_ACTIVE.enter();
    let validated = match std::mem::replace(&mut d.validated, Err(String::new())) {
        Ok(v) => v,
        Err(msg) => {
            remove_unpromoted_worktree(&d).await;
            return child_run_output(failure_result(&d.request, &msg), d.completion_data, None);
        }
    };
    let ns = validated.config.namespace.clone();
    let mut warnings = Vec::new();

    // §5.4 step 3
    let vendor_model = match &d.definition.model {
        ModelOverride::Override(m) => m.clone(),
        ModelOverride::Inherit => {
            remove_unpromoted_worktree(&d).await;
            let msg = format!("mod agent '{}' must declare `model`", d.request.subagent_type);
            return child_run_output(failure_result(&d.request, &msg), d.completion_data, None);
        }
    };
    let effective_model_id = format!("{ns}/{vendor_model}");

    // §5.5
    if d.request.fork_context {
        warnings.push("fork_context is not supported by mod subagents; started fresh".to_owned());
    }
    if !d.definition.mcp_servers.is_empty() {
        warnings.push("agent-owned mcpServers are ignored by mod subagents; only inherited servers are bridged".to_owned());
    }

    // §5.4 step 2
    let mut prompt = d.request.prompt.clone();
    if !d.is_wake
        && let Some(msg) = d.request.runtime_overrides.native_agent.as_ref().and_then(|n| n.message.as_ref())
    {
        if msg.native.as_ref().is_some_and(|n| n.encrypted) {
            remove_unpromoted_worktree(&d).await;
            return child_run_output(
                failure_result(&d.request, "encrypted native messages are not supported by mod subagents"),
                d.completion_data,
                None,
            );
        }
        prompt = format!("{prompt}\n\n{}", msg.body);
    }

    // §5.4 step 1
    let child_cwd = resolve_child_cwd(
        d.worktree_path.as_deref(),
        select_override_cwd(d.resume_source.as_ref(), d.request.cwd.as_deref()),
        &d.ctx.parent_cwd,
    );
    let child_cwd_string = child_cwd.to_string_lossy().into_owned();
    let parent_session_dir = session::persistence::session_dir(&SessionInfo {
        id: acp::SessionId::new(d.ctx.parent_session_id.clone()),
        cwd: d.ctx.parent_cwd.to_string_lossy().to_string(),
    });
    let meta_dir = meta_dir_for(&d.ctx, &parent_session_dir, &d.request.id);
    let resume_key = select_resume_key(&d.request.id, d.request.resume_from.as_deref(), d.is_wake);
    let source_str = if resume_key.is_some() { "resumed" } else { "new" };

    // §5.4 step 4 — field lists mirror handle_request.rs:1164-1232
    let meta = SubagentMeta {
        subagent_id: d.request.id.clone(),
        attempt_id: d.attempt_id.clone(),
        parent_session_id: d.ctx.parent_session_id.clone(),
        child_session_id: d.request.id.clone(),
        subagent_type: d.request.subagent_type.clone(),
        description: d.request.description.clone(),
        prompt: d.request.prompt.clone(),
        status: "running".to_owned(),
        started_at: chrono::Utc::now(),
        completed_at: None,
        duration_ms: None,
        tool_calls: None,
        turns: None,
        error: None,
        effective_context_source: Some(source_str.to_owned()),
        context_normalized: false,
        fork_copy_error: None,
        persona: d.effective_runtime.persona.clone(),
        resumed_from: d.request.resume_from.clone(),
        child_cwd: Some(child_cwd_string.clone()),
        worktree_path: d.worktree_path.as_ref().map(|p| p.to_string_lossy().to_string()),
        snapshot_ref: None,
        effective_model_id: Some(effective_model_id.clone()),
    };
    let advertised_address = xai_message_delivery_core::advertised_address(
        d.agent_address.as_ref().map(|a| a.as_str()),
        d.request.owner.is_workflow(),
    )
    .map(str::to_owned);
    let spawned = SessionUpdate::SubagentSpawned {
        subagent_id: d.request.id.clone(),
        attempt_id: d.attempt_id.clone(),
        child_session_id: d.request.id.clone(),
        parent_session_id: d.ctx.parent_session_id.clone(),
        parent_prompt_id: d.request.parent_prompt_id.clone(),
        subagent_type: d.request.subagent_type.clone(),
        description: d.request.description.clone(),
        effective_context_source: Some(source_str.to_owned()),
        context_normalized: false,
        capability_mode: d.effective_runtime.capability_mode
            .and_then(|m| serde_json::to_value(m).ok())
            .and_then(|v| v.as_str().map(String::from)),
        persona: d.effective_runtime.persona.clone(),
        role: d.effective_runtime.role_name.clone(),
        model: Some(effective_model_id.clone()),
        resumed_from: d.request.resume_from.clone(),
        workflow_run_id: d.request.owner.workflow_run_id().map(str::to_string),
        agent_address: advertised_address.clone(),
    };
    let boundary = if d.is_wake { PublicationBoundary::Started } else { PublicationBoundary::Prepared };
    let mut start_artifacts = StartArtifactPublication::new(
        boundary,
        (!d.is_wake).then(|| d.spawn_prepare_span.span().clone()),
        PreparedStartArtifacts {
            meta_dir: meta_dir.clone(),
            meta,
            spawned,
            advertised_address,
            reasoning_effort: d.effective_runtime.reasoning_effort.clone(),
            role_name: d.effective_runtime.role_name.clone(),
            parent_prompt_id: d.request.parent_prompt_id.clone(),
        },
        d.gateway.clone(),
        d.reporter.clone(),
        &d.ctx,
    );
    start_artifacts.publish_at(PublicationBoundary::Prepared);
    if !d.is_wake {
        d.completion_data.mark_spawned_notification_emitted();
    }
    let start_artifacts = Arc::new(Mutex::new(start_artifacts));

    // §5.4 step 5
    let read_only = d.effective_runtime.capability_mode == Some(SubagentCapabilityMode::ReadOnly);
    let caps = &validated.config.capabilities;
    let pool = if caps.mcp_bridge && (!read_only || caps.mcp_in_read_only) {
        resolve_inherited_mcp_pool(d.ctx.parent_mcp_pool.take(), &d.definition.mcp_inheritance)
    } else {
        None
    };
    let tools = bridged_tools(
        pool.as_ref(),
        d.ctx.parent_tool_definitions.as_deref(),
        &validated.config.mcp_bridge.deny_servers,
    );
    let bridged_server_count = tools.iter().map(|t| t.server.as_str()).collect::<std::collections::HashSet<_>>().len();

    // §5.4 step 10 (launch half)
    xai_grok_telemetry::session_ctx::log_event(xai_grok_telemetry::events::SubagentLaunched {
        subagent_id: d.request.id.clone(),
        parent_session_id: d.request.parent_session_id.clone(),
        subagent_type: d.request.subagent_type.clone(),
        owner: telemetry_owner_kind(&d.request),
        workflow_run_id: d.request.owner.workflow_run_id().map(str::to_string),
        queued_ms: d.queued_for.map(|q| u64::try_from(q.as_millis()).unwrap_or(u64::MAX)),
        session_running: u32::try_from(d.session_running).unwrap_or(u32::MAX),
        persona: d.request.runtime_overrides.persona.clone(),
        fork_context: false,
        resume_from: d.request.resume_from.clone(),
        isolated_worktree: d.worktree_path.is_some(),
        mcp_inherited_count: u32::try_from(bridged_server_count).unwrap_or(u32::MAX),
        mcp_owned_count: 0,
        skills_inherited_count: 0,
    });
    d.spawn_timer.record(
        xai_grok_telemetry::subagent_spawn::SubagentSpawnPhase::SpawnPrepare,
        d.start.elapsed(),
    );

    // §5.4 steps 6-8
    let host = Arc::new(ShellModHost {
        reporter: d.reporter.clone(),
        gateway: d.gateway.clone(),
        parent_cmd_tx: d.ctx.parent_cmd_tx.clone(),
        parent_session_id: d.ctx.parent_session_id.clone(),
        subagent_id: d.request.id.clone(),
        attempt_id: d.attempt_id.clone(),
        started_at: d.start,
        is_wake: d.is_wake,
        child_template: Mutex::new(Some(StartedChildTemplate {
            persona: d.effective_runtime.persona.clone(),
            resumed_from: d.request.resume_from.clone(),
            child_cwd: child_cwd_string.clone(),
            worktree_path: d.worktree_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            effective_model_id: effective_model_id.clone(),
            definition_background: d.definition.background.unwrap_or(false),
        })),
        start_artifacts: Arc::clone(&start_artifacts),
        completion_data: d.completion_data.clone(),
        mcp_pool: pool,
    });
    let outcome = xai_grok_mods::run_mod_child(
        ModRunInput {
            validated,
            subagent_id: d.request.id.clone(),
            parent_session_id: d.ctx.parent_session_id.clone(),
            subagent_type: d.request.subagent_type.clone(),
            agent_name: d.definition.name.clone(),
            vendor_model,
            cwd: child_cwd,
            prompt,
            system_prompt: d.definition.prompt_body.clone(),
            resume_key,
            read_only,
            reasoning_effort: d.effective_runtime.reasoning_effort.clone(),
            bridged_tools: tools,
            cancellation: d.cancel_token.clone(),
        },
        host,
    )
    .await;

    if !outcome.promoted {
        remove_unpromoted_worktree(&d).await;
    }
    warnings.extend(outcome.warnings.iter().cloned());
    if outcome.admission_uncertain {
        warnings.push("active-message admission did not settle before finalize; a late message may be dropped".to_owned());
    }

    let mut result = SubagentResult {
        success: outcome.error.is_none() && !outcome.cancelled,
        output: Arc::from(outcome.final_text.as_str()),
        error: outcome.error.clone(),
        cancelled: outcome.cancelled,
        subagent_id: d.request.id.clone(),
        child_session_id: d.request.id.clone(),
        tool_calls: outcome.tool_calls,
        turns: outcome.turns,
        duration_ms: u64::try_from(d.start.elapsed().as_millis()).unwrap_or(u64::MAX),
        tokens_used: outcome.tokens_used,
        output_tokens_used: 0,
        total_tokens_used: outcome.tokens_used,
        output_usage_incomplete: true,
        worktree_path: d.worktree_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
        backgrounded: false,
        warnings,
    };

    // §5.4 step 9
    let gcs_upload_ctx = GcsUploadContext {
        bucket_url: d.ctx.gcs_bucket_url.clone(),
        upload_method: d.ctx.gcs_upload_method.clone(),
        model_id: Some(effective_model_id.clone()),
        cwd: Some(child_cwd_string),
        reasoning_effort: d.effective_runtime.reasoning_effort.clone(),
        role_name: d.effective_runtime.role_name.clone(),
        parent_prompt_id: d.request.parent_prompt_id.clone(),
        auth_manager: d.ctx.auth_manager.clone(),
        isolation_mode: Some(format!("{:?}", d.effective_runtime.isolation)),
        capability_mode: d.effective_runtime.capability_mode.as_ref().map(|m| format!("{m:?}")),
        depth: d.child_depth,
    };
    let terminal_persistence_allowed = start_artifacts.lock().terminal_persistence_allowed();
    if terminal_persistence_allowed {
        d.completion_data.set_persisted_output_dir(persist_subagent_output(&meta_dir, &result));
        persist_subagent_completion(&meta_dir, &result, &gcs_upload_ctx);
    }
    d.completion_data.set_telemetry_tokens(result.tokens_used);
    let mut disposed = None;
    if outcome.promoted
        && terminal_persistence_allowed
        && d.ctx.resolve_subagent_worktree_snapshot_enabled()
        && let Some(wt) = d.worktree_path.as_deref()
    {
        let final_status = result.status().to_string();
        let disposal = super::handle_request::dispose_worktree_after_completion(
            wt,
            &resolve_subagent_source_repo(&d.ctx),
            &meta_dir,
            &final_status,
            &d.request.id,
        )
        .await;
        if disposal.worktree_removed() {
            result.worktree_path = None;
        }
        disposed = disposal.snapshot_ref().map(str::to_owned);
    }

    // §5.4 step 10 (completion half)
    let telemetry_outcome = if result.success {
        xai_grok_telemetry::events::Outcome::Completed
    } else if result.cancelled {
        xai_grok_telemetry::events::Outcome::Cancelled
    } else {
        xai_grok_telemetry::events::Outcome::Error
    };
    let mut completed = xai_grok_telemetry::events::SubagentCompleted {
        subagent_id: d.request.id.clone(),
        parent_session_id: d.request.parent_session_id.clone(),
        owner: telemetry_owner_kind(&d.request),
        workflow_run_id: d.request.owner.workflow_run_id().map(str::to_string),
        outcome: telemetry_outcome,
        duration_ms: result.duration_ms,
        tool_calls: result.tool_calls,
        tokens_used: (result.tokens_used > 0).then_some(result.tokens_used),
        queue_wait_ms: None,
        spawn_prepare_ms: None,
        session_bootstrap_ms: None,
        agent_build_ms: None,
        tool_setup_ms: None,
        ready_to_first_turn_ms: None,
    };
    d.spawn_timer.write_event_phases(&mut completed);
    xai_grok_telemetry::session_ctx::log_event(completed);

    child_run_output(result, d.completion_data, disposed)
}
```
`attempt_identity` and `wake_origin.message_id` are intentionally not carried: `ShellCompletionData::from_context` already captured the attempt identity, and the coordinator (`coordinator/wake.rs:99-112`) owns the wake message id and acknowledges it when the child is admitted. `spawn_prepare_span` is dropped at the end of `run`; `Region::close` is a no-op `fn close(self) {}` (`xai-grok-telemetry/src/region.rs:30`), so drop is equivalent.

Run: `cargo check -p xai-grok-shell`
Expected: PASS once Step 3 is applied.

- [ ] **Step 3: Wire the dispatch in `handle_request.rs` and wrap `ShellChildRuntime::Native` at line 1956**

In `crates/codegen/xai-grok-shell/src/agent/subagent/handle_request.rs`:
1. Line 22: change `static SUBAGENTS_ACTIVE:` to `pub(super) static SUBAGENTS_ACTIVE:` (0 net lines; the mod path holds the same gauge).
2. Immediately after the workflow scheduler-tool strip that ends at line 919 (and before `if request.fork_context {` at `:920`), insert:
```rust
    if let Some(validated) = super::mod_adapter::detect(&definition, &ctx) {
        return super::mod_adapter::run(super::mod_adapter::Dispatch {
            validated, request, ctx, completion_data, gateway, reporter,
            cancel_token, attempt_id, agent_address, is_wake, queued_for,
            session_running, definition, effective_runtime, resume_source,
            worktree_path, worktree_freshly_created, child_depth,
            spawn_prepare_span, spawn_timer, start,
        })
        .await;
    }
```
   Every name above is a local already bound at `:919` (verified against `handle_request.rs:392` `spawn_prepare_span`, `:842` `worktree_freshly_created`, `:886-889` `child_depth`, `:882` `effective_runtime`). If `cargo check` reports a local has a different name in your checkout, rename the field binding (`field: local_name`) rather than renaming the native local.
3. At line 1956, change `control: ShellChildRuntime { ... }` to `control: ShellChildRuntime::Native(super::child_runtime::NativeShellChildRuntime { ... })` (body unchanged).
4. In `crates/codegen/xai-grok-shell/src/agent/subagent/mod.rs`, add `mod mod_adapter;` after `mod handle_request;` (`:68`).
5. In `crates/codegen/xai-grok-shell/Cargo.toml` `[dependencies]`, add `xai-grok-mods = { workspace = true }`.

Run: `cargo check -p xai-grok-shell && cargo clippy -p xai-grok-shell -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Add shell integration tests T8, T9, T10, T11**

The tests live in a child module of `tests/wake.rs` so they reuse its private `RunShellChildTestRunner`, `configure_completion_harness`, and `acknowledge_parent_usage` without widening visibility. Append to `crates/codegen/xai-grok-shell/src/agent/subagent/tests/wake.rs`:
```rust
mod mod_adapter_tests;
```

Create `crates/codegen/xai-grok-shell/src/agent/subagent/tests/wake/mod_adapter_tests.rs`:
```rust
//! T8–T11: mod subagents through the real coordinator + `run_shell_child` dispatch.
use super::*;
use xai_grok_tools::implementations::grok_build::task::backend::{ChannelBackend, SubagentBackend};
use xai_grok_tools::implementations::grok_build::task::coordinator::{
    CoordinatorConfig, SubagentCoordinator,
};
use xai_grok_tools::implementations::grok_build::task::types::{
    AgentMailboxMessage, AgentMailboxMessageKind, NativeAgentSpawn,
};

const MOCK_SIDECAR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../xai-grok-mods/tests/fixtures/mock-sidecar.mjs"
);

/// Writes a minimal `cursor` mod plugin and returns a registry with it enabled.
fn mod_plugin_registry(
    dir: &std::path::Path,
    trusted: bool,
) -> std::sync::Arc<xai_grok_agent::plugins::PluginRegistry> {
    use xai_grok_agent::plugins::TrustStore;
    use xai_grok_agent::plugins::discovery::{DiscoveryConfig, discover_plugins};
    let root = dir.join("cursor");
    std::fs::create_dir_all(root.join(".grok-plugin")).unwrap();
    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::create_dir_all(root.join("sidecar")).unwrap();
    std::fs::write(
        root.join(".grok-plugin/plugin.json"),
        r#"{"name":"cursor","version":"1.0.0","agents":["./agents/"]}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("agents/composer-2.md"),
        "---\nname: composer-2\ndescription: mock cursor agent\nmodel: composer-2\n---\n",
    )
    .unwrap();
    std::fs::copy(MOCK_SIDECAR, root.join("sidecar/runner.mjs")).unwrap();
    std::fs::write(
        root.join("mod.json"),
        serde_json::json!({
            "schemaVersion": 1,
            "namespace": "cursor",
            "runtime": {
                "command": "node",
                "args": ["sidecar/runner.mjs"],
                "timeoutSecs": 20,
                "gracefulKillMs": 100,
                "envAllowlist": ["PATH", "HOME"]
            },
            "capabilities": {
                "liveSteer": true, "sessionResume": true,
                "mcpBridge": false, "mcpInReadOnly": false
            }
        })
        .to_string(),
    )
    .unwrap();
    let config = DiscoveryConfig {
        cli_plugin_dirs: vec![root],
        ..Default::default()
    };
    let trust = TrustStore::load_from(dir.join("trust.json"));
    let mut discovered = discover_plugins(Some(dir), &config, &trust, true);
    discovered.retain(|p| p.manifest.name == "cursor");
    assert_eq!(discovered.len(), 1, "mock cursor plugin must be discovered");
    discovered[0].trusted = trusted;
    std::sync::Arc::new(xai_grok_agent::plugins::PluginRegistry::from_discovered(
        discovered,
        &[],
        &["cursor".to_owned()],
    ))
}

fn mod_ctx(
    server: &xai_grok_test_support::MockInferenceServer,
    meta_dir: &std::path::Path,
    registry: &std::sync::Arc<xai_grok_agent::plugins::PluginRegistry>,
    parent_cmd_tx: &mpsc::UnboundedSender<SessionCommand>,
) -> SubagentSpawnContext {
    let mut ctx = ctx_with_toggle(HashMap::new());
    configure_completion_harness(
        &mut ctx,
        server,
        RunShellChildHarnessConfig::new(meta_dir.to_path_buf(), InitialAttemptBehavior::Normal),
    );
    ctx.plugin_registry = Some(std::sync::Arc::clone(registry));
    ctx.parent_cmd_tx = Some(parent_cmd_tx.clone());
    ctx
}

fn mod_request(id: &str, prompt: &str) -> SubagentRequest {
    let mut request = auto_wake_test_request(id);
    request.subagent_type = "cursor:composer-2".into();
    request.prompt = prompt.into();
    request.parent_session_id = "setup-parent".into();
    request
}

fn count_updates(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<xai_acp_lib::AcpClientMessage>,
    needle: &str,
) -> usize {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|message| {
            matches!(
                message,
                xai_acp_lib::AcpClientMessage::ExtNotification(args)
                    if args.request.params.get().contains(needle)
            )
        })
        .count()
}

macro_rules! require_node {
    () => {
        if std::process::Command::new("node").arg("--version").output().is_err() {
            eprintln!("skipping: node not on PATH");
            return;
        }
    };
}

#[tokio::test(flavor = "current_thread")]
async fn t8_task_mod_spawn_writes_meta_and_output_and_emits_notifications() {
    require_node!();
    xai_test_utils::require_git!();
    use xai_test_utils::git::seed_repo_with_remote;
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let (repo, _remote) = seed_repo_with_remote(temp.path());
            let meta_dir = temp.path().join("meta");
            let registry = mod_plugin_registry(temp.path(), true);
            let server = xai_grok_test_support::MockInferenceServer::start().await.unwrap();
            let (parent_cmd_tx, parent_cmd_rx) = mpsc::unbounded_channel();
            let usage_ack = tokio::task::spawn_local(acknowledge_parent_usage(parent_cmd_rx));
            let mut ctx = mod_ctx(&server, &meta_dir, &registry, &parent_cmd_tx);
            ctx.parent_cwd = repo;
            ctx.remote_settings = Some(crate::util::config::RemoteSettings {
                subagent_worktree_snapshot_enabled: Some(true),
                ..Default::default()
            });
            let (gateway, mut gateway_rx) = test_gateway_with_receiver();
            let (command_tx, command_rx) = SubagentCoordinator::<RunShellChildTestRunner>::channel();
            let coordinator = tokio::task::spawn_local(
                SubagentCoordinator::from_channel(
                    command_rx,
                    RunShellChildTestRunner::new([ctx], false, gateway),
                    CoordinatorConfig::default(),
                )
                .run(),
            );
            let backend = ChannelBackend::for_coordinator_session(command_tx, "setup-parent");
            let id = uuid::Uuid::now_v7().to_string();

            let mut req = mod_request(&id, "hello");
            req.runtime_overrides.isolation =
                Some(xai_tool_types::SubagentIsolationMode::Worktree);
            let result = backend.spawn(req, None).await.expect("spawn");
            assert!(result.success, "mod spawn failed: {:?}", result.error);
            assert_eq!(result.output.as_ref(), format!("reply:hello:vsid=v-{id}"));

            let persisted: SubagentMeta = serde_json::from_str(
                &std::fs::read_to_string(meta_dir.join("meta.json")).expect("meta"),
            )
            .unwrap();
            assert_eq!(persisted.status, "completed");
            assert_eq!(persisted.effective_model_id.as_deref(), Some("cursor/composer-2"));
            assert!(persisted.snapshot_ref.is_some());
            assert_eq!(
                read_subagent_output(&meta_dir).as_deref(),
                Some(result.output.as_ref())
            );
            let updates: Vec<_> = std::iter::from_fn(|| gateway_rx.try_recv().ok()).collect();
            let count = |needle: &str| {
                updates
                    .iter()
                    .filter(|m| matches!(m, xai_acp_lib::AcpClientMessage::ExtNotification(a)
                        if a.request.params.get().contains(needle)))
                    .count()
            };
            assert_eq!(count("\"subagent_spawned\""), 1);
            assert_eq!(count("\"subagent_finished\""), 1);
            assert!(count("\"subagent_progress\"") >= 1);

            drop(backend);
            coordinator.await.unwrap();
            usage_ack.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn t9_v2_spawn_folds_message_steers_and_wakes_with_deferred_start() {
    require_node!();
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let meta_dir = temp.path().join("meta");
            let registry = mod_plugin_registry(temp.path(), true);
            let server = xai_grok_test_support::MockInferenceServer::start().await.unwrap();
            let (parent_cmd_tx, parent_cmd_rx) = mpsc::unbounded_channel();
            let usage_ack = tokio::task::spawn_local(acknowledge_parent_usage(parent_cmd_rx));
            let first = mod_ctx(&server, &meta_dir, &registry, &parent_cmd_tx);
            let wake = mod_ctx(&server, &meta_dir, &registry, &parent_cmd_tx);
            let (gateway, mut gateway_rx) = test_gateway_with_receiver();
            let (command_tx, command_rx) = SubagentCoordinator::<RunShellChildTestRunner>::channel();
            let coordinator = tokio::task::spawn_local(
                SubagentCoordinator::from_channel(
                    command_rx,
                    RunShellChildTestRunner::new([first, wake], false, gateway),
                    CoordinatorConfig::default(),
                )
                .run(),
            );
            let backend = ChannelBackend::for_coordinator_session(command_tx, "setup-parent");
            let id = uuid::Uuid::now_v7().to_string();

            // (a) v2 spawn: native_agent.message body is folded into run.start.prompt.
            let mut request = mod_request(&id, "WAIT_FOR_ACTIVE_MSG");
            request.runtime_overrides.native_agent = Some(NativeAgentSpawn {
                task_name: "worker".into(),
                message: Some(AgentMailboxMessage {
                    message_id: "m-0".into(),
                    team_scope_id: "setup-parent".into(),
                    from_agent_id: "setup-parent".into(),
                    to_agent_id: id.clone(),
                    kind: AgentMailboxMessageKind::NativeMessage,
                    body: "inbox body".into(),
                    created_at_ms: 1,
                    native: None,
                }),
                ..Default::default()
            });
            let spawn = tokio::task::spawn_local({
                let backend = backend.clone();
                async move { backend.spawn(request, None).await }
            });

            // (b) live steer while running ⇒ Accepted (ChildControl returned Admitted).
            let mut steered = None;
            for _ in 0..200 {
                let outcome = backend
                    .send_active_message(
                        ActiveAgentMessageRequest::try_new(&id, "steer-1").expect("request"),
                    )
                    .await;
                if matches!(outcome, ActiveAgentMessageOutcome::Accepted { .. }) {
                    steered = Some(outcome);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            assert!(steered.is_some(), "steer was never admitted");
            let first_result = spawn.await.unwrap().expect("spawn");
            assert!(first_result.success, "{:?}", first_result.error);
            assert!(
                first_result
                    .output
                    .starts_with("reply:WAIT_FOR_ACTIVE_MSG\n\ninbox body:vsid="),
                "prompt fold missing: {}",
                first_result.output
            );
            assert!(first_result.output.contains("|steered:"));
            while gateway_rx.try_recv().is_ok() {}

            // (c) idle wake ⇒ deferred start settles, resume key = request.id ⇒ same vendor session.
            let outcome = backend
                .send_active_message(
                    ActiveAgentMessageRequest::try_new(&id, "continue").expect("wake request"),
                )
                .await;
            assert!(
                matches!(outcome, ActiveAgentMessageOutcome::Accepted { .. }),
                "wake not accepted: {outcome:?}"
            );
            let woke = backend.query(&id, true, Some(20_000)).await.expect("woken snapshot");
            match woke.status {
                SubagentSnapshotStatus::Completed { ref output, .. } => {
                    assert_eq!(output.to_string(), format!("reply:continue:vsid=v-{id}"));
                }
                other => panic!("expected completed wake, got {other:?}"),
            }
            assert_eq!(count_updates(&mut gateway_rx, "\"subagent_spawned\""), 1);

            drop(backend);
            coordinator.await.unwrap();
            usage_ack.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn t10_rhai_retry_resumes_vendor_session_id() {
    require_node!();
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let meta_dir = temp.path().join("meta");
            let registry = mod_plugin_registry(temp.path(), true);
            let server = xai_grok_test_support::MockInferenceServer::start().await.unwrap();
            let (parent_cmd_tx, parent_cmd_rx) = mpsc::unbounded_channel();
            let usage_ack = tokio::task::spawn_local(acknowledge_parent_usage(parent_cmd_rx));
            let a = mod_ctx(&server, &meta_dir, &registry, &parent_cmd_tx);
            let b = mod_ctx(&server, &meta_dir, &registry, &parent_cmd_tx);
            let (gateway, _gateway_rx) = test_gateway_with_receiver();
            let (command_tx, command_rx) = SubagentCoordinator::<RunShellChildTestRunner>::channel();
            let coordinator = tokio::task::spawn_local(
                SubagentCoordinator::from_channel(
                    command_rx,
                    RunShellChildTestRunner::new([a, b], false, gateway),
                    CoordinatorConfig::default(),
                )
                .run(),
            );
            let backend = ChannelBackend::for_coordinator_session(command_tx, "setup-parent");

            // Attempt 1: sidecar returns invalid JSON (what Rhai's agent_json would reject).
            let first_id = uuid::Uuid::now_v7().to_string();
            let first = backend
                .spawn(mod_request(&first_id, "JSON_RETRY"), None)
                .await
                .expect("first");
            assert_eq!(first.output.as_ref(), "not-json");
            assert!(serde_json::from_str::<serde_json::Value>(&first.output).is_err());

            // Attempt 2: the Rhai retry path re-spawns with resume_from = first id.
            let retry_id = uuid::Uuid::now_v7().to_string();
            let mut retry = mod_request(&retry_id, "JSON_RETRY");
            retry.resume_from = Some(first_id.clone());
            let second = backend.spawn(retry, None).await.expect("retry");
            let parsed: serde_json::Value =
                serde_json::from_str(&second.output).expect("retry output is valid JSON");
            assert_eq!(parsed["vsid"], format!("v-{first_id}"));

            drop(backend);
            coordinator.await.unwrap();
            usage_ack.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn t11_untrusted_mod_plugin_fails_closed_without_spawning() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let meta_dir = temp.path().join("meta");
            let registry = mod_plugin_registry(temp.path(), false);
            // Make any accidental spawn observable: replace the sidecar with a marker writer.
            let marker = temp.path().join("spawned.marker");
            std::fs::write(
                temp.path().join("cursor/sidecar/runner.mjs"),
                format!(
                    "require('node:fs').writeFileSync({:?}, 'x');",
                    marker.to_string_lossy()
                ),
            )
            .unwrap();
            let server = xai_grok_test_support::MockInferenceServer::start().await.unwrap();
            let (parent_cmd_tx, parent_cmd_rx) = mpsc::unbounded_channel();
            let usage_ack = tokio::task::spawn_local(acknowledge_parent_usage(parent_cmd_rx));
            let ctx = mod_ctx(&server, &meta_dir, &registry, &parent_cmd_tx);
            let (gateway, mut gateway_rx) = test_gateway_with_receiver();
            let (command_tx, command_rx) = SubagentCoordinator::<RunShellChildTestRunner>::channel();
            let coordinator = tokio::task::spawn_local(
                SubagentCoordinator::from_channel(
                    command_rx,
                    RunShellChildTestRunner::new([ctx], false, gateway),
                    CoordinatorConfig::default(),
                )
                .run(),
            );
            let backend = ChannelBackend::for_coordinator_session(command_tx, "setup-parent");
            let id = uuid::Uuid::now_v7().to_string();

            let result = backend.spawn(mod_request(&id, "hello"), None).await.expect("spawn");
            assert!(!result.success);
            assert!(
                result.error.as_deref().unwrap_or_default().contains("not trusted"),
                "unexpected error: {:?}",
                result.error
            );
            assert!(!marker.exists(), "untrusted mod must not start a process");
            assert!(!meta_dir.join("meta.json").exists());
            assert_eq!(count_updates(&mut gateway_rx, "\"subagent_spawned\""), 0);

            drop(backend);
            coordinator.await.unwrap();
            usage_ack.abort();
        })
        .await;
}
```
Notes for the implementer:
- `require_node!` keeps CI hosts without Node green; it does not mask logic failures because all four assertions run whenever Node exists. T11 needs no Node.
- If `ChannelBackend` is not `Clone` in your checkout, replace the `backend.clone()` in T9 with a second `ChannelBackend::for_coordinator_session(command_tx.clone(), "setup-parent")`.
- If `which` is not already a `xai-grok-shell` dev-dependency, use `std::process::Command::new("node").arg("--version").output().is_err()` in `require_node!` instead of adding a dependency.
- The existing native tests (`tests/wake.rs`, `child_runtime_tests.rs`) are the T11 "non-mod plugin agent ⇒ native path unchanged" guard.

Run: `cargo test -p xai-grok-shell subagent::`
Expected: PASS (all existing native subagent tests + `t8_*`, `t9_*`, `t10_*`, `t11_*`).

- [ ] **Step 5: Commit Task 5**

```bash
git add crates/codegen/xai-grok-shell/
git commit -m "feat(shell): wire ShellChildRuntime::Mod and mod_adapter into run_shell_child"
```

---

### Task 6: Reference `plugins/cursor` Sidecar Plugin & Upstream Diff Budget Gate (T12)

**Files:**
- Create: `plugins/cursor/.grok-plugin/plugin.json`
- Create: `plugins/cursor/mod.json`
- Create: `plugins/cursor/agents/composer-2.md`, `gpt-5-4.md`, `claude-opus-4-6.md`, `claude-sonnet-4-6.md`
- Create: `plugins/cursor/agents.generated/.gitkeep`
- Create: `plugins/cursor/hooks/hooks.json`
- Create: `plugins/cursor/sidecar/package.json`
- Create: `plugins/cursor/sidecar/runner.mjs`
- Create: `plugins/cursor/sidecar/catalog.mjs`
- Create: `plugins/cursor/sidecar/runner.test.mjs` (protocol smoke test with a stubbed SDK)
- Create: `plugins/cursor/.gitignore` (`sidecar/node_modules/`, `agents.generated/*.md`)

**Interfaces:**
- Consumes: `xai-grok-mods` NDJSON protocol v1 (`run.start`, `run.message`, `run.drain`, `run.finish`, `run.interrupt`, `run.cancel`) and the Unix-socket bridge RPC (`BridgeRpcRequest { id, token, method: "tools/call", server, tool, arguments }` → `BridgeRpcResponse { id, ok, result?, error? }`, one JSON object per line).
- Produces: plugin agents `cursor:composer-2`, `cursor:gpt-5-4`, `cursor:claude-opus-4-6`, `cursor:claude-sonnet-4-6`, plus `cursor:<model>` for every file `catalog.mjs` writes into `agents.generated/`.

- [ ] **Step 1: Manifests, static agents, hook**

`plugins/cursor/.grok-plugin/plugin.json` (Spec §4.3):
```json
{
  "name": "cursor",
  "version": "1.0.0",
  "description": "Cursor SDK subagents via Grok-Build mod protocol v1",
  "agents": ["./agents/", "./agents.generated/"],
  "hooks": "./hooks/hooks.json"
}
```

`plugins/cursor/mod.json` (Spec §4.4):
```json
{
  "schemaVersion": 1,
  "namespace": "cursor",
  "runtime": {
    "command": "node",
    "args": ["sidecar/runner.mjs"],
    "timeoutSecs": 1800,
    "gracefulKillMs": 2000,
    "envAllowlist": ["PATH","HOME","USER","TMPDIR","LANG","LC_ALL","CURSOR_API_KEY","HTTPS_PROXY","HTTP_PROXY","NO_PROXY"]
  },
  "capabilities": {
    "liveSteer": true,
    "sessionResume": true,
    "mcpBridge": true,
    "mcpInReadOnly": false
  },
  "mcpBridge": { "denyServers": ["pty", "codebase-memory", "weave"] }
}
```

`plugins/cursor/agents/composer-2.md` (repeat for the other three, changing `name`, `description`, and `model` to `gpt-5.4`, `claude-opus-4-6`, `claude-sonnet-4-6`; file stems use `-` because the stem is the `subagent_type` suffix):
```markdown
---
name: composer-2
description: Cursor Composer 2 coding subagent (via grok-mod cursor). Fast multi-file edits; use for implementation tasks.
model: composer-2
capabilityMode: full
---
```

`plugins/cursor/agents.generated/.gitkeep`: empty file (so `dunce::canonicalize` of `./agents.generated/` succeeds before the first catalog refresh).

`plugins/cursor/hooks/hooks.json` (format verified against `xai-grok-agent/src/plugins/hooks_adapter.rs:248-256,349-350`):
```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "node \"${GROK_PLUGIN_ROOT}/sidecar/catalog.mjs\" --write-agents --detach",
            "timeout": 5
          }
        ]
      }
    ]
  }
}
```

`plugins/cursor/.gitignore`:
```
sidecar/node_modules/
agents.generated/*.md
```

`plugins/cursor/sidecar/package.json`:
```json
{
  "name": "grok-mod-cursor-sidecar",
  "private": true,
  "type": "module",
  "engines": { "node": ">=22.13" },
  "dependencies": { "@cursor/sdk": "1.0.37" },
  "scripts": { "test": "node --test runner.test.mjs" }
}
```
(`@cursor/sdk` pin copied from `refs/netbrah-cursor-subagents/sidecar/package.json`.)

- [ ] **Step 2: `sidecar/runner.mjs` (protocol v1 over `@cursor/sdk`)**

Ported from `refs/netbrah-cursor-subagents/sidecar/run.mjs`: env scrub + `ZDOTDIR` isolation, key redaction, stdout purity, per-agent `JsonlLocalAgentStore`, `Agent.create`/`Agent.resume`, the `run.wait()` hang fallback, and custom-tool bridging. Dropped: the HTTP-over-socket result channel (the host now owns the socket and speaks line-delimited RPC) and the one-shot stdin request (replaced by the long-lived frame loop).
```javascript
#!/usr/bin/env node
// Grok-Build mod protocol v1 sidecar for the Cursor SDK. Frames: NDJSON on stdin/stdout.
import { Agent, JsonlLocalAgentStore } from "@cursor/sdk";
import { randomUUID } from "node:crypto";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import net from "node:net";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import readline from "node:readline";

const KEY = process.env.CURSOR_API_KEY ?? "";
// The host already env_clear()s to mod.json envAllowlist; keep zsh from re-reading user rc files.
try {
  const z = mkdtempSync(join(tmpdir(), "grok-mod-zdot-"));
  process.env.ZDOTDIR = z;
  process.on("exit", () => rmSync(z, { recursive: true, force: true }));
} catch {}
const redact = (s) => (KEY ? String(s).split(KEY).join("[redacted]") : String(s));
const rawErr = process.stderr.write.bind(process.stderr);
process.stderr.write = (c, ...r) => rawErr(redact(typeof c === "string" ? c : Buffer.from(c).toString("utf8")), ...r);
console.log = console.info = console.debug = (...a) => process.stderr.write(a.join(" ") + "\n");
process.on("unhandledRejection", (e) => process.stderr.write(`unhandledRejection ${e?.message ?? e}\n`));
const emit = (o) => process.stdout.write(redact(JSON.stringify(o)) + "\n");

let start; // run.start frame
let agent;
let currentRun;
let turns = 0;
let toolCalls = 0;
let tokensUsed = 0;
let lastText = "";
const queued = []; // { message_id, mode, from, text }
let busy = false;
let finished = false;

// ---- MCP bridge client (host-owned Unix socket, line-delimited JSON RPC) ----
let bridgeSock;
const bridgeWaiters = new Map();
function bridgeConnect(info) {
  if (!info?.socket_path) return Promise.resolve();
  return new Promise((resolve, reject) => {
    bridgeSock = net.createConnection(info.socket_path, resolve);
    bridgeSock.once("error", reject);
    readline.createInterface({ input: bridgeSock }).on("line", (line) => {
      let resp;
      try { resp = JSON.parse(line); } catch { return; }
      const w = bridgeWaiters.get(resp.id);
      if (w) { bridgeWaiters.delete(resp.id); w(resp); }
    });
    bridgeSock.on("close", () => {
      for (const [, w] of bridgeWaiters) w({ ok: false, error: "bridge closed" });
      bridgeWaiters.clear();
    });
  });
}
function bridgeCall(tool, args) {
  const id = randomUUID();
  return new Promise((resolve) => {
    bridgeWaiters.set(id, resolve);
    bridgeSock.write(JSON.stringify({
      id, token: start.bridge.token, method: "tools/call",
      server: tool.server, tool: tool.tool, arguments: args ?? {},
    }) + "\n");
  });
}
const sanitize = (n) => String(n ?? "").replace(/[^A-Za-z0-9_-]/g, "_").slice(0, 64);
function customTools() {
  if (!start.bridge || !start.bridged_tools?.length) return {};
  const out = {};
  for (const t of start.bridged_tools) {
    out[sanitize(t.name)] = {
      description: t.description ?? "",
      inputSchema: t.input_schema ?? { type: "object" },
      execute: async (args) => {
        const r = await bridgeCall(t, args);
        const text = r.ok ? (typeof r.result === "string" ? r.result : JSON.stringify(r.result)) : `error: ${r.error}`;
        return { content: [{ type: "text", text }], isError: !r.ok };
      },
    };
  }
  return { customTools: out };
}

// ---- Cursor SDK turn driver ----
function storeDir() {
  const dir = join(process.env.HOME || homedir(), ".grok", "mods", "cursor", "cursor-store", sanitize(start.parent_session_id));
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  return dir;
}
async function openAgent() {
  const tools = customTools();
  const opts = {
    apiKey: KEY,
    model: start.model,
    // read_only maps to Cursor "ask" mode at send() time and strips write-capable tools here.
    disallowedTools: start.read_only ? ["task", "webSearch", "webFetch", "edit", "shell"] : ["task", "webSearch", "webFetch"],
    local: { cwd: start.cwd, settingSources: [], store: new JsonlLocalAgentStore(storeDir()), ...tools },
  };
  if (start.system_prompt) opts.systemPrompt = start.system_prompt;
  agent = start.resume_vendor_session_id
    ? await Agent.resume(start.resume_vendor_session_id, opts)
    : await Agent.create(opts);
  emit({ type: "run.started", vendor_session_id: agent.agentId, effective_model: start.model });
}
async function runTurn(prompt) {
  busy = true;
  turns += 1;
  currentRun = await agent.send(prompt, {
    ...(start.read_only ? { mode: "ask" } : {}),
    local: { force: Boolean(start.resume_vendor_session_id) && turns === 1 },
    onDelta: ({ update }) => {
      if (update.type === "tool-call-started") {
        toolCalls += 1;
        const a = update.toolCall?.args ?? {};
        emit({
          type: "run.progress", turns, tool_calls: toolCalls, tokens_used: tokensUsed,
          tool: { name: String(update.toolCall?.type ?? "tool"), summary: String(a.path ?? a.command ?? a.query ?? "").slice(0, 200) },
        });
      } else if (update.type === "token-delta") {
        tokensUsed += Number(update.tokens ?? 0);
      }
    },
  });
  // run.wait() can hang after the stream ends; fall back to status 15s after a terminal status.
  let waited = false;
  let timer;
  const r = await Promise.race([
    currentRun.wait().finally(() => (waited = true)),
    new Promise((res) => {
      const arm = (st) => {
        if (st !== "running" && !timer) timer = setTimeout(() => !waited && res({ status: st, result: currentRun.result, usage: currentRun.usage }), 15000);
      };
      currentRun.onDidChangeStatus(arm);
      arm(currentRun.status);
    }),
  ]);
  clearTimeout(timer);
  currentRun = undefined;
  busy = false;
  if (r.usage?.totalTokens) tokensUsed = Math.max(tokensUsed, Number(r.usage.totalTokens));
  if (r.status === "error") throw new Error(redact(r.error?.message ?? "cursor run failed"));
  if (r.status === "cancelled") throw Object.assign(new Error("cancelled"), { cancelled: true });
  lastText = r.result ?? "";
  emit({ type: "run.idle", final_text: lastText });
}
function drainPrompt() {
  const batch = queued.splice(0);
  for (const m of batch) emit({ type: "run.consumed", message_id: m.message_id });
  return batch.map((m) => `Message from ${m.from} (${m.mode}):\n${m.text}`).join("\n\n");
}
function fail(e) {
  emit({
    type: "run.failed", error: redact(e?.message ?? e), cancelled: Boolean(e?.cancelled),
    turns, tool_calls: toolCalls, tokens_used: tokensUsed,
  });
  shutdown(e?.cancelled ? 130 : 1);
}
async function shutdown(code) {
  finished = true;
  try { await agent?.[Symbol.asyncDispose]?.(); } catch {}
  bridgeSock?.end();
  await new Promise((res) => process.stdout.write("", res));
  process.exit(code);
}

// ---- frame loop ----
readline.createInterface({ input: process.stdin, crlfDelay: Infinity }).on("line", async (line) => {
  if (!line.trim() || finished) return;
  let f;
  try { f = JSON.parse(line); } catch { return; }
  try {
    switch (f.type) {
      case "run.start":
        if (f.protocol !== 1) return fail(new Error(`unsupported protocol ${f.protocol}`));
        if (!KEY) return fail(new Error("CURSOR_API_KEY is not set"));
        start = f;
        await bridgeConnect(f.bridge);
        await openAgent();
        await runTurn(f.prompt);
        break;
      case "run.message":
        // The SDK has no mid-turn injection: steer degrades to queue (Spec §10 R3).
        queued.push(f);
        break;
      case "run.drain":
        if (!busy && queued.length) await runTurn(drainPrompt());
        else emit({ type: "run.idle", final_text: lastText });
        break;
      case "run.finish":
        emit({ type: "run.completed", final_text: lastText, turns, tool_calls: toolCalls, tokens_used: tokensUsed });
        await shutdown(0);
        break;
      case "run.interrupt":
        await currentRun?.cancel().catch(() => {});
        break;
      case "run.cancel":
        await currentRun?.cancel().catch(() => {});
        fail(Object.assign(new Error(f.reason || "cancelled"), { cancelled: true }));
        break;
    }
  } catch (e) {
    fail(e);
  }
});
process.stdin.on("end", () => !finished && fail(new Error("host closed stdin")));
```

- [ ] **Step 3: `sidecar/catalog.mjs --write-agents`**

```javascript
#!/usr/bin/env node
// Refreshes agents.generated/<model>.md from the live Cursor model catalog. Never blocks a session:
// --detach re-spawns itself in the background and exits 0 immediately.
import { spawn } from "node:child_process";
import { mkdirSync, readdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(here, "..", "agents.generated");
const staticStems = new Set(["composer-2", "gpt-5-4", "claude-opus-4-6", "claude-sonnet-4-6"]);

if (process.argv.includes("--detach")) {
  const args = process.argv.slice(1).filter((a) => a !== "--detach");
  spawn(process.execPath, args, { detached: true, stdio: "ignore", env: process.env }).unref();
  process.exit(0);
}
if (!process.argv.includes("--write-agents")) {
  process.stderr.write("usage: catalog.mjs --write-agents [--detach]\n");
  process.exit(2);
}
if (!process.env.CURSOR_API_KEY) process.exit(0); // nothing to refresh; static agents still work

const stem = (id) => String(id).toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 64);
const yaml = (s) => JSON.stringify(String(s)); // JSON strings are valid YAML scalars

try {
  const { Cursor } = await import("@cursor/sdk");
  const models = await Cursor.models.list();
  mkdirSync(outDir, { recursive: true });
  const keep = new Set([".gitkeep"]);
  for (const m of models) {
    const id = m.id ?? m.name;
    const s = stem(id);
    if (!s || staticStems.has(s)) continue;
    const body = [
      "---",
      `name: ${s}`,
      `description: ${yaml(`Cursor ${m.displayName ?? id} subagent (via grok-mod cursor)`)}`,
      `model: ${yaml(id)}`,
      "capabilityMode: full",
      "---",
      "",
    ].join("\n");
    const tmp = join(outDir, `.${s}.md.${process.pid}.tmp`);
    writeFileSync(tmp, body, { mode: 0o644 });
    renameSync(tmp, join(outDir, `${s}.md`)); // atomic on the same filesystem
    keep.add(`${s}.md`);
  }
  for (const f of readdirSync(outDir)) if (!keep.has(f)) rmSync(join(outDir, f), { force: true });
} catch (e) {
  const key = process.env.CURSOR_API_KEY;
  process.stderr.write(`catalog refresh failed: ${String(e?.message ?? e).split(key).join("[redacted]")}\n`);
}
// The SDK keeps HTTP/2 handles open for ~80s; do not wait on them.
process.exit(0);
```

- [ ] **Step 4: Sidecar protocol smoke test with a stubbed SDK**

`plugins/cursor/sidecar/runner.test.mjs` runs `runner.mjs` against a stub `@cursor/sdk` placed in a temp `node_modules`, so the protocol is checked in CI without network or a key:
```javascript
import { spawn } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import readline from "node:readline";
import test from "node:test";
import assert from "node:assert/strict";

const STUB = `
export class JsonlLocalAgentStore { constructor(d) { this.d = d; } }
const mk = (id) => ({
  agentId: id,
  async send(prompt, o) {
    o.onDelta?.({ update: { type: "tool-call-started", toolCall: { type: "read", args: { path: "a.rs" } } } });
    const run = { status: "finished", result: "echo:" + prompt, usage: { totalTokens: 7 },
      wait: async () => ({ status: "finished", result: "echo:" + prompt, usage: { totalTokens: 7 } }),
      onDidChangeStatus() {}, cancel: async () => {} };
    return run;
  },
  async [Symbol.asyncDispose]() {},
});
export const Agent = { create: async () => mk("cur-new"), resume: async (id) => mk(id) };
`;

function harness() {
  const dir = mkdtempSync(join(tmpdir(), "grok-mod-cursor-"));
  mkdirSync(join(dir, "node_modules/@cursor/sdk"), { recursive: true });
  writeFileSync(join(dir, "node_modules/@cursor/sdk/package.json"), '{"name":"@cursor/sdk","type":"module","main":"index.js"}');
  writeFileSync(join(dir, "node_modules/@cursor/sdk/index.js"), STUB);
  cpSync(new URL("./runner.mjs", import.meta.url), join(dir, "runner.mjs"));
  const child = spawn(process.execPath, [join(dir, "runner.mjs")], {
    cwd: dir, env: { PATH: process.env.PATH, HOME: dir, CURSOR_API_KEY: "k-test" }, stdio: ["pipe", "pipe", "inherit"],
  });
  const frames = [];
  const waiters = [];
  readline.createInterface({ input: child.stdout }).on("line", (l) => {
    const f = JSON.parse(l);
    frames.push(f);
    waiters.splice(0).forEach((w) => w());
  });
  const send = (f) => child.stdin.write(JSON.stringify(f) + "\n");
  const until = async (type) => {
    while (!frames.some((f) => f.type === type)) await new Promise((r) => waiters.push(r));
    return frames.find((f) => f.type === type);
  };
  return { dir, child, frames, send, until };
}

const startFrame = (dir, extra = {}) => ({
  type: "run.start", protocol: 1, subagent_id: "s1", parent_session_id: "p1",
  subagent_type: "cursor:composer-2", agent_name: "composer-2", model: "composer-2",
  cwd: dir, prompt: "hi", system_prompt: null, resume_vendor_session_id: null,
  read_only: false, reasoning_effort: null, bridge: null, bridged_tools: [], ...extra,
});

test("start → started → progress → idle → drain(consumed) → finish → completed", async () => {
  const h = harness();
  h.send(startFrame(h.dir));
  assert.equal((await h.until("run.started")).vendor_session_id, "cur-new");
  assert.equal((await h.until("run.idle")).final_text, "echo:hi");
  assert.ok(h.frames.some((f) => f.type === "run.progress" && f.tool.summary === "a.rs"));
  h.send({ type: "run.message", message_id: "m1", mode: "steer", from: "root", text: "also x" });
  h.send({ type: "run.drain" });
  assert.equal((await h.until("run.consumed")).message_id, "m1");
  h.send({ type: "run.finish" });
  const done = await h.until("run.completed");
  assert.match(done.final_text, /also x/);
  assert.equal(done.turns, 2);
});

test("resume_vendor_session_id resumes the same Cursor agent", async () => {
  const h = harness();
  h.send(startFrame(h.dir, { resume_vendor_session_id: "cur-prev" }));
  assert.equal((await h.until("run.started")).vendor_session_id, "cur-prev");
  h.send({ type: "run.cancel", reason: "test" });
  assert.equal((await h.until("run.failed")).cancelled, true);
});
```

Run (from `plugins/cursor/sidecar/`): `node --test runner.test.mjs`
Expected: 2 tests PASS without network access.

- [ ] **Step 5: Full verification and T12 upstream diff budget**

Run:
```bash
cargo test -p xai-grok-mods
cargo test -p xai-grok-shell subagent::
(cd plugins/cursor/sidecar && node --test runner.test.mjs)
git diff --numstat 2fcd97294a23cfb19b1d6e4c1a788840a17dc569 -- \
  ':!crates/codegen/xai-grok-mods' \
  ':!crates/codegen/xai-grok-shell/src/agent/subagent/mod_adapter.rs' \
  ':!crates/codegen/xai-grok-shell/src/agent/subagent/tests/wake/mod_adapter_tests.rs' \
  ':!plugins' \
  ':!Cargo.lock' \
  | awk '{a+=$1; d+=$2; n+=1} END {print n" files, +"a" -"d", total "a+d}'
```
Expected:
- All Rust and Node tests PASS.
- The budget line reports **≤ 85 total changed lines** across at most 8 existing files: root `Cargo.toml` (workspace member + dependency), `xai-grok-shell/Cargo.toml`, `subagent/mod.rs`, `child_runtime.rs`, `child_runtime_tests.rs`, `handle_request.rs`, `tests/wake.rs`. Most of the count is the delegating enum in `child_runtime.rs` (~30 lines) and the dispatch block (~11 lines).
- `git diff --stat 2fcd9729 -- crates/codegen/xai-grok-agent crates/codegen/xai-grok-tools crates/codegen/xai-workflow crates/codegen/xai-grok-shell/src/agent/subagent/spawn.rs crates/codegen/xai-grok-shell/src/agent/subagent/start_artifact_publication.rs` prints nothing.

If the total exceeds 85, collapse the enum match arms onto single lines (they are pure delegation) before anything else; do not move logic back into `handle_request.rs`.

- [ ] **Step 6: Manual dogfood (requires `CURSOR_API_KEY`)**

```bash
(cd plugins/cursor/sidecar && npm ci)
grok --plugin-dir "$PWD/plugins/cursor"   # CLI scope ⇒ auto-trusted + auto-enabled
```
In the session: `task(subagent_type="cursor:composer-2", prompt="List the crates under crates/codegen and summarize xai-grok-mods")`, then `send_message` to the running child, then wait for completion and send another message to wake it. Expected: `subagent_spawned`/`subagent_progress`/`subagent_finished` render in the TUI; the wake turn resumes the same Cursor agent (`~/.grok/mods/cursor/sessions/` record unchanged vendor id).

- [ ] **Step 7: Commit Task 6**

```bash
git add plugins/cursor/
git commit -m "feat(plugins): add cursor mod subagent plugin and verify upstream diff budget"
```
