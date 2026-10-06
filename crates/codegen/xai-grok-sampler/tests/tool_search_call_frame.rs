//! apex-6c6 — a real `tool_search_call` frame must not kill the turn.
//!
//! Same invariant as
//! `stream::responses::tests::tool_search_call_frame_survives_the_stream_deserialiser`,
//! driven through the crate's public stream entry point. The two are kept together because
//! that in-crate copy sits in the lib test target, which cannot compile while a sibling lane
//! holds an error in `client.rs`; this target links the lib without its `#[cfg(test)]` tree.
//!
//! The bytes are the verbatim `data:` lines 5 and 6 of the ratchet-live capture
//! `ratchet-capture/captures/2026-09-25-ratchet-live/wire2-live3/resp-003.sse` for discovery
//! item `tsc_02005a…`, banked as tracked fixtures here because that capture directory is
//! gitignored. The `data:` prefix is kept on the wire frames and stripped the way the
//! transport strips it. The terminal frame is a real captured `response.completed` from the
//! tracked parity fixture tree, so the stream ends on `ResponseCompleted` rather than on the
//! harness's own "no completed event received" failure.
//!
//! T1: an async-openai `OutputItem` is internally tagged on `type` with no catch-all, so an
//! item type the pinned dependency does not know is a hard deserialise error — the stream loop
//! turns a decode error into `SamplingEvent::Failed` and the turn ends before any harness-side
//! arm can see the item.
//! T2: once it deserialises, the item must surface as an observable. The `OutputItem`
//! catch-all in the `ResponseOutputItemDone` arm makes a dropped item indistinguishable from a
//! clean turn, so silence is not evidence — the `name` is asserted, not just the absence of an
//! error. The observable is asserted as a whole (exactly one, its `name`, the `call_id` it is
//! keyed on, and the payload it carries), because a `name`-only check passes for an arm that
//! emits the right label with a dropped payload or a borrowed id.
//! T3: the sibling discovery item type, `tool_search_output`, must be known to the pinned
//! dependency for the same reason as T1. Round 1 of mutant-before-trust found the fixture set
//! covered only the call side, so no mutation could ever reach a future
//! `OutputItem::ToolSearchOutput` arm.

use async_openai::types::responses as rs;
use futures_util::{StreamExt, stream};
use xai_grok_sampler::{RequestId, SamplingEvent, stream::stream_responses};
use xai_grok_sampling_types::SamplingError;

const ADDED_FRAME: &str = include_str!("fixtures/tool_search_call_added.sse");
const DONE_FRAME: &str = include_str!("fixtures/tool_search_call_done.sse");
const COMPLETED_FRAME: &str =
    include_str!("../../../../smoke/redteam/fixtures/parity/101/response_completed_frame.json");
/// Real captured discovery output item (see the T3 test for its exact provenance).
const OUTPUT_ITEM_FRAME: &str = include_str!("fixtures/tool_search_output_item.json");

/// The discovery item's own wire id (`item.id`), which is what the harness keys the
/// completed observable on — not the model's `call_id`.
const DISCOVERY_ITEM_ID: &str = "tsc_02005a6c7856d15c016ab6aa70e9208194bc939a9b4707c556";
/// The model-minted call id carried alongside it on the same frame.
const DISCOVERY_CALL_ID: &str = "call_AOphypzlL1KKckJugyBS2PYn";

/// The SSE fixtures carry the transport's `data:` prefix; the parity fixture is bare JSON.
fn frame_json(frame: &str) -> &str {
    let frame = frame.trim();
    frame.strip_prefix("data:").unwrap_or(frame).trim()
}

#[tokio::test]
async fn tool_search_call_frame_survives_the_stream_deserialiser() {
    let mut frames: Vec<Result<rs::ResponseStreamEvent, SamplingError>> = Vec::new();
    for frame in [ADDED_FRAME, DONE_FRAME, COMPLETED_FRAME] {
        let ev: Result<rs::ResponseStreamEvent, _> = serde_json::from_str(frame_json(frame));
        assert!(
            ev.is_ok(),
            "real tool_search_call frame must deserialise, got: {}",
            ev.unwrap_err()
        );
        frames.push(Ok(ev.expect("T1 asserts this frame is Ok")));
    }

    let raw = stream::iter(frames).boxed();
    let events = stream_responses(
        raw,
        None,
        RequestId::from("resp-test"),
        std::time::Duration::from_secs(60),
        None,
    )
    .collect::<Vec<SamplingEvent>>()
    .await;

    if let Some(event) = events
        .iter()
        .find(|event| matches!(event, SamplingEvent::Failed { .. }))
    {
        panic!("tool_search_call frame killed the turn: {event:?}");
    }

    // T2 — the item surfaces. Collected as a whole observable, because the shape of the
    // event is the contract the pager consumes: `name` selects the card, `call_id` correlates
    // it, `result` is the only copy of the item the caller ever sees.
    let observables: Vec<(&str, &str, Option<serde_json::Value>)> = events
        .iter()
        .filter_map(|event| {
            if let SamplingEvent::BackendToolCallCompleted {
                name,
                call_id,
                result,
                ..
            } = event
            {
                Some((name.as_str(), call_id.as_str(), result.clone()))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        observables.as_slice(),
        &[(
            "tool_search",
            DISCOVERY_ITEM_ID,
            Some(serde_json::json!({
                "id": DISCOVERY_ITEM_ID,
                "call_id": DISCOVERY_CALL_ID,
                "execution": "client",
                "arguments": { "query": "crm order management", "limit": 8 },
                "status": "completed"
            }))
        )][..],
        "the done frame must surface exactly once as BackendToolCallCompleted named \
         \"tool_search\", keyed on the item's own id, carrying the item verbatim"
    );

    assert!(
        matches!(events.last(), Some(SamplingEvent::Completed { .. })),
        "the turn must still reach its terminal Completed event, got {:?}",
        events.last()
    );
}

/// T3 — the sibling discovery item type must be known to the pinned dependency too.
///
/// Bytes are the verbatim `tool_search_output` item at `input[12]` of the captured request
/// `ratchet-capture/captures/2026-09-25-ratchet-live/wire2-live3/req-004.json` (item id
/// `tso_01a0d989…`, answer to the `tsc_02005a…` call above), banked as a tracked fixture
/// because that capture directory is gitignored. Only the file-terminating newline is added;
/// every item byte is the capture's.
///
/// Provenance matters here and limits what this test may claim: a structural grep of the whole
/// capture tree (`grep -c tool_search --include='*.sse'`) finds this item type in **no** SSE
/// response — discovery is `execution:"client"`, so the output item is minted by the client and
/// only ever appears in the *outbound* `input`. This test therefore pins the enum-level half of
/// the invariant (the pinned `OutputItem` can represent the item, so a frame of that type cannot
/// be a hard deserialise error) and deliberately makes no claim about a stream frame that has
/// never been observed on the wire; building one by hand would be a hand-built request, which is
/// vacuous for any live-path claim.
#[test]
fn tool_search_output_item_is_known_to_the_pinned_dependency() {
    let item: rs::OutputItem = serde_json::from_str(OUTPUT_ITEM_FRAME.trim())
        .expect("the real captured tool_search_output item must deserialise into OutputItem");
    match item {
        rs::OutputItem::ToolSearchOutput(output) => {
            assert_eq!(output.call_id.as_deref(), Some(DISCOVERY_CALL_ID));
            assert_eq!(
                output.tools.len(),
                2,
                "the captured search returned two namespaces"
            );
        }
        other => panic!("expected OutputItem::ToolSearchOutput, got {other:?}"),
    }
}
