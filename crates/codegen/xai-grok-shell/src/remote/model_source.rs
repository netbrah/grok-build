//! Picks the URL and auth used to fetch the model list.
use crate::agent::config::EndpointsConfig;
use crate::agent::remote_config::ModelFetchAuth;
use crate::remote::client::{BackendError, FetchModelsResult};
use xai_grok_login::GrokAuth;
pub(crate) mod oai;

pub(crate) trait ModelSource: Send + Sync {
    /// Identifies this source in the models disk cache, so entries fetched from one URL never load for another.
    fn cache_origin(&self) -> String;
    fn fetch(&self, auth: Option<&GrokAuth>) -> Result<FetchModelsResult, BackendError>;
    /// CATALOG-LIVEHYDRATE-1 (apex-8jo): the batch `/model_group/info`
    /// observation, parsed to the observed-shape section. Default `Ok(None)`:
    /// non-OAI sources (and test fakes) stay section-free; the composition
    /// site degrades every failure class to exactly today's behavior (R1).
    fn fetch_model_group_info(
        &self,
        auth: Option<&GrokAuth>,
    ) -> Result<Option<indexmap::IndexMap<String, serde_json::Value>>, BackendError> {
        Ok(None)
    }
}
pub(crate) fn active_model_source(
    endpoints: &EndpointsConfig,
    fetch_auth: ModelFetchAuth,
) -> impl ModelSource {
    oai::OaiModelSource::new(endpoints, fetch_auth)
}
