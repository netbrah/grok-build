//! Shared cache-aligned request building for recap-style auxiliary model calls (recap, turn summary).
//! `/btw` reuses the request skeleton.

use super::*;

#[derive(Debug, PartialEq)]
struct PromptCacheUsage {
    prompt_tokens: u32,
    cached_prompt_tokens: u32,
    cache_creation_prompt_tokens: u32,
    uncached_prompt_tokens: u32,
    cache_read_rate: f64,
    cache_write_rate: f64,
}

impl From<&xai_grok_sampling_types::TokenUsage> for PromptCacheUsage {
    fn from(usage: &xai_grok_sampling_types::TokenUsage) -> Self {
        let prompt_tokens = usage.prompt_tokens;
        let uncached_prompt_tokens = prompt_tokens
            .saturating_sub(usage.cached_prompt_tokens)
            .saturating_sub(usage.cache_creation_prompt_tokens);
        let rate = |tokens| {
            if prompt_tokens == 0 {
                0.0
            } else {
                (f64::from(tokens) / f64::from(prompt_tokens) * 1_000.0).round() / 1_000.0
            }
        };
        Self {
            prompt_tokens,
            cached_prompt_tokens: usage.cached_prompt_tokens,
            cache_creation_prompt_tokens: usage.cache_creation_prompt_tokens,
            uncached_prompt_tokens,
            cache_read_rate: rate(usage.cached_prompt_tokens),
            cache_write_rate: rate(usage.cache_creation_prompt_tokens),
        }
    }
}

/// Logs the provider-reported prompt cache buckets for one auxiliary call.
pub(crate) fn log_prompt_cache_usage(
    call: &str,
    backend: crate::sampling::ApiBackend,
    response: &xai_grok_sampling_types::ConversationResponse,
) {
    let Some(usage) = response.usage.as_ref() else {
        return;
    };
    let usage = PromptCacheUsage::from(usage);
    tracing::info!(
        call,
        backend = ?backend,
        prompt_tokens = usage.prompt_tokens,
        cached_prompt_tokens = usage.cached_prompt_tokens,
        cache_creation_prompt_tokens = usage.cache_creation_prompt_tokens,
        uncached_prompt_tokens = usage.uncached_prompt_tokens,
        cache_read_rate = usage.cache_read_rate,
        cache_write_rate = usage.cache_write_rate,
        cache_key_forwarded = backend.forwards_prompt_cache_key(),
        "auxiliary call prompt cache usage"
    );
}

/// What differs between the two calls that reuse the parent's prompt cache.
/// The shared parts live in [`SessionActor::parent_cached_request`].
pub(crate) struct AuxCall {
    pub(crate) items: Vec<ConversationItem>,
    pub(crate) tools: ToolSpecBundle,
    pub(crate) hosted_tools: Vec<xai_grok_sampling_types::HostedTool>,
    pub(crate) model: String,
    pub(crate) supports_search_tool: bool,
    /// Must match the main turn's, or the prompt differs before the conversation history even starts.
    pub(crate) reasoning_effort: Option<xai_grok_sampling_types::ReasoningEffort>,
    /// apex-ayl.86 (SDD §3.6 fill directive, review-fix m-1): the shell-resolved wire tier for ultra; `None` lets egress fall back to wire "max" (I9).
    pub(crate) ultra_wire_effort: Option<xai_grok_sampling_types::ReasoningEffort>,
    /// Says whether the cache key gets sent, which is what decides the conv id below.
    pub(crate) backend: crate::sampling::ApiBackend,
    pub(crate) conv_id: String,
    pub(crate) req_id: String,
}

/// Shared setup for a recap-style side-call; see [`SessionActor::prepare_side_call`].
pub(crate) struct SideCallSetup {
    pub(crate) binding: xai_chat_state::RequestTransportBinding,
    pub(crate) client: xai_grok_sampler::SamplingClient,
    pub(crate) strip_reasoning: bool,
    pub(crate) context_window: u64,
    pub(crate) model: String,
    /// Must match the main turn so the side-call shares the prompt-cache prefix.
    pub(crate) reasoning_effort: Option<xai_grok_sampling_types::ReasoningEffort>,
    /// apex-ayl.86 (SDD §3.6 fill directive, review-fix m-1): carried to the AuxCall sites below so side-call egress keeps the parent's resolved wire tier.
    pub(crate) ultra_wire_effort: Option<xai_grok_sampling_types::ReasoningEffort>,
}

pub(super) fn should_strip_side_call_reasoning(
    backend: crate::sampling::ApiBackend,
    reasoning_effort: Option<xai_grok_sampling_types::ReasoningEffort>,
) -> bool {
    matches!(backend, crate::sampling::ApiBackend::Messages)
        && reasoning_effort
            .and_then(|effort| effort.to_messages_api())
            .is_none()
}

impl SessionActor {
    /// Request skeleton for an auxiliary call that replays the parent conversation under the parent's `prompt_cache_key`.
    /// Temperature stays unset: cli-chat-proxy may inject a `thinking` config, and the Messages API then requires temperature == 1.
    pub(crate) fn parent_cached_request(&self, call: AuxCall) -> ConversationRequest {
        let session_id = self.session_info.id.to_string();
        // Only the Responses mapping sends the cache key
        // On the other backends the conv id is what ties a call to its conversation, so it has to stay the parent session id
        // The `btw-`/`recap-` label still shows up in `x_grok_req_id`
        let conv_id = if call.backend.forwards_prompt_cache_key() {
            call.conv_id
        } else {
            session_id.clone()
        };
        // Admission uses the captured installed flag and the selected tool surface.
        let search_admission = Some(xai_grok_sampling_types::SearchAdmission::for_row(
            call.supports_search_tool,
            &call.tools.specs,
        ));
        let tools = call.tools.specs.clone();
        let projection = BridgeDeclarationProjection::prepared(call.tools, Default::default());
        let mut request = ConversationRequest {
            items: xai_chat_state::compaction_utils::ModelRequestHistory::from_raw(call.items)
                .into_items(),
            tools,
            hosted_tools: call.hosted_tools,
            search_admission,
            model: Some(call.model),
            temperature: None,
            // Effort changes the prompt ahead of the conversation history, so dropping it here would share no prefix with the main turn.
            reasoning_effort: call.reasoning_effort,
            // apex-ayl.86 (SDD §3.6 fill directive, review-fix m-1): carry the parent's resolved wire tier so side-call egress does not degrade ultra to the "max" fallback.
            ultra_wire_effort: call.ultra_wire_effort,
            x_grok_conv_id: Some(conv_id),
            x_grok_req_id: Some(call.req_id),
            x_grok_session_id: Some(session_id.clone()),
            x_grok_agent_id: Some(xai_grok_telemetry::id::agent_id()),
            prompt_cache_key: Some(session_id),
            // Side calls persist text and never execute tools (the attached tools only align the prompt-cache prefix)
            // A Length sample must fail rather than salvage
            length_policy: xai_grok_sampling_types::LengthPolicy::Fail,
            ..Default::default()
        };
        #[cfg(test)]
        tests::record_auxiliary_build(&request);
        if let Err(error) =
            self.project_bridge_declarations(&mut request, &call.backend, &projection)
        {
            tracing::error!(%error, "auxiliary bridge projection failed; retaining selected tools");
        }
        request
    }

    /// Prepare the shared pieces of a recap-style side-call (recap and turn summary): the sampling client and the config both need.
    /// Recap-style side-calls preserve reasoning so their conversation prefix stays byte-identical to the parent turn.
    /// Messages strips reasoning only when the matching effort cannot emit a top-level thinking configuration.
    pub(crate) async fn prepare_side_call(&self) -> Result<SideCallSetup, acp::Error> {
        let mut binding = self.capture_transport_binding_or_default().await;
        #[cfg(test)]
        tests::after_auxiliary_capture(self).await;
        let client = self
            .prepare_chat_completion_from_binding(&mut binding, false)
            .await?;
        let context_window = binding.config.context_window.get();
        let reasoning_effort = binding.config.reasoning_effort;
        let ultra_wire_effort = binding.config.ultra_wire_effort;
        let strip_reasoning =
            should_strip_side_call_reasoning(client.api_backend(), reasoning_effort);
        let model = binding.config.model.clone();
        Ok(SideCallSetup {
            binding,
            client,
            strip_reasoning,
            context_window,
            model,
            reasoning_effort,
            ultra_wire_effort,
        })
    }

    pub(super) async fn prepare_side_call_tool_specs(
        &self,
        binding: &xai_chat_state::RequestTransportBinding,
    ) -> ToolSpecBundle {
        let (prepared, _) = self.prepare_native_tool_candidates_timed().await;
        let bundle = self.prepared_tool_bundle(&prepared);
        let selected = self
            .turn_native_tool_candidates(&prepared.builtin_defs, &prepared.mcp_specs)
            .select_for_row(&binding.config);
        // The selector retains the builtin prefix and keeps or drops the entire MCP suffix.
        let coordinates = bundle
            .companion
            .expect("prepared_tool_bundle constructs its identity companion")
            .coordinates;
        ToolSpecBundle::from_selected(prepared.inventory, coordinates.into_iter().zip(selected))
    }

    /// Build the cache-aligned request for a recap-style side-call via [`Self::parent_cached_request`].
    /// Uses the main turn's tool and hosted-tool specs and matching reasoning effort so the prompt-cache prefix stays warm.
    /// The instructions keep outputs short and the clean helpers cap length as a safety net, so an explicit token cap isn't needed.
    pub(crate) async fn side_call_request(
        &self,
        setup: &SideCallSetup,
        items: Vec<ConversationItem>,
        x_grok_conv_id: String,
        x_grok_req_id: String,
    ) -> ConversationRequest {
        let tools = self.prepare_side_call_tool_specs(&setup.binding).await;
        // Mirror the main turn's hosted tools (overrides folded in) so a side-call can't search past the active cutoff.
        let hosted_tools = self.hosted_tools_for_turn();
        self.parent_cached_request(AuxCall {
            items,
            tools,
            hosted_tools,
            model: setup.model.clone(),
            supports_search_tool: setup.binding.config.supports_search_tool,
            reasoning_effort: setup.reasoning_effort,
            ultra_wire_effort: setup.ultra_wire_effort,
            backend: setup.client.api_backend(),
            conv_id: x_grok_conv_id,
            req_id: x_grok_req_id,
        })
    }

    /// Invalidate in-flight recap-style side-calls when a real user prompt is accepted (at queue time or turn start).
    /// Bumps the recap epoch so a finishing recap cannot commit, and aborts an in-flight turn summary.
    /// Idempotent under the queue-accept and turn-start double bump.
    pub(crate) fn invalidate_side_calls_for_new_prompt(&self) {
        self.recap_epoch.set(self.recap_epoch.get().wrapping_add(1));
        self.abort_turn_summary();
        // The title refresh is deliberately NOT aborted here.
        // It describes the whole conversation, so completing against the pre-prompt snapshot is still valid.
        // Aborting on every prompt would leave the checkpoint unconsumed and re-spawn a call each turn.
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::support;
    use super::{AuxCall, PromptCacheUsage};
    use xai_grok_sampling_types::TokenUsage;

    #[test]
    fn prompt_cache_usage_projects_provider_buckets_and_rates() {
        let usage = PromptCacheUsage::from(&TokenUsage {
            prompt_tokens: 1_000,
            cached_prompt_tokens: 700,
            cache_creation_prompt_tokens: 200,
            ..Default::default()
        });

        assert_eq!(usage.prompt_tokens, 1_000);
        assert_eq!(usage.cached_prompt_tokens, 700);
        assert_eq!(usage.cache_creation_prompt_tokens, 200);
        assert_eq!(usage.uncached_prompt_tokens, 100);
        assert_eq!(usage.cache_read_rate, 0.7);
        assert_eq!(usage.cache_write_rate, 0.2);

        let rounded = PromptCacheUsage::from(&TokenUsage {
            prompt_tokens: 144_860,
            cached_prompt_tokens: 141_663,
            cache_creation_prompt_tokens: 3_195,
            ..Default::default()
        });
        assert_eq!(rounded.cache_read_rate, 0.978);
        assert_eq!(rounded.cache_write_rate, 0.022);
    }

    #[test]
    fn prompt_cache_usage_saturates_invalid_buckets_and_zero_rates() {
        let usage = PromptCacheUsage::from(&TokenUsage {
            prompt_tokens: 0,
            cached_prompt_tokens: 10,
            cache_creation_prompt_tokens: 20,
            ..Default::default()
        });

        assert_eq!(usage.uncached_prompt_tokens, 0);
        assert_eq!(usage.cache_read_rate, 0.0);
        assert_eq!(usage.cache_write_rate, 0.0);
    }

    /// An actor booted on a row the operator flagged, built from the scaffolding shared with
    /// `acp_session_impl/model_switch.rs` (`acp_session_tests/support.rs`) so the two modules cannot
    /// drift apart. The tempdir handed to the manager's `AuthManager` as its `grok_home` comes back
    /// with the actor and the caller binds it, so it outlives every use of that manager.
    async fn actor_on_flagged_row(
        model: &str,
    ) -> (
        std::sync::Arc<super::super::SessionActor>,
        tempfile::TempDir,
    ) {
        let (manager, tmp) = support::manager_with_entries();
        manager.insert_test_entry(
            model,
            support::entry_with_search_flag(model, /* supports_search_tool */ true),
        );
        let actor = support::actor_on_row(model, manager).await;
        (actor, tmp)
    }

    fn aux_call(model: &str, tools: Vec<xai_grok_sampling_types::ToolSpec>) -> AuxCall {
        AuxCall {
            items: Vec::new(),
            tools: super::super::ToolSpecBundle::without_identity(tools),
            hosted_tools: Vec::new(),
            model: model.to_owned(),
            supports_search_tool: true,
            reasoning_effort: None,
            ultra_wire_effort: None,
            backend: crate::sampling::ApiBackend::Responses,
            conv_id: "aux-conv".to_owned(),
            req_id: "aux-req".to_owned(),
        }
    }

    /// apex-waj.35 F7: two of this door's three production callers hand it the turn's BASE
    /// tool surface — `recap.rs:176` and `side_call.rs:204` both build it with
    /// `turn_base_tool_specs` (`sampler_turn.rs:346`), the fn the non-forked turn calls
    /// at `turn.rs:2797`. `title_refresh.rs:134` passes none, and the door itself only
    /// forwards `call.tools` (`side_call.rs:146`). Three turn-side transforms are not reproduced:
    /// the forked override (`turn.rs:2784`), the subagent rewrap (`turn.rs:2798`), the
    /// structured-output append (`turn.rs:2810`). On an admitted row the turn's body
    /// carries the `tool_search` declaration at `tools[0]`
    /// (`xai-grok-sampler/src/client.rs:952`), so leaving the admission to this type's
    /// `Default` sends a `tools` array the turn does not send.
    #[tokio::test(flavor = "current_thread")]
    async fn a_side_call_on_an_admitted_row_carries_the_turns_admission() {
        const ROW: &str = "flagged-aux-row";
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (actor, _tmp) = actor_on_flagged_row(ROW).await;
                let turn = support::turn_request(&actor).await;
                let aux = actor.parent_cached_request(aux_call(ROW, turn.tools.clone()));

                assert!(
                    turn.search_admission.is_some_and(|a| a.admitted()),
                    "anti-vacuity: the row this session booted on admits the turn: {:?}",
                    turn.search_admission
                );
                assert_eq!(
                    aux.search_admission, turn.search_admission,
                    "an aux call that replays the turn's tool surface must carry the turn's \
                     admission, or its `tools` array shifts the prefix it shares with the turn"
                );
            })
            .await;
    }

    /// The other half of the same door: a DELIBERATELY tool-less call (`title_refresh.rs:134` passes
    /// `tools: Vec::new()`) on the same admitting row stays un-admitted, because it is
    /// `SearchAdmission::for_row`'s surface half that closes it and not a literal set here. Its
    /// entries stay byte-identical, which is what `extra_tool_entries_for_route` answers for any
    /// admission whose `admitted()` is false.
    #[tokio::test(flavor = "current_thread")]
    async fn a_tool_less_side_call_on_an_admitting_row_stays_unadmitted() {
        const ROW: &str = "flagged-aux-row";
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (actor, _tmp) = actor_on_flagged_row(ROW).await;
                let aux = actor.parent_cached_request(aux_call(ROW, Vec::new()));
                assert_eq!(
                    aux.search_admission,
                    Some(xai_grok_sampling_types::SearchAdmission {
                        supports_search_tool: true,
                        has_searchable_tools: false,
                    }),
                    "the row admits and the surface does not: the surface half must close the gate"
                );
            })
            .await;
    }

    #[derive(Debug)]
    struct AuxiliaryAttribution;

    impl xai_grok_sampler::Auth401AttributionCallback for AuxiliaryAttribution {
        fn record_401(&self, _: xai_grok_sampler::SamplingConsumer, _: Option<&str>) {}
    }

    #[derive(Default)]
    struct AuxiliaryTrace {
        next: Option<xai_chat_state::RequestTransportBinding>,
        sends: Vec<(&'static str, xai_grok_sampling_types::ConversationRequest)>,
        builds: Vec<xai_grok_sampling_types::ConversationRequest>,
        suggest_configs: Vec<xai_grok_sampler::SamplerConfig>,
        refresh_count: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
        refreshes_before_send: Vec<usize>,
    }

    thread_local! {
        static AUXILIARY_TRACE: std::cell::RefCell<Option<AuxiliaryTrace>> = const { std::cell::RefCell::new(None) };
    }

    pub(in crate::session::acp_session) struct AuxiliaryGuard;

    impl AuxiliaryGuard {
        pub(in crate::session::acp_session) fn new(
            next: xai_chat_state::RequestTransportBinding,
        ) -> Self {
            AUXILIARY_TRACE.with(|trace| {
                assert!(trace.borrow().is_none());
                *trace.borrow_mut() = Some(AuxiliaryTrace {
                    next: Some(next),
                    ..Default::default()
                });
            });
            Self
        }

        pub(in crate::session::acp_session) fn count_refreshes(
            &self,
            count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        ) {
            AUXILIARY_TRACE
                .with(|trace| trace.borrow_mut().as_mut().unwrap().refresh_count = Some(count));
        }

        pub(in crate::session::acp_session) fn sends(
            &self,
        ) -> Vec<(&'static str, xai_grok_sampling_types::ConversationRequest)> {
            AUXILIARY_TRACE.with(|trace| trace.borrow().as_ref().unwrap().sends.clone())
        }

        pub(in crate::session::acp_session) fn builds(
            &self,
        ) -> Vec<xai_grok_sampling_types::ConversationRequest> {
            AUXILIARY_TRACE.with(|trace| trace.borrow().as_ref().unwrap().builds.clone())
        }

        pub(in crate::session::acp_session) fn suggest_configs(
            &self,
        ) -> Vec<xai_grok_sampler::SamplerConfig> {
            AUXILIARY_TRACE.with(|trace| trace.borrow().as_ref().unwrap().suggest_configs.clone())
        }

        pub(in crate::session::acp_session) fn refreshes_before_send(&self) -> Vec<usize> {
            AUXILIARY_TRACE.with(|trace| {
                trace
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .refreshes_before_send
                    .clone()
            })
        }
    }

    impl Drop for AuxiliaryGuard {
        fn drop(&mut self) {
            AUXILIARY_TRACE.with(|trace| *trace.borrow_mut() = None);
        }
    }

    pub(in crate::session::acp_session) async fn after_auxiliary_capture(
        actor: &super::SessionActor,
    ) {
        let next = AUXILIARY_TRACE.with(|trace| {
            trace
                .borrow_mut()
                .as_mut()
                .and_then(|trace| trace.next.take())
        });
        if let Some(next) = next {
            actor
                .chat_state_handle
                .install_transport_binding(next)
                .await
                .unwrap();
        }
    }

    pub(in crate::session::acp_session) fn record_auxiliary_send(
        site: &'static str,
        request: &xai_grok_sampling_types::ConversationRequest,
    ) {
        AUXILIARY_TRACE.with(|trace| {
            if let Some(trace) = trace.borrow_mut().as_mut() {
                assert!(trace.sends.len() < 8);
                trace.sends.push((site, request.clone()));
                if let Some(count) = &trace.refresh_count {
                    trace
                        .refreshes_before_send
                        .push(count.load(std::sync::atomic::Ordering::SeqCst));
                }
            }
        });
    }

    pub(in crate::session::acp_session) fn record_auxiliary_build(
        request: &xai_grok_sampling_types::ConversationRequest,
    ) {
        AUXILIARY_TRACE.with(|trace| {
            if let Some(trace) = trace.borrow_mut().as_mut() {
                assert!(trace.builds.len() < 8);
                trace.builds.push(request.clone());
            }
        });
    }

    pub(in crate::session::acp_session) fn record_suggest_config(
        config: &xai_grok_sampler::SamplerConfig,
    ) {
        AUXILIARY_TRACE.with(|trace| {
            if let Some(trace) = trace.borrow_mut().as_mut() {
                assert!(trace.suggest_configs.len() < 2);
                trace.suggest_configs.push(config.clone());
            }
        });
    }

    pub(in crate::session::acp_session) struct AuxiliaryFixture {
        pub(in crate::session::acp_session) actor: super::SessionActor,
        pub(in crate::session::acp_session) a: xai_chat_state::RequestTransportBinding,
        pub(in crate::session::acp_session) b: xai_chat_state::RequestTransportBinding,
        pub(in crate::session::acp_session) server_a: xai_grok_test_support::MockInferenceServer,
        pub(in crate::session::acp_session) server_b: xai_grok_test_support::MockInferenceServer,
        pub(in crate::session::acp_session) persistence:
            tokio::sync::mpsc::UnboundedReceiver<super::PersistenceMsg>,
        pub(in crate::session::acp_session) home: tempfile::TempDir,
    }

    pub(in crate::session::acp_session) async fn auxiliary_fixture(
        backend: xai_grok_sampling_types::ApiBackend,
        admitted: bool,
    ) -> AuxiliaryFixture {
        use xai_grok_sampling_types::{ApiBackend, HostedTool, ReasoningEffort};
        let (gateway_tx, gateway_rx) = tokio::sync::mpsc::unbounded_channel();
        support::drain_gateway(gateway_rx);
        let (persistence_tx, persistence) = tokio::sync::mpsc::unbounded_channel();
        let mut actor =
            support::create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
        actor.auth_method_id = support::test_auth_method_id("xai.api_key");
        actor.attribution_callback = Some(std::sync::Arc::new(AuxiliaryAttribution));
        actor.title_refresh_enabled = true;
        *actor.agent.borrow_mut() = support::test_agent_with_goal_tool().await;
        {
            let mut slot = actor.agent.borrow_mut();
            let agent = &*slot;
            *slot = xai_grok_agent::Agent::new(
                agent.definition().clone(),
                agent.prompt_context().clone(),
                agent.system_prompt().to_string(),
                std::sync::Arc::clone(agent.tool_bridge()),
                agent.reminder_policy().clone(),
                agent.compaction_policy().clone(),
                vec![HostedTool::WebSearch { options: None }],
                true,
            );
        }
        actor.supports_backend_search.set(true);
        let (manager, home) = support::manager_with_entries();
        actor.models_manager = manager;
        let defs = actor.prepare_tool_definitions().await;
        let marked = defs
            .iter()
            .find(|def| def.function.name != "web_search")
            .unwrap()
            .function
            .name
            .clone();
        actor.models_manager.insert_test_entry(
            "u6-operator",
            support::entry_with_deferred_tools("u6-operator", true, &[&marked]),
        );
        actor
            .models_manager
            .set_current_model_id(agent_client_protocol::ModelId::new("u6-operator"));
        actor.models_manager.insert_test_entry(
            "u6-row-a",
            support::entry_with_search_flag("u6-row-a", admitted),
        );
        actor.models_manager.insert_test_entry(
            "u6-row-b",
            support::entry_with_search_flag("u6-row-b", !admitted),
        );
        let server_a = xai_grok_test_support::MockInferenceServer::start()
            .await
            .unwrap();
        let server_b = xai_grok_test_support::MockInferenceServer::start()
            .await
            .unwrap();
        server_a.set_response("A concise summary");
        server_b.set_response("B concise summary");
        let mut a = actor
            .chat_state_handle
            .capture_transport_binding()
            .await
            .unwrap();
        a.config.model = "u6-row-a".into();
        a.config.base_url = server_a.url();
        a.config.api_backend = backend;
        a.config.context_window = std::num::NonZeroU64::new(384_000).unwrap();
        a.config.reasoning_effort = Some(ReasoningEffort::Ultra);
        a.config.ultra_wire_effort = Some(ReasoningEffort::High);
        a.config.supports_search_tool = admitted;
        a.config.max_completion_tokens = Some(8192);
        a.config.extra_headers = indexmap::indexmap! { "x-u6-row".into() => "a".into() };
        a.credentials = xai_chat_state::Credentials {
            api_key: Some("u6-a-key".into()),
            auth_type: xai_chat_state::AuthType::ApiKey,
            alpha_test_key: Some("u6-a-alpha".into()),
            client_version: Some("u6-a-version".into()),
        };
        let mut b = a.clone();
        b.config.model = "u6-row-b".into();
        b.config.base_url = server_b.url();
        b.config.api_backend = if a.config.api_backend == ApiBackend::Messages {
            ApiBackend::Responses
        } else {
            ApiBackend::Messages
        };
        b.config.context_window = std::num::NonZeroU64::new(96_000).unwrap();
        b.config.reasoning_effort = Some(ReasoningEffort::None);
        b.config.ultra_wire_effort = None;
        b.config.supports_search_tool = !admitted;
        b.config.extra_headers = indexmap::indexmap! { "x-u6-row".into() => "b".into() };
        b.credentials.api_key = Some("u6-b-key".into());
        b.credentials.alpha_test_key = Some("u6-b-alpha".into());
        b.credentials.client_version = Some("u6-b-version".into());
        actor
            .chat_state_handle
            .install_transport_binding(a.clone())
            .await
            .unwrap();
        actor.chat_state_handle.replace_conversation(vec![
            super::ConversationItem::system("You are a coding agent"),
            super::ConversationItem::user("first question"),
            super::ConversationItem::assistant("first answer"),
            super::ConversationItem::user("second question"),
            super::ConversationItem::assistant("second answer"),
            super::ConversationItem::user("third question"),
            super::ConversationItem::assistant("third answer"),
        ]);
        actor.chat_state_handle.get_conversation().await;
        AuxiliaryFixture {
            actor,
            a,
            b,
            server_a,
            server_b,
            persistence,
            home,
        }
    }

    pub(in crate::session::acp_session) fn assert_auxiliary_binding(
        actual: &xai_chat_state::RequestTransportBinding,
        expected: &xai_chat_state::RequestTransportBinding,
    ) {
        assert_eq!(
            serde_json::to_value(&actual.config).unwrap(),
            serde_json::to_value(&expected.config).unwrap()
        );
        assert_eq!(actual.credentials, expected.credentials);
    }

    pub(in crate::session::acp_session) fn assert_auxiliary_wire(
        fixture: &AuxiliaryFixture,
        model: &str,
    ) {
        assert!(
            fixture.server_b.requests().is_empty(),
            "auxiliary transport must never use installed B"
        );
        let requests = fixture.server_a.requests();
        assert_eq!(
            requests.len(),
            1,
            "one actual auxiliary send through captured A"
        );
        let request = &requests[0];
        let expected_path = match fixture.a.config.api_backend {
            xai_grok_sampling_types::ApiBackend::Messages => "/v1/messages",
            xai_grok_sampling_types::ApiBackend::Responses => "/v1/responses",
            xai_grok_sampling_types::ApiBackend::ChatCompletions => "/v1/chat/completions",
        };
        assert_eq!(request.path, expected_path);
        assert_eq!(request.authorization.as_deref(), Some("Bearer u6-a-key"));
        assert_eq!(request.header("x-u6-row"), Some("a"));
        assert_eq!(
            request.body.as_ref().unwrap()["model"],
            model,
            "auxiliary send must keep captured request row A"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auxiliary_setup_and_request_share_one_binding() {
        use xai_grok_sampling_types::{ApiBackend, ConversationItem, ToolExposure};
        tokio::task::LocalSet::new()
            .run_until(async {
                for (backend, admitted) in [
                    (ApiBackend::Messages, true),
                    (ApiBackend::Messages, false),
                    (ApiBackend::Responses, true),
                ] {
                    let fixture = auxiliary_fixture(backend, admitted).await;
                    let actor = &fixture.actor;
                    let items = vec![ConversationItem::user("auxiliary question")];
                    let baseline = actor.prepare_side_call().await.unwrap();
                    let expected = actor
                        .side_call_request(
                            &baseline,
                            items.clone(),
                            "aux-conv".into(),
                            "aux-req".into(),
                        )
                        .await;
                    assert!(!expected.tools.is_empty());
                    assert!(
                        expected
                            .tools
                            .iter()
                            .any(|tool| tool.exposure == ToolExposure::Deferred),
                        "prepared operator exposure must be exercised"
                    );
                    assert!(!expected.hosted_tools.is_empty());
                    actor.models_manager.insert_test_entry(
                        "u6-row-a",
                        support::entry_with_search_flag("u6-row-a", !admitted),
                    );
                    let _guard = AuxiliaryGuard::new(fixture.b.clone());
                    let setup = actor.prepare_side_call().await.unwrap();
                    assert_auxiliary_binding(&setup.binding, &fixture.a);
                    assert_eq!(
                        (
                            setup.client.api_backend(),
                            setup.model.as_str(),
                            setup.context_window,
                            setup.reasoning_effort,
                            setup.ultra_wire_effort,
                            setup.strip_reasoning
                        ),
                        (
                            baseline.client.api_backend(),
                            baseline.model.as_str(),
                            baseline.context_window,
                            baseline.reasoning_effort,
                            baseline.ultra_wire_effort,
                            baseline.strip_reasoning
                        ),
                        "auxiliary setup must keep every captured A request field"
                    );
                    let actual = actor
                        .side_call_request(&setup, items, "aux-conv".into(), "aux-req".into())
                        .await;
                    assert_eq!(
                        format!("{actual:?}"),
                        format!("{expected:?}"),
                        "auxiliary request must use captured admission and unchanged prepared specs"
                    );
                    setup.client.conversation_collect(actual).await.unwrap();
                    assert_auxiliary_wire(&fixture, &fixture.a.config.model);
                    assert_auxiliary_binding(
                        &actor
                            .chat_state_handle
                            .capture_transport_binding()
                            .await
                            .unwrap(),
                        &fixture.b,
                    );
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auxiliary_title_remains_tool_free() {
        use xai_grok_sampling_types::{ApiBackend, SearchAdmission};
        tokio::task::LocalSet::new()
            .run_until(async {
                let AuxiliaryFixture {
                    actor,
                    a,
                    b,
                    server_a,
                    server_b,
                    mut persistence,
                    home: _home,
                } = auxiliary_fixture(ApiBackend::Messages, true).await;
                assert!(
                    !actor
                        .turn_base_tool_specs(&actor.prepare_tool_definitions().await)
                        .is_empty()
                );
                assert!(!actor.hosted_tools_for_turn().is_empty());
                actor.models_manager.insert_test_entry(
                    "u6-row-a",
                    support::entry_with_search_flag("u6-row-a", false),
                );
                let actor = std::sync::Arc::new(actor);
                let guard = AuxiliaryGuard::new(b.clone());
                actor.maybe_refresh_title();
                let task = actor
                    .title_refresh_task
                    .borrow_mut()
                    .take()
                    .expect("eligible title refresh owns a task");
                tokio::time::timeout(std::time::Duration::from_secs(10), task)
                    .await
                    .unwrap()
                    .unwrap();
                let sends = guard.sends();
                assert_eq!(sends.len(), 1, "one production title send must be observed");
                assert_eq!(sends[0].0, "title");
                let request = &sends[0].1;
                assert_eq!(
                    (
                        serde_json::to_value(&request.tools).unwrap(),
                        &request.hosted_tools
                    ),
                    (serde_json::json!([]), &Vec::new()),
                    "production title send must remain completely tool-free"
                );
                assert_eq!(
                    (
                        request.model.as_deref(),
                        request.reasoning_effort,
                        request.ultra_wire_effort,
                        request.search_admission
                    ),
                    (
                        Some(a.config.model.as_str()),
                        a.config.reasoning_effort,
                        a.config.ultra_wire_effort,
                        Some(SearchAdmission {
                            supports_search_tool: true,
                            has_searchable_tools: false
                        })
                    ),
                    "production title request must use its setup binding"
                );
                assert!(server_b.requests().is_empty());
                let requests = server_a.requests();
                assert_eq!(requests.len(), 1);
                assert_eq!(requests[0].path, "/v1/messages");
                assert_eq!(
                    requests[0].authorization.as_deref(),
                    Some("Bearer u6-a-key")
                );
                let body = requests[0].body.as_ref().unwrap();
                assert_eq!(body["model"], a.config.model);
                assert!(
                    body.get("tools")
                        .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty))
                );
                assert!(actor.next_title_refresh_idx.get() > 0);
                assert!(
                    std::iter::from_fn(|| persistence.try_recv().ok()).any(|message| matches!(
                        message,
                        super::PersistenceMsg::RegenerateTitle(_)
                    ))
                );
                assert_auxiliary_binding(
                    &actor
                        .chat_state_handle
                        .capture_transport_binding()
                        .await
                        .unwrap(),
                    &b,
                );
            })
            .await;
    }

    #[derive(Debug, Clone)]
    struct AuxiliaryMcpTool {
        name: &'static str,
        description: &'static str,
    }

    impl xai_grok_tools::types::tool_metadata::ToolMetadata for AuxiliaryMcpTool {
        fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
            xai_grok_tools::types::tool::ToolKind::Other
        }
        fn tool_namespace(&self) -> xai_grok_tools::types::tool::ToolNamespace {
            xai_grok_tools::types::tool::ToolNamespace::MCP
        }
        fn description_template(&self) -> &str {
            self.description
        }
    }

    impl xai_tool_runtime::Tool for AuxiliaryMcpTool {
        type Args = serde_json::Value;
        type Output = xai_grok_tools::types::output::ToolOutput;

        fn id(&self) -> xai_tool_protocol::ToolId {
            xai_tool_protocol::ToolId::new(self.name).unwrap()
        }
        fn description(
            &self,
            _: &xai_tool_runtime::ListToolsContext,
        ) -> xai_tool_types::ToolDescription {
            xai_tool_types::ToolDescription::new(self.name, self.description)
        }
        async fn run(
            &self,
            _: xai_tool_runtime::ToolCallContext,
            _: Self::Args,
        ) -> Result<Self::Output, xai_tool_runtime::ToolError> {
            panic!("auxiliary declaration fixtures never execute tools")
        }
    }

    fn register_auxiliary_mcp(
        actor: &super::SessionActor,
    ) -> Vec<xai_grok_sampling_types::ToolSpec> {
        let mut specs = Vec::new();
        for (name, description, schema) in [
            (
                "zeta__fetch",
                "Fetch one URL with an optional limit",
                serde_json::json!({"type":"object","properties":{"url":{"type":"string"},"limit":{"type":"integer"}},"required":["url"],"additionalProperties":false}),
            ),
            (
                "alpha__lookup",
                "Look up one exact query",
                serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}),
            ),
        ] {
            actor
                .agent
                .borrow()
                .tool_bridge()
                .register_mcp_tools(
                    name.into(),
                    AuxiliaryMcpTool { name, description },
                    Some(schema.clone()),
                )
                .unwrap();
            specs.push(xai_grok_sampling_types::ToolSpec {
                name: name.into(),
                description: Some(description.into()),
                parameters: schema,
                exposure: xai_grok_sampling_types::ToolExposure::Deferred,
            });
        }
        specs.reverse();
        specs
    }

    fn auxiliary_lowered_body(
        request: &super::ConversationRequest,
        backend: &xai_grok_sampling_types::ApiBackend,
    ) -> serde_json::Value {
        use xai_grok_sampling_types::{ApiBackend, ChatCompletionRequest};
        match backend {
            ApiBackend::Messages => serde_json::to_value(
                xai_grok_sampling_types::conversation::build_messages_request(request),
            )
            .unwrap(),
            ApiBackend::Responses => {
                serde_json::to_value(xai_grok_sampling_types::rs::CreateResponse::from(request))
                    .unwrap()
            }
            ApiBackend::ChatCompletions => {
                serde_json::to_value(ChatCompletionRequest::from(request.clone())).unwrap()
            }
        }
    }

    async fn auxiliary_actor_request(
        binding: &xai_chat_state::RequestTransportBinding,
        candidates: xai_chat_state::NativeToolCandidates,
    ) -> super::ConversationRequest {
        let (events, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let cancel = tokio_util::sync::CancellationToken::new();
        let handle = xai_chat_state::ChatStateActor::spawn(
            vec![super::ConversationItem::user("auxiliary parity")],
            binding.config.clone(),
            Box::new(xai_chat_state::NullChatPersistence),
            events,
            cancel.clone(),
        );
        handle
            .install_transport_binding(binding.clone())
            .await
            .unwrap();
        let (request, actual_binding) = handle
            .build_request_with_binding(
                candidates,
                None,
                false,
                None,
                "aux-parity-conv".into(),
                "aux-parity-req".into(),
            )
            .await
            .unwrap();
        cancel.cancel();
        assert_auxiliary_binding(&actual_binding, binding);
        request
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auxiliary_candidates_use_captured_binding_selector() {
        use xai_grok_sampling_types::{
            ApiBackend, ConversationItem, SearchAdmission, ToolExposure,
        };
        tokio::task::LocalSet::new().run_until(async {
            for (backend, flag, next_backend, next_flag) in [
                (ApiBackend::Messages, true, ApiBackend::Messages, false),
                (ApiBackend::Messages, true, ApiBackend::Responses, false),
                (ApiBackend::Messages, false, ApiBackend::Messages, true),
                (ApiBackend::Responses, true, ApiBackend::Messages, false),
                (ApiBackend::Responses, false, ApiBackend::Messages, true),
                (ApiBackend::ChatCompletions, true, ApiBackend::Messages, false),
                (ApiBackend::ChatCompletions, false, ApiBackend::Messages, true),
            ] {
                for backend_search in [false, true] {
                    let mut fixture = auxiliary_fixture(backend.clone(), flag).await;
                    let mut control = auxiliary_fixture(backend.clone(), flag).await;
                    for actor in [&fixture.actor, &control.actor] {
                        use xai_grok_tools::implementations::grok_build::{update_goal::UpdateGoalTool, web_search::WebSearchTool};
                        use xai_grok_tools::registry::types::ToolConfig;
                        let agent = support::test_agent_with_tools(vec![
                            ToolConfig::for_tool::<UpdateGoalTool>(),
                            ToolConfig::for_tool::<WebSearchTool>(),
                        ]).await;
                        *actor.agent.borrow_mut() = xai_grok_agent::Agent::new(
                            agent.definition().clone(), agent.prompt_context().clone(),
                            agent.system_prompt().to_string(), std::sync::Arc::clone(agent.tool_bridge()),
                            agent.reminder_policy().clone(), agent.compaction_policy().clone(),
                            vec![xai_grok_sampling_types::HostedTool::WebSearch { options: None }], true,
                        );
                    }
                    fixture.b.config.api_backend = next_backend.clone();
                    fixture.b.config.supports_search_tool = next_flag;
                    fixture.actor.supports_backend_search.set(backend_search);
                    control.actor.supports_backend_search.set(backend_search);
                    control.actor.session_info.id = fixture.actor.session_info.id.clone();
                    let mcp = register_auxiliary_mcp(&fixture.actor);
                    let items = vec![ConversationItem::user("auxiliary selection question")];
                    let baseline_defs = control.actor.prepare_tool_definitions().await;
                    let baseline_tools = control.actor.turn_base_tool_specs(&baseline_defs);
                    let baseline_setup = control.actor.prepare_side_call().await.unwrap();
                    let baseline = control.actor.side_call_request(
                        &baseline_setup, items.clone(), "aux-selection-conv".into(), "aux-selection-req".into(),
                    ).await;
                    assert_eq!(serde_json::to_value(&baseline.tools).unwrap(), serde_json::to_value(&baseline_tools).unwrap(),
                        "U6 isolated builtin request must retain the original non-MCP preparation snapshot");
                    assert!(baseline.tools.iter().any(|tool| tool.exposure == ToolExposure::Deferred));
                    assert_eq!(baseline.tools.iter().any(|tool| tool.name == "web_search"), !backend_search);
                    assert_eq!(!baseline.hosted_tools.is_empty(), backend_search);
                    let (prepared, _) = fixture.actor.prepare_native_tool_candidates_timed().await;
                    let candidates = fixture.actor.turn_native_tool_candidates(&prepared.builtin_defs, &prepared.mcp_specs);
                    let eligible = backend == ApiBackend::Messages && flag;
                    let expected: Vec<_> = baseline.tools.iter().cloned()
                        .chain(if eligible { mcp.clone() } else { Vec::new() }).collect();
                    assert_eq!(serde_json::to_value(candidates.clone().select_for_row(&fixture.a.config)).unwrap(), serde_json::to_value(&expected).unwrap());
                    let history = fixture.actor.chat_state_handle.get_conversation().await;
                    let _guard = AuxiliaryGuard::new(fixture.b.clone());
                    let setup = fixture.actor.prepare_side_call().await.unwrap();
                    assert_auxiliary_binding(&setup.binding, &fixture.a);
                    assert_eq!((setup.model.as_str(), setup.client.api_backend(), setup.context_window, setup.reasoning_effort, setup.ultra_wire_effort, setup.strip_reasoning),
                        (baseline_setup.model.as_str(), baseline_setup.client.api_backend(), baseline_setup.context_window, baseline_setup.reasoning_effort, baseline_setup.ultra_wire_effort, baseline_setup.strip_reasoning));
                    let actual = fixture.actor.side_call_request(
                        &setup, items, "aux-selection-conv".into(), "aux-selection-req".into(),
                    ).await;
                    setup.client.conversation_collect(actual.clone()).await.unwrap();
                    baseline_setup.client.conversation_collect(baseline.clone()).await.unwrap();
                    assert_auxiliary_wire(&fixture, &fixture.a.config.model);
                    assert_auxiliary_wire(&control, &control.a.config.model);
                    let non_mcp: Vec<_> = actual.tools.iter().filter(|tool| !mcp.iter().any(|m| m.name == tool.name)).cloned().collect();
                    assert_eq!(serde_json::to_value(&non_mcp).unwrap(), serde_json::to_value(&baseline.tools).unwrap(),
                        "U6 non-MCP preparation snapshot must not be re-resolved on the captured or live row");
                    assert_eq!(serde_json::to_value(&actual.tools).unwrap(), serde_json::to_value(&expected).unwrap(),
                        "U6 auxiliary captured MCP specs must retain exact qualified schemas and Deferred suffix only on captured Messages-on");
                    assert_eq!(actual.search_admission, Some(SearchAdmission::for_row(flag, &expected)),
                        "U6 auxiliary admission must use the captured flag and actual selected tools");
                    let actor_request = auxiliary_actor_request(&setup.binding, candidates).await;
                    assert_eq!((serde_json::to_value(&actual.tools).unwrap(), actual.search_admission),
                        (serde_json::to_value(&actor_request.tools).unwrap(), actor_request.search_admission),
                        "U6 auxiliary and actor paths must share complete selection and admission");
                    let mut expected_request = baseline.clone();
                    expected_request.tools = expected;
                    assert_eq!(format!("{actual:?}"), format!("{expected_request:?}"),
                        "U6 selected tools are the only change from the isolated builtin request");
                    assert_eq!(auxiliary_lowered_body(&actual, &backend), auxiliary_lowered_body(&expected_request, &backend));
                    let body = fixture.server_a.requests()[0].body.clone().unwrap();
                    let control_body = control.server_a.requests()[0].body.clone().unwrap();
                    if eligible {
                        let baseline_tools = control_body["tools"].as_array().unwrap();
                        let mut expected_tools = baseline_tools.clone();
                        expected_tools.splice(baseline.tools.len()..baseline.tools.len(), mcp.iter().map(|spec| serde_json::json!({
                            "name": spec.name, "description": spec.description,
                            "input_schema": spec.parameters, "defer_loading": true,
                        })));
                        assert_eq!(body["tools"], serde_json::to_value(expected_tools).unwrap(),
                            "U6 actual Messages send preserves complete builtin objects and exact qualified MCP wire declarations");
                        assert_eq!(baseline_tools.iter().filter(|tool| tool["name"] == "DeferredToolPlaceholder").count(), 1);
                        assert!(baseline.search_admission.unwrap().admitted());
                    } else {
                        assert_eq!(format!("{actual:?}"), format!("{baseline:?}"));
                        assert_eq!(auxiliary_lowered_body(&actual, &backend), auxiliary_lowered_body(&baseline, &backend));
                        assert_eq!(body["tools"], control_body["tools"], "U6 excluded MCP rows retain the entire legacy wire tool surface");
                    }
                    assert_eq!(serde_json::to_value(fixture.actor.chat_state_handle.get_conversation().await).unwrap(), serde_json::to_value(history).unwrap());
                    assert_auxiliary_binding(&fixture.actor.chat_state_handle.capture_transport_binding().await.unwrap(), &fixture.b);
                }
            }
        }).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_candidates_do_not_admit_title() {
        use xai_grok_sampling_types::{ApiBackend, SearchAdmission};
        tokio::task::LocalSet::new().run_until(async {
            let fixture = auxiliary_fixture(ApiBackend::Messages, true).await;
            let mcp = register_auxiliary_mcp(&fixture.actor);
            let (prepared, _) = fixture.actor.prepare_native_tool_candidates_timed().await;
            let candidates = fixture.actor.turn_native_tool_candidates(&prepared.builtin_defs, &prepared.mcp_specs);
            let selected = candidates.select_for_row(&fixture.a.config);
            assert_eq!(serde_json::to_value(&selected[selected.len() - mcp.len()..]).unwrap(), serde_json::to_value(&mcp).unwrap(),
                "title exclusion must have registered, eligible MCP candidates to exclude");
            assert!(!fixture.actor.hosted_tools_for_turn().is_empty());
            let auxiliary_setup = fixture.actor.prepare_side_call().await.unwrap();
            let auxiliary_request = fixture.actor.side_call_request(
                &auxiliary_setup, vec![super::ConversationItem::user("title exclusion control")],
                "title-control-conv".into(), "title-control-req".into(),
            ).await;
            let AuxiliaryFixture { actor, a, b, server_a, server_b, mut persistence, home: _home } = fixture;
            let history = actor.chat_state_handle.get_conversation().await;
            let actor = std::sync::Arc::new(actor);
            let guard = AuxiliaryGuard::new(b.clone());
            actor.maybe_refresh_title();
            let task = actor.title_refresh_task.borrow_mut().take().expect("eligible title lifecycle owns its task");
            tokio::time::timeout(std::time::Duration::from_secs(10), task).await.unwrap().unwrap();
            let sends = guard.sends();
            assert_eq!(sends.len(), 1);
            assert_eq!(sends[0].0, "title");
            let request = &sends[0].1;
            assert_eq!((serde_json::to_value(&request.tools).unwrap(), &request.hosted_tools), (serde_json::json!([]), &Vec::new()),
                "U6 actual title must stay empty despite eligible registered MCP candidates");
            assert_eq!(request.search_admission, Some(SearchAdmission { supports_search_tool: true, has_searchable_tools: false }),
                "U6 actual title must remain unadmitted on captured Messages-on");
            assert_eq!((request.model.as_deref(), request.reasoning_effort, request.ultra_wire_effort),
                (Some(a.config.model.as_str()), a.config.reasoning_effort, a.config.ultra_wire_effort));
            assert_eq!(request.prompt_cache_key.as_deref(), Some(actor.session_info.id.0.as_ref()));
            assert_eq!(request.x_grok_conv_id, request.prompt_cache_key);
            assert!(server_b.requests().is_empty());
            let wire = server_a.requests();
            assert_eq!(wire.len(), 1);
            assert_eq!(wire[0].path, "/v1/messages");
            assert_eq!(wire[0].authorization.as_deref(), Some("Bearer u6-a-key"));
            assert_eq!(wire[0].header("x-u6-row"), Some("a"));
            let body = wire[0].body.as_ref().unwrap();
            assert_eq!(body["model"], a.config.model);
            assert!(body.get("tools").is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty)),
                "U6 actual title wire must carry neither MCP nor native search declarations");
            assert!(actor.next_title_refresh_idx.get() > 0);
            assert!(std::iter::from_fn(|| persistence.try_recv().ok()).any(|message| matches!(message, super::PersistenceMsg::RegenerateTitle(_))));
            assert_eq!(serde_json::to_value(actor.chat_state_handle.get_conversation().await).unwrap(), serde_json::to_value(history).unwrap());
            assert_auxiliary_binding(&actor.chat_state_handle.capture_transport_binding().await.unwrap(), &b);
            assert!(actor.title_refresh_task.borrow().is_none());
            assert_eq!(serde_json::to_value(&auxiliary_request.tools).unwrap(), serde_json::to_value(&selected).unwrap(),
                "U6 title exclusion must contrast with the real candidate-bearing auxiliary request");
        }).await;
    }

    // The config home is process-cached; isolate before any actor reads it.
    fn parent_golden_isolated(name: &str) -> bool {
        const CHILD: &str = "GROK_TEST_3V1S_PARENT_ISOLATED";
        if std::env::var(CHILD).as_deref() == Ok(name) {
            return false;
        }
        let home = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home.path())
            .env("GROK_HOME", home.path().join("grok"))
            .env("TMPDIR", home.path())
            .env("RUST_MIN_STACK", "67108864")
            .env(CHILD, name)
            .args(["--exact", name, "--nocapture"])
            .stdin(std::process::Stdio::null());
        for variable in ["GROK_TOOLS_BUNDLE_RG_PATH", "GROK_SHELL_BUNDLE_RG_PATH"] {
            if let Some(value) = std::env::var_os(variable) {
                command.env(variable, value);
            }
        }
        xai_tty_utils::detach_std_command(&mut command);
        let output = command.output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(stdout.matches("running 1 test").count(), 1, "{stdout}");
        assert!(
            output.status.success(),
            "isolated parent witness failed: {stdout}\n{stderr}"
        );
        assert!(stdout.contains(&format!("test {name} ... ok")), "{stdout}");
        true
    }

    #[derive(Clone)]
    struct ParentGoldenExchange {
        path: String,
        request_id: Option<String>,
        headers: Vec<(String, bool)>,
        raw_body: Vec<u8>,
        parsed_body: serde_json::Value,
        response_body: String,
    }

    struct ParentGoldenWire {
        url: String,
        exchanges: std::rc::Rc<std::cell::RefCell<Vec<ParentGoldenExchange>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for ParentGoldenWire {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl ParentGoldenWire {
        async fn start() -> Self {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let url = format!("http://{}/v1", listener.local_addr().unwrap());
            let exchanges = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let captured = exchanges.clone();
            let task = tokio::task::spawn_local(async move {
                for _ in 0..8 {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut received = Vec::new();
                    let mut buffer = [0; 8192];
                    let header_end = loop {
                        let n = socket.read(&mut buffer).await.unwrap();
                        assert_ne!(n, 0, "complete HTTP headers required");
                        received.extend_from_slice(&buffer[..n]);
                        assert!(received.len() <= 2 * 1024 * 1024);
                        if let Some(index) =
                            received.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                        {
                            break index + 4;
                        }
                    };
                    let headers = std::str::from_utf8(&received[..header_end]).unwrap();
                    let mut lines = headers.lines();
                    let first = lines.next().unwrap();
                    let mut request_line = first.split_whitespace();
                    assert_eq!(request_line.next(), Some("POST"));
                    let path = request_line.next().unwrap().to_owned();
                    let mut length = None;
                    let mut request_id = None;
                    let mut header_flags = Vec::new();
                    for line in lines.filter(|line| !line.is_empty()) {
                        let (name, value) = line.split_once(':').unwrap();
                        header_flags.push((name.to_ascii_lowercase(), value.trim().is_empty()));
                        if name.eq_ignore_ascii_case("content-length") {
                            length = Some(value.trim().parse::<usize>().unwrap());
                        }
                        if name.eq_ignore_ascii_case("x-grok-req-id") {
                            request_id = Some(value.trim().to_owned());
                        }
                        assert!(!name.eq_ignore_ascii_case("transfer-encoding"));
                    }
                    let length = length.expect("encoded request has a Content-Length");
                    assert!(length <= 2 * 1024 * 1024);
                    while received.len() < header_end + length {
                        let n = socket.read(&mut buffer).await.unwrap();
                        assert_ne!(n, 0, "complete HTTP body required");
                        received.extend_from_slice(&buffer[..n]);
                    }
                    // Keep exactly the received entity bytes before any JSON parsing.
                    let raw_body = received[header_end..header_end + length].to_vec();
                    let parsed_body: serde_json::Value = serde_json::from_slice(&raw_body).unwrap();
                    let response_body = parent_golden_response(&path, &parsed_body);
                    captured.borrow_mut().push(ParentGoldenExchange {
                        path,
                        request_id,
                        headers: header_flags,
                        raw_body,
                        parsed_body,
                        response_body: response_body.clone(),
                    });
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\nx-request-id: parent-capture-response\r\n\r\n{response_body}",
                        response_body.len(),
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                    socket.shutdown().await.unwrap();
                }
            });
            Self {
                url,
                exchanges,
                task,
            }
        }
    }

    fn parent_golden_response(path: &str, body: &serde_json::Value) -> String {
        let model = body["model"].as_str().unwrap();
        if path == "/v1/responses" {
            return xai_grok_test_support::sse::responses_api_script_exact(
                "Parent capture complete",
                model,
            )
            .into_iter()
            .map(|event| format!("data: {}\n\n", event.data))
            .collect();
        }
        assert_eq!(path, "/v1/messages");
        let events = vec![
            serde_json::json!({"type":"message_start","message":{"id":"msg_parent_capture","type":"message","role":"assistant","content":[],"model":model,"stop_reason":null,"usage":{"input_tokens":10,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}),
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Parent capture complete"}}),
            serde_json::json!({"type":"content_block_stop","index":0}),
            serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5,"input_tokens":10}}),
            serde_json::json!({"type":"message_stop"}),
        ];
        events
            .into_iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect()
    }

    fn parent_golden_record(case: &str, value: serde_json::Value) {
        let frozen = &value["frozen"];
        let expected_raw = match case {
            "btw/chat-admission-off"
            | "btw/chat-admission-on"
            | "btw/messages-admission-off"
            | "btw/responses-admission-off"
            | "btw/responses-admission-on"
            | "btw/same-bound-row" => Some(
                r####"{"model":"r12-parent","messages":[{"role":"user","content":[{"type":"text","text":"Find the parent schema"}]},{"role":"assistant","content":[{"type":"tool_use","id":"call_parent_capture","name":"search_tool","input":{"query":"parent schema"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_parent_capture","content":[{"type":"tool_reference","tool_name":"historic__lookup"}]}]},{"role":"assistant","content":[{"type":"tool_use","id":"srvtoolu_parent_capture","name":"tool_search_tool_bm25","input":{"query":"server schema"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"srvtoolu_parent_capture","content":[{"type":"tool_reference","tool_name":"alpha__lookup"}]}]},{"role":"assistant","content":[{"type":"text","text":"Found both schema families"}]},{"role":"user","content":[{"type":"text","text":"Check the second step"}]},{"role":"assistant","content":[{"type":"text","text":"Second step complete"}]},{"role":"user","content":[{"type":"text","text":"Check the third step","cache_control":{"type":"ephemeral"}}]},{"role":"assistant","content":[{"type":"text","text":"Third step complete"}]},{"role":"user","content":[{"type":"text","text":"<system-reminder>This is a side question from the user. You must answer this question directly in a single response.\n\nIMPORTANT CONTEXT:\n- You are a separate, lightweight agent spawned to answer this one question\n- The main agent is NOT interrupted - it continues working independently in the background\n- You share the conversation context but are a completely separate instance\n- Do NOT reference being interrupted or what you were \"previously doing\" - that framing is incorrect\n\nCRITICAL CONSTRAINTS:\n- Do NOT call any tools, respond with plain text only\n- A tool call cannot help you: nothing runs on the user's machine and you get no turn in which to read a result\n- This is a one-off response - there will be no follow-up turns\n- You can ONLY provide information based on what you already know from the conversation context\n- NEVER say things like \"Let me try...\", \"I'll now...\", \"Let me check...\", or promise to take any action\n- If you don't know the answer, say so - do not offer to look it up or investigate\n\nSimply answer the question with the information you have.</system-reminder>\n\nWhat remains after discovery?","cache_control":{"type":"ephemeral"}}]}],"max_tokens":512,"system":[{"type":"text","text":"Parent capture system"},{"type":"text","text":"The following deferred tools are now available via search_tool. Their schemas are NOT loaded — calling them directly will fail with InputValidationError. Use search_tool with query \"select:<name>[,<name>...]\" to load tool schemas before calling them:\nhistoric__lookup","cache_control":{"type":"ephemeral"}}],"tools":[{"name":"search_tool","description":"Fetches full schema definitions for deferred tools so they can be called.\n\nDeferred tools appear by name in <system-reminder> messages. Until fetched, only the name is known — there is no parameter schema, so the tool cannot be invoked. This tool takes a query, matches it against the deferred tool list, and returns the matched tools' complete JSONSchema definitions inside a <functions> block. Once a tool's schema appears in that result, it is callable exactly like any tool defined at the top of the prompt.\n\nResult format: each matched tool appears as one <function>{\"description\": \"...\", \"name\": \"...\", \"parameters\": {...}}</function> line inside the <functions> block — the same encoding as the tool list at the top of this prompt.\n\nQuery forms:\n- \"select:Read,Edit,Grep\" — fetch these exact tools by name\n- \"notebook jupyter\" — keyword search, up to max_results best matches\n- \"+slack send\" — require \"slack\" in the name, rank by remaining terms","input_schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"query":{"description":"Query to find deferred tools. Use \"select:<tool_name>\" for direct selection, or keywords to search.","type":"string"},"max_results":{"description":"Maximum number of results to return (default: 5)","default":5,"type":"number"}},"required":["query","max_results"],"additionalProperties":false}},{"name":"use_tool","description":"Call an MCP integration tool.\n\nThe `tool_name` must be the qualified `server__tool` name (e.g., `linear__save_issue`). The `tool_input` must conform exactly to the tool's input schema as returned by `search_tool`.","input_schema":{"$schema":"http://json-schema.org/draft-07/schema#","required":["tool_name","tool_input"],"properties":{"tool_name":{"description":"The qualified name of the integration tool to call (e.g., \"linear__save_issue\").\nMust be a tool previously discovered via `search_tool`.","type":"string"},"tool_input":{"description":"The arguments to pass to the tool, as a JSON object.\nUse the parameter schema returned by `search_tool` to construct this.","type":"object","additionalProperties":true}},"type":"object"}},{"name":"z_builtin","description":"z_builtin","input_schema":{"type":"object","properties":{"value":{"type":"string","description":"z_builtin"}},"required":["value"],"additionalProperties":false},"defer_loading":true},{"name":"builtin__renamed","description":"builtin__renamed","input_schema":{"type":"object","properties":{"value":{"type":"string","description":"builtin__renamed"}},"required":["value"],"additionalProperties":false}},{"name":"policy__root_message","description":"policy__root_message","input_schema":{"type":"object","properties":{"value":{"type":"string","description":"policy__root_message"}},"required":["value"],"additionalProperties":false}},{"name":"alpha__lookup","description":"alpha__lookup","input_schema":{"type":"object","properties":{"value":{"type":"string","description":"alpha__lookup"}},"required":["value"],"additionalProperties":false},"defer_loading":true},{"name":"zeta__fetch","description":"zeta__fetch","input_schema":{"type":"object","properties":{"value":{"type":"string","description":"zeta__fetch"}},"required":["value"],"additionalProperties":false},"defer_loading":true},{"name":"DeferredToolPlaceholder","description":"Reserved placeholder that keeps deferred tool loading active; never call this tool.","input_schema":{"type":"object","properties":{}},"defer_loading":true},{"name":"historic__lookup","description":"Frozen historical lookup","input_schema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false},"defer_loading":true},{"type":"tool_search_tool_bm25_20251119","name":"tool_search_tool_bm25"}],"stream":true}"####,
            ),
            "turn-summary/chat-admission-off"
            | "turn-summary/chat-admission-on"
            | "turn-summary/messages-admission-off"
            | "turn-summary/responses-admission-off"
            | "turn-summary/responses-admission-on"
            | "turn-summary/same-bound-row" => Some(
                r####"{"model":"r12-parent","messages":[{"role":"user","content":[{"type":"text","text":"Find the parent schema"}]},{"role":"assistant","content":[{"type":"tool_use","id":"call_parent_capture","name":"search_tool","input":{"query":"parent schema"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_parent_capture","content":[{"type":"tool_reference","tool_name":"historic__lookup"}]}]},{"role":"assistant","content":[{"type":"tool_use","id":"srvtoolu_parent_capture","name":"tool_search_tool_bm25","input":{"query":"server schema"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"srvtoolu_parent_capture","content":[{"type":"tool_reference","tool_name":"alpha__lookup"}]}]},{"role":"assistant","content":[{"type":"text","text":"Found both schema families"}]},{"role":"user","content":[{"type":"text","text":"Check the second step"}]},{"role":"assistant","content":[{"type":"text","text":"Second step complete"}]},{"role":"user","content":[{"type":"text","text":"Check the third step","cache_control":{"type":"ephemeral"}}]},{"role":"assistant","content":[{"type":"text","text":"Third step complete"}]},{"role":"user","content":[{"type":"text","text":"<system-reminder>Write an ultra-short dashboard line that captures the AGENT'S REPLY for the last turn only — everything after the user message beginning: \"Check the third step\". Focus on what the assistant concluded, answered, recommended, or delivered — not a meta description of the turn (avoid \"Explained…\", \"Answered…\", \"Greeted…\", \"Reviewed…\"). User-role messages wrapped in reminder tags like this one are injected context, not the user.\n\nOutput ONLY the fragment: 5-12 words, plain text, glanceable on a status row. Prefer the payload: answer, finding, change, or decision needed. Do NOT call any tools — respond with plain text only.\n\nSynthetic examples (style only — adapt to THIS turn, do not copy):\n`queue_worker` shutdown race fixed; suite green\nPayment retries: exp backoff in `billing/retry.rs`, 5× on 429\nRetry backoff wired into `billing/retry.rs`; tests pending\nNeed decision: keep or drop `sqlx` cache before refactor\nBlack — matches the terminal aesthetic\n\nBad (never):\n- Lead with Explained / Answered / Greeted / Reviewed / Confirmed / Flagged / Summarized\n- Labels, quotes, bullets, markdown, code fences, multi-sentence dumps\n- Filler like \"no code changes\" or \"awaiting task\" unless that is the whole point\n- Summarize earlier turns or the whole session\n- Call tools or invent content not in the agent's reply</system-reminder>","cache_control":{"type":"ephemeral"}}]}],"max_tokens":512,"system":[{"type":"text","text":"Parent capture system"},{"type":"text","text":"The following deferred tools are now available via search_tool. Their schemas are NOT loaded — calling them directly will fail with InputValidationError. Use search_tool with query \"select:<name>[,<name>...]\" to load tool schemas before calling them:\nhistoric__lookup","cache_control":{"type":"ephemeral"}}],"tools":[{"name":"search_tool","description":"Fetches full schema definitions for deferred tools so they can be called.\n\nDeferred tools appear by name in <system-reminder> messages. Until fetched, only the name is known — there is no parameter schema, so the tool cannot be invoked. This tool takes a query, matches it against the deferred tool list, and returns the matched tools' complete JSONSchema definitions inside a <functions> block. Once a tool's schema appears in that result, it is callable exactly like any tool defined at the top of the prompt.\n\nResult format: each matched tool appears as one <function>{\"description\": \"...\", \"name\": \"...\", \"parameters\": {...}}</function> line inside the <functions> block — the same encoding as the tool list at the top of this prompt.\n\nQuery forms:\n- \"select:Read,Edit,Grep\" — fetch these exact tools by name\n- \"notebook jupyter\" — keyword search, up to max_results best matches\n- \"+slack send\" — require \"slack\" in the name, rank by remaining terms","input_schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"query":{"description":"Query to find deferred tools. Use \"select:<tool_name>\" for direct selection, or keywords to search.","type":"string"},"max_results":{"description":"Maximum number of results to return (default: 5)","default":5,"type":"number"}},"required":["query","max_results"],"additionalProperties":false}},{"name":"use_tool","description":"Call an MCP integration tool.\n\nThe `tool_name` must be the qualified `server__tool` name (e.g., `linear__save_issue`). The `tool_input` must conform exactly to the tool's input schema as returned by `search_tool`.","input_schema":{"$schema":"http://json-schema.org/draft-07/schema#","required":["tool_name","tool_input"],"properties":{"tool_name":{"description":"The qualified name of the integration tool to call (e.g., \"linear__save_issue\").\nMust be a tool previously discovered via `search_tool`.","type":"string"},"tool_input":{"description":"The arguments to pass to the tool, as a JSON object.\nUse the parameter schema returned by `search_tool` to construct this.","type":"object","additionalProperties":true}},"type":"object"}},{"name":"z_builtin","description":"z_builtin","input_schema":{"type":"object","properties":{"value":{"type":"string","description":"z_builtin"}},"required":["value"],"additionalProperties":false},"defer_loading":true},{"name":"builtin__renamed","description":"builtin__renamed","input_schema":{"type":"object","properties":{"value":{"type":"string","description":"builtin__renamed"}},"required":["value"],"additionalProperties":false}},{"name":"policy__root_message","description":"policy__root_message","input_schema":{"type":"object","properties":{"value":{"type":"string","description":"policy__root_message"}},"required":["value"],"additionalProperties":false}},{"name":"alpha__lookup","description":"alpha__lookup","input_schema":{"type":"object","properties":{"value":{"type":"string","description":"alpha__lookup"}},"required":["value"],"additionalProperties":false},"defer_loading":true},{"name":"zeta__fetch","description":"zeta__fetch","input_schema":{"type":"object","properties":{"value":{"type":"string","description":"zeta__fetch"}},"required":["value"],"additionalProperties":false},"defer_loading":true},{"name":"DeferredToolPlaceholder","description":"Reserved placeholder that keeps deferred tool loading active; never call this tool.","input_schema":{"type":"object","properties":{}},"defer_loading":true},{"name":"historic__lookup","description":"Frozen historical lookup","input_schema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false},"defer_loading":true},{"type":"tool_search_tool_bm25_20251119","name":"tool_search_tool_bm25"}],"stream":true}"####,
            ),
            "title-Responses/chat-admission-off"
            | "title-Responses/chat-admission-on"
            | "title-Responses/messages-admission-off"
            | "title-Responses/responses-admission-off"
            | "title-Responses/responses-admission-on"
            | "title-Responses/same-bound-row" => Some(
                r####"{"include":["reasoning.encrypted_content"],"input":[{"type":"message","role":"system","content":"Parent capture system"},{"type":"message","role":"user","content":"Find the parent schema"},{"type":"message","role":"assistant","content":"[tool_search] \"parent schema\""},{"type":"message","role":"assistant","content":"[tool_search results] 1 tool"},{"type":"message","role":"assistant","content":"[tool_search] \"server schema\""},{"type":"message","role":"assistant","content":"[tool_search results] 1 tool"},{"type":"message","role":"assistant","content":"Found both schema families"},{"type":"message","role":"user","content":"Check the second step"},{"type":"message","role":"assistant","content":"Second step complete"},{"type":"message","role":"user","content":"Check the third step"},{"type":"message","role":"assistant","content":"Third step complete"},{"type":"message","role":"user","content":"<system-reminder>Generate a session title for the conversation above. It should be a short and distinctive 5-10 word descriptive title capturing what this session is actually about (the main task or topic), based on the WHOLE conversation — not just the first message. Super info dense, no filler. User-role messages wrapped in reminder tags like this one are injected context, not the user.\n\nOutput ONLY the title: plain text, no quotes, no labels, no markdown. Do NOT call any tools — respond with plain text only.</system-reminder>"},{"type":"message","role":"developer","content":[{"type":"input_text","text":"<multi_agent_mode>Multi-agent mode: explicit_request_only — subagents may be spawned only when the user explicitly requests.</multi_agent_mode>"}]}],"max_output_tokens":512,"model":"r12-parent","prompt_cache_key":"test-actor","reasoning":{"summary":"concise"},"store":false,"stream":true}"####,
            ),
            "title-Messages/chat-admission-off"
            | "title-Messages/chat-admission-on"
            | "title-Messages/messages-admission-off"
            | "title-Messages/responses-admission-off"
            | "title-Messages/responses-admission-on"
            | "title-Messages/same-bound-row" => Some(
                r####"{"model":"r12-parent","messages":[{"role":"user","content":[{"type":"text","text":"Find the parent schema"}]},{"role":"assistant","content":[{"type":"text","text":"[tool_search] \"parent schema\""},{"type":"text","text":"[tool_search results] 1 tool"},{"type":"text","text":"[tool_search results] 1 tool"},{"type":"text","text":"Found both schema families"}]},{"role":"user","content":[{"type":"text","text":"Check the second step"}]},{"role":"assistant","content":[{"type":"text","text":"Second step complete"}]},{"role":"user","content":[{"type":"text","text":"Check the third step","cache_control":{"type":"ephemeral"}}]},{"role":"assistant","content":[{"type":"text","text":"Third step complete"}]},{"role":"user","content":[{"type":"text","text":"<system-reminder>Generate a session title for the conversation above. It should be a short and distinctive 5-10 word descriptive title capturing what this session is actually about (the main task or topic), based on the WHOLE conversation — not just the first message. Super info dense, no filler. User-role messages wrapped in reminder tags like this one are injected context, not the user.\n\nOutput ONLY the title: plain text, no quotes, no labels, no markdown. Do NOT call any tools — respond with plain text only.</system-reminder>","cache_control":{"type":"ephemeral"}}]}],"max_tokens":512,"system":[{"type":"text","text":"Parent capture system","cache_control":{"type":"ephemeral"}}],"tools":[{"name":"historic__lookup","description":"Frozen historical lookup","input_schema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false},"defer_loading":true}],"stream":true}"####,
            ),
            _ => None,
        };
        if let Some(expected_raw) = expected_raw {
            let expected: serde_json::Value = serde_json::from_str(expected_raw).unwrap();
            assert_eq!(
                frozen["raw_body"].as_str(),
                Some(expected_raw),
                "frozen parent encoded body: {case}"
            );
            assert_eq!(
                frozen["parsed_body"], expected,
                "frozen parent parsed body: {case}"
            );
            return;
        }
        let expected = match case {
            _ => panic!("no valid frozen parent outcome for {case}"),
        };
        let expected: serde_json::Value = serde_json::from_str(expected).unwrap();
        assert_eq!(*frozen, expected, "frozen parent lifecycle outcome: {case}");
    }

    #[derive(Clone, Debug)]
    struct ParentGoldenTool {
        name: &'static str,
        namespace: xai_grok_tools::types::tool::ToolNamespace,
        kind: xai_grok_tools::types::tool::ToolKind,
    }

    impl xai_grok_tools::types::tool_metadata::ToolMetadata for ParentGoldenTool {
        fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
            self.kind
        }
        fn tool_namespace(&self) -> xai_grok_tools::types::tool::ToolNamespace {
            self.namespace
        }
        fn description_template(&self) -> &str {
            self.name
        }
    }

    impl xai_tool_runtime::Tool for ParentGoldenTool {
        type Args = serde_json::Value;
        type Output = xai_grok_tools::types::output::ToolOutput;
        fn id(&self) -> xai_tool_protocol::ToolId {
            xai_tool_protocol::ToolId::new(self.name).unwrap()
        }
        fn description(
            &self,
            _: &xai_tool_runtime::ListToolsContext,
        ) -> xai_tool_types::ToolDescription {
            xai_tool_types::ToolDescription::new(self.name, self.name)
        }
        async fn run(
            &self,
            _: xai_tool_runtime::ToolCallContext,
            _: Self::Args,
        ) -> Result<Self::Output, xai_tool_runtime::ToolError> {
            panic!("parent capture never executes a declared fixture tool")
        }
    }

    fn parent_golden_register(
        actor: &crate::session::acp_session::SessionActor,
        name: &'static str,
        namespace: xai_grok_tools::types::tool::ToolNamespace,
        kind: xai_grok_tools::types::tool::ToolKind,
    ) {
        let schema = serde_json::json!({"type":"object","properties":{"value":{"type":"string","description":name}},"required":["value"],"additionalProperties":false});
        actor
            .agent
            .borrow()
            .tool_bridge()
            .register_mcp_tools(
                name.into(),
                ParentGoldenTool {
                    name,
                    namespace,
                    kind,
                },
                Some(schema),
            )
            .unwrap();
    }

    fn parent_golden_history() -> Vec<xai_grok_sampling_types::ConversationItem> {
        use xai_grok_sampling_types::ConversationItem;
        use xai_grok_sampling_types::conversation::tool_search::ToolSearchItem;
        let raw = vec![
            serde_json::json!({"type":"tool_search_call","id":"tsc_parent_capture","call_id":"call_parent_capture","execution":"client","status":"completed","arguments":{"query":"parent schema"}}),
            serde_json::json!({"type":"tool_search_output","id":"tso_parent_capture","call_id":"call_parent_capture","execution":"client","status":"completed","tools":[{"type":"function","name":"historic__lookup","description":"Frozen historical lookup","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}}]}),
            serde_json::json!({"type":"server_tool_use","id":"srvtoolu_parent_capture","name":"tool_search_tool_bm25","input":{"query":"server schema"},"status":"completed"}),
            serde_json::json!({"type":"tool_search_tool_result","tool_use_id":"srvtoolu_parent_capture","content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":"alpha__lookup"}]},"status":"completed"}),
        ];
        let discovery: Vec<_> = raw
            .into_iter()
            .map(|raw| ToolSearchItem::from_wire(raw).unwrap())
            .collect();
        assert_eq!(
            xai_grok_sampling_types::conversation::tool_search::partner_indices(&discovery),
            vec![Some(1), Some(0), Some(3), Some(2)]
        );
        let mut history = vec![
            ConversationItem::system("Parent capture system"),
            ConversationItem::user("Find the parent schema"),
        ];
        history.extend(
            discovery
                .into_iter()
                .map(|item| ConversationItem::Discovery { item }),
        );
        history.extend([
            ConversationItem::assistant("Found both schema families"),
            ConversationItem::user("Check the second step"),
            ConversationItem::assistant("Second step complete"),
            ConversationItem::user("Check the third step"),
            ConversationItem::assistant("Third step complete"),
        ]);
        history
    }

    struct ParentGoldenFixture {
        actor: std::sync::Arc<crate::session::acp_session::SessionActor>,
        binding: xai_chat_state::RequestTransportBinding,
        wire: ParentGoldenWire,
        live_wire: ParentGoldenWire,
        history: Vec<xai_grok_sampling_types::ConversationItem>,
        _home: tempfile::TempDir,
    }

    async fn parent_golden_fixture(
        backend: xai_grok_sampling_types::ApiBackend,
    ) -> ParentGoldenFixture {
        use crate::session::acp_session::support;
        use xai_grok_tools::types::tool::{ToolKind, ToolNamespace};
        let wire = ParentGoldenWire::start().await;
        let live_wire = ParentGoldenWire::start().await;
        let (gateway_tx, gateway_rx) = tokio::sync::mpsc::unbounded_channel();
        let (persistence_tx, persistence_rx) = tokio::sync::mpsc::unbounded_channel();
        support::drain_gateway(gateway_rx);
        support::drain_persistence(persistence_rx);
        let mut actor =
            support::create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
        actor.auth_method_id = support::test_auth_method_id("xai.api_key");
        actor.agent.replace(
            support::test_agent_with_tools(vec![
                xai_grok_tools::registry::types::ToolConfig::for_tool::<
                    xai_grok_tools::implementations::search_tool::SearchTool,
                >(),
                xai_grok_tools::registry::types::ToolConfig::for_tool::<
                    xai_grok_tools::implementations::use_tool::UseTool,
                >(),
            ])
            .await,
        );
        for (name, namespace, kind) in [
            ("z_builtin", ToolNamespace::GrokBuild, ToolKind::Other),
            (
                "builtin__renamed",
                ToolNamespace::GrokBuild,
                ToolKind::Other,
            ),
            (
                "policy__root_message",
                ToolNamespace::GrokBuild,
                ToolKind::ActiveAgentMessage,
            ),
            ("zeta__fetch", ToolNamespace::MCP, ToolKind::Other),
            ("alpha__lookup", ToolNamespace::MCP, ToolKind::Other),
        ] {
            parent_golden_register(&actor, name, namespace, kind);
        }
        let (manager, home) = support::manager_with_entries();
        for model in ["r12-parent", "r12-live"] {
            manager.insert_test_entry(
                model,
                support::entry_with_deferred_tools(model, true, &["z_builtin"]),
            );
        }
        manager.set_current_model_id(agent_client_protocol::ModelId::new("r12-parent"));
        actor.models_manager = manager;
        let mut binding = actor
            .chat_state_handle
            .capture_transport_binding()
            .await
            .unwrap();
        binding.config.model = "r12-parent".into();
        binding.config.base_url = wire.url.clone();
        binding.config.api_backend = backend;
        binding.config.supports_search_tool = true;
        binding.config.max_retries = Some(0);
        binding.config.max_completion_tokens = Some(512);
        binding.config.extra_headers.clear();
        binding.credentials = xai_chat_state::Credentials {
            api_key: Some("parent-capture-fixture".into()),
            auth_type: xai_chat_state::AuthType::ApiKey,
            alpha_test_key: None,
            client_version: None,
        };
        actor
            .chat_state_handle
            .install_transport_binding(binding.clone())
            .await
            .unwrap();
        let history = parent_golden_history();
        actor
            .chat_state_handle
            .replace_conversation(history.clone());
        assert_eq!(
            serde_json::to_value(actor.chat_state_handle.get_conversation().await).unwrap(),
            serde_json::to_value(&history).unwrap()
        );
        ParentGoldenFixture {
            actor: std::sync::Arc::new(actor),
            binding,
            wire,
            live_wire,
            history,
            _home: home,
        }
    }

    fn parent_golden_variants() -> [(&'static str, xai_grok_sampling_types::ApiBackend, bool); 6] {
        use xai_grok_sampling_types::ApiBackend;
        [
            ("same-bound-row", ApiBackend::Messages, true),
            ("messages-admission-off", ApiBackend::Messages, false),
            ("responses-admission-on", ApiBackend::Responses, true),
            ("responses-admission-off", ApiBackend::Responses, false),
            ("chat-admission-on", ApiBackend::ChatCompletions, true),
            ("chat-admission-off", ApiBackend::ChatCompletions, false),
        ]
    }

    fn parent_golden_live_binding(
        fixture: &ParentGoldenFixture,
        scenario: &str,
        backend: xai_grok_sampling_types::ApiBackend,
        flag: bool,
    ) -> xai_chat_state::RequestTransportBinding {
        if scenario == "same-bound-row" {
            return fixture.binding.clone();
        }
        let mut live = fixture.binding.clone();
        live.config.model = "r12-live".into();
        live.config.base_url = fixture.live_wire.url.clone();
        live.config.api_backend = backend;
        live.config.supports_search_tool = flag;
        live.credentials.api_key = Some("live-capture-fixture".into());
        live
    }

    fn parent_golden_registry(
        actor: &crate::session::acp_session::SessionActor,
    ) -> serde_json::Value {
        let bridge = actor.agent.borrow().tool_bridge().clone();
        let inventory: Vec<_> = bridge.toolset().tools_namespaced().into_iter().enumerate()
            .map(|(coordinate, (definition, namespace))| serde_json::json!({
                "coordinate":coordinate, "kind":format!("{:?}", bridge.tool_kind(&definition.function.name)),
                "namespace":format!("{namespace:?}"), "definition":definition,
            })).collect();
        serde_json::to_value(inventory).unwrap()
    }

    fn parent_golden_capture_body(
        case: &str,
        fixture: &ParentGoldenFixture,
        request: &xai_grok_sampling_types::ConversationRequest,
        selected_from: &str,
        parent_registry: serde_json::Value,
    ) {
        assert!(
            fixture.live_wire.exchanges.borrow().is_empty(),
            "installed B must receive no submission"
        );
        let exchanges = fixture.wire.exchanges.borrow();
        assert_eq!(exchanges.len(), 1, "exactly one actual surface send");
        let exchange = &exchanges[0];
        let expected_path = match fixture.binding.config.api_backend {
            xai_grok_sampling_types::ApiBackend::Messages => "/v1/messages",
            xai_grok_sampling_types::ApiBackend::Responses => "/v1/responses",
            xai_grok_sampling_types::ApiBackend::ChatCompletions => "/v1/chat/completions",
        };
        assert_eq!(exchange.path, expected_path);
        assert_eq!(exchange.parsed_body["model"], fixture.binding.config.model);
        assert_eq!(
            request.model.as_deref(),
            Some(fixture.binding.config.model.as_str())
        );
        if case.starts_with("title-") {
            assert!(request.tools.is_empty());
            assert!(request.hosted_tools.is_empty());
            assert!(!request.search_admission.unwrap().admitted());
        } else {
            assert!(request.search_admission.unwrap().admitted());
            let tools = exchange.parsed_body["tools"].as_array().unwrap();
            for name in [
                "search_tool",
                "use_tool",
                "historic__lookup",
                "tool_search_tool_bm25",
            ] {
                assert!(
                    tools.iter().any(|tool| tool["name"] == name),
                    "parent declaration missing: {name}"
                );
            }
            let contents: Vec<_> = exchange.parsed_body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|message| message["content"].as_array().unwrap().iter())
                .collect();
            assert!(
                contents
                    .iter()
                    .any(|block| block["type"] == "tool_use" && block["name"] == "search_tool"),
                "full canonical D2 bridge call must be present"
            );
            assert!(
                contents.iter().any(|block| block["type"] == "tool_result"
                    && block["tool_use_id"] == "call_parent_capture"),
                "full canonical D2 result must be present"
            );
            if !case.starts_with("fork/") {
                let mcp: Vec<_> = request
                    .tools
                    .iter()
                    .filter(|tool| ["alpha__lookup", "zeta__fetch"].contains(&tool.name.as_str()))
                    .collect();
                assert_eq!(mcp.len(), 2);
                for spec in mcp {
                    assert_eq!(
                        spec.exposure,
                        xai_grok_sampling_types::ToolExposure::Deferred
                    );
                    let client = tools.iter().find(|tool| tool["name"] == spec.name).unwrap();
                    assert_eq!(client["input_schema"], spec.parameters);
                    assert_eq!(client["defer_loading"], true);
                }
            } else {
                assert!(!request.tools.iter().any(|tool| {
                    ["alpha__lookup", "zeta__fetch", "child__lookup"].contains(&tool.name.as_str())
                }));
            }
        }
        if fixture.binding.config.api_backend == xai_grok_sampling_types::ApiBackend::Responses {
            assert_eq!(exchange.parsed_body["store"], false);
        }
        parent_golden_record(
            case,
            serde_json::json!({
                "frozen": {"raw_body":String::from_utf8(exchange.raw_body.clone()).unwrap(), "parsed_body":exchange.parsed_body},
                "provenance": {
                    "binding_config":fixture.binding.config,
                    "backend":fixture.binding.config.api_backend,
                    "dialect":format!("{:?}", fixture.binding.config.api_backend),
                    "admission":request.search_admission.map(|a| serde_json::json!({"supports_search_tool":a.supports_search_tool,"has_searchable_tools":a.has_searchable_tools})),
                    "input_history":fixture.history,
                    "request_items":request.items,
                    "selected_specs":request.tools,
                    "selected_hosted_tools":format!("{:?}", request.hosted_tools),
                    "selected_specs_observation":selected_from,
                    "parent_registry":parent_registry,
                    "request_path":exchange.path,
                    "request_id":exchange.request_id,
                    "header_flags":exchange.headers,
                    "response_id":"parent-capture-response",
                    "response_body":exchange.response_body,
                    "submission_count":exchanges.len(),
                    "normalization_mapping":[],
                    "normalization_note":"No body normalization. Request correlation IDs exist only in headers and are retained separately.",
                }
            }),
        );
    }

    async fn parent_golden_auxiliary(surface: &str, backend: xai_grok_sampling_types::ApiBackend) {
        use crate::session::acp_session::side_call::tests::AuxiliaryGuard;
        for (scenario, installed_backend, flag) in parent_golden_variants() {
            let mut fixture = parent_golden_fixture(backend.clone()).await;
            let actor = std::sync::Arc::get_mut(&mut fixture.actor).unwrap();
            actor.turn_summary_enabled = true;
            actor.title_refresh_enabled = true;
            let registry = parent_golden_registry(actor);
            let selected = if surface == "title" {
                Vec::new()
            } else {
                actor
                    .prepare_side_call_tool_specs(&fixture.binding)
                    .await
                    .specs
            };
            let original_admission =
                xai_grok_sampling_types::SearchAdmission::for_row(true, &selected);
            let live = parent_golden_live_binding(&fixture, scenario, installed_backend, flag);
            let guard = AuxiliaryGuard::new(live.clone());
            match surface {
                "btw" => assert_eq!(
                    fixture
                        .actor
                        .handle_side_question("What remains after discovery?")
                        .await
                        .unwrap(),
                    "Parent capture complete"
                ),
                "recap" => {
                    fixture.actor.handle_recap(false).await;
                    assert!(
                        fixture.actor.last_recap_main_turn.get() > 0,
                        "actual recap commits its watermark"
                    );
                }
                "turn-summary" => {
                    fixture
                        .actor
                        .restart_turn_summary("parent-summary-prompt".into());
                    assert!(fixture.actor.turn_summary_task.borrow().is_some());
                    tokio::time::timeout(std::time::Duration::from_secs(60), async {
                        while fixture.actor.turn_summary_task.borrow().is_some() {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("independently entered turn summary completes");
                }
                "title" => {
                    fixture.actor.maybe_refresh_title();
                    assert!(fixture.actor.title_refresh_task.borrow().is_some());
                    tokio::time::timeout(std::time::Duration::from_secs(60), async {
                        while fixture.actor.title_refresh_task.borrow().is_some() {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("actual title lifecycle completes");
                    assert!(fixture.actor.next_title_refresh_idx.get() > 0);
                }
                _ => unreachable!(),
            }
            let sends = guard.sends();
            let (request, source) = if surface == "turn-summary" {
                assert!(
                    sends.is_empty(),
                    "the parent has no summary request observation hook"
                );
                // This metadata is not a second request build; the actual summary is captured at HTTP.
                (
                    xai_grok_sampling_types::ConversationRequest {
                        tools: selected.clone(),
                        hosted_tools: fixture.actor.hosted_tools_for_turn(),
                        search_admission: Some(original_admission),
                        model: Some(fixture.binding.config.model.clone()),
                        ..Default::default()
                    },
                    "same immutable registered inventory through the production captured-row selector, validated against the actual summary HTTP body; no pre-lowering request hook exists",
                )
            } else {
                assert_eq!(sends.len(), 1);
                assert_eq!(sends[0].0, surface);
                (
                    sends[0].1.clone(),
                    "actual auxiliary request observation before send",
                )
            };
            assert_eq!(
                serde_json::to_value(&request.tools).unwrap(),
                serde_json::to_value(&selected).unwrap()
            );
            assert_eq!(request.search_admission, Some(original_admission));
            assert_eq!(
                serde_json::to_value(fixture.actor.chat_state_handle.get_conversation().await)
                    .unwrap(),
                serde_json::to_value(&fixture.history).unwrap(),
                "auxiliary entry is display-only"
            );
            let installed = fixture
                .actor
                .chat_state_handle
                .capture_transport_binding()
                .await
                .unwrap();
            assert_eq!(
                serde_json::to_value(&installed.config).unwrap(),
                serde_json::to_value(&live.config).unwrap()
            );
            assert_eq!(installed.credentials, live.credentials);
            let label = if surface == "title" {
                format!("title-{backend:?}")
            } else {
                surface.to_owned()
            };
            parent_golden_capture_body(
                &format!("{label}/{scenario}"),
                &fixture,
                &request,
                source,
                registry,
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn interim_messages_btw_matches_committed_parent() {
        const NAME: &str =
            "session::acp_session::side_call::tests::interim_messages_btw_matches_committed_parent";
        if parent_golden_isolated(NAME) {
            return;
        }
        tokio::task::LocalSet::new()
            .run_until(parent_golden_auxiliary(
                "btw",
                xai_grok_sampling_types::ApiBackend::Messages,
            ))
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn interim_messages_turn_summary_matches_committed_parent() {
        const NAME: &str = "session::acp_session::side_call::tests::interim_messages_turn_summary_matches_committed_parent";
        if parent_golden_isolated(NAME) {
            return;
        }
        tokio::task::LocalSet::new()
            .run_until(parent_golden_auxiliary(
                "turn-summary",
                xai_grok_sampling_types::ApiBackend::Messages,
            ))
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn title_refresh_body_matches_committed_parent() {
        const NAME: &str =
            "session::acp_session::side_call::tests::title_refresh_body_matches_committed_parent";
        if parent_golden_isolated(NAME) {
            return;
        }
        tokio::task::LocalSet::new()
            .run_until(async {
                for backend in [
                    xai_grok_sampling_types::ApiBackend::Responses,
                    xai_grok_sampling_types::ApiBackend::Messages,
                ] {
                    parent_golden_auxiliary("title", backend).await;
                }
            })
            .await;
    }

    async fn a3_auxiliary_fixture(admitted: bool) -> ParentGoldenFixture {
        use xai_grok_tools::registry::types::ToolConfig;
        use xai_grok_tools::types::tool::{ToolKind, ToolNamespace};
        let mut fixture =
            parent_golden_fixture(xai_grok_sampling_types::ApiBackend::Responses).await;
        fixture.binding.config.supports_search_tool = admitted;
        let actor = std::sync::Arc::get_mut(&mut fixture.actor).unwrap();
        actor.agent.replace(
            support::test_agent_with_tools(vec![
                ToolConfig::for_tool::<xai_grok_tools::implementations::search_tool::SearchTool>()
                    .with_name("bridge__discover"),
                ToolConfig::for_tool::<xai_grok_tools::implementations::use_tool::UseTool>()
                    .with_name("bridge__invoke"),
            ])
            .await,
        );
        for (name, namespace) in [
            ("builtin__renamed", ToolNamespace::GrokBuild),
            ("search_tool", ToolNamespace::GrokBuild),
            ("use_tool", ToolNamespace::GrokBuild),
            ("alpha__lookup", ToolNamespace::MCP),
            ("zeta__fetch", ToolNamespace::MCP),
        ] {
            parent_golden_register(actor, name, namespace, ToolKind::Other);
        }
        let mut entry = support::entry_with_deferred_tools(
            "r12-parent",
            admitted,
            &["bridge__invoke", "builtin__renamed"],
        );
        entry.info.model_family = Some("xai".into());
        actor.models_manager.insert_test_entry("r12-parent", entry);
        actor.turn_summary_enabled = true;
        actor
            .chat_state_handle
            .install_transport_binding(fixture.binding.clone())
            .await
            .unwrap();
        fixture
    }

    pub(in crate::session::acp_session) async fn a3_auxiliary_surface(surface: &str) {
        use xai_grok_sampling_types::ApiBackend;
        for admitted in [true, false] {
            let fixture = a3_auxiliary_fixture(admitted).await;
            let live = parent_golden_live_binding(
                &fixture,
                "opposite-admission",
                if admitted {
                    ApiBackend::Messages
                } else {
                    ApiBackend::Responses
                },
                !admitted,
            );
            let guard = AuxiliaryGuard::new(live.clone());
            match surface {
                "btw" => assert_eq!(
                    fixture
                        .actor
                        .handle_side_question("What remains after discovery?")
                        .await
                        .unwrap(),
                    "Parent capture complete"
                ),
                "recap" => {
                    fixture.actor.handle_recap(false).await;
                    assert!(
                        fixture.actor.last_recap_main_turn.get() > 0,
                        "A3 actual recap commits its watermark"
                    );
                }
                "turn-summary" => {
                    fixture
                        .actor
                        .restart_turn_summary("a3-summary-prompt".into());
                    assert!(fixture.actor.turn_summary_task.borrow().is_some());
                    tokio::time::timeout(std::time::Duration::from_secs(60), async {
                        while fixture.actor.turn_summary_task.borrow().is_some() {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("A3 independently entered turn summary completes");
                }
                _ => unreachable!(),
            }
            let builds = guard.builds();
            assert_eq!(builds.len(), 1, "A3 one actual auxiliary build: {surface}");
            let original = &builds[0];
            assert_eq!(
                original.search_admission.unwrap().admitted(),
                admitted,
                "A3 auxiliary has its own captured admission: {surface}"
            );
            assert_eq!(
                original.model.as_deref(),
                Some(fixture.binding.config.model.as_str())
            );
            for name in [
                "bridge__discover",
                "bridge__invoke",
                "search_tool",
                "use_tool",
                "builtin__renamed",
            ] {
                assert!(
                    original.tools.iter().any(|spec| spec.name == name),
                    "A3 actual auxiliary anti-vacuity: {surface}/{name}"
                );
            }
            assert!(
                !original
                    .tools
                    .iter()
                    .any(|spec| ["alpha__lookup", "zeta__fetch"].contains(&spec.name.as_str())),
                "A3 P0-R1 keeps item-2 captured Responses MCP exclusion"
            );
            assert_eq!(
                serde_json::to_value(fixture.actor.chat_state_handle.get_conversation().await)
                    .unwrap(),
                serde_json::to_value(&fixture.history).unwrap(),
                "A3 auxiliary is display-only"
            );
            let installed = fixture
                .actor
                .chat_state_handle
                .capture_transport_binding()
                .await
                .unwrap();
            assert_eq!(
                serde_json::to_value(&installed.config).unwrap(),
                serde_json::to_value(&live.config).unwrap()
            );
            assert_eq!(installed.credentials, live.credentials);
            assert!(
                fixture.live_wire.exchanges.borrow().is_empty(),
                "A3 auxiliary keeps captured URL/backend: {surface}"
            );
            assert_eq!(fixture.wire.exchanges.borrow().len(), 1);
            let sent = fixture.wire.exchanges.borrow()[0].clone();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&sent.raw_body).unwrap(),
                sent.parsed_body
            );
            let client = xai_grok_sampler::SamplingClient::new(
                fixture
                    .actor
                    .turn_sampler_config_from_binding(&fixture.binding)
                    .await,
            )
            .unwrap();
            client.conversation_collect(original.clone()).await.unwrap();
            assert_eq!(fixture.wire.exchanges.borrow().len(), 2);
            let mut expected = fixture.wire.exchanges.borrow()[1].parsed_body.clone();
            if admitted {
                expected["tools"].as_array_mut().unwrap().retain(|tool| {
                    !(tool["type"] == "function"
                        && ["bridge__discover", "bridge__invoke"]
                            .contains(&tool["name"].as_str().unwrap_or_default()))
                });
            }
            assert_eq!(
                sent.parsed_body["tools"], expected["tools"],
                "A3 actual auxiliary removes typed bridge declarations: {surface}, admitted={admitted}"
            );
            assert_eq!(
                sent.parsed_body, expected,
                "A3 auxiliary complete remaining body: {surface}"
            );
            assert_eq!(sent.parsed_body["store"], false);
            assert_eq!(
                sent.parsed_body["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|tool| tool["type"] == "tool_search")
                    .count(),
                usize::from(admitted),
                "A3 auxiliary preserves original native admission: {surface}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn responses_btw_bridge_cutover() {
        const NAME: &str = "session::acp_session::side_call::tests::responses_btw_bridge_cutover";
        if parent_golden_isolated(NAME) {
            return;
        }
        tokio::task::LocalSet::new()
            .run_until(a3_auxiliary_surface("btw"))
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn responses_turn_summary_bridge_cutover() {
        const NAME: &str =
            "session::acp_session::side_call::tests::responses_turn_summary_bridge_cutover";
        if parent_golden_isolated(NAME) {
            return;
        }
        tokio::task::LocalSet::new()
            .run_until(a3_auxiliary_surface("turn-summary"))
            .await;
    }
}
