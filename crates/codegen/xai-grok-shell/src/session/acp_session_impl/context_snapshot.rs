//! Counts the tokens each context category occupies when a session starts.
//!
//! Token counts come from `POST {xai_api_base_url}/tokenize-text` (the model's tokenizer), not the bytes/4 `/context` heuristic.
//! Category texts are sent in parallel; the session-metrics event only receives the resulting counts.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BoundProjectionSurface {
    Main,
    RebuiltChild,
    Fork,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LastBoundProjectionSummary {
    pub(super) model: String,
    pub(super) backend: xai_grok_sampling_types::ApiBackend,
    pub(super) admission: Option<xai_grok_sampling_types::SearchAdmission>,
    pub(super) surface: BoundProjectionSurface,
    pub(super) selected_functions: usize,
    pub(super) removed_functions: usize,
    pub(super) retained_functions: usize,
    pub(super) declaration_view: String,
    pub(super) declaration_view_truncated: bool,
    pub(super) vendor_entries: Option<usize>,
    pub(super) system_bytes: usize,
    pub(super) history_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LastBoundProjectionState {
    NotCaptured,
    LastBound(LastBoundProjectionSummary),
    StaleCapturedRow(LastBoundProjectionSummary),
}

#[derive(Clone, Default)]
pub(super) struct LastBoundProjectionSlot(
    pub(super) std::sync::Arc<parking_lot::Mutex<Option<LastBoundProjectionSummary>>>,
);

impl SessionActor {
    /// Record itemized context occupancy for this session.
    /// No-op when session metrics are disabled or there is no credential for the tokenizer endpoint.
    pub(super) async fn emit_session_context_snapshot(&self) {
        if !self.telemetry_enabled || !xai_grok_telemetry::is_session_metrics_enabled() {
            return;
        }
        let Some(api_key) = self.tokenize_api_key().await else {
            tracing::debug!("session_context_snapshot: no api key");
            return;
        };
        let info = self.build_session_info().await;
        // `/tokenize-text` is the xAI tokenizer
        // Always use the baked product default (grok-4.6 from `default_models.json`), not the session model
        // The session model may be a third-party id the endpoint does not serve
        let model = crate::models::default_model();
        tracing::info!(model, "session_context_snapshot: tokenizing");
        let texts = self.snapshot_texts().await;
        let counts = texts.item_counts();
        let tokens = tokenize_texts_parallel(
            &tokenize_text_url(&xai_api_base_url()),
            &api_key,
            model,
            texts.jobs(),
        )
        .await;
        tracing::info!(
            model,
            skills_tokens = tokens.skills,
            system_prompt_tokens = tokens.system,
            tool_definitions_tokens = tokens.tools,
            mcp_tokens = tokens.mcp,
            agents_md_tokens = tokens.agents_md,
            workflows_tokens = tokens.workflows,
            skills_count = counts.skills_count,
            "session_context_snapshot: emitted"
        );
        xai_grok_telemetry::session_ctx::log_session_event(session_context_snapshot(
            self.session_info.id.0.to_string(),
            &info,
            &counts,
            &tokens,
        ));
        xai_grok_telemetry::session_ctx::drain_pending(xai_grok_telemetry::session_ctx::CLI_DRAIN)
            .await;
    }

    async fn tokenize_api_key(&self) -> Option<String> {
        if let Some(manager) = &self.auth_manager
            && let Ok(auth) = manager.auth().await
            && !auth.key.is_empty()
        {
            return Some(auth.key);
        }
        self.chat_state_handle
            .get_credentials()
            .await
            .api_key
            .filter(|key| !key.is_empty())
    }

    pub(super) async fn last_bound_projection_slot(&self) -> LastBoundProjectionSlot {
        let resources = self
            .agent
            .borrow()
            .tool_bridge()
            .toolset()
            .resources
            .clone();
        let slot = resources
            .lock()
            .await
            .get_or_default::<LastBoundProjectionSlot>()
            .clone();
        slot
    }

    pub(super) async fn last_bound_projection(&self) -> LastBoundProjectionState {
        let slot = self.last_bound_projection_slot().await;
        let Some(summary) = slot.0.lock().clone() else {
            return LastBoundProjectionState::NotCaptured;
        };
        let current = self.chat_state_handle.get_sampling_config().await;
        if current.is_some_and(|current| {
            current.model == summary.model && current.api_backend == summary.backend
        }) {
            LastBoundProjectionState::LastBound(summary)
        } else {
            LastBoundProjectionState::StaleCapturedRow(summary)
        }
    }

    pub(super) async fn publish_bound_projection(
        &self,
        request: &ConversationRequest,
        binding: &xai_chat_state::RequestTransportBinding,
        selected_functions: usize,
    ) {
        let surface = if self.forked_tool_override.is_some() {
            BoundProjectionSurface::Fork
        } else if self.startup_hints.is_subagent {
            BoundProjectionSurface::RebuiltChild
        } else {
            BoundProjectionSurface::Main
        };
        let mut declaration_view = String::new();
        let mut declaration_view_truncated = false;
        let limit = xai_grok_telemetry::external::truncate::MAX_TOOL_INPUT_JSON_BYTES;
        for spec in &request.tools {
            let bytes = spec.name.len() + 1;
            if declaration_view.len().saturating_add(bytes) > limit {
                declaration_view_truncated = true;
                break;
            }
            declaration_view.push_str(&spec.name);
            declaration_view.push('\n');
        }
        let mut system_bytes = 0;
        let mut history_bytes = 0;
        for item in &request.items {
            let bytes = item.text_content().len();
            if matches!(item, ConversationItem::System(_)) {
                system_bytes += bytes;
            } else {
                history_bytes += bytes;
            }
        }
        let summary = LastBoundProjectionSummary {
            model: binding.config.model.clone(),
            backend: binding.config.api_backend.clone(),
            admission: request.search_admission,
            surface,
            selected_functions,
            removed_functions: selected_functions.saturating_sub(request.tools.len()),
            retained_functions: request.tools.len(),
            declaration_view,
            declaration_view_truncated,
            vendor_entries: None,
            system_bytes,
            history_bytes,
        };
        let slot = self.last_bound_projection_slot().await;
        *slot.0.lock() = Some(summary);
    }

    async fn snapshot_texts(&self) -> SnapshotTexts {
        let system = self
            .chat_state_handle
            .get_system_message()
            .await
            .map(|item| item.text_content())
            .unwrap_or_default();
        let backend_search_active = self.backend_search_active();
        let tool_defs: Vec<_> = self
            .prepare_tool_definitions_inner()
            .await
            .into_iter()
            .filter(|td| !backend_search_active || td.function.name != "web_search")
            .collect();
        let tool_definitions_count = tool_defs.len() as u64;
        let tools = tool_definitions_text(&tool_defs);
        let skills = self.tool_bridge_handle().skill_listing_snapshot().await;
        let mcp = self.mcp_announcement_snapshot().await;
        let workflows = self.workflow_listing_snapshot();
        let agents = self.agents_md_category_text();
        SnapshotTexts {
            system,
            tools,
            tool_definitions_count,
            skills_text: skills.as_ref().map(|s| s.text.clone()).unwrap_or_default(),
            skills_count: skills.map(|s| s.skill_count as u64).unwrap_or(0),
            mcp_text: mcp.as_ref().map(|s| s.text.clone()).unwrap_or_default(),
            mcp_server_count: mcp.map(|s| s.server_count as u64).unwrap_or(0),
            agents_md_text: agents
                .as_ref()
                .map(|(text, _)| text.clone())
                .unwrap_or_default(),
            agents_md_file_count: agents.map(|(_, n)| n).unwrap_or(0),
            workflows_text: workflows
                .as_ref()
                .map(|(text, _)| text.clone())
                .unwrap_or_default(),
            workflows_count: workflows.map(|(_, n)| n as u64).unwrap_or(0),
        }
    }

    fn agents_md_category_text(&self) -> Option<(String, u64)> {
        let agent = self.agent.borrow();
        let file_count = agent.prompt_context().agents_md_files.len() as u64;
        if file_count == 0 {
            return None;
        }
        Some((agent.agents_md_section()?, file_count))
    }
}

struct SnapshotTexts {
    system: String,
    tools: String,
    tool_definitions_count: u64,
    skills_text: String,
    skills_count: u64,
    mcp_text: String,
    mcp_server_count: u64,
    agents_md_text: String,
    agents_md_file_count: u64,
    workflows_text: String,
    workflows_count: u64,
}

impl SnapshotTexts {
    fn item_counts(&self) -> ItemCounts {
        ItemCounts {
            tool_definitions_count: self.tool_definitions_count,
            skills_count: self.skills_count,
            mcp_server_count: self.mcp_server_count,
            agents_md_file_count: self.agents_md_file_count,
            workflows_count: self.workflows_count,
        }
    }

    fn jobs(&self) -> Vec<TokenizeJob> {
        [
            TokenizeJob::new(TokenizeField::System, &self.system),
            TokenizeJob::new(TokenizeField::Tools, &self.tools),
            TokenizeJob::new(TokenizeField::Skills, &self.skills_text),
            TokenizeJob::new(TokenizeField::Mcp, &self.mcp_text),
            TokenizeJob::new(TokenizeField::AgentsMd, &self.agents_md_text),
            TokenizeJob::new(TokenizeField::Workflows, &self.workflows_text),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

struct ItemCounts {
    tool_definitions_count: u64,
    skills_count: u64,
    mcp_server_count: u64,
    agents_md_file_count: u64,
    workflows_count: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokenizeField {
    System,
    Tools,
    Skills,
    Mcp,
    AgentsMd,
    Workflows,
}

struct TokenizeJob {
    field: TokenizeField,
    text: String,
}

impl TokenizeJob {
    fn new(field: TokenizeField, text: &str) -> Option<Self> {
        if text.is_empty() {
            return None;
        }
        Some(Self {
            field,
            text: text.to_string(),
        })
    }
}

#[derive(Default)]
struct TokenCounts {
    system: u64,
    tools: u64,
    skills: u64,
    mcp: u64,
    agents_md: u64,
    workflows: u64,
}

impl TokenCounts {
    fn set(&mut self, field: TokenizeField, tokens: u64) {
        match field {
            TokenizeField::System => self.system = tokens,
            TokenizeField::Tools => self.tools = tokens,
            TokenizeField::Skills => self.skills = tokens,
            TokenizeField::Mcp => self.mcp = tokens,
            TokenizeField::AgentsMd => self.agents_md = tokens,
            TokenizeField::Workflows => self.workflows = tokens,
        }
    }
}

fn xai_api_base_url() -> String {
    crate::config::load_effective_config()
        .ok()
        .and_then(|effective| crate::agent::config::Config::new_from_toml_cfg(&effective).ok())
        .map(|cfg| cfg.endpoints.xai_api_base_url)
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| crate::agent::config::EndpointsConfig::default().xai_api_base_url)
}

fn tokenize_text_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    format!("{base}/tokenize-text")
}

fn parse_token_count(body: &serde_json::Value) -> Option<u64> {
    Some(body.get("token_ids")?.as_array()?.len() as u64)
}

fn tool_definitions_text(defs: &[ToolDefinition]) -> String {
    defs.iter()
        .map(|td| {
            format!(
                "{}{}{}",
                td.function.name,
                td.function.description.as_deref().unwrap_or(""),
                td.function.parameters
            )
        })
        .collect()
}

async fn tokenize_texts_parallel(
    url: &str,
    api_key: &str,
    model: &str,
    jobs: Vec<TokenizeJob>,
) -> TokenCounts {
    let client = crate::http::shared_client();
    let futs = jobs.into_iter().map(|job| {
        let client = client.clone();
        let url = url.to_string();
        let api_key = api_key.to_string();
        let model = model.to_string();
        async move {
            let tokens = tokenize_one(&client, &url, &api_key, &model, &job.text).await;
            (job.field, tokens)
        }
    });
    let mut counts = TokenCounts::default();
    for (field, tokens) in futures::future::join_all(futs).await {
        if let Some(tokens) = tokens {
            counts.set(field, tokens);
        }
    }
    counts
}

async fn tokenize_one(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    model: &str,
    text: &str,
) -> Option<u64> {
    let Ok(auth) = reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}")) else {
        tracing::debug!("session_context_snapshot: invalid api key header");
        return None;
    };
    let resp = client
        .post(url)
        .header(reqwest::header::AUTHORIZATION, auth)
        .header("x-grok-client-version", xai_grok_version::VERSION)
        .json(&serde_json::json!({
            "text": text,
            "model": model,
        }))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "tokenize-text request failed");
            e
        })
        .ok()?;
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(%status, "tokenize-text failed");
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    parse_token_count(&body)
}

fn session_context_snapshot(
    session_id: String,
    info: &SessionInfoData,
    counts: &ItemCounts,
    tokens: &TokenCounts,
) -> crate::agent::session_metrics::SessionContextSnapshot {
    let ctx = &info.context;
    crate::agent::session_metrics::SessionContextSnapshot {
        session_id,
        model_id: info.model.clone().unwrap_or_default(),
        context_window: ctx.total,
        used_tokens: ctx.used,
        usage_pct: ctx.usage_pct,
        free_tokens: ctx.free_tokens,
        system_prompt_tokens: tokens.system,
        tool_definitions_tokens: tokens.tools,
        tool_definitions_count: counts.tool_definitions_count,
        message_tokens: ctx.message_tokens,
        skills_tokens: tokens.skills,
        skills_count: counts.skills_count,
        mcp_tokens: tokens.mcp,
        mcp_server_count: counts.mcp_server_count,
        agents_md_tokens: tokens.agents_md,
        agents_md_file_count: counts.agents_md_file_count,
        workflows_tokens: tokens.workflows,
        workflows_count: counts.workflows_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> SessionInfoData {
        SessionInfoData {
            agent_name: Some("grok-build".into()),
            model: Some("grok-4".into()),
            model_display_name: None,
            resolved_model_id: None,
            model_fingerprint: None,
            show_model_fingerprint: false,
            api_backend: None,
            conversation_id: None,
            turns: 0,
            turn_index: 0,
            context: ContextInfo {
                used: 40_000,
                total: 1_000_000,
                system_prompt_tokens: 8_000,
                tool_definitions_count: 12,
                tool_definitions_tokens: 5_000,
                compaction_count: 0,
                turn_count: 0,
                tool_call_count: 0,
                message_count: 1,
                message_tokens: 2_000,
                free_tokens: 960_000,
                usage_pct: 4,
                auto_compact_threshold_percent: 85,
                usage_categories: vec![],
            },
            last_bound: None,
        }
    }

    #[test]
    fn tokenize_uses_baked_product_default_model() {
        // Operator ruling 2026-09-22 (mid apex-ayl.129): the baked
        // product default pin moved grok-4.6 -> gpt-5.6-terra.
        assert_eq!(crate::models::default_model(), "gpt-5.6-terra");
    }

    #[test]
    fn tokenize_text_url_trims_trailing_slash() {
        assert_eq!(
            tokenize_text_url("https://api.x.ai/v1/"),
            "https://api.x.ai/v1/tokenize-text"
        );
        assert_eq!(
            tokenize_text_url("https://api.x.ai/v1"),
            "https://api.x.ai/v1/tokenize-text"
        );
    }

    #[test]
    fn parse_token_count_uses_token_ids_len() {
        let body = serde_json::json!({
            "token_ids": [
                {"token_id": 1},
                {"token_id": 2},
                {"token_id": 3}
            ]
        });
        assert_eq!(parse_token_count(&body), Some(3));
        assert_eq!(parse_token_count(&serde_json::json!({})), None);
        assert_eq!(
            parse_token_count(&serde_json::json!({"token_ids": []})),
            Some(0)
        );
    }

    #[test]
    fn empty_texts_are_not_tokenized() {
        let texts = SnapshotTexts {
            system: String::new(),
            tools: "defs".into(),
            tool_definitions_count: 2,
            skills_text: String::new(),
            skills_count: 0,
            mcp_text: String::new(),
            mcp_server_count: 0,
            agents_md_text: String::new(),
            agents_md_file_count: 0,
            workflows_text: String::new(),
            workflows_count: 0,
        };
        let jobs = texts.jobs();
        assert_eq!(jobs.len(), 1);
        assert!(matches!(jobs[0].field, TokenizeField::Tools));
    }

    #[test]
    fn snapshot_uses_tokenizer_counts_not_bytes4() {
        let ev = session_context_snapshot(
            "s1".into(),
            &info(),
            &ItemCounts {
                tool_definitions_count: 12,
                skills_count: 282,
                mcp_server_count: 4,
                agents_md_file_count: 2,
                workflows_count: 3,
            },
            &TokenCounts {
                system: 111,
                tools: 222,
                skills: 27_000,
                mcp: 333,
                agents_md: 444,
                workflows: 55,
            },
        );
        assert_eq!(ev.session_id, "s1");
        assert_eq!(ev.model_id, "grok-4");
        assert_eq!(ev.context_window, 1_000_000);
        assert_eq!(ev.used_tokens, 40_000);
        assert_eq!(ev.system_prompt_tokens, 111);
        assert_eq!(ev.tool_definitions_tokens, 222);
        assert_eq!(ev.skills_tokens, 27_000);
        assert_eq!(ev.skills_count, 282);
        assert_eq!(ev.mcp_tokens, 333);
        assert_eq!(ev.agents_md_tokens, 444);
        assert_eq!(ev.workflows_tokens, 55);
        assert_ne!(ev.system_prompt_tokens, 8_000);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn context_snapshot_candidates_do_not_change_builtin_text() {
        use super::super::support;
        use xai_grok_sampling_types::{ApiBackend, HostedTool, ToolSpec};
        use xai_grok_tools::implementations::grok_build::{
            update_goal::UpdateGoalTool, web_search::WebSearchTool,
        };
        use xai_grok_tools::registry::types::ToolConfig;

        tokio::task::LocalSet::new().run_until(async {
            for (backend, flag) in [
                (ApiBackend::Messages, true),
                (ApiBackend::Messages, false),
                (ApiBackend::Responses, true),
                (ApiBackend::Responses, false),
            ] {
                let baseline = support::plain_actor().await;
                let candidate = support::plain_actor().await;
                for actor in [&baseline, &candidate] {
                    let agent = support::test_agent_with_tools(vec![
                        ToolConfig::for_tool::<UpdateGoalTool>(),
                        ToolConfig::for_tool::<WebSearchTool>(),
                    ]).await;
                    actor.agent.replace(xai_grok_agent::Agent::new(
                        agent.definition().clone(), agent.prompt_context().clone(),
                        agent.system_prompt().to_string(), agent.tool_bridge().clone(),
                        agent.reminder_policy().clone(), agent.compaction_policy().clone(),
                        vec![HostedTool::WebSearch { options: None }], true,
                    ));
                    let mut binding = actor.chat_state_handle.capture_transport_binding().await.unwrap();
                    binding.config.api_backend = backend.clone();
                    binding.config.supports_search_tool = flag;
                    actor.chat_state_handle.install_transport_binding(binding).await.unwrap();
                }
                let schema = serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false});
                candidate.agent.borrow().tool_bridge().register_mcp_tools(
                    "snapshot__lookup".into(), support::StubMcpTool("snapshot__lookup"), Some(schema.clone()),
                ).unwrap();
                let (prepared, _) = candidate.prepare_native_tool_candidates_timed().await;
                let expected_mcp = vec![ToolSpec {
                    name: "snapshot__lookup".into(), description: Some("stub MCP tool".into()),
                    parameters: schema, exposure: xai_grok_sampling_types::ToolExposure::Immediate,
                }];
                assert_eq!(serde_json::to_value(prepared.mcp_specs).unwrap(), serde_json::to_value(expected_mcp).unwrap(),
                    "U5 context fixture has a parser-qualified typed candidate");
                for backend_search in [false, true] {
                    for actor in [&baseline, &candidate] {
                        actor.supports_backend_search.set(backend_search);
                    }
                    let defs: Vec<_> = baseline.prepare_tool_definitions_inner().await.into_iter()
                        .filter(|definition| !backend_search || definition.function.name != "web_search")
                        .collect();
                    assert_eq!(defs.iter().any(|definition| definition.function.name == "web_search"), !backend_search);
                    let expected_text = tool_definitions_text(&defs);
                    let baseline_texts = baseline.snapshot_texts().await;
                    let candidate_texts = candidate.snapshot_texts().await;
                    assert_eq!((&baseline_texts.tools, baseline_texts.tool_definitions_count), (&expected_text, defs.len() as u64));
                    assert_eq!((&candidate_texts.tools, candidate_texts.tool_definitions_count), (&expected_text, defs.len() as u64),
                        "U5 context snapshot formats only the complete builtin definitions");
                    let jobs: Vec<_> = candidate_texts.jobs().into_iter()
                        .filter(|job| matches!(job.field, TokenizeField::Tools)).map(|job| job.text).collect();
                    assert_eq!(jobs, vec![expected_text], "U5 tokenizer tool input is the builtin-only formatted text");
                }
            }
        }).await;
    }
}
