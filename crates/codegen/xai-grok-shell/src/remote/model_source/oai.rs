//! Reads the model list from an OpenAI-compatible `/v1/models`.
use crate::agent::config::EndpointsConfig;
use crate::agent::remote_config::ModelFetchAuth;
use crate::remote::client::{
    BackendError, BakedCapRow, FetchModelsResult, baked_cap_rows, parse_model_group_info,
    parse_remote_model_value,
};
use crate::remote::model_source::ModelSource;
use serde::Deserialize;
use xai_grok_login::GrokAuth;
use xai_grok_login::backend::{ActiveAuthBackend, AuthBackend};

/// CATALOG-LIVEHYDRATE-1 (apex-8jo, R1b): the group-info request's strict
/// per-request timeout. Kept below the outer `STARTUP_FETCH_TIMEOUT` window
/// (asserted in tests), so a slow/dead group call can never sink a models
/// result that would have landed today.
pub(crate) const GROUP_INFO_FETCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// Derive the `/model_group/info` URL from the models-list URL — observed
/// shapes only (SDD §3.3): the group URL hangs off the same host base as the
/// list URL, with any `/v1` segment dropped:
///   `{base}/v1/models` → `{base}/model_group/info`
///   `{base}/models`    → `{base}/model_group/info`
/// A base carrying any other path segment (e.g. `/v2`) was never observed;
/// deriving from it would invent a shape, so it yields `None` (no call).
pub(crate) fn group_info_url(models_list_url: &str) -> Option<String> {
    let without_models = models_list_url.strip_suffix("/models")?;
    let base = without_models.strip_suffix("/v1").unwrap_or(without_models);
    let host_and_path = base.split_once("://")?.1;
    if host_and_path.contains('/') {
        return None;
    }
    Some(format!("{base}/model_group/info"))
}
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<serde_json::Value>,
}
/// Reads `/v1/models` from whichever host the endpoint config resolved to.
pub(crate) struct OaiModelSource {
    endpoint: ListModelsEndpoint,
    inference_base_url: String,
    /// CATALOG-LIVEHYDRATE-1 (apex-8jo, R2): the baked-cap table for the
    /// null/absent live-cap backfill at parse time. Built once per source.
    baked: indexmap::IndexMap<String, BakedCapRow>,
}
impl OaiModelSource {
    pub(crate) fn new(endpoints: &EndpointsConfig, fetch_auth: ModelFetchAuth) -> Self {
        Self {
            endpoint: ListModelsEndpoint::from_endpoints(endpoints, fetch_auth),
            inference_base_url: endpoints.resolve_inference_base_url(),
            baked: baked_cap_rows(),
        }
    }

    /// Apply this source's auth headers — byte-identical between the models
    /// call and the group-info call (R1: same credential, same origin).
    fn with_auth(
        &self,
        request: reqwest::blocking::RequestBuilder,
        auth: Option<&GrokAuth>,
    ) -> Result<reqwest::blocking::RequestBuilder, BackendError> {
        match self.endpoint.auth {
            EndpointAuth::ApiKey => {
                let api_key = crate::agent::auth_method::read_xai_api_key_env()
                    .or_else(|_| {
                        auth.map(|a| a.key.clone())
                            .ok_or(std::env::VarError::NotPresent)
                    })
                    .map_err(|_| {
                        BackendError::Auth(
                            "No API key for custom models endpoint. Set XAI_API_KEY.".into(),
                        )
                    })?;
                Ok(request.header("Authorization", format!("Bearer {}", api_key)))
            }
            EndpointAuth::Session => {
                let auth = auth
                    .filter(|_| ActiveAuthBackend::default().is_xai_authority())
                    .ok_or_else(|| {
                        BackendError::Auth("No auth credentials for cli-chat-proxy".into())
                    })?;
                let mut request = request
                    .header("Authorization", format!("Bearer {}", &auth.key))
                    .header("X-XAI-Token-Auth", "xai-grok-cli")
                    .header("x-userid", &auth.user_id)
                    .header("x-grok-client-version", xai_grok_version::VERSION)
                    .header(
                        crate::http::CLIENT_MODE_HEADER,
                        crate::http::process_client_mode(),
                    );
                if let Some(email) = &auth.email {
                    request = request.header("x-email", email);
                }
                Ok(request)
            }
        }
    }
}
impl ModelSource for OaiModelSource {
    fn cache_origin(&self) -> String {
        self.endpoint.url.clone()
    }
    fn fetch(&self, auth: Option<&GrokAuth>) -> Result<FetchModelsResult, BackendError> {
        let client = crate::http::shared_startup_blocking_client();
        tracing::info!("Fetching models from {}", self.endpoint.url);
        let request = self.with_auth(client.get(&self.endpoint.url), auth)?;
        let response = request.send()?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().unwrap_or_default();
            tracing::warn!("Failed to fetch models: {} - {}", status, body);
            return Err(BackendError::RequestFailed { status, body });
        }
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let models_response: ModelsResponse = response.json()?;
        tracing::info!(
            "Fetched {} models from {}",
            models_response.data.len(),
            self.endpoint.url
        );
        let mut models = Vec::with_capacity(models_response.data.len());
        for (idx, value) in models_response.data.into_iter().enumerate() {
            match parse_remote_model_value(&value, &self.inference_base_url, &self.baked) {
                Some(model) => models.push(model),
                None => {
                    tracing::warn!(
                        "Skipping model at index {}: missing required field ('model' or 'context_window') or invalid types",
                        idx
                    )
                }
            }
        }
        Ok(FetchModelsResult { models, etag })
    }

    fn fetch_model_group_info(
        &self,
        auth: Option<&GrokAuth>,
    ) -> Result<Option<indexmap::IndexMap<String, serde_json::Value>>, BackendError> {
        let Some(url) = group_info_url(&self.endpoint.url) else {
            // Unobserved list-URL shape: no call, no assumption (R1).
            return Ok(None);
        };
        let client = crate::http::shared_startup_blocking_client();
        let request = self.with_auth(client.get(&url), auth)?;
        let response = request.timeout(GROUP_INFO_FETCH_BUDGET).send()?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().unwrap_or_default();
            tracing::warn!("Failed to fetch model_group/info: {status} - {body}");
            return Err(BackendError::RequestFailed { status, body });
        }
        let value: serde_json::Value = response.json()?;
        match parse_model_group_info(&value) {
            Some(rows) => {
                tracing::info!(
                    "Fetched {} model_group/info records from {}",
                    rows.len(),
                    url
                );
                Ok(Some(rows))
            }
            None => {
                tracing::warn!("model_group/info body has an unobserved shape; degrading");
                Ok(None)
            }
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointAuth {
    ApiKey,
    Session,
}
struct ListModelsEndpoint {
    url: String,
    auth: EndpointAuth,
}
impl ListModelsEndpoint {
    fn from_endpoints(endpoints: &EndpointsConfig, fetch_auth: ModelFetchAuth) -> Self {
        if endpoints.has_custom_endpoint() {
            Self {
                url: endpoints.resolve_models_list_url(),
                auth: EndpointAuth::ApiKey,
            }
        } else if fetch_auth == ModelFetchAuth::ApiKey {
            Self {
                url: format!("{}/models", endpoints.xai_api_base_url),
                auth: EndpointAuth::ApiKey,
            }
        } else {
            Self {
                url: endpoints.resolve_models_list_url(),
                auth: EndpointAuth::Session,
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[serial_test::serial]
    fn models_fetch_endpoint_matches_auth_mode() {
        use crate::agent::config::EndpointsConfig;
        use crate::agent::config::CLI_CHAT_PROXY_BASE_URL_DEFAULT;
        use crate::agent::config::XAI_API_BASE_URL_DEFAULT;
        use crate::agent::remote_config::ModelFetchAuth;
        for k in [
            "GROK_CLI_CHAT_PROXY_BASE_URL",
            "GROK_XAI_API_BASE_URL",
            "GROK_MODELS_LIST_URL",
        ] {
            unsafe { std::env::remove_var(k) };
        }
        let cfg = EndpointsConfig::from_config_value(
            &toml::from_str(
                r#"[endpoints]
                    xai_api_base_url = "https://inference.acme-corp.example/xai/v1""#,
            )
            .unwrap(),
        );
        let session = ListModelsEndpoint::from_endpoints(&cfg, ModelFetchAuth::Session);
        assert_eq!(session.url, format!("{CLI_CHAT_PROXY_BASE_URL_DEFAULT}/models"));
        assert_eq!(session.auth, EndpointAuth::Session);
        let deployment = ListModelsEndpoint::from_endpoints(&cfg, ModelFetchAuth::Deployment);
        assert_eq!(deployment.url, format!("{CLI_CHAT_PROXY_BASE_URL_DEFAULT}/models"));
        assert_eq!(deployment.auth, EndpointAuth::Session);
        let api = ListModelsEndpoint::from_endpoints(&cfg, ModelFetchAuth::ApiKey);
        assert_eq!(api.url, "https://inference.acme-corp.example/xai/v1/models");
        assert_eq!(api.auth, EndpointAuth::ApiKey);
        let default = EndpointsConfig::from_config_value(&toml::Value::Table(Default::default()));
        assert_eq!(
            ListModelsEndpoint::from_endpoints(&default, ModelFetchAuth::ApiKey).url,
            format!("{XAI_API_BASE_URL_DEFAULT}/models")
        );
        let custom = EndpointsConfig::from_config_value(
            &toml::from_str(
                r#"[endpoints]
                    models_base_url = "https://models.acme.com/v1""#,
            )
            .unwrap(),
        );
        let ep = ListModelsEndpoint::from_endpoints(&custom, ModelFetchAuth::Session);
        assert_eq!(ep.url, "https://models.acme.com/v1/models");
        assert_eq!(ep.auth, EndpointAuth::ApiKey);
    }
}
