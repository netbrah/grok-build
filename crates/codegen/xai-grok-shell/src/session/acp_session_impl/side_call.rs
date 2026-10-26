//! Shared cache-aligned request building for recap-style auxiliary model calls (recap, title refresh).
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
    pub(crate) tools: Vec<ToolSpec>,
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
            &call.tools,
        ));
        ConversationRequest {
            items: xai_chat_state::compaction_utils::ModelRequestHistory::from_raw(call.items)
                .into_items(),
            tools: call.tools,
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
        }
    }

    /// Prepare the shared pieces of a recap-style side-call (recap and title refresh): the sampling client and the config both need.
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
    ) -> Vec<ToolSpec> {
        let (prepared, _) = self.prepare_native_tool_candidates_timed().await;
        self.turn_native_tool_candidates(&prepared.builtin_defs, &prepared.mcp_specs)
            .select_for_row(&binding.config)
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
            tools,
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
}
