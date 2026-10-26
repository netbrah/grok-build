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

    fn write_manifest(root: &Path, body: &str, stamp: i64) {
        let path = root.join("mod.json");
        std::fs::write(&path, body).unwrap();
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(1_700_000_000 + stamp, 0))
            .unwrap();
    }

    #[test]
    fn t1_manifest_validates_namespace_and_containment() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("sidecar")).unwrap();
        std::fs::write(root.join("sidecar/runner.mjs"), "// ok\n").unwrap();

        // Valid manifest
        write_manifest(
            &root,
            r#"{"schemaVersion":1,"namespace":"cursor","runtime":{"command":"node","args":["sidecar/runner.mjs"]}}"#,
            1,
        );
        let v = ModConfig::load("cursor", &root).expect("valid mod.json");
        assert_eq!(v.config.namespace, "cursor");
        assert_eq!(v.args.len(), 1);

        // Namespace mismatch rejected
        assert!(matches!(
            ModConfig::load("other", &root),
            Err(ModError::NamespaceMismatch { .. })
        ));

        // Disallowed bare command rejected
        write_manifest(
            &root,
            r#"{"schemaVersion":1,"namespace":"cursor","runtime":{"command":"bash","args":[]}}"#,
            2,
        );
        assert!(matches!(
            ModConfig::load("cursor", &root),
            Err(ModError::DisallowedCommand(_))
        ));

        // Path traversal in args rejected
        write_manifest(
            &root,
            r#"{"schemaVersion":1,"namespace":"cursor","runtime":{"command":"node","args":["../escape.mjs"]}}"#,
            3,
        );
        assert!(matches!(
            ModConfig::load("cursor", &root),
            Err(ModError::PathEscape(_))
        ));

        // Unknown field rejected by deny_unknown_fields
        write_manifest(
            &root,
            r#"{"schemaVersion":1,"namespace":"cursor","unknownField":true,"runtime":{"command":"node","args":[]}}"#,
            4,
        );
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
            write_manifest(
                &root,
                r#"{"schemaVersion":1,"namespace":"cursor","runtime":{"command":"node","args":["sidecar/escape_link.mjs"]}}"#,
                5,
            );
            assert!(matches!(
                ModConfig::load("cursor", &root),
                Err(ModError::PathEscape(_))
            ));
        }
    }
}
