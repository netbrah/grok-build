//! HTTP binding for the `/v1/models` catalog endpoint.

use std::future::Future;
use std::pin::Pin;

use super::{ModelFetchAuth, prefetch_models_blocking};
use crate::agent::config;
use xai_grok_login::GrokAuth;

use super::fetch::ModelsFetchOutcome;

/// Boxed future returned by [`ModelsEndpoint::fetch_models`].
pub(crate) type ModelsFetchFuture =
    Pin<Box<dyn Future<Output = Option<ModelsFetchOutcome>> + Send>>;

/// The `/v1/models` fetch behind a trait so tests can inject a fake.
pub(crate) trait ModelsEndpoint: Send + Sync {
    fn fetch_models(
        &self,
        endpoints: config::EndpointsConfig,
        auth: Option<GrokAuth>,
        fetch_auth: ModelFetchAuth,
    ) -> ModelsFetchFuture;
}

/// The default implementation: the real `/v1/models` fetch.
pub(crate) struct HttpModelsEndpoint;

impl ModelsEndpoint for HttpModelsEndpoint {
    fn fetch_models(
        &self,
        endpoints: config::EndpointsConfig,
        auth: Option<GrokAuth>,
        fetch_auth: ModelFetchAuth,
    ) -> ModelsFetchFuture {
        Box::pin(fetch_models_async(endpoints, auth, fetch_auth))
    }
}

pub(crate) async fn fetch_models_async(
    endpoints: config::EndpointsConfig,
    auth: Option<GrokAuth>,
    fetch_auth: ModelFetchAuth,
) -> Option<ModelsFetchOutcome> {
    tokio::task::spawn_blocking(move || {
        prefetch_models_blocking(&endpoints, auth.as_ref(), fetch_auth)
    })
    .await
    .unwrap_or(None)
}
