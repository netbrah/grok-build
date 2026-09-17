// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Regression guard: a reasoning model with NO server `reasoning_efforts` list falls back to the built-in `/effort` rows, descriptions included.
/// The feature is a no-op until a server opts in.
/// The mock uses a claude slug on purpose: since catalog slug inference (47b, dec6b27) fills
/// XAI/OpenAI rows with a family menu, a grok/gpt slug renders the inferred server menu instead of
/// the built-in fallback. A no-menu claude row is SDD §5's "claude /effort menu = 6 rows" acceptance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn reasoning_efforts_fallback_menu_matches_builtin() {
    let content = ContentController::start_with_models(vec![
        MockModel::new("claude-opus-5").with_supports_reasoning_effort(true),
    ])
    .await
    .expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} turn."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn rendered");

    inject_keys_paced(&mut harness, b"/effort ");
    // Assert the full built-in row set (all six levels, keyed by their unique descriptions) renders
    // Asserting all six, not just a couple, proves the fallback matches the shared
    // LEGACY_REASONING_EFFORTS const (EFFORT-SEAM-1 / apex-ayl.59, R2: max + ultra rows added)
    harness
        .wait_for_text("Ultra reasoning with automatic delegation", Duration::from_secs(10))
        .expect("built-in ultra row");
    let screen = harness.screen_contents();
    for description in [
        "Ultra reasoning with automatic delegation", // ultra
        "Maximum reasoning",                         // max
        "Extended reasoning",                        // xhigh
        "Heavy reasoning",                           // high
        "Balanced reasoning",                        // medium
        "Faster, lighter reasoning",                 // low
    ] {
        assert!(
            screen.contains(description),
            "built-in fallback menu missing row {description:?}\nscreen:\n{screen}"
        );
    }

    harness.quit().expect("clean quit");
}
