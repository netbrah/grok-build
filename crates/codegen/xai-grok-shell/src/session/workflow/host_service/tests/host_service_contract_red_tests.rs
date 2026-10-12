// apex-ayl.164 — expected-red cell for the workflow-host output-contract
// discard (M-NEW-A), plus its green anti-vacuity pin. Lives in a fork-owned
// file included from `host_service.rs`'s tests module (review UCH-1,
// 2026-10-12): upstream merges must never touch 170+ lines of red fixtures
// riding inside the upstream-owned product file. `use super::*` keeps this
// module in the same scope as the inline tests.

use super::*;

/// Synthetic child final message (apex-ayl.164, redcell draft
/// `workflow-child-anomaly-07`): prose preamble carrying a salvage sentinel,
/// then a ```json fenced object whose `confirmations` array holds 14 strings
/// against `maxItems: 12` — the shape of the mined discard (a completed
/// child, 55 tool calls, whose payload the host threw away). Not copied from
/// any live session; only the structure is reproduced.
fn payload_14() -> String {
    let confirmations = (1..=14)
        .map(|i| format!("\"c{i:02}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "SALVAGE-PREAMBLE-7f3a — recap of the stands, labeled by number.\n\n\
         ```json\n{{\"findings\": [\"f01\", \"f02\"], \"confirmations\": [{confirmations}]}}\n```"
    )
}

/// Clone of the mined run's contract (`script.rhai:79`): 12-item ceiling on
/// `confirmations`.
fn contract_violation_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["confirmations"],
        "properties": {
            "confirmations": { "type": "array", "maxItems": 12, "items": { "type": "string" } }
        }
    })
}

/// Green anti-vacuity pin (apex-ayl.164, C2a): the validator is not the bug.
/// PAYLOAD_14 fails exactly on the item count, and the jsonschema 0.30
/// Display quotes the violated constraint — the class seen verbatim in every
/// journal of the mined cluster — so a fix that only restates the constraint
/// would not touch the defect.
#[test]
fn contract_validator_flags_the_item_count_not_the_fence() {
    let validator = compile_contract_schema(&contract_violation_schema()).unwrap();
    let err = validate_contract_output(&validator, &payload_14())
        .expect_err("14 confirmations must violate maxItems 12");
    assert!(err.contains("does not match the required schema"), "{err}");
    assert!(err.contains("has more than 12 items"), "{err}");
}

/// Expected-red (apex-ayl.164, M-NEW-A): when the workflow-host output
/// contract exhausts its single in-band retry, the **completed** child's
/// captured final text is discarded — `AgentResult.output` keeps only the
/// validation-error string (`host_service.rs` exhaustion arm) — while the
/// sibling subagent plane preserves `final_text` on the identical failure
/// (`crates/codegen/xai-grok-shell/src/agent/subagent/prompt_turn_result.rs:71-77`).
/// The mined cluster re-paid 1.7 M–19.9 M tokens of finished work per
/// occurrence because the rhai script lost the payload and re-ran the whole
/// stand.
#[tokio::test]
#[ignore = "expected-red apex-ayl.164 — contract exhaustion discards the completed child's output (M-NEW-A); run with --ignored until the salvage fix lands"]
async fn contract_violation_discards_completed_child_output() {
    use xai_grok_tools::implementations::grok_build::task::types::{
        SubagentEvent as ToolSubagentEvent, SubagentResult,
    };
    let run_id = "wf_contract_discard".to_string();
    let mut tracker = WorkflowTracker::default();
    tracker.start_run(
        run_id.clone(),
        "demo".into(),
        "objective".into(),
        vec![],
        Some(1000),
        None,
    );
    let tracker = Arc::new(parking_lot::Mutex::new(tracker));
    let (subagent_tx, mut subagent_rx) = mpsc::unbounded_channel();
    let (params, _persist_rx) = test_host_params(
        &run_id,
        1,
        "wf-scratch-contract-discard",
        tracker.clone(),
        subagent_tx,
    );
    let cancel = params.cancel.clone();
    let (host_tx, host_rx) = mpsc::unbounded_channel();
    let (handle, _drained) = spawn_workflow_host_service(params, host_rx);

    let (reply_tx, reply_rx) = oneshot::channel();
    host_tx
        .send(WorkflowHostRequest::SpawnAgent {
            opts: AgentOpts {
                prompt: "verify each stand and report by number".to_owned(),
                output_schema: Some(contract_violation_schema()),
                ..Default::default()
            },
            reply: reply_tx,
        })
        .unwrap();

    let first = tokio::time::timeout(Duration::from_secs(5), subagent_rx.recv())
        .await
        .expect("the agent reaches the coordinator")
        .expect("subagent channel open");
    let ToolSubagentEvent::Spawn(first) = first else {
        panic!("expected a spawn event");
    };
    assert!(
        first.prompt.contains("<output-contract>"),
        "the schema constraint must be visible to the child (guard: the fix is \
         the discard arm, not an invisible-constraint story)"
    );

    let respond = |request: &SubagentRequest, child_id: &str| SubagentResult {
        success: true,
        output: Arc::from(payload_14().as_str()),
        subagent_id: request.id.clone(),
        child_session_id: child_id.into(),
        ..Default::default()
    };
    first
        .respond_with(|request| respond(request, "child-contract-1"))
        .expect("first result delivered");

    let second = tokio::time::timeout(Duration::from_secs(5), subagent_rx.recv())
        .await
        .expect("the contract retry re-spawns the child")
        .expect("subagent channel open");
    let ToolSubagentEvent::Spawn(second) = second else {
        panic!("expected a spawn event");
    };
    // Green pins (C2b): the corrective IS constraint-visible and it resumes
    // the same child — and it commands a full re-emit "and nothing else",
    // which is why re-emitting the same oversized payload exhausts the budget.
    assert!(
        second.prompt.contains("has more than 12 items"),
        "the corrective prompt must quote the violated constraint, got: {}",
        second.prompt
    );
    assert!(
        second.prompt.contains("and nothing else."),
        "the corrective commands a full re-emit — the behaviour-ineffectiveness pin"
    );
    assert_eq!(
        second.resume_from.as_deref(),
        Some("child-contract-1"),
        "the in-band retry resumes the same child session"
    );
    second
        .respond_with(|request| respond(request, "child-contract-2"))
        .expect("retry result delivered");

    let result = tokio::time::timeout(Duration::from_secs(5), reply_rx)
        .await
        .expect("the run completes")
        .expect("reply channel open")
        .expect("host accepts the spawn");

    assert!(
        !result.success,
        "contract exhaustion stays a typed failure (salvage must not fake success)"
    );
    // Damage record today: output is exactly
    // `"structured output validation failed: output does not match the
    // required schema: [...] has more than 12 items"` — the bare error
    // string, child text gone. (Pinned only as a comment: a structured
    // error/payload split fix must still pass, so no prefix assertion here.
    // `contains("c14")` would be vacuous — jsonschema 0.30 Display embeds
    // the failing array in the error text, so item strings survive by
    // accident; the parent key and the prose preamble do not.)
    let output = result.output.to_string();
    // RED: the child's captured final text must stay reachable to the script.
    assert!(
        output.contains("SALVAGE-PREAMBLE-7f3a"),
        "salvage: the child's preamble must survive contract exhaustion, got: {output}"
    );
    // Unquoted on purpose (review R1): `result.output.to_string()` JSON-escapes
    // interior quotes, so a quoted `"findings"` pattern could never appear under
    // the raw-text salvage the spec mandates (Value::String(final_text)) —
    // over-constrained. The bare key is still RED today: the MaxItems error
    // embeds only the failing array instance, never the parent key — and it
    // goes green under both spec-sanctioned salvage shapes.
    assert!(
        output.contains("findings"),
        "salvage: the fenced payload's parent object must survive, got: {output}"
    );

    drop(host_tx);
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}
