// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// EFFORT-SEAM-1 (apex-ayl.59), test 9 — B1 dogfood pinned end-to-end.
/// The session starts on the sol-shape model at ultra (global `[models].default_reasoning_effort`
/// seeds the NewSession arm). A plain `/model` switch carries the session effort to the
/// qwen-shape model, whose menu is {xhigh (default), medium, low}: the implicit carry must
/// PROJECT to the target's default — the status label shows xhigh and the wire carries
/// `reasoning.effort == "xhigh"`, never the raw ultra (wire: 400, or the ultra→max remap).
/// Then explicit `/effort ultra` on the qwen-shape model keeps its explicit error naming the
/// valid ids (R6), instead of being projected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn effort_projection_on_switch_remaps_on_wire() {
    let content = ContentController::start_with_models(vec![
        MockModel::new("sol-5")
            .with_api_backend("responses")
            .with_supports_reasoning_effort(true)
            .with_reasoning_efforts(vec![
                json!({ "id": "low", "value": "low", "label": "Low" }),
                json!({ "id": "medium", "value": "medium", "label": "Medium" }),
                json!({ "id": "high", "value": "high", "label": "High" }),
                json!({ "id": "max", "value": "max", "label": "Max" }),
                json!({ "id": "ultra", "value": "ultra", "label": "Ultra" }),
            ]),
        MockModel::new("qwen-3.8")
            .with_api_backend("responses")
            .with_supports_reasoning_effort(true)
            .with_reasoning_efforts(vec![
                json!({ "id": "xhigh", "value": "xhigh", "label": "Xhigh", "default": true }),
                json!({ "id": "medium", "value": "medium", "label": "Medium" }),
                json!({ "id": "low", "value": "low", "label": "Low" }),
            ]),
    ])
    .await
    .expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} turn."));

    // The session starts on sol-5 at ultra: the global cursor (default_reasoning_effort) seeds
    // the NewSession arm, and sol-5's menu advertises ultra, so turn 1 runs at ultra.
    std::fs::write(
        content.sandbox().grok_home().join("config.toml"),
        "[models]\ndefault = \"sol-5\"\ndefault_reasoning_effort = \"ultra\"\n",
    )
    .expect("write sandbox config.toml");

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Turn 1 on sol-5 @ ultra — the status label renders `model (effort)`
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("first turn rendered");
    harness.update(Duration::from_millis(300));
    assert!(
        harness.contains_text("sol-5 (ultra)"),
        "status label must show sol-5 (ultra) before the switch\nscreen:\n{}",
        harness.screen_contents()
    );

    // Plain model switch — carries the session's current effort (SwitchEffort::Preserve).
    // ESC closes the autocomplete WITHOUT inserting the row (a reasoning model's row would
    // insert a trailing space and chain into the effort phase); the bare exact-id commit is
    // SetDefaultModel → SetSessionModelRequest without an effort meta → Preserve.
    harness.inject_keys(b"/model qwen-3.8").expect("type model switch");
    harness.update(Duration::from_millis(500));
    harness.inject_keys(keys::ESC).expect("dismiss model autocomplete");
    harness.update(Duration::from_millis(200));
    harness.inject_keys(b"\r").expect("commit model switch");

    // (A) The carried ultra must project to the target's default: status shows xhigh
    harness
        .wait_for_text("qwen-3.8 (xhigh)", Duration::from_secs(15))
        .expect("status label shows projected xhigh after switch");

    // (B) Turn 2 on qwen-3.8 — the wire carries the projected value, not the carried ultra.
    // The pre-switch sol-5 turn legitimately carries ultra's wire remap ("max" — the frozen-spec
    // wire ceiling), so the leak check is scoped to the post-switch qwen-3.8 turns only.
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} second turn."));
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit second prompt");
    harness
        .wait_for_text("second turn", Duration::from_secs(30))
        .expect("second turn rendered");

    let bodies = content.request_bodies();
    let qwen_bodies: Vec<_> = bodies
        .iter()
        .filter(|b| b.pointer("/model").and_then(|v| v.as_str()) == Some("qwen-3.8"))
        .collect();
    assert!(
        !qwen_bodies.is_empty(),
        "expected at least one wire turn on qwen-3.8\nbodies: {:#?}",
        bodies
    );
    let sent_xhigh = qwen_bodies
        .iter()
        .any(|b| b.pointer("/reasoning/effort").and_then(|v| v.as_str()) == Some("xhigh"));
    assert!(
        sent_xhigh,
        "post-switch turn must send reasoning.effort=xhigh (projected to the target menu), \
         not the carried ultra\nqwen-3.8 bodies: {qwen_bodies:#?}"
    );
    let leaked_raw_ultra = qwen_bodies.iter().any(|b| {
            matches!(
                b.pointer("/reasoning/effort").and_then(|v| v.as_str()),
                Some("ultra") | Some("max")
            )
    });
    assert!(
        !leaked_raw_ultra,
        "no post-switch (qwen-3.8) turn may carry the raw carried effort (ultra) or its wire \
         remap (max)\nqwen-3.8 bodies: {qwen_bodies:#?}"
    );

    // (C) R6: explicit input keeps its explicit error — `/effort ultra` on the qwen-shape model
    // must fail naming the model's valid ids, instead of being projected
    harness.inject_keys(keys::ESC).expect("dismiss any dropdown");
    harness
        .inject_keys(b"\x15")
        .expect("clear composer (Ctrl+U)");
    harness.update(Duration::from_millis(200));
    harness.inject_keys(b"/effort ultra\r").expect("explicit ultra on qwen-shape");
    harness
        .wait_for_text("unknown effort level 'ultra'", Duration::from_secs(10))
        .expect("explicit ultra is rejected, not projected");
    let screen = harness.screen_contents();
    assert!(
        screen.contains("use one of: xhigh, medium, low"),
        "the explicit error must name the model's valid ids\nscreen:\n{screen}"
    );

    harness.quit().expect("clean quit");
}
