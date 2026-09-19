//! Model-catalog fetch transport.

use chrono::{DateTime, Utc};
use indexmap::IndexMap;

use super::{ModelFetchAuth, ModelsCacheManager, ModelsCacheScope};
use crate::agent::config::{self, ModelEntry};
use crate::remote::{FetchModelsResult, ModelSource, active_model_source};
use xai_grok_login::GrokAuth;

/// CATALOG-LIVEHYDRATE-1 (apex-8jo): one catalog fetch's boundary payload —
/// the live models section plus the observed `/model_group/info` section.
/// The outer `Option` (pipeline result) stays on the callers.
/// CATALOG-LIVEHYDRATE-1 (apex-8jo): `pub` because the `bootstrap` /
/// `bootstrap_with_cancel` signatures (pub, agent/init.rs) name this type —
/// external consumers (xai-grok-pager, the shell integration-test targets)
/// must be able to resolve it even when they pass `None` (E0446/private-type
/// at the call site). Fields stay crate-private: only the shell builds or
/// reads the outcome.
#[derive(Debug)]
pub struct ModelsFetchOutcome {
    pub(crate) models: IndexMap<String, ModelEntry>,
    pub(crate) model_groups: IndexMap<String, serde_json::Value>,
}

pub(crate) fn build_prefetched_map(
    models: Vec<config::ModelEntryConfig>,
    api_base_url_override: Option<String>,
) -> IndexMap<String, ModelEntry> {
    let mut map: IndexMap<String, ModelEntry> = IndexMap::with_capacity(models.len());
    for m in models {
        let key = m.id.clone().unwrap_or_else(|| m.model.clone());
        let info = config::ModelInfo::from_config(&m);
        let entry = ModelEntry {
            info,
            mtls_cert_dir: None,
            api_key: None,
            env_key: None,
            auth_provider: None,
            api_base_url: m.api_base_url.clone().or(api_base_url_override.clone()),
        };
        map.insert(key, entry);
    }
    map
}

pub(crate) fn prefetch_models_blocking(
    endpoints: &config::EndpointsConfig,
    auth: Option<&GrokAuth>,
    fetch_auth: ModelFetchAuth,
) -> Option<ModelsFetchOutcome> {
    prefetch_models_blocking_gated(
        endpoints,
        auth,
        fetch_auth,
        crate::util::config::resolve_remote_fetch_enabled(),
    )
}

fn prefetch_models_blocking_gated(
    endpoints: &config::EndpointsConfig,
    auth: Option<&GrokAuth>,
    fetch_auth: ModelFetchAuth,
    remote_fetch_enabled: bool,
) -> Option<ModelsFetchOutcome> {
    fetch_models_uncommitted(endpoints, auth, fetch_auth, remote_fetch_enabled).commit()
}

/// A models fetch not yet written to the disk cache; the commit point decides
/// whether any state lands.
pub(in crate::agent::remote_config) enum ModelsPrefetch {
    Cached {
        models: IndexMap<String, ModelEntry>,
        model_groups: IndexMap<String, serde_json::Value>,
    },
    Fetched(ModelsCacheWrite),
    Unavailable,
}

impl ModelsPrefetch {
    fn commit(self) -> Option<ModelsFetchOutcome> {
        match self {
            Self::Cached {
                models,
                model_groups,
            } => Some(ModelsFetchOutcome {
                models,
                model_groups,
            }),
            Self::Fetched(write) => Some(write.commit()),
            Self::Unavailable => None,
        }
    }
}

pub(in crate::agent::remote_config) struct ModelsCacheWrite {
    models: IndexMap<String, ModelEntry>,
    model_groups: IndexMap<String, serde_json::Value>,
    etag: Option<String>,
    scope: ModelsCacheScope,
    fetched_at: DateTime<Utc>,
}

impl ModelsCacheWrite {
    pub(in crate::agent::remote_config) fn commit(self) -> ModelsFetchOutcome {
        ModelsCacheManager::new().persist(
            &self.models,
            &self.model_groups,
            self.etag.as_deref(),
            &self.scope,
            self.fetched_at,
        );
        ModelsFetchOutcome {
            models: self.models,
            model_groups: self.model_groups,
        }
    }

    /// The fetched catalog without persisting it: serve a live session not yet
    /// on disk without writing into the auth-method/origin-scoped cache.
    pub(in crate::agent::remote_config) fn into_outcome(self) -> ModelsFetchOutcome {
        ModelsFetchOutcome {
            models: self.models,
            model_groups: self.model_groups,
        }
    }
}

pub(in crate::agent::remote_config) fn fetch_models_uncommitted(
    endpoints: &config::EndpointsConfig,
    auth: Option<&GrokAuth>,
    fetch_auth: ModelFetchAuth,
    remote_fetch_enabled: bool,
) -> ModelsPrefetch {
    let source = active_model_source(endpoints, fetch_auth);
    fetch_models_uncommitted_source(&source, endpoints, auth, fetch_auth, remote_fetch_enabled)
}

/// CATALOG-LIVEHYDRATE-1 (apex-8jo, R1): the composition seam — a
/// `&dyn ModelSource` instead of the endpoint-resolved one, so the group
/// splice is unit-testable without HTTP.
///
/// The group observation runs CONCURRENTLY with the models fetch (own OS
/// thread; the fetch already runs on a blocking thread with no tokio
/// context), single attempt, budgeted below the outer models window. Every
/// failure class degrades to exactly today's result: the models map is
/// computed from the models response alone and is byte-identical no matter
/// what the group call does (R1a).
pub(in crate::agent::remote_config) fn fetch_models_uncommitted_source(
    source: &dyn ModelSource,
    endpoints: &config::EndpointsConfig,
    auth: Option<&GrokAuth>,
    fetch_auth: ModelFetchAuth,
    remote_fetch_enabled: bool,
) -> ModelsPrefetch {
    let scope = ModelsCacheScope::resolve(endpoints, fetch_auth, auth);

    let cache = ModelsCacheManager::new();
    if let Some(cached) = cache.load_fresh(&scope) {
        return ModelsPrefetch::Cached {
            models: cached.models,
            model_groups: cached.model_groups,
        };
    }

    if !remote_fetch_enabled {
        tracing::info!("models fetch skipped: remote_fetch disabled");
        return ModelsPrefetch::Unavailable;
    }

    let _timer = crate::instrumentation_timer!("startup.fetch_models_blocking");
    let fetched_at = Utc::now();
    // `std::thread::scope`: the group thread borrows `source`/`auth` from
    // this frame (an unscoped spawn would require 'static) and is always
    // joined before the scope ends — a late group result can never outlive
    // the fetch (R1: single attempt, no orphaned request beyond the budget).
    std::thread::scope(|s| {
        let group_handle = s.spawn(move || source.fetch_model_group_info(auth));
        match source.fetch(auth) {
            Ok(FetchModelsResult { models, etag }) if !models.is_empty() => {
                let model_groups = match group_handle.join() {
                    Ok(Ok(rows)) => rows.unwrap_or_default(),
                    Ok(Err(e)) => {
                        tracing::warn!(
                            error = ?e,
                            "model_group/info fetch failed; models catalog applies as today"
                        );
                        IndexMap::new()
                    }
                    Err(_) => {
                        tracing::warn!("model_group/info thread panicked; models catalog applies as today");
                        IndexMap::new()
                    }
                };
                let api_base_url_override = match fetch_auth {
                    ModelFetchAuth::ApiKey => Some(endpoints.xai_api_base_url.clone()),
                    _ => None,
                };
                let map = build_prefetched_map(models, api_base_url_override);

                tracing::info!(
                    count = map.len(),
                    group_count = model_groups.len(),
                    etag = ?etag,
                    "Prefetched models"
                );
                ModelsPrefetch::Fetched(ModelsCacheWrite {
                    models: map,
                    model_groups,
                    etag,
                    scope,
                    fetched_at,
                })
            }
            Ok(FetchModelsResult { .. }) => {
                tracing::warn!("Models endpoint returned empty list");
                ModelsPrefetch::Unavailable
            }
            Err(e) => {
                tracing::warn!(error = ?e, "Failed to fetch models");
                ModelsPrefetch::Unavailable
            }
        }
    })
}

#[cfg(test)]
mod tests {
    //! CATALOG-LIVEHYDRATE-1 (apex-8jo, R1): the group splice is
    //! additive-only — every group-fetch failure class degrades to exactly
    //! today's composed result, the group call is a single attempt (no
    //! retry storm), runs concurrent with the models fetch, and the models
    //! result is byte-identical no matter what the group call does.

    use super::*;
    use crate::agent::config::EndpointsConfig;
    use crate::remote::client::{BackendError, parse_remote_model_value};
    use crate::remote::model_source::ModelSource;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// A source with scripted models/group outcomes and concurrency probes.
    struct FakeSource {
        // Scripted once per field: `BackendError` is not `Clone`, so each
        // outcome is taken (not cloned) by the single call that consumes it.
        models: Mutex<Option<Result<crate::remote::client::FetchModelsResult, BackendError>>>,
        groups: Mutex<
            Option<Result<Option<indexmap::IndexMap<String, serde_json::Value>>, BackendError>>,
        >,
        group_calls: Arc<AtomicUsize>,
        models_start: Arc<AtomicBool>,
        group_started_before_models_done: Arc<AtomicBool>,
    }

    fn model_config(id: &str) -> crate::agent::config::ModelEntryConfig {
        parse_remote_model_value(
            &serde_json::json!({
                "id": id,
                "object": "model",
                "max_input_tokens": 200_000,
                "max_output_tokens": 64_000,
            }),
            "https://default.url",
            &indexmap::IndexMap::new(),
        )
        .unwrap()
    }

    impl ModelSource for FakeSource {
        fn cache_origin(&self) -> String {
            "https://fake-origin.example/v1/models".to_string()
        }

        fn fetch(&self, _auth: Option<&GrokAuth>) -> Result<crate::remote::client::FetchModelsResult, BackendError> {
            self.models_start.store(true, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(300));
            // Concurrency probe: was the group call already started while
            // the models fetch was still in flight?
            if self
                .group_calls
                .load(Ordering::SeqCst)
                > 0
            {
                self.group_started_before_models_done
                    .store(true, Ordering::SeqCst);
            }
            self.models
                .lock()
                .unwrap()
                .take()
                .expect("scripted models outcome consumed exactly once")
        }

        fn fetch_model_group_info(
            &self,
            _auth: Option<&GrokAuth>,
        ) -> Result<
            Option<indexmap::IndexMap<String, serde_json::Value>>,
            BackendError,
        > {
            self.group_calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(300));
            self.groups
                .lock()
                .unwrap()
                .take()
                .expect("scripted group outcome consumed exactly once")
        }
    }

    fn fake_source(
        groups: Result<Option<indexmap::IndexMap<String, serde_json::Value>>, BackendError>,
    ) -> Arc<FakeSource> {
        let models = crate::remote::client::FetchModelsResult {
            models: vec![model_config("m-1"), model_config("m-2")],
            etag: Some("etag-m".to_string()),
        };
        Arc::new(FakeSource {
            models: Mutex::new(Some(Ok(models))),
            groups: Mutex::new(Some(groups)),
            group_calls: Arc::new(AtomicUsize::new(0)),
            models_start: Arc::new(AtomicBool::new(false)),
            group_started_before_models_done: Arc::new(AtomicBool::new(false)),
        })
    }

    fn default_endpoints() -> EndpointsConfig {
        EndpointsConfig::from_config_value(&toml::Value::Table(Default::default()))
    }

    fn compose(
        source: &Arc<FakeSource>,
    ) -> indexmap::IndexMap<String, crate::agent::config::ModelEntry> {
        match fetch_models_uncommitted_source(
            source.as_ref(),
            &default_endpoints(),
            None,
            ModelFetchAuth::ApiKey,
            true,
        ) {
            ModelsPrefetch::Fetched(write) => {
                // No commit here: the real grok_home must stay untouched.
                write.models
            }
            other => panic!("expected a fetched write (got a non-Fetched variant)"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn group_fetch_failure_classes_degrade_to_today() {
        let net_fail = BackendError::Auth("network class (dns/timeout)".into());
        let non200 = BackendError::RequestFailed {
            status: 500,
            body: "internal error".into(),
        };
        let malformed =
            BackendError::Serialization(serde_json::from_str::<serde_json::Value>("nope").unwrap_err());
        let absent = Ok(None);
        for (label, groups) in [
            ("network/timeout class", Err(net_fail)),
            ("non-200 class", Err(non200)),
            ("malformed JSON class", Err(malformed)),
            ("absent endpoint class", absent),
        ] {
            let source = fake_source(groups);
            let models = compose(&source);
            assert_eq!(
                models.len(),
                2,
                "{label}: the models catalog must apply exactly as today"
            );
            assert!(
                models.contains_key("m-1") && models.contains_key("m-2"),
                "{label}: every live row must survive"
            );
            assert_eq!(
                source.group_calls.load(Ordering::SeqCst),
                1,
                "{label}: exactly one group attempt — no retry storm"
            );
        }
        // Byte-identical models result across every group outcome.
        let baseline = compose(&fake_source(Ok(None)));
        let with_groups = {
            let rows: indexmap::IndexMap<String, serde_json::Value> =
                [("g-1".to_string(), serde_json::json!({"model_group": "g-1"}))]
                    .into_iter()
                    .collect();
            compose(&fake_source(Ok(Some(rows))))
        };
        let err_models = compose(&fake_source(Err(BackendError::RequestFailed {
            status: 404,
            body: "nope".into(),
        })));
        let baseline_json = serde_json::to_string(&baseline).unwrap();
        assert_eq!(
            serde_json::to_string(&with_groups).unwrap(),
            baseline_json,
            "a successful group section must not alter the models catalog"
        );
        assert_eq!(
            serde_json::to_string(&err_models).unwrap(),
            baseline_json,
            "a failed group call must not alter the models catalog"
        );
    }

    #[test]
    #[serial_test::serial]
    fn group_fetch_success_lands_section() {
        let rows: indexmap::IndexMap<String, serde_json::Value> = [
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
        .collect();
        let source = fake_source(Ok(Some(rows.clone())));
        match fetch_models_uncommitted_source(
            source.as_ref(),
            &default_endpoints(),
            None,
            ModelFetchAuth::ApiKey,
            true,
        ) {
            ModelsPrefetch::Fetched(write) => {
                assert_eq!(
                    write.model_groups, rows,
                    "the observed section must ride the cache write verbatim"
                );
                assert_eq!(write.etag.as_deref(), Some("etag-m"));
                assert_eq!(write.models.len(), 2);
            }
            other => panic!("expected a fetched write (got a non-Fetched variant)"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn group_fetch_runs_concurrent_with_models() {
        // Both sides sleep 300ms. Concurrency: the group call must start
        // while the models fetch is still in flight (probe, no wall-clock
        // margin), and the total must stay below the serial sum by a wide
        // (3x) margin.
        let source = fake_source(Ok(None));
        let started = std::time::Instant::now();
        let models = compose(&source);
        let elapsed = started.elapsed();
        assert_eq!(models.len(), 2);
        assert!(
            source.models_start.load(Ordering::SeqCst),
            "the models fetch ran"
        );
        assert!(
            source
                .group_started_before_models_done
                .load(Ordering::SeqCst),
            "the group call must run CONCURRENTLY with the models fetch (not after it)"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(900),
            "concurrent composition took {elapsed:?} — must beat the 600ms serial sum comfortably"
        );
        // R1b budget: the group request's strict timeout must stay inside
        // the outer models-fetch window.
        assert!(
            crate::remote::model_source::oai::GROUP_INFO_FETCH_BUDGET
                < crate::http::STARTUP_FETCH_TIMEOUT,
            "the group budget must be strictly smaller than the outer window"
        );
    }
}
