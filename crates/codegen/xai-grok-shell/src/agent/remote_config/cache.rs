//! Unsigned on-disk model-catalog cache.

use chrono::{DateTime, Utc};
use indexmap::IndexMap;

use super::cache_file::{CacheLoadError, is_fresh, read_capped, write_atomic};
use super::{CacheAuthMethod, ModelsCacheScope};
use crate::agent::config::ModelEntry;

pub(crate) const MODELS_CACHE_FILE: &str = "models_cache.json";
pub(crate) const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);
/// Cap on the unsigned models cache read, mirroring the settings cache: a
/// corrupt or oversized file is a miss, not an unbounded read into memory.
const MODELS_CACHE_MAX_BYTES: u64 = 4 << 20;

/// Serializes every read-check-write of the cache file so a concurrent startup
/// commit and TTL renewal cannot both pass the monotonic check and let the
/// older fetch win.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ModelsCache {
    /// Fetch-initiation time. The monotonic content key: an older fetch never
    /// replaces a newer stored catalog.
    pub(crate) fetched_at: DateTime<Utc>,
    /// TTL/freshness clock, bumped by a TTL renewal. Kept separate from
    /// `fetched_at` so renewing the TTL cannot shadow a newer-content write.
    /// Absent means freshness is measured from `fetched_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) renewed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) grok_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) auth_method: Option<CacheAuthMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) origin: Option<String>,
    /// Per-account-and-alpha scope, like the settings cache: a different user or
    /// alpha cohort must miss. A legacy `None` entry misses and refetches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) etag: Option<String>,
    pub(crate) models: IndexMap<String, ModelEntry>,
    /// CATALOG-LIVEHYDRATE-1 (apex-8jo): the observed `/model_group/info`
    /// section — verbatim records keyed by `model_group`. Absent on disk when
    /// empty, so a degraded fetch serializes a byte-identical pre-cut file
    /// (R1c). Old binaries ignore the key (no deny_unknown_fields on this
    /// read path); new binaries read absent as empty.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub(crate) model_groups: IndexMap<String, serde_json::Value>,
}

pub(in crate::agent::remote_config) struct CacheResult {
    pub(crate) models: IndexMap<String, ModelEntry>,
    pub(crate) etag: Option<String>,
    pub(crate) model_groups: IndexMap<String, serde_json::Value>,
}

pub(crate) struct ModelsCacheManager {
    pub(crate) path: std::path::PathBuf,
    pub(crate) ttl: std::time::Duration,
}

impl ModelsCacheManager {
    pub(crate) fn new() -> Self {
        Self {
            path: crate::util::grok_home::grok_home().join(MODELS_CACHE_FILE),
            ttl: CACHE_TTL,
        }
    }

    pub(in crate::agent::remote_config) fn load_fresh(
        &self,
        scope: &ModelsCacheScope,
    ) -> Option<CacheResult> {
        match self.try_load_fresh(scope) {
            Ok(hit) => Some(hit),
            Err(status) => {
                status.log(&self.path);
                None
            }
        }
    }

    fn try_load_fresh(&self, scope: &ModelsCacheScope) -> Result<CacheResult, CacheLoadError> {
        let data =
            read_capped(&self.path, MODELS_CACHE_MAX_BYTES).ok_or(CacheLoadError::NotFound)?;
        let cache: ModelsCache =
            serde_json::from_slice(&data).map_err(|_| CacheLoadError::ParseFailed)?;
        if cache.grok_version.as_deref() != Some(xai_grok_version::VERSION) {
            return Err(CacheLoadError::VersionMismatch);
        }
        if cache.auth_method.as_ref() != Some(&scope.auth_method) {
            return Err(CacheLoadError::ScopeMismatch("auth method"));
        }
        if cache.origin.as_deref() != Some(scope.origin.as_str()) {
            return Err(CacheLoadError::ScopeMismatch("origin"));
        }
        if cache.identity.as_deref() != Some(scope.identity.as_str()) {
            return Err(CacheLoadError::ScopeMismatch("identity"));
        }
        if !is_fresh(cache.renewed_at.unwrap_or(cache.fetched_at), self.ttl) {
            return Err(CacheLoadError::Stale);
        }
        tracing::debug!(count = cache.models.len(), "loaded models from disk cache");
        Ok(CacheResult {
            model_groups: cache.model_groups,
            models: cache.models,
            etag: cache.etag,
        })
    }

    /// `fetched_at` is the fetch-initiation time. Monotonic: never replace a
    /// newer same-scope catalog with an older fetch, so a slow load cannot roll
    /// the cache back.
    pub(crate) fn persist(
        &self,
        models: &IndexMap<String, ModelEntry>,
        model_groups: &IndexMap<String, serde_json::Value>,
        etag: Option<&str>,
        scope: &ModelsCacheScope,
        fetched_at: DateTime<Utc>,
    ) {
        let _guard = WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if self
            .disk_fetched_at(scope)
            .is_some_and(|disk| disk > fetched_at)
        {
            tracing::debug!("models cache commit skipped: a newer fetch is already stored");
            return;
        }
        let cache = ModelsCache {
            fetched_at,
            renewed_at: None,
            grok_version: Some(xai_grok_version::VERSION.to_string()),
            auth_method: Some(scope.auth_method.clone()),
            origin: Some(scope.origin.clone()),
            identity: Some(scope.identity.clone()),
            etag: etag.map(|s| s.to_string()),
            models: models.clone(),
            model_groups: model_groups.clone(),
        };
        self.atomic_write(&cache);
    }

    /// The on-disk fetch time for a matching scope; `None` when absent,
    /// unparseable, a different scope, or future-dated (a corrupt file or clock
    /// rollback must not pin the cache).
    fn disk_fetched_at(&self, scope: &ModelsCacheScope) -> Option<DateTime<Utc>> {
        let data = read_capped(&self.path, MODELS_CACHE_MAX_BYTES)?;
        let cache: ModelsCache = serde_json::from_slice(&data).ok()?;
        if cache.auth_method.as_ref() != Some(&scope.auth_method)
            || cache.origin.as_deref() != Some(scope.origin.as_str())
            || cache.identity.as_deref() != Some(scope.identity.as_str())
        {
            return None;
        }
        (cache.fetched_at <= Utc::now()).then_some(cache.fetched_at)
    }

    /// Bump `renewed_at` forward when the on-disk catalog is unchanged, extending
    /// its TTL without touching the `fetched_at` content key. The read-modify-write
    /// holds `WRITE_LOCK` and re-reads inside it, so a concurrent in-process commit
    /// is never clobbered. `WRITE_LOCK` is process-local, so a cross-process commit
    /// can still race; that self-heals on the next etag-driven refresh.
    pub(crate) async fn renew_ttl(&self, scope: &ModelsCacheScope) {
        let path = self.path.clone();
        let ttl = self.ttl;
        let scope = scope.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _guard = WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let Some(data) = read_capped(&path, MODELS_CACHE_MAX_BYTES) else {
                return;
            };
            let Ok(mut cache) = serde_json::from_slice::<ModelsCache>(&data) else {
                return;
            };
            if cache.auth_method.as_ref() != Some(&scope.auth_method)
                || cache.origin.as_deref() != Some(scope.origin.as_str())
                || cache.identity.as_deref() != Some(scope.identity.as_str())
            {
                tracing::debug!("models cache TTL renewal skipped: scope mismatch");
                return;
            }
            cache.renewed_at = Some(Utc::now());
            ModelsCacheManager { path, ttl }.atomic_write(&cache);
            tracing::debug!("models cache TTL renewed");
        })
        .await;
    }

    pub(crate) fn invalidate(&self) {
        let _guard = WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        match std::fs::remove_file(&self.path) {
            Ok(()) => tracing::info!("models disk cache invalidated"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(error = %e, "failed to invalidate models disk cache"),
        }
    }

    pub(crate) fn atomic_write(&self, cache: &ModelsCache) {
        let Ok(json) = serde_json::to_vec_pretty(cache) else {
            return;
        };
        write_atomic(&self.path, self.ttl, &json, false);
    }
}

#[cfg(test)]
mod tests {
    //! CATALOG-LIVEHYDRATE-1 (apex-8jo): the second section's cache
    //! discipline — TTL rides together, the monotonic guard protects it,
    //! renew_ttl preserves it, and the file format stays backward
    //! compatible in the new-binary direction (old files parse; a
    //! degraded write is section-free).

    use super::*;
    use crate::remote::client::parse_remote_model_value;

    fn test_cache(path: &std::path::Path) -> ModelsCacheManager {
        ModelsCacheManager {
            path: path.join(MODELS_CACHE_FILE),
            ttl: CACHE_TTL,
        }
    }

    fn scope() -> ModelsCacheScope {
        ModelsCacheScope {
            auth_method: CacheAuthMethod::ApiKey,
            origin: "https://o.example/v1/models".to_string(),
            identity: "id-1".to_string(),
        }
    }

    fn entry(id: &str) -> crate::agent::config::ModelEntry {
        let cfg = parse_remote_model_value(
            &serde_json::json!({
                "id": id,
                "object": "model",
                "max_input_tokens": 200_000,
                "max_output_tokens": 64_000,
            }),
            "https://default.url",
            &indexmap::IndexMap::new(),
        )
        .unwrap();
        crate::agent::config::ModelEntry::from_config_entry(&cfg)
    }

    fn models(ids: &[&str]) -> indexmap::IndexMap<String, crate::agent::config::ModelEntry> {
        ids.iter().map(|id| (id.to_string(), entry(id))).collect()
    }

    fn section() -> indexmap::IndexMap<String, serde_json::Value> {
        [
            (
                "claude-opus-4.8".to_string(),
                serde_json::json!({"model_group": "claude-opus-4.8", "tpm": null}),
            ),
            (
                "claude-opus-4-8".to_string(),
                serde_json::json!({"model_group": "claude-opus-4-8", "tpm": 128}),
            ),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn new_binary_reads_old_cache_file() {
        // A pre-cut file (no section key at all) must load cleanly:
        // `#[serde(default)]` makes the section an optional empty table.
        let tmp = tempfile::TempDir::new().unwrap();
        let cache = test_cache(tmp.path());
        let cache_struct = ModelsCache {
            fetched_at: Utc::now(),
            renewed_at: None,
            grok_version: Some(xai_grok_version::VERSION.to_string()),
            auth_method: Some(scope().auth_method.clone()),
            origin: Some(scope().origin.clone()),
            identity: Some(scope().identity.clone()),
            etag: Some("etag-old".to_string()),
            models: models(&["grok-4.6"]),
            model_groups: indexmap::IndexMap::new(),
        };
        cache.atomic_write(&cache_struct);
        let raw = std::fs::read_to_string(&cache.path).unwrap();
        assert!(
            !raw.contains("model_groups"),
            "an empty section must not appear on disk (pre-cut file shape)"
        );
        let hit = cache
            .load_fresh(&scope())
            .expect("an old-format file must load on the new binary");
        assert!(hit.models.contains_key("grok-4.6"));
        assert!(hit.model_groups.is_empty(), "absent section = empty table");
    }

    #[test]
    fn persist_empty_groups_omits_section() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache = test_cache(tmp.path());
        cache.persist(
            &models(&["grok-4.6"]),
            &indexmap::IndexMap::new(),
            Some("etag-1"),
            &scope(),
            Utc::now(),
        );
        let raw = std::fs::read_to_string(&cache.path).unwrap();
        assert!(
            !raw.contains("model_groups"),
            "a degraded fetch (empty section) must serialize a pre-cut-shaped file"
        );
    }

    #[test]
    fn cached_section_serves_on_fresh_load() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache = test_cache(tmp.path());
        cache.persist(
            &models(&["grok-4.6"]),
            &section(),
            Some("etag-1"),
            &scope(),
            Utc::now(),
        );
        let hit = cache.load_fresh(&scope()).expect("fresh cache");
        assert_eq!(
            hit.model_groups,
            section(),
            "the section must ride the fresh load with the models"
        );
        assert!(hit.model_groups["claude-opus-4.8"]["tpm"].is_null());
    }

    #[tokio::test]
    async fn renew_ttl_preserves_group_section() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache = test_cache(tmp.path());
        let fetched_at = Utc::now();
        cache.persist(
            &models(&["grok-4.6"]),
            &section(),
            Some("etag-1"),
            &scope(),
            fetched_at,
        );
        cache.renew_ttl(&scope()).await;
        let raw = std::fs::read_to_string(&cache.path).unwrap();
        let reread: ModelsCache = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            reread.model_groups,
            section(),
            "the TTL bump must rewrite the file WITH the section intact"
        );
        assert_eq!(
            reread.fetched_at, fetched_at,
            "the content key must not move on a TTL renewal"
        );
        assert!(reread.renewed_at.is_some(), "the TTL must actually bump");
    }

    #[test]
    fn stale_cache_with_section_is_a_miss() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache = test_cache(tmp.path());
        cache.persist(
            &models(&["grok-4.6"]),
            &section(),
            Some("etag-1"),
            &scope(),
            Utc::now() - chrono::Duration::seconds(CACHE_TTL.as_secs() as i64 + 60),
        );
        assert!(
            cache.load_fresh(&scope()).is_none(),
            "staleness is per-file: a stale section must miss like a stale catalog"
        );
    }

    #[test]
    fn monotonic_guard_protects_newer_write() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache = test_cache(tmp.path());
        cache.persist(
            &models(&["grok-new"]),
            &section(),
            Some("etag-new"),
            &scope(),
            Utc::now(),
        );
        // An older fetch (no section) must not roll the file back.
        cache.persist(
            &models(&["grok-old"]),
            &indexmap::IndexMap::new(),
            Some("etag-old"),
            &scope(),
            Utc::now() - chrono::Duration::seconds(120),
        );
        let hit = cache.load_fresh(&scope()).expect("the newer write must stand");
        assert!(hit.models.contains_key("grok-new"));
        assert_eq!(
            hit.model_groups,
            section(),
            "the section of the NEWER fetch must survive the older write attempt"
        );
    }
}
