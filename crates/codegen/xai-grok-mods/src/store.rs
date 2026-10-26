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
