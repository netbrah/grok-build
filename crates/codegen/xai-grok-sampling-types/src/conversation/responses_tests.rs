use super::responses::{
    MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES, SearchAdmission, ToolSearchExecution,
    ToolSearchSource, ToolSearchSourceListing, extra_tool_entries_with_declaration,
    tool_search_declaration_entry, tool_search_description,
};
use super::test_support::*;
use super::*;
use crate::tool_overrides::*;
use assert_matches::assert_matches;

#[test]
fn test_conversation_request_to_responses_api() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("System prompt"),
        ConversationItem::user("User message"),
    ])
    .with_model("grok-3")
    .with_temperature(0.7);

    let responses_req: rs::CreateResponse = (&req).into();
    assert_eq!(responses_req.model, Some("grok-3".to_string()));
    assert_eq!(responses_req.temperature, Some(0.7));

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    assert_eq!(items.len(), 2);
}

#[test]
fn function_tool_colliding_with_hosted_web_search_is_dropped() {
    let mut req =
        ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_tools(vec![
            ToolSpec {
                name: "web_search".to_string(),
                description: Some("local web search".to_string()),
                parameters: serde_json::json!({"type": "object"}),
                exposure: ToolExposure::default(),
            },
            ToolSpec {
                name: "read_file".to_string(),
                description: None,
                parameters: serde_json::json!({"type": "object"}),
                exposure: ToolExposure::default(),
            },
        ]);
    req.hosted_tools = vec![HostedTool::WebSearch { options: None }];

    let responses_req: rs::CreateResponse = (&req).into();
    let tools = responses_req.tools.expect("tools should be set");

    // web_search is emitted as a raw-JSON `extra_tool_entries` entry, so it never appears as a native `rs::Tool::WebSearch`
    // The raw entry can carry `excluded_domains`, which async_openai's typed filter omits
    let web_search_count = tools
        .iter()
        .filter(|t| matches!(t, rs::Tool::WebSearch(_)))
        .count();
    assert_eq!(
        web_search_count, 0,
        "web_search is not a native tool: {tools:?}"
    );
    let function_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| match t {
            rs::Tool::Function(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        function_names,
        vec!["read_file"],
        "colliding function tool must be dropped"
    );

    // The hosted web_search is emitted as a raw entry instead.
    let entries = extra_tool_entries(&req.hosted_tools);
    assert_eq!(entries, vec![serde_json::json!({"type": "web_search"})]);
}

#[test]
fn function_tool_colliding_with_hosted_x_search_is_dropped() {
    let mut req =
        ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_tools(vec![
            ToolSpec {
                name: "x_search".to_string(),
                description: None,
                parameters: serde_json::json!({"type": "object"}),
                exposure: ToolExposure::default(),
            },
        ]);
    req.hosted_tools = vec![HostedTool::XSearch { options: None }];

    let responses_req: rs::CreateResponse = (&req).into();
    let tools = responses_req.tools.unwrap_or_default();
    assert!(tools.is_empty(), "expected no tools, got: {tools:?}");
    let entries = extra_tool_entries(&req.hosted_tools);
    assert_eq!(entries, vec![serde_json::json!({"type": "x_search"})]);
}

/// The hosted `web_search` domain policy only reaches the API through this raw entry (async_openai's typed filters model no blocklist).
/// Both filters must survive the conversion from `HostedTool` to `extra_tool_entries`.
/// An empty or absent policy must stay byte-identical to the bare tool.
#[test]
fn web_search_domain_filters_reach_the_tool_entry() {
    let hosted = |options: Option<WebSearchOptions>| {
        extra_tool_entries(&[HostedTool::WebSearch { options }])
    };
    assert_eq!(
        hosted(Some(WebSearchOptions {
            allowed_domains: Some(vec!["docs.x.ai".into(), "arxiv.org".into()]),
            excluded_domains: None,
        })),
        vec![serde_json::json!({
            "type": "web_search",
            "filters": { "allowed_domains": ["docs.x.ai", "arxiv.org"] },
        })]
    );
    assert_eq!(
        hosted(Some(WebSearchOptions {
            allowed_domains: None,
            excluded_domains: Some(vec!["reddit.com".into()]),
        })),
        vec![serde_json::json!({
            "type": "web_search",
            "filters": { "excluded_domains": ["reddit.com"] },
        })]
    );

    // No policy (absent, default, or empty lists) emits the bare tool.
    let bare = vec![serde_json::json!({ "type": "web_search" })];
    assert_eq!(hosted(None), bare);
    assert_eq!(hosted(Some(WebSearchOptions::default())), bare);
    assert_eq!(
        hosted(Some(WebSearchOptions {
            allowed_domains: Some(vec![]),
            excluded_domains: Some(vec![]),
        })),
        bare
    );
}

#[test]
fn x_search_serializes_to_the_tool_entry() {
    // A full bound reaches the flat snake_case entry; an empty or `None` bound emits the bare entry.
    let dated = extra_tool_entries(&[HostedTool::XSearch {
        options: Some(XSearchOptions {
            date_bound: Some(
                SearchDateBound::new(Some("2024-01-01".into()), Some("2024-03-15".into())).unwrap(),
            ),
        }),
    }]);
    assert_eq!(
        dated,
        vec![serde_json::json!({
            "type": "x_search",
            "from_date": "2024-01-01",
            "to_date": "2024-03-15",
        })]
    );
    let bare = vec![serde_json::json!({"type": "x_search"})];
    assert_eq!(
        extra_tool_entries(&[HostedTool::XSearch {
            options: Some(XSearchOptions {
                date_bound: Some(SearchDateBound::new(None, None).unwrap()),
            }),
        }]),
        bare
    );
    assert_eq!(
        extra_tool_entries(&[HostedTool::XSearch { options: None }]),
        bare
    );
}

#[test]
fn function_web_search_kept_when_no_hosted_tools() {
    let req = ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_tools(vec![
        ToolSpec {
            name: "web_search".to_string(),
            description: None,
            parameters: serde_json::json!({"type": "object"}),
            exposure: ToolExposure::default(),
        },
    ]);

    let responses_req: rs::CreateResponse = (&req).into();
    let tools = responses_req.tools.expect("tools should be set");
    let function_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| match t {
            rs::Tool::Function(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(function_names, vec!["web_search"]);
}

/// The `- <name>: <description>` block the declaration's description carries, sliced out
/// of the surrounding donor template so a test can pin the block without repeating it.
fn tool_search_source_block(description: &str) -> &str {
    let (_, rest) = description
        .split_once("You have access to tools from the following sources:\n")
        .expect("sources sentence");
    rest.split_once("\nSome of the tools may not have been provided")
        .expect("discovery instructions")
        .0
}

/// An object's keys in serialized order. `serde_json`'s object equality ignores key order, so a
/// reorder that does move the bytes is only visible as this sequence.
fn json_keys(value: &serde_json::Value) -> Vec<String> {
    value
        .as_object()
        .expect("json value is an object")
        .keys()
        .cloned()
        .collect()
}

/// Donor key order and the fixed half of the declaration. `preserve_order` makes the insertion
/// order the wire order, and `serde_json`'s object equality ignores it, so the sequence is
/// asserted beside the value.
fn assert_declaration_fixed_half_is_donor_exact(entry: &serde_json::Value) {
    assert_eq!(
        entry["parameters"],
        serde_json::json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "number",
                    "description": "Maximum number of tools to return. Defaults to 8.",
                },
                "query": {
                    "type": "string",
                    "description": "Search query for deferred tools.",
                },
            },
            "required": ["query"],
            "additionalProperties": false,
        }),
        "`limit` before `query` is the donor `BTreeMap` sort"
    );

    let keys_at = |pointer: &str| -> Vec<String> {
        entry
            .pointer(pointer)
            .and_then(serde_json::Value::as_object)
            .expect("object at pointer")
            .keys()
            .cloned()
            .collect()
    };
    assert_eq!(entry["type"], "tool_search");
    assert_eq!(entry["execution"], "client");
    assert_eq!(
        keys_at(""),
        ["type", "execution", "description", "parameters"]
    );
    assert_eq!(
        keys_at("/parameters"),
        ["type", "properties", "required", "additionalProperties"]
    );
    assert_eq!(keys_at("/parameters/properties"), ["limit", "query"]);
    assert_eq!(
        keys_at("/parameters/properties/limit"),
        ["type", "description"]
    );
    assert_eq!(
        keys_at("/parameters/properties/query"),
        ["type", "description"]
    );
}

/// The half of the declaration that carries no source data — `type`, `execution`,
/// `parameters` and their key order — does not move with the advertised source set.
#[test]
fn tool_search_declaration_fixed_half_is_donor_exact() {
    let drive =
        "Use Google Drive as the single entrypoint for Drive, Docs, Sheets, and Slides work.";
    let sources = [ToolSearchSource {
        name: "Google Drive",
        description: Some(drive),
    }];
    for listing in [
        ToolSearchSourceListing::Include,
        ToolSearchSourceListing::Omit,
    ] {
        let entry = tool_search_declaration_entry(
            ToolSearchExecution::Client,
            &sources,
            listing,
            /*default_limit*/ 8,
        );
        assert_declaration_fixed_half_is_donor_exact(&entry);
    }
}

/// `default_limit` is interpolated into the `limit` description, never baked into it: a row
/// configured with a different default must advertise its own number or the schema describes a
/// tool that does not exist. Pinned at a non-default value because every call site here and the
/// CX1 capture use 8, which a constant string would satisfy.
///
/// The second half of that claim — that `default_limit` interpolates ONLY into the `limit`
/// description — is enforced by building the entry twice with `ToolSearchSourceListing::Include`
/// and a real source, at `default_limit` 3 and 8, and requiring the two `description`s to be
/// byte-identical: an added interpolation site anywhere in the prose splits the two strings. That
/// form was chosen over a second hand-written literal because the description's bytes are already
/// donor-pinned twice in this file, and a third copy would rot the same way the copy it duplicates
/// would; the limit itself is the only moving part here. A hard-coded interpolation of
/// this test's own value would survive that compare, so the donor prose — the description with its
/// rendered source block removed — is additionally required to carry no `3`. The sentinel need only
/// be a digit the prose lacks: it carries exactly the digits `2` and `5`, both from `BM25`, so 0, 1,
/// 3, 4, 6, 7 and 9 would serve equally, while a default of 2 or 5 would trip that guard on a digit
/// the prose carries anyway. `8` is absent from the prose too, but it would duplicate the other
/// side of the `at_3 == at_8` compare, so it is not used here. The
/// block is excluded because its text is this test's own input: a source
/// name that happened to contain the digit would otherwise fail the guard for the wrong reason.
#[test]
fn tool_search_declaration_limit_description_tracks_default_limit() {
    let entry = tool_search_declaration_entry(
        ToolSearchExecution::Client,
        &[],
        ToolSearchSourceListing::Omit,
        /*default_limit*/ 3,
    );

    assert_eq!(
        entry["parameters"],
        serde_json::json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "number",
                    "description": "Maximum number of tools to return. Defaults to 3.",
                },
                "query": {
                    "type": "string",
                    "description": "Search query for deferred tools.",
                },
            },
            "required": ["query"],
            "additionalProperties": false,
        }),
        "only the interpolated default moves; the rest of the schema stays donor-pinned"
    );

    let drive = "Use Google Drive as the single entrypoint for Drive, Docs, Sheets, and Slides.";
    let description_at = |default_limit| {
        tool_search_declaration_entry(
            ToolSearchExecution::Client,
            &[ToolSearchSource {
                name: "Google Drive",
                description: Some(drive),
            }],
            ToolSearchSourceListing::Include,
            default_limit,
        )["description"]
            .as_str()
            .expect("description is a string")
            .to_owned()
    };
    let at_3 = description_at(3);
    let at_8 = description_at(8);
    assert!(
        at_3.contains("- Google Drive: "),
        "the listing has to be rendered for the compare below to mean anything: {at_3}"
    );
    assert_eq!(
        at_3, at_8,
        "`default_limit` interpolates into the `limit` description only, never into the prose"
    );
    let prose = at_3.replace(tool_search_source_block(&at_3), "");
    assert!(
        !prose.contains('3'),
        "the donor prose carries no trace of the request's default limit: {prose}"
    );
}

/// Whole-entry byte parity against captured wire bytes. The expected entry is a copy of the
/// `tool_search` declaration captured at index 11 of the 13-entry `tools` array in
/// `/Users/palanisd/Projects/upstream/grok/plans/harness/hosted-tool-search/ratchet-capture/fixtures/codex/CX1-toolsearch-mcp-dryrun/request.json`.
/// That capture lives in the campaign tree, outside this worktree, so nothing reads it at run
/// time and a transcription error here cannot fail on its own —
/// [`tool_search_declaration_fixed_half_is_donor_exact`] pins the source-independent half
/// alongside it for that reason.
#[test]
fn tool_search_declaration_matches_cx1_fixture_bytes() {
    let entry = tool_search_declaration_entry(
        ToolSearchExecution::Client,
        &[
            ToolSearchSource {
                name: "Multi-agent tools",
                description: Some("Spawn and manage sub-agents."),
            },
            ToolSearchSource {
                name: "ratchet_fixture",
                description: Some("Deterministic fixture server for wire-fingerprint capture."),
            },
        ],
        ToolSearchSourceListing::Include,
        /*default_limit*/ 8,
    );

    assert_eq!(
        entry,
        serde_json::json!({
            "type": "tool_search",
            "execution": "client",
            "description": "# Tool discovery\n\nSearches over deferred tool metadata with BM25 and exposes matching tools for the next model call.\n\nYou have access to tools from the following sources:\n- Multi-agent tools: Spawn and manage sub-agents.\n- ratchet_fixture: Deterministic fixture server for wire-fingerprint capture.\nSome of the tools may not have been provided to you upfront, and you should use this tool (`tool_search`) to search for the required tools. For MCP tool discovery, always use `tool_search` instead of `list_mcp_resources` or `list_mcp_resource_templates`.",
            "parameters": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of tools to return. Defaults to 8.",
                    },
                    "query": {
                        "type": "string",
                        "description": "Search query for deferred tools.",
                    },
                },
                "required": ["query"],
                "additionalProperties": false,
            },
        }),
    );
    assert_declaration_fixed_half_is_donor_exact(&entry);
}

/// `execution` is total over the only two values the wire accepts, so the donor's `sync` —
/// which the API 400s — has no value to reach the entry through.
#[test]
fn tool_search_declaration_execution_is_total() {
    assert_eq!(ToolSearchExecution::Server.as_str(), "server");
    assert_eq!(
        ToolSearchExecution::Client.as_str(),
        super::tool_search::CLIENT_EXECUTION,
        "`client` is defined once in the crate, by the discovery module"
    );

    let wire_execution = |execution: ToolSearchExecution| {
        tool_search_declaration_entry(
            execution,
            &[],
            ToolSearchSourceListing::Omit,
            /*default_limit*/ 8,
        )["execution"]
            .clone()
    };
    assert_eq!(wire_execution(ToolSearchExecution::Server), "server");
    assert_eq!(wire_execution(ToolSearchExecution::Client), "client");
}

/// Donor listing semantics: one line per name sorted by name, the first description a name ever
/// carries winning whichever order the duplicates arrive in, a bare line for an undescribed
/// source, and no listing under `Omit`. Both duplicate pairs pinned here carry exactly one blank
/// description, so they read the same under first-non-empty and last-non-empty coalescing; two
/// competing non-empty descriptions are pinned by
/// [`tool_search_source_listing_first_description_wins_among_duplicates`].
#[test]
fn tool_search_source_listing_rendering() {
    let drive =
        "Use Google Drive as the single entrypoint for Drive, Docs, Sheets, and Slides work.";
    assert_eq!(
        tool_search_source_block(&tool_search_description(
            &[
                ToolSearchSource {
                    name: "Google Drive",
                    description: Some(drive),
                },
                ToolSearchSource {
                    name: "Google Drive",
                    description: None,
                },
                ToolSearchSource {
                    name: "docs",
                    description: None,
                },
            ],
            ToolSearchSourceListing::Include,
        )),
        format!("- Google Drive: {drive}\n- docs"),
        "a later blank must not erase an earlier description"
    );

    assert_eq!(
        tool_search_source_block(&tool_search_description(
            &[
                ToolSearchSource {
                    name: "Slack",
                    description: None,
                },
                ToolSearchSource {
                    name: "Slack",
                    description: Some("Search Slack messages and channels."),
                },
            ],
            ToolSearchSourceListing::Include,
        )),
        "- Slack: Search Slack messages and channels.",
        "a later description must fill the earlier blank"
    );

    assert_eq!(
        tool_search_source_block(&tool_search_description(
            &[],
            ToolSearchSourceListing::Include,
        )),
        "None currently enabled."
    );

    let omitted = tool_search_description(
        &[ToolSearchSource {
            name: "Google Drive",
            description: Some(drive),
        }],
        ToolSearchSourceListing::Omit,
    );
    assert!(
        omitted
            .contains("for the next model call.\n\nSome of the tools may not have been provided"),
        "omit must join the template with a bare blank line: {omitted}"
    );
    assert!(!omitted.contains("You have access to tools from the following sources"));
    assert!(!omitted.contains("Google Drive"));
}

/// The coalescing rule is first-to-arrive, not last-non-empty and not lexicographically smallest.
/// Neither duplicate pair in [`tool_search_source_listing_rendering`] can see this: each carries
/// one blank description, so first-non-empty, last-non-empty and "skip the blanks, keep one" all
/// agree there. Here both duplicates are non-empty and distinct, and both arrival orders are
/// pinned so the surviving description is the one that arrived first.
#[test]
fn tool_search_source_listing_first_description_wins_among_duplicates() {
    let search = "Search Slack messages and channels.";
    let summarize = "Summarize Slack threads and channels.";

    assert_eq!(
        tool_search_source_block(&tool_search_description(
            &[
                ToolSearchSource {
                    name: "Slack",
                    description: Some(search),
                },
                ToolSearchSource {
                    name: "Slack",
                    description: Some(summarize),
                },
            ],
            ToolSearchSourceListing::Include,
        )),
        format!("- Slack: {search}"),
        "a second non-empty description must not displace the first"
    );

    assert_eq!(
        tool_search_source_block(&tool_search_description(
            &[
                ToolSearchSource {
                    name: "Slack",
                    description: Some(summarize),
                },
                ToolSearchSource {
                    name: "Slack",
                    description: Some(search),
                },
            ],
            ToolSearchSourceListing::Include,
        )),
        format!("- Slack: {summarize}"),
        "the survivor follows arrival order, not the two strings' own order"
    );
}

/// The one input in `render_tool_search_sources`'s domain no other fixture in this directory
/// reaches: a description that is present but empty, in both duplicate orders — empty then real,
/// empty then `None`. This is THIS HARNESS's behaviour, not donor-pinned parity: the donor's fold
/// (`tool_search_spec.rs:38-45`) and render (`:73-81`) are byte-EQUIVALENT to this crate's `by_name`
/// BTreeMap loop and `for (name, description) in by_name` loop in `render_tool_search_sources`, not
/// identical to them. The fold matches line for line, the donor just cloning the
/// `String`/`Option<String>` this crate carries by `&str`; the render differs at its truncation call
/// — this crate's `truncate_bytes` call against `take_bytes_at_char_boundary` (`:78-79`) — and that
/// call's wrapped second line is why the donor's clause runs to `:81`; this crate's is one line.
/// The helpers agree byte for byte: whole value if it fits, else back off to a char boundary
/// (`conversation.rs:33-42` against `codex-rs/utils/string/src/lib.rs:13-26`). So the bytes the
/// donor would produce for an empty description are citable and would agree, but the donor never
/// exercises one — its three tests pass `Some(..)`/`None` only (`:122`, `:129`, `:133`, `:164`,
/// `:183`) — so no donor expectation backs
/// these pins. The coalescing fills a `None` and nothing else, so an empty first arrival survives
/// and the real description behind it is dropped; the render matches `Some(_)`, so it still emits
/// `": "` and still charges those 2 bytes to the shared budget.
#[test]
fn tool_search_source_listing_treats_an_empty_description_as_a_description() {
    assert_eq!(
        tool_search_source_block(&tool_search_description(
            &[
                ToolSearchSource {
                    name: "Slack",
                    description: Some(""),
                },
                ToolSearchSource {
                    name: "Slack",
                    description: Some("Search Slack messages and channels."),
                },
            ],
            ToolSearchSourceListing::Include,
        )),
        "- Slack: ",
        "an empty first arrival is `is_some()`, so the later real description is dropped"
    );

    // The other direction of the same rule: a later `None` does not evict an empty first arrival, so
    // the empty slot keeps its separator. Read an empty description as absent anywhere in the fold
    // or the render and this collapses to a bare `- Slack`.
    assert_eq!(
        tool_search_source_block(&tool_search_description(
            &[
                ToolSearchSource {
                    name: "Slack",
                    description: Some(""),
                },
                ToolSearchSource {
                    name: "Slack",
                    description: None,
                },
            ],
            ToolSearchSourceListing::Include,
        )),
        "- Slack: ",
        "an empty first arrival survives a later `None`"
    );

    // The dangling separator is not free. `reserved = (2 - 1) + (2 + 3) + (2 + 3) = 11` opens the
    // budget at `524_288 - 11 = 524_277`; `aaa` spends 2 budget bytes on its separator and nothing on
    // text, leaving 524_275. `bbb` then pays 2 more for its own separator, so its 524_274-byte
    // description has 524_273 bytes of room and is cut by 1: the charge is 2 budget bytes, the loss
    // is the single byte it happened to be over. Without `aaa`'s charge the room is 524_275 and the
    // description stays whole.
    let long = "y".repeat(524_274);
    let sources = [
        ToolSearchSource {
            name: "aaa",
            description: Some(""),
        },
        ToolSearchSource {
            name: "bbb",
            description: Some(&long),
        },
    ];
    assert_eq!(
        tool_search_source_block(&tool_search_description(
            &sources,
            ToolSearchSourceListing::Include,
        )),
        format!("- aaa: \n- bbb: {}", "y".repeat(524_273)),
        "an empty description renders the separator alone and charges it to the shared budget"
    );
}

/// The donor keys the source map by `String`, so lines sort in byte order: `Alpha` (0x41) leads,
/// `Zeta` (0x5A) follows, `alpha` (0x61) trails. What this trio alone pins is a name-keyed render
/// against a description-keyed one: its descriptions open `A` (0x41), `m` (0x6D), `z` (0x7A), the
/// exact reverse of the names' byte order, so a render keyed on the description rather than the name
/// emits the three lines backwards. A length-keyed sort puts the 4-byte `Zeta` first, because
/// `Alpha`/`alpha` are 5 bytes apiece; an insertion-order render starts at `alpha`. Case-folding is
/// split here too — a case-insensitive sort groups `Alpha` with `alpha` and sends `Zeta` last — but
/// that is not this trio's own contribution: the `Google Drive`/`docs` pair in
/// [`tool_search_source_listing_rendering`] splits it as well (`G` 0x47 leads `d` 0x64 bytewise, `g`
/// 0x67 trails it case-folded), while the CX1 fixture's two names sort the same both ways and cannot
/// see it at all. Only a name-keyed byte order yields the block below.
#[test]
fn tool_search_source_listing_sorts_by_byte_order() {
    assert_eq!(
        tool_search_source_block(&tool_search_description(
            &[
                ToolSearchSource {
                    name: "alpha",
                    description: Some("Apple: 0x61 trails, tied on length with Alpha"),
                },
                ToolSearchSource {
                    name: "Zeta",
                    description: Some("middle: 0x5A follows, though it is the shortest name"),
                },
                ToolSearchSource {
                    name: "Alpha",
                    description: Some("zebra: 0x41 leads the set"),
                },
            ],
            ToolSearchSourceListing::Include,
        )),
        "- Alpha: zebra: 0x41 leads the set\n\
         - Zeta: middle: 0x5A follows, though it is the shortest name\n\
         - alpha: Apple: 0x61 trails, tied on length with Alpha",
        "only a name-keyed byte order sort keeps this three-line block"
    );
}

/// Past the shared budget the descriptions give way and the names never do: every source keeps a
/// complete `- name` line, a cut description stops on a UTF-8 char boundary, a source whose own
/// `- name` line cannot fit is skipped whole rather than sliced, and a source that reaches the
/// render with no budget left still carries the `": "` separator.
///
/// These pins are deliberately STRICTER than the donor's own budget test, which asserts only
/// `len() <= MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES` plus a tolerant name read
/// (`split_once(": ").map_or(..)`). That is a ratchet on the reserved-bytes formula: a donor change
/// to how the name lines are reserved, or to the cap itself, is expected to fail here loudly, and
/// the cap is asserted under its own name first so such a change never reports as a char-boundary
/// failure. The literal `11_044` is kept rather than derived from the constant, because deriving it
/// would make the pin tautological.
#[test]
fn tool_search_source_listing_budget_keeps_names() {
    assert_eq!(MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES, 512 * 1024);
    let long = "🦀".repeat(20_000);
    let names: Vec<String> = (0..8).map(|index| format!("source-{index:02}")).collect();
    let sources: Vec<ToolSearchSource<'_>> = names
        .iter()
        .map(|name| ToolSearchSource {
            name: name.as_str(),
            description: Some(&long),
        })
        .collect();

    let described = tool_search_description(&sources, ToolSearchSourceListing::Include);
    let block = tool_search_source_block(&described);
    let lines: Vec<&str> = block.lines().collect();
    let advertised_names: Vec<&str> = lines
        .iter()
        .map(|line| {
            let source = line
                .strip_prefix("- ")
                .expect("each source should be a complete list item");
            source.split_once(": ").map_or(source, |(name, _)| name)
        })
        .collect();

    assert_eq!(
        advertised_names, names,
        "names are never truncated or dropped"
    );
    assert!(
        block.len() <= MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES,
        "source block is {} bytes",
        block.len(),
    );
    assert!(block.starts_with("- source-00: 🦀"));
    assert_eq!(
        block.matches(&long).count(),
        6,
        "a description that fits must stay whole"
    );

    // The name lines are reserved out of the shared budget first: `(8 - 1) + 8 * (2 + 9)` = 95
    // bytes for these names leaves 524_193 for eight identical 80_000-byte descriptions, so six
    // fit whole, the seventh loses 2 bytes to its separator and is cut from the remaining
    // 44_179 down to 44_176 (11_044 whole code points), leaving 3.
    assert_eq!(
        lines[6],
        format!("- source-06: {}", "🦀".repeat(11_044)),
        "the last described line takes the remaining budget on a char boundary"
    );
    // No per-line boundary scan is asserted: `lines[6]` and the exhausted-budget line below pin the
    // only two cut descriptions byte for byte, `matches(&long)` above pins the six whole ones, and a
    // `&str` cannot hold a partial code point anyway — a scan could only restate those pins.
    let cut_description = lines[6]
        .split_once(": ")
        .expect("the cut line keeps its separator")
        .1;
    assert_eq!(
        cut_description.len(),
        44_176,
        "bytes taken by the partially rendered description"
    );

    // The gate is `description_budget >= 2`, not "some description still fits", so the last line
    // pays 2 bytes for the separator and `truncate_bytes(.., 1)` yields an empty remainder. The
    // donor is byte-equivalent here — `tool_search_spec.rs:74-79` pushes `": "` (`:76`), subtracts
    // 2 (`:77`) and calls `take_bytes_at_char_boundary` (`:78-79`; this crate's `truncate_bytes`) —
    // and its own budget test reads names tolerantly (`split_once(": ").map_or(..)`) for exactly
    // this tail. The dangling separator is parity; do not "clean it up".
    assert_eq!(
        lines.last().copied(),
        Some("- source-07: "),
        "an exhausted budget keeps the separator with an empty description"
    );

    let oversized = "x".repeat(MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES);
    assert_eq!(
        tool_search_source_block(&tool_search_description(
            &[
                ToolSearchSource {
                    name: "a",
                    description: Some("short"),
                },
                ToolSearchSource {
                    name: &oversized,
                    description: Some("short"),
                },
            ],
            ToolSearchSourceListing::Include,
        )),
        "- a",
        "an unfillable name is skipped whole, and its reserved bytes leave no description budget"
    );
}

/// The reserved-name fold runs over the deduplicated names, not over the `sources` slice: three
/// arrivals of one name reserve their `- <name>` bytes once, and exactly one line renders. The
/// 8-unique-name fixture in [`tool_search_source_listing_budget_keeps_names`] cannot see the
/// difference — with every name unique, folding `sources` adds the same bytes folding
/// `by_name.keys()` does. The survivor stays described only because the first arrival carries the
/// description, which is the coalescing rule pinned by
/// [`tool_search_source_listing_first_description_wins_among_duplicates`]: change that rule and this
/// test reddens as well, with a failure message that blames the reserved bytes rather than the
/// coalescing.
///
/// The cap is the only seam that can show it: `MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES` is a
/// fixed constant with no per-request input, so a reservation is observable only once the
/// descriptions actually reach it — the same reason the 8-name fixture is 8 × 80 000 bytes.
///
/// Three arrivals of `dup` (3 bytes), the first described: the keys fold reserves
/// `(1 - 1) + (2 + 3) = 5`, so the description budget is `524_288 - 5 = 524_283`; the `": "`
/// separator takes 2 of that and a `524_275`-byte description still fits whole, for a
/// `7 + 524_275 = 524_282`-byte block. A fold over the input slice would reserve
/// `(3 - 1) + 3 * (2 + 3) = 17`, leaving `524_269` after the separator and cutting 6 bytes off the
/// description.
#[test]
fn tool_search_source_listing_budget_counts_a_duplicated_name_once() {
    let long = "y".repeat(524_275);
    let sources = [
        ToolSearchSource {
            name: "dup",
            description: Some(&long),
        },
        ToolSearchSource {
            name: "dup",
            description: None,
        },
        ToolSearchSource {
            name: "dup",
            description: None,
        },
    ];

    let described = tool_search_description(&sources, ToolSearchSourceListing::Include);
    let block = tool_search_source_block(&described);

    assert_eq!(
        block.lines().count(),
        1,
        "three arrivals of one name render exactly one line"
    );
    assert_eq!(
        block,
        format!("- dup: {long}"),
        "the `- dup` line is reserved once, so the whole description still fits"
    );
    assert!(
        block.len() <= MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES,
        "source block is {} bytes",
        block.len(),
    );
}

/// The fit gate is `render_tool_search_sources`'s `if required > MAX - rendered.len()` comparison,
/// and the comparisons either side of `required == MAX - rendered.len()` are its whole behaviour.
/// No pre-existing test in this directory sat on that boundary: the smallest `room` any of their
/// gate comparisons sees is the 15 that [`tool_search_source_listing_budget_keeps_names`] presents
/// at its last gate (`required` 12; the block it emits is 524_287 bytes, ONE byte under the cap, so
/// 15 is the room open at that gate and not the margin of the result). That fixture's own over-cap
/// case fires the gate from 6 bytes past the line (`required` 524_291 against `room` 524_285). The
/// nearest miss anywhere else is 2, in
/// [`tool_search_source_listing_accounts_name_bytes_not_char_count`] (`required` 524_287 against
/// the same `room` 524_285). Past those three the next-nearest
/// comparison sits 44_193 bytes clear of the line. This test is the only one that stands on the
/// line: the 524_282-byte name has `required` exactly equal to `room` (524_285), so the gate does
/// not fire, and the 524_283-byte name has `required` one byte PAST `room` (524_286), so it skips.
///
/// The boundary is reached through NAME length, not description length, and the reason is an exact
/// cancellation. While `reserved_name_bytes <= MAX`, `room - required` for the LAST name becomes the
/// unspent `description_budget` and is never negative: 3 against 3 at `source-07` of the 8-name
/// fixture, 0 against 0 in the fitting case below. A saturated reservation cancels nothing: the
/// reduction is then `MAX - reserved - Σspent`, negative 6 in this file's own over-cap case.
/// Descriptions can therefore never supply the last byte, which is why name length is the
/// only handle that walks onto the boundary. For a name at index `i` the same cancellation EXCLUDES
/// that name's own charge — `required` pays `separator_bytes + 2 + name.len()` and cancels it
/// against the same amount in the reservation — so `room - required` is the unspent
/// `description_budget` plus `(n - 1 - i) + Σ (2 + name.len())` over the names STRICTLY AFTER `i`:
/// `44_181 + 1 + 11 = 44_193` at `source-06` of the 8-name fixture, `0 + 1 + 524_284 = 524_285` at
/// `a` of the boundary one. While `reserved_name_bytes` fits inside `MAX` the gate can therefore
/// never fire: every skip this render can produce comes from a reservation that saturated. When
/// each name fits on its own that saturation is a property of the whole deduplicated NAME SET —
/// `(n - 1) + Σ (2 + name.len())` over `by_name.keys()` — not of the name being skipped; one
/// oversized name can also do it alone, and the `oversized` name of
/// [`tool_search_source_listing_budget_keeps_names`] charges `2 + 524_288` by itself. A short name
/// is not exempt either: `required` is `3 + name.len()` for every name after the first, so once
/// `rendered` leaves `room` under 4 even a 1-byte name is skipped whole. Name-length arithmetic
/// therefore walks onto the boundary without any prose at all.
///
/// Both cases start from one source, `a` with no description, which renders `- a` (3 bytes) and
/// leaves `room = 524_288 - 3 = 524_285` for the second name (`separator_bytes` 1, so
/// `required = 3 + name.len()`):
/// * a 524_282-byte name: `required` = 524_285 = `room`, so it renders, and the block lands on
///   exactly `MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES` (`3 + 1 + 2 + 524_282`);
/// * a 524_283-byte name: `required` = 524_286 = `room + 1`, so it is skipped whole and the block
///   stays the 3-byte `- a`. The margin is exactly the byte that a dropped `separator_bytes` gives
///   back: under that mutant the line is admitted and the block becomes
///   `3 + 1 + 2 + 524_283 = 524_289`, one byte past the donor cap.
#[test]
fn tool_search_source_listing_fit_gate_boundary() {
    let a = ToolSearchSource {
        name: "a",
        description: None,
    };
    // `a` (0x61) sorts before `x` (0x78), so the short name always renders first.
    let fitting_name = "x".repeat(524_282);
    let fitting = ToolSearchSource {
        name: &fitting_name,
        description: None,
    };
    let fitting_sources = [a, fitting];
    let fitting_described =
        tool_search_description(&fitting_sources, ToolSearchSourceListing::Include);
    let fits = tool_search_source_block(&fitting_described);
    assert_eq!(
        fits.len(),
        MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES,
        "`required == MAX - rendered.len()` must render the line: the gate is `>`, not `>=`"
    );

    let over_name = "x".repeat(524_283);
    let over = ToolSearchSource {
        name: &over_name,
        description: None,
    };
    let over_sources = [a, over];
    let over_described = tool_search_description(&over_sources, ToolSearchSourceListing::Include);
    let skipped = tool_search_source_block(&over_described);
    assert_eq!(
        skipped.len(),
        "- a".len(),
        "`required` one byte past the room must skip the source whole"
    );
}

/// Both `name.len()` sites in `render_tool_search_sources` — the `reserved_name_bytes` fold and the
/// `if required >` comparison — charge UTF-8 BYTES, and every other fixture name in this directory is
/// ASCII, so a slip to `name.chars().count()` is invisible there. Each case below is sized so the
/// two accountings disagree about an observable:
/// * Reservation: one source named `🦀` (4 UTF-8 bytes, 1 code point) with a 524_285-byte
///   description. The byte reserve is `(1 - 1) + (2 + 4) = 6`, so the budget opens at 524_282; the
///   `": "` costs 2 of that and the description is cut to 524_280, filling the block to exactly the
///   524_288-byte cap. A code-point reserve of 3 would open the budget at 524_285, give the
///   description 3 more bytes and emit a 524_291-byte block — 3 past the cap, one per uncounted byte.
/// * Fit gate: `a` plus a name of 262_142 `é`s (524_284 bytes, 262_142 code points). After `- a` the
///   room is 524_285 and `required` is `1 + 2 + 524_284 = 524_287`, 2 over, so the source is skipped
///   whole and the block stays 3 bytes. Under code-point counting `required` would be 262_145, the
///   line would render, and the block would be `3 + 1 + 2 + 524_284 = 524_290` — 2 past the cap. The
///   margin is 2 rather than 1 so this pin stays green under the dropped-`separator_bytes` mutant
///   above and reddens only for a code-point slip.
#[test]
fn tool_search_source_listing_accounts_name_bytes_not_char_count() {
    let long = "y".repeat(524_285);
    let described = tool_search_description(
        &[ToolSearchSource {
            name: "🦀",
            description: Some(&long),
        }],
        ToolSearchSourceListing::Include,
    );
    let (_, description) = tool_search_source_block(&described)
        .split_once(": ")
        .expect("the described line keeps its separator");
    assert_eq!(
        description.len(),
        524_280,
        "the reservation charges the crab's 4 UTF-8 bytes, not its 1 code point"
    );

    let wide_name = "é".repeat(262_142);
    let a = ToolSearchSource {
        name: "a",
        description: None,
    };
    let wide = ToolSearchSource {
        name: &wide_name,
        description: None,
    };
    let wide_sources = [a, wide];
    let wide_described = tool_search_description(&wide_sources, ToolSearchSourceListing::Include);
    let skipped = tool_search_source_block(&wide_described);
    assert_eq!(
        skipped.len(),
        "- a".len(),
        "a multi-byte name wider than the room is skipped whole, not admitted on a code-point count"
    );
}

/// D3-A placement, as far as this crate can see it: an admitted route leads the raw-JSON channel
/// this crate returns with the declaration, so the entries handed to the sampler are
/// `tool_search, web_search, x_search` — that slice's own order, not a rule placed here, exactly as
/// `extra_tool_entries_with_declaration`'s own doc has it. The splice of those entries into the
/// serialized body's top-level `tools` array is the sampler's
/// (`xai-grok-sampler/src/client.rs:879`, `splice_extra_tool_entries`), and a wire-order
/// assertion for it belongs there.
///
/// What the entry compare here pins is order and count only: its expected declaration is built by
/// the same producer, so the entry's own bytes contribute nothing to it. They are pinned by the
/// donor-parity tests instead — [`tool_search_declaration_matches_cx1_fixture_bytes`] for the whole
/// entry and [`tool_search_declaration_fixed_half_is_donor_exact`] for the half that carries no
/// source data — and both that helper and a key-order pin read the element the function actually
/// emitted, not the value handed to it, so a re-key or a normalisation inside
/// `extra_tool_entries_with_declaration` cannot hide behind `serde_json`'s order-blind equality.
///
/// No production caller passes a declaration yet: `extra_tool_entries`
/// passes `declaration: None` and all three sampler call sites (`client.rs:2652`, `client.rs:3415`,
/// `client.rs:3509`) call exactly that, so no route can put this entry on the wire and this test
/// cannot fail on that account. The top-level splice is the sampler's own
/// (`client.rs:879` `splice_extra_tool_entries`; its tests at `client.rs:3779-3800` splice hosted
/// entries only, never a declaration). The end-to-end proof is owed by apex-waj.9 (wiring) and
/// apex-waj.20 (live arm).
#[test]
fn declaration_leads_the_raw_json_channel_when_admitted() {
    assert!(
        SearchAdmission {
            supports_search_tool: true,
            has_searchable_tools: true,
        }
        .admitted(),
        "an admitted route advertises the declaration"
    );
    let mut admission = SearchAdmission {
        supports_search_tool: false,
        has_searchable_tools: false,
    };
    for (supports_search_tool, has_searchable_tools) in
        [(false, false), (false, true), (true, false)]
    {
        admission.supports_search_tool = supports_search_tool;
        admission.has_searchable_tools = has_searchable_tools;
        assert!(
            !admission.admitted(),
            "both inputs are required, got {admission:?}"
        );
    }

    let hosted = [HostedTool::WebSearch { options: None }];
    let declaration = || {
        tool_search_declaration_entry(
            ToolSearchExecution::Client,
            &[],
            ToolSearchSourceListing::Omit,
            /*default_limit*/ 8,
        )
    };
    // The value compare cannot pin the entry this test places (object equality ignores key
    // order), so every pin below reads the emitted element rather than the value handed in.
    let entries = extra_tool_entries_with_declaration(&hosted, Some(declaration()));
    assert_declaration_fixed_half_is_donor_exact(&entries[0]);
    assert_eq!(
        json_keys(&entries[0]),
        ["type", "execution", "description", "parameters"],
        "the declaration keeps the donor key order on its way out"
    );
    assert_eq!(
        entries,
        vec![declaration(), serde_json::json!({"type": "web_search"})],
        "the declaration leads the hosted entries"
    );

    // With no function tools the typed body this crate builds carries no `tools` key at all, so
    // the raw-JSON channel is the declaration's only route; what the splice makes of that is the
    // sampler's `splice_extra_tool_entries_creates_tools_array_when_absent` test.
    let mut req = ConversationRequest::from_items(vec![ConversationItem::user("hi")]);
    req.hosted_tools = hosted.to_vec();
    let body: rs::CreateResponse = (&req).into();
    assert_matches!(body.tools, None);
}

/// What this pins is the rendering of a declaration built with `ToolSearchSourceListing::Include`
/// and an EMPTY source list: it still renders the donor's `None currently enabled.`
/// (`core/src/tools/handlers/tool_search_spec.rs:48-49`) and it still leads the hosted entries.
/// Admission is a separate question and is not asserted here — `SearchAdmission` has no source-list
/// input at all, so the emptiness of this list could not change an `admitted()` verdict even by
/// mistake (a coupling would be a compile error), and the truth table is pinned by
/// [`declaration_leads_the_raw_json_channel_when_admitted`]. The admission-to-manifest wiring is
/// owned by apex-waj.9 and the live arm by apex-waj.20.
///
/// No production caller passes a declaration yet: `extra_tool_entries` passes
/// `declaration: None` and all three sampler call sites (`client.rs:2652`, `client.rs:3415`,
/// `client.rs:3509`) call exactly that, so this declaration cannot reach the wire and this test
/// cannot fail on that account. The top-level splice is the sampler's own (`client.rs:879`
/// `splice_extra_tool_entries`; its tests at `client.rs:3779-3800` splice hosted entries only,
/// never a declaration). The end-to-end proof is owed by apex-waj.9 and apex-waj.20.
#[test]
fn admitted_route_with_no_advertised_sources_still_declares_search() {
    let declaration = || {
        tool_search_declaration_entry(
            ToolSearchExecution::Client,
            &[],
            ToolSearchSourceListing::Include,
            /*default_limit*/ 8,
        )
    };
    let hosted = [HostedTool::WebSearch { options: None }];
    // Every pin below reads the element the function emitted, never the value handed to it, so a
    // re-key or a normalisation inside `extra_tool_entries_with_declaration` cannot hide behind
    // `serde_json`'s order-blind value equality.
    let entries = extra_tool_entries_with_declaration(&hosted, Some(declaration()));
    assert_declaration_fixed_half_is_donor_exact(&entries[0]);
    assert_eq!(
        json_keys(&entries[0]),
        ["type", "execution", "description", "parameters"],
        "the declaration keeps the donor key order on its way out"
    );
    assert_eq!(
        entries,
        vec![declaration(), serde_json::json!({"type": "web_search"})],
        "the declaration is emitted with an empty source list"
    );
    assert_eq!(
        tool_search_source_block(
            entries[0]["description"]
                .as_str()
                .expect("description is a string"),
        ),
        "None currently enabled.",
        "the donor declares the tool rather than withholding it"
    );
}

/// A route that is not admitted passes no declaration, and that path must not change a single
/// byte: the hosted entries are pinned by VALUE and by KEY ORDER. `serde_json`'s object equality
/// ignores key order (see [`assert_declaration_fixed_half_is_donor_exact`]), so the value compare
/// alone would survive a reorder inside `WebSearchOptions::to_tool_entry()` that does move the
/// serialized bytes; the key sequences below close that hole. The `filters` sub-object carries a
/// single key on every validating ingress (`WebSearchOptions::validate` rejects both lists
/// together), so only that one key is pinned here; the two-key order a directly constructed
/// [`WebSearchOptions`] still emits is pinned by
/// [`directly_constructed_web_search_options_pin_both_filter_keys`]. Nothing at all stays an empty
/// vec (which the splice short-circuits, leaving `tools` untouched).
///
/// No production caller passes a declaration yet: `extra_tool_entries` passes
/// `declaration: None`, which is exactly the branch under test, so this is the only path any route
/// takes today; the admitted branch is inert until apex-waj.9 (wiring) lands and apex-waj.20 (live
/// arm) proves it. The top-level splice a declaration would ride is the sampler's own
/// (`client.rs:879` `splice_extra_tool_entries`).
#[test]
fn non_admitted_routes_emit_no_declaration() {
    let hosted = [
        HostedTool::WebSearch {
            options: Some(WebSearchOptions {
                allowed_domains: Some(vec!["docs.x.ai".into()]),
                excluded_domains: None,
            }),
        },
        HostedTool::XSearch { options: None },
    ];
    let entries = extra_tool_entries_with_declaration(&hosted, None);
    assert_eq!(
        // Pinned by value so a drift in `extra_tool_entries` cannot cancel itself out here.
        entries,
        vec![
            serde_json::json!({
                "type": "web_search",
                "filters": { "allowed_domains": ["docs.x.ai"] },
            }),
            serde_json::json!({"type": "x_search"}),
        ]
    );

    // `preserve_order` makes the producer's insertion order the wire order, so the sequences are
    // asserted next to the value: a reorder is a wire change value equality cannot see.
    assert_eq!(json_keys(&entries[0]), ["type", "filters"]);
    assert_eq!(
        json_keys(&entries[0]["filters"]),
        ["allowed_domains"],
        "an absent `excluded_domains` is skipped, never emitted as null"
    );
    assert_eq!(json_keys(&entries[1]), ["type"]);

    assert_eq!(
        extra_tool_entries_with_declaration(&[], None),
        Vec::<serde_json::Value>::new()
    );
}

/// The pair order the single-key pin above cannot reach. Both keys are emitted whenever both lists
/// hold entries — `to_tool_entry` serializes what it is handed — and both fields are public, so
/// nothing type-level stops a caller from building that value. `validate` rejects the combination only
/// when both lists are non-empty (`tool_overrides.rs:189-192`: a `Some(vec![])` beside a populated
/// list passes), and it gates every literal outside a test module: the deserialize ingress builds
/// through `TryFrom<WebSearchOptionsWire>` (`tool_overrides.rs:273`, validated at `:277`), and the
/// one builder that skips that ingress — `web_search_options_from_section` in the sibling
/// `xai-grok-shell` crate (`src/util/config/resolve/toolset.rs:500`, validated at `:507`, its
/// both-set arm rebuilt with `excluded_domains: None` at `:517-519`) — is the only other one. Every
/// remaining literal in the tree sits in a test module, so no config path reaches this shape and a
/// direct construction is the only way to pin it. The expected sequence is the producer's own field
/// order: the `WebSearchToolFilters` local to `to_tool_entry` (`tool_overrides.rs:214-219`) declares
/// `allowed_domains` before `excluded_domains`, each `skip_serializing_if` only `Option::is_none`,
/// emptiness having been normalised to `None` one statement earlier (`:232-233`) — never an explicit
/// null. That pair is also alphabetical order, so the sequence catches a swapped field pair but
/// cannot tell declaration order from a sorted one; do not over-trust it as a declaration-order
/// proof.
///
/// The whole-entry value compare freezes PRODUCER behaviour, not an API-accepted request: `validate`
/// rejects this combination, a request carrying both filters is expected to 400 upstream, and no
/// capture under `smoke/redteam/` carries either filter key, let alone both. It is kept as a freeze
/// on what `to_tool_entry` emits for a directly constructed value.
#[test]
fn directly_constructed_web_search_options_pin_both_filter_keys() {
    let options = WebSearchOptions {
        allowed_domains: Some(vec!["docs.x.ai".into()]),
        excluded_domains: Some(vec!["reddit.com".into()]),
    };
    // `tool_overrides.rs:420-426 deserialize_hard_errors_on_both_set` already proves this
    // combination is rejected, but tolerantly — `is_err()` on the deserialize ingress. This is the
    // only assertion that NAMES the variant, so a guard that started rejecting for some other
    // reason would still read green over there.
    assert_matches!(
        options.validate(),
        Err(WebSearchOptionsError::BothAllowedAndExcluded)
    );

    let entries = extra_tool_entries_with_declaration(
        &[HostedTool::WebSearch {
            options: Some(options),
        }],
        None,
    );
    assert_eq!(
        entries,
        vec![serde_json::json!({
            "type": "web_search",
            "filters": {
                "allowed_domains": ["docs.x.ai"],
                "excluded_domains": ["reddit.com"],
            },
        })]
    );
    assert_eq!(json_keys(&entries[0]), ["type", "filters"]);
    // Declaration order, which here is also alphabetical order — see the doc above.
    assert_eq!(
        json_keys(&entries[0]["filters"]),
        ["allowed_domains", "excluded_domains"],
        "the allowlist precedes the blocklist, as the wire struct declares them"
    );
}

#[test]
fn test_responses_api_response_to_conversation_item() {
    use crate::rs;

    // Create a Response with text output
    let response = rs::Response {
        background: None,
        billing: None,
        conversation: None,
        created_at: 1234567890,
        completed_at: None,
        error: None,
        id: "resp_123".to_string(),
        incomplete_details: None,
        instructions: None,
        max_output_tokens: None,
        metadata: None,
        model: "grok-3".to_string(),
        object: "response".to_string(),
        output: vec![rs::OutputItem::Message(rs::OutputMessage {
            content: vec![rs::OutputMessageContent::OutputText(
                rs::OutputTextContent {
                    text: "Hello from Responses API!".to_string(),
                    annotations: vec![],
                    logprobs: None,
                },
            )],
            id: "msg_123".to_string(),
            role: rs::AssistantRole::Assistant,
            status: rs::OutputStatus::Completed,
        })],
        parallel_tool_calls: None,
        previous_response_id: None,
        prompt: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
        reasoning: None,
        safety_identifier: None,
        service_tier: None,
        status: rs::Status::Completed,
        temperature: None,
        text: None,
        tool_choice: None,
        tools: None,
        top_logprobs: None,
        top_p: None,
        truncation: None,
        usage: None,
    };

    let items = response_to_conversation_items(response);
    let item = items
        .into_iter()
        .next_back()
        .expect("response produces at least a trailing Assistant");
    assert_eq!(item.text_content(), "Hello from Responses API!");
    let ConversationItem::Assistant(a) = &item else {
        panic!("Expected Assistant item");
    };
    assert_eq!(a.model_id, Some("grok-3".to_string()));
    assert_eq!(
        a.reasoning_effort, None,
        "no reasoning config on the response => no effort recorded"
    );

    // Response with function call
    let response_with_fc = rs::Response {
        background: None,
        billing: None,
        conversation: None,
        created_at: 1234567890,
        completed_at: None,
        error: None,
        id: "resp_456".to_string(),
        incomplete_details: None,
        instructions: None,
        max_output_tokens: None,
        metadata: None,
        model: "grok-3".to_string(),
        object: "response".to_string(),
        output: vec![rs::OutputItem::FunctionCall(rs::FunctionToolCall {
            arguments: r#"{"path": "/bar.txt"}"#.to_string(),
            call_id: "call_789".to_string(),
            name: "read_file".to_string(),
            id: None,
            status: None,
        })],
        parallel_tool_calls: None,
        previous_response_id: None,
        prompt: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
        reasoning: None,
        safety_identifier: None,
        service_tier: None,
        status: rs::Status::Completed,
        temperature: None,
        text: None,
        tool_choice: None,
        tools: None,
        top_logprobs: None,
        top_p: None,
        truncation: None,
        usage: None,
    };

    let items = response_to_conversation_items(response_with_fc);
    let item = items
        .into_iter()
        .next_back()
        .expect("response produces at least a trailing Assistant");
    let ConversationItem::Assistant(a) = &item else {
        panic!("Expected Assistant item");
    };
    assert_eq!(a.tool_calls.len(), 1);
    assert_eq!(a.tool_calls[0].id.as_ref(), "call_789");
    assert_eq!(a.tool_calls[0].name, "read_file");
}

#[test]
fn test_response_reasoning_effort_stamped_on_assistant() {
    use crate::rs;

    let response = rs::Response {
        background: None,
        billing: None,
        conversation: None,
        created_at: 1234567890,
        completed_at: None,
        error: None,
        id: "resp_eff".to_string(),
        incomplete_details: None,
        instructions: None,
        max_output_tokens: None,
        metadata: None,
        model: "grok-3".to_string(),
        object: "response".to_string(),
        output: vec![],
        parallel_tool_calls: None,
        previous_response_id: None,
        prompt: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
        reasoning: Some(rs::Reasoning {
            effort: Some(rs::ReasoningEffort::Xhigh),
            summary: None,
        }),
        safety_identifier: None,
        service_tier: None,
        status: rs::Status::Completed,
        temperature: None,
        text: None,
        tool_choice: None,
        tools: None,
        top_logprobs: None,
        top_p: None,
        truncation: None,
        usage: None,
    };

    let items = response_to_conversation_items(response);
    let ConversationItem::Assistant(a) = items.last().expect("trailing Assistant") else {
        panic!("Expected Assistant item");
    };
    assert_eq!(a.reasoning_effort, Some(crate::ReasoningEffort::Xhigh));

    // Round-trips through the persisted representation.
    let json = serde_json::to_string(&items.last().unwrap()).unwrap();
    assert!(json.contains(r#""reasoning_effort":"xhigh""#), "{json}");
    let back: ConversationItem = serde_json::from_str(&json).unwrap();
    let ConversationItem::Assistant(b) = back else {
        panic!("Expected Assistant item");
    };
    assert_eq!(b.reasoning_effort, Some(crate::ReasoningEffort::Xhigh));
}

#[test]
fn test_tool_calls_to_responses_api() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("System"),
        ConversationItem::user("User"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "bash".to_string(),
            arguments: r#"{"command": "ls"}"#.into(),
        }]),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let fc_items: Vec<_> = items
        .iter()
        .filter(|item| matches!(item, rs::InputItem::Item(rs::Item::FunctionCall(_))))
        .collect();

    assert_eq!(fc_items.len(), 1, "Expected exactly one FunctionCall item");

    let rs::InputItem::Item(rs::Item::FunctionCall(fc)) = fc_items[0] else {
        panic!("Expected FunctionCall item");
    };
    assert_eq!(fc.call_id, "call_1");
    assert_eq!(fc.name, "bash");
    assert_eq!(fc.arguments, r#"{"command": "ls"}"#);
}

#[test]
fn test_tool_result_to_responses_api() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("System"),
        ConversationItem::user("User"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "bash".to_string(),
            arguments: r#"{"command": "ls"}"#.into(),
        }]),
        ConversationItem::tool_result("call_1", "file1.txt\nfile2.txt\nfile3.txt"),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let fco_items: Vec<_> = items
        .iter()
        .filter(|item| matches!(item, rs::InputItem::Item(rs::Item::FunctionCallOutput(_))))
        .collect();

    assert_eq!(
        fco_items.len(),
        1,
        "Expected exactly one FunctionCallOutput item"
    );

    let rs::InputItem::Item(rs::Item::FunctionCallOutput(fco)) = fco_items[0] else {
        panic!("Expected FunctionCallOutput item");
    };
    assert_eq!(fco.call_id, "call_1");
    let rs::FunctionCallOutput::Text(text) = &fco.output else {
        panic!("Expected Text output");
    };
    assert_eq!(text, "file1.txt\nfile2.txt\nfile3.txt");
}

#[test]
fn test_multiple_tool_results_to_responses_api() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Run these commands"),
        ConversationItem::assistant_tool_calls(vec![
            ToolCall {
                id: "call_1".into(),
                name: "bash".to_string(),
                arguments: r#"{"command": "ls"}"#.into(),
            },
            ToolCall {
                id: "call_2".into(),
                name: "bash".to_string(),
                arguments: r#"{"command": "pwd"}"#.into(),
            },
        ]),
        ConversationItem::tool_result("call_1", "output1"),
        ConversationItem::tool_result("call_2", "output2"),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let fco_items: Vec<_> = items
        .iter()
        .filter_map(|item| {
            if let rs::InputItem::Item(rs::Item::FunctionCallOutput(fco)) = item {
                Some(fco)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(fco_items.len(), 2);
    assert_eq!(fco_items[0].call_id, "call_1");
    assert_eq!(fco_items[1].call_id, "call_2");
}

#[test]
fn test_responses_api_with_encrypted_reasoning() {
    let response = rs::Response {
        background: None,
        billing: None,
        conversation: None,
        created_at: 1234567890,
        completed_at: None,
        error: None,
        id: "resp_456".to_string(),
        incomplete_details: None,
        instructions: None,
        max_output_tokens: None,
        metadata: None,
        model: "grok-3".to_string(),
        object: "response".to_string(),
        output: vec![
            rs::OutputItem::Reasoning(rs::ReasoningItem {
                id: "reasoning_enc".to_string(),
                summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                    text: "Visible thinking summary".to_string(),
                })],
                content: None,
                encrypted_content: Some("enc_base64_encrypted_reasoning_data_here".to_string()),
                status: Some(rs::OutputStatus::Completed),
            }),
            rs::OutputItem::Message(rs::OutputMessage {
                content: vec![rs::OutputMessageContent::OutputText(
                    rs::OutputTextContent {
                        text: "My response based on reasoning.".to_string(),
                        annotations: vec![],
                        logprobs: None,
                    },
                )],
                id: "msg_456".to_string(),
                role: rs::AssistantRole::Assistant,
                status: rs::OutputStatus::Completed,
            }),
        ],
        parallel_tool_calls: None,
        previous_response_id: None,
        prompt: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
        reasoning: None,
        safety_identifier: None,
        service_tier: None,
        status: rs::Status::Completed,
        temperature: None,
        text: None,
        tool_choice: None,
        tools: None,
        top_logprobs: None,
        top_p: None,
        truncation: None,
        usage: None,
    };

    // Exercise the flat-list path: reasoning lives as a sibling
    let items = response_to_conversation_items(response);
    let assistant_idx = items
        .iter()
        .position(|i| matches!(i, ConversationItem::Assistant(_)))
        .expect("assistant present");
    let ConversationItem::Assistant(a) = &items[assistant_idx] else {
        unreachable!()
    };
    assert_eq!(a.content.as_ref(), "My response based on reasoning.");

    let reasoning_sibling = items
        .iter()
        .find_map(|i| match i {
            ConversationItem::Reasoning(r) => Some(r),
            _ => None,
        })
        .expect("reasoning sibling present");
    // Both the text summary and the encrypted content survive
    assert_eq!(
        reasoning_sibling.summary.first().map(|sp| match sp {
            rs::SummaryPart::SummaryText(t) => t.text.as_str(),
        }),
        Some("Visible thinking summary")
    );
    assert_eq!(
        reasoning_sibling.encrypted_content.as_deref(),
        Some("enc_base64_encrypted_reasoning_data_here")
    );
}

#[test]
fn test_responses_api_with_only_encrypted_reasoning() {
    let response = rs::Response {
        background: None,
        billing: None,
        conversation: None,
        created_at: 1234567890,
        completed_at: None,
        error: None,
        id: "resp_789".to_string(),
        incomplete_details: None,
        instructions: None,
        max_output_tokens: None,
        metadata: None,
        model: "grok-3".to_string(),
        object: "response".to_string(),
        output: vec![
            rs::OutputItem::Reasoning(rs::ReasoningItem {
                id: "reasoning_only_enc".to_string(),
                summary: vec![],
                content: None,
                encrypted_content: Some("enc_only_encrypted_no_visible_summary".to_string()),
                status: Some(rs::OutputStatus::Completed),
            }),
            rs::OutputItem::Message(rs::OutputMessage {
                content: vec![rs::OutputMessageContent::OutputText(
                    rs::OutputTextContent {
                        text: "Response.".to_string(),
                        annotations: vec![],
                        logprobs: None,
                    },
                )],
                id: "msg_789".to_string(),
                role: rs::AssistantRole::Assistant,
                status: rs::OutputStatus::Completed,
            }),
        ],
        parallel_tool_calls: None,
        previous_response_id: None,
        prompt: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
        reasoning: None,
        safety_identifier: None,
        service_tier: None,
        status: rs::Status::Completed,
        temperature: None,
        text: None,
        tool_choice: None,
        tools: None,
        top_logprobs: None,
        top_p: None,
        truncation: None,
        usage: None,
    };

    // Flat-list path: reasoning sibling carries the encrypted blob, empty summary maps to an empty `Vec<SummaryPart>`
    let items = response_to_conversation_items(response);
    let reasoning_sibling = items
        .iter()
        .find_map(|i| match i {
            ConversationItem::Reasoning(r) => Some(r),
            _ => None,
        })
        .expect("reasoning sibling present");
    assert!(reasoning_sibling.summary.is_empty());
    assert_eq!(
        reasoning_sibling.encrypted_content.as_deref(),
        Some("enc_only_encrypted_no_visible_summary")
    );
}

#[test]
fn test_encrypted_reasoning_included_in_responses_api_request() {
    // Encrypted reasoning is crucial for context continuity across turns
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("You are helpful"),
        ConversationItem::user("What is 2+2?"),
        // Previous reasoning and assistant: reasoning is a sibling
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: "r1".to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: "Let me calculate 2+2...".to_string(),
            })],
            content: None,
            encrypted_content: Some("enc_secret_reasoning_chain".to_string()),
            status: None,
        }.into()),
        ConversationItem::Assistant(AssistantItem {
            content: "The answer is 4.".into(),
            tool_calls: vec![],
            model_id: Some("grok-3".to_string()),
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::user("Now what is 3+3?"),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let reasoning_items: Vec<_> = items
        .iter()
        .filter_map(|item| {
            if let rs::InputItem::Item(rs::Item::Reasoning(r)) = item {
                Some(r)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        reasoning_items.len(),
        1,
        "Should have exactly one reasoning item"
    );

    let reasoning = reasoning_items[0];
    assert_eq!(
        reasoning.encrypted_content,
        Some("enc_secret_reasoning_chain".to_string())
    );

    // Verify summary text is included
    assert_eq!(reasoning.summary.len(), 1);
    let rs::SummaryPart::SummaryText(summary) = &reasoning.summary[0];
    assert_eq!(summary.text, "Let me calculate 2+2...");
}

#[test]
fn test_only_encrypted_reasoning_included_in_request() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Hello"),
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: String::new(),
            summary: vec![],
            content: None,
            encrypted_content: Some("enc_hidden_thoughts".to_string()),
            status: None,
        }.into()),
        ConversationItem::Assistant(AssistantItem {
            content: "Hi!".into(),
            tool_calls: vec![],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let reasoning_items: Vec<_> = items
        .iter()
        .filter_map(|item| {
            if let rs::InputItem::Item(rs::Item::Reasoning(r)) = item {
                Some(r)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(reasoning_items.len(), 1);
    let reasoning = reasoning_items[0];

    assert_eq!(
        reasoning.encrypted_content,
        Some("enc_hidden_thoughts".to_string())
    );

    assert!(reasoning.summary.is_empty());
}

#[test]
fn test_no_reasoning_item_when_no_reasoning() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Hello"),
        ConversationItem::assistant("Hi!"), // No reasoning
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let reasoning_items: Vec<_> = items
        .iter()
        .filter(|item| matches!(item, rs::InputItem::Item(rs::Item::Reasoning(_))))
        .collect();

    assert!(reasoning_items.is_empty(), "Should have no reasoning items");
}

#[test]
fn test_conversation_request_with_tools_to_responses_api() {
    let tools = vec![ToolSpec {
        name: "search".to_string(),
        description: Some("Search the codebase".to_string()),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            }
        }),
        exposure: ToolExposure::default(),
    }];

    let req = ConversationRequest::from_items(vec![ConversationItem::user("Find TODO comments")])
        .with_tools(tools);

    let responses_req: rs::CreateResponse = (&req).into();
    assert!(responses_req.tools.is_some());
    let tools = responses_req.tools.unwrap();
    assert_eq!(tools.len(), 1);

    let rs::Tool::Function(ft) = &tools[0] else {
        panic!("Expected Function tool");
    };
    assert_eq!(ft.name, "search");
    assert_eq!(ft.description, Some("Search the codebase".to_string()));
}

#[test]
fn test_tool_choice_to_responses_api() {
    // Test Auto
    let req = ConversationRequest::from_items(vec![ConversationItem::user("test")])
        .with_tool_choice(ConversationToolChoice::Auto);
    let responses_req: rs::CreateResponse = (&req).into();
    assert_matches!(
        responses_req.tool_choice,
        Some(rs::ToolChoiceParam::Mode(rs::ToolChoiceOptions::Auto))
    );

    // Test Required
    let req = ConversationRequest::from_items(vec![ConversationItem::user("test")])
        .with_tool_choice(ConversationToolChoice::Required);
    let responses_req: rs::CreateResponse = (&req).into();
    assert_matches!(
        responses_req.tool_choice,
        Some(rs::ToolChoiceParam::Mode(rs::ToolChoiceOptions::Required))
    );

    // Test Function
    let req = ConversationRequest::from_items(vec![ConversationItem::user("test")])
        .with_tool_choice(ConversationToolChoice::Function("bash".to_string()));
    let responses_req: rs::CreateResponse = (&req).into();
    let Some(rs::ToolChoiceParam::Function(fc)) = responses_req.tool_choice else {
        panic!("Expected Function tool choice");
    };
    assert_eq!(fc.name, "bash");
}

#[test]
fn test_malformed_tool_arguments_sanitized_in_responses_api() {
    let bad_args = r#"{"file_path": "/testbed/cxx_polynomial/include/emsr/remez.h", "old_string": "", new_string": "x"}"#;

    let tool_call = ToolCall {
        id: "call_bad".into(),
        name: "search_replace".to_string(),
        arguments: bad_args.into(),
    };

    let item = ConversationItem::assistant_tool_calls(vec![tool_call]);
    let req = ConversationRequest {
        items: vec![item],
        ..Default::default()
    };

    let rs_req: crate::rs::CreateResponse = (&req).into();

    // The FunctionCall input item must carry sanitized arguments.
    let crate::rs::InputParam::Items(items) = rs_req.input else {
        panic!("Expected InputParam::Items");
    };
    let fc_args = items.iter().find_map(|inp| {
        if let crate::rs::InputItem::Item(crate::rs::Item::FunctionCall(fc)) = inp {
            Some(fc.arguments.clone())
        } else {
            None
        }
    });

    let fc_args = fc_args.expect("should find a FunctionCall input item");
    assert_eq!(
        fc_args, "{}",
        "malformed arguments must be replaced with {{}} in Responses API path"
    );
}

#[test]
fn test_responses_request_carries_reasoning_effort_nested() {
    for (variant, expected) in [
        (crate::ReasoningEffort::None, "none"),
        (crate::ReasoningEffort::Minimal, "minimal"),
        (crate::ReasoningEffort::Low, "low"),
        (crate::ReasoningEffort::Medium, "medium"),
        (crate::ReasoningEffort::High, "high"),
        (crate::ReasoningEffort::Xhigh, "xhigh"),
        (crate::ReasoningEffort::Max, "max"),
    ] {
        let req = ConversationRequest {
            reasoning_effort: Some(variant),
            ..ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_model("test")
        };
        let resp: crate::rs::CreateResponse = (&req).into();
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(
            json.pointer("/reasoning/effort").and_then(|v| v.as_str()),
            Some(expected),
            "{variant:?} should serialize as reasoning.effort={expected:?}; got: {json:#}",
        );
    }
}

#[test]
fn test_responses_request_omits_effort_when_unset() {
    let req =
        ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_model("test");
    let resp: crate::rs::CreateResponse = (&req).into();
    let json = serde_json::to_value(&resp).unwrap();
    assert!(
        json.pointer("/reasoning/effort").is_none(),
        "reasoning.effort must be absent when unset; got: {json:#}",
    );
}

#[test]
fn test_btw_cross_api_responses_no_regressions() {
    let items = btw_prepare_items(btw_mid_turn_conversation());
    let req = ConversationRequest::from_items(items);
    let resp: rs::CreateResponse = (&req).into();
    let json = serde_json::to_value(&resp).unwrap();

    let rs::InputParam::Items(input_items) = &resp.input else {
        panic!("Expected InputParam::Items");
    };

    // Count FunctionCall and FunctionCallOutput items.
    let function_calls: Vec<_> = input_items
        .iter()
        .filter_map(|item| {
            if let rs::InputItem::Item(rs::Item::FunctionCall(fc)) = item {
                Some(fc.call_id.clone())
            } else {
                None
            }
        })
        .collect();
    let function_outputs: Vec<_> = input_items
        .iter()
        .filter_map(|item| {
            if let rs::InputItem::Item(rs::Item::FunctionCallOutput(fco)) = item {
                Some(fco.call_id.clone())
            } else {
                None
            }
        })
        .collect();

    // call_1 must be present as both FunctionCall and FunctionCallOutput.
    assert!(
        function_calls.contains(&"call_1".to_string()),
        "completed FunctionCall call_1 must survive; got calls: {function_calls:?}"
    );
    assert!(
        function_outputs.contains(&"call_1".to_string()),
        "completed FunctionCallOutput call_1 must survive; got outputs: {function_outputs:?}"
    );

    assert!(
        !function_calls.contains(&"call_2".to_string()),
        "orphaned FunctionCall call_2 must be removed"
    );

    // No Reasoning items (reasoning was stripped).
    let has_reasoning = input_items
        .iter()
        .any(|item| matches!(item, rs::InputItem::Item(rs::Item::Reasoning(_))));
    assert!(!has_reasoning, "reasoning items must be stripped");

    assert!(
        json.get("temperature").is_none()
            || json.pointer("/temperature").is_some_and(|v| v.is_null()),
        "temperature must be absent; got: {json:#}",
    );
}

#[test]
fn test_transform_cwd_rewrites_reasoning_sibling() {
    // Reasoning siblings are subject to CWD rewriting via `transform_conversation_cwd` (see the `Reasoning(_)` arm)
    let worktree = "/workspace/.grok/worktrees/project/ab-uuid-a";
    let root = "/workspace/project";

    let mut items = vec![
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: "rs_1".to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: format!("thinking about {worktree}"),
            })],
            content: None,
            encrypted_content: None,
            status: None,
        }.into()),
        ConversationItem::Assistant(AssistantItem {
            content: format!("I edited {worktree}/src/main.rs").into(),
            tool_calls: vec![],
            model_id: Some("grok-3".to_string()),
            model_fingerprint: None,
            reasoning_effort: None,
        }),
    ];

    transform_conversation_cwd(&mut items, worktree, root);

    assert_eq!(
        items[1].text_content(),
        format!("I edited {root}/src/main.rs")
    );
    let ConversationItem::Reasoning(r) = &items[0] else {
        panic!("expected Reasoning sibling");
    };
    let rs::SummaryPart::SummaryText(t) = &r.summary[0];
    assert!(
        !t.text.contains(worktree),
        "reasoning sibling text should be rewritten"
    );
    assert!(t.text.contains(root));
}

// ── Tool result with images tests ──────────────────────────────────────────

#[test]
fn test_tool_result_with_images_to_responses_api() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("System"),
        ConversationItem::user("Read this image"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "read_file".to_string(),
            arguments: r#"{"target_file": "photo.png"}"#.into(),
        }]),
        ConversationItem::tool_result_with_images(
            "call_1",
            "Read image file: photo.png",
            vec![ContentPart::Image {
                url: "data:image/png;base64,iVBOR".into(),
            }],
        ),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let fco_items: Vec<_> = items
        .iter()
        .filter_map(|item| {
            if let rs::InputItem::Item(rs::Item::FunctionCallOutput(fco)) = item {
                Some(fco)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(fco_items.len(), 1);
    assert_eq!(fco_items[0].call_id, "call_1");

    // Should be Content variant, not Text
    let rs::FunctionCallOutput::Content(parts) = &fco_items[0].output else {
        panic!("Expected Content output with images, got Text");
    };
    assert_eq!(parts.len(), 2, "Expected text + 1 image");
    assert!(
        matches!(&parts[0], rs::InputContent::InputText(t) if t.text == "Read image file: photo.png")
    );
    assert!(
        matches!(&parts[1], rs::InputContent::InputImage(img) if img.image_url.as_deref() == Some("data:image/png;base64,iVBOR"))
    );
}

#[test]
fn test_tool_result_without_images_stays_text() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Run ls"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "bash".to_string(),
            arguments: r#"{"command": "ls"}"#.into(),
        }]),
        ConversationItem::tool_result("call_1", "file1.txt\nfile2.txt"),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();
    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let fco = items
        .iter()
        .find_map(|item| {
            if let rs::InputItem::Item(rs::Item::FunctionCallOutput(fco)) = item {
                Some(fco)
            } else {
                None
            }
        })
        .unwrap();

    assert!(matches!(&fco.output, rs::FunctionCallOutput::Text(t) if t == "file1.txt\nfile2.txt"));
}

/// Lowers `item` through the real request conversion and returns the output of
/// the single `function_call_output` item it produced.
fn lowered_tool_result_output(item: ConversationItem) -> rs::FunctionCallOutput {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Read this image"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "read_file".to_string(),
            arguments: r#"{"target_file": "photo.png"}"#.into(),
        }]),
        item,
    ]);

    let responses_req: rs::CreateResponse = (&req).into();
    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let outputs: Vec<_> = items
        .iter()
        .filter_map(|item| {
            if let rs::InputItem::Item(rs::Item::FunctionCallOutput(fco)) = item {
                Some(fco)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].call_id, "call_1");
    outputs[0].output.clone()
}

/// An `images` vec that carries no image part must not lower to an all-text
/// output array: a shim in front of a ChatCompletions backend rejects that
/// shape, so the donor collapses it to a bare string (`normalize_tool_output`,
/// codex-rs/codex-api/src/endpoint/content_type_compat.rs:94 in the external donor tree).
#[test]
fn test_tool_result_with_non_image_parts_collapses_to_text() {
    let output = lowered_tool_result_output(ConversationItem::tool_result_with_images(
        "call_1",
        "Read image file: photo.png",
        vec![ContentPart::Text {
            text: "metadata".into(),
        }],
    ));

    assert_eq!(
        output,
        rs::FunctionCallOutput::Text("Read image file: photo.png".to_string())
    );
}

#[test]
fn test_tool_result_with_text_and_image_parts_keeps_text_first() {
    let output = lowered_tool_result_output(ConversationItem::tool_result_with_images(
        "call_1",
        "Read image file: photo.png",
        vec![
            ContentPart::Text {
                text: "metadata".into(),
            },
            ContentPart::Image {
                url: "data:image/png;base64,iVBOR".into(),
            },
        ],
    ));

    assert_eq!(
        output,
        rs::FunctionCallOutput::Content(vec![
            rs::InputContent::InputText(rs::InputTextContent {
                text: "Read image file: photo.png".to_string(),
            }),
            rs::InputContent::InputImage(rs::InputImageContent {
                detail: rs::ImageDetail::Auto,
                file_id: None,
                image_url: Some("data:image/png;base64,iVBOR".to_string()),
            }),
        ])
    );
}

/// A blank result text keeps its text part when an image rides with it. The donor filters
/// blank segments only inside the all-text collapse
/// (`codex-rs/codex-api/src/endpoint/content_type_compat.rs:117-129`, the filter at `:124`); the
/// mixed branch relabels the parts in place (`:130-132`) and leaves the empty one standing.
/// Pinned so dropping the blank part reads as the wire change it is, not a cleanup.
#[test]
fn test_tool_result_with_blank_content_and_image_keeps_blank_text_part() {
    let output = lowered_tool_result_output(ConversationItem::tool_result_with_images(
        "call_1",
        "",
        vec![ContentPart::Image {
            url: "data:image/png;base64,iVBOR".into(),
        }],
    ));

    assert_eq!(
        output,
        rs::FunctionCallOutput::Content(vec![
            rs::InputContent::InputText(rs::InputTextContent {
                text: String::new(),
            }),
            rs::InputContent::InputImage(rs::InputImageContent {
                detail: rs::ImageDetail::Auto,
                file_id: None,
                image_url: Some("data:image/png;base64,iVBOR".to_string()),
            }),
        ]),
        "a mixed output keeps its blank text part"
    );
}

/// The result text rides first and the image parts follow in input order.
#[test]
fn test_tool_result_with_two_images_keeps_images_in_input_order() {
    let output = lowered_tool_result_output(ConversationItem::tool_result_with_images(
        "call_1",
        "Read 2 image files",
        vec![
            ContentPart::Image {
                url: "data:image/png;base64,aG90".into(),
            },
            ContentPart::Image {
                url: "data:image/png;base64,aW1n".into(),
            },
        ],
    ));

    assert_eq!(
        output,
        rs::FunctionCallOutput::Content(vec![
            rs::InputContent::InputText(rs::InputTextContent {
                text: "Read 2 image files".to_string(),
            }),
            rs::InputContent::InputImage(rs::InputImageContent {
                detail: rs::ImageDetail::Auto,
                file_id: None,
                image_url: Some("data:image/png;base64,aG90".to_string()),
            }),
            rs::InputContent::InputImage(rs::InputImageContent {
                detail: rs::ImageDetail::Auto,
                file_id: None,
                image_url: Some("data:image/png;base64,aW1n".to_string()),
            }),
        ]),
        "the images follow the order they were carried in"
    );
}

/// A text part riding BETWEEN two images is dropped — not appended, not merged into the item's own
/// content: the harness gives a tool result one textual part (its result text, leading the array),
/// so mid-array text is dropped by the `ContentPart::Text { .. } => None` arm of the ToolResult
/// lowering. This known loss is handoff H-4; the sibling dialects drop it identically. Pinned so the
/// loss reads as the deliberate contract it is: an implementation that started carrying this text is
/// a wire change to review, not a silent cleanup.
#[test]
fn test_tool_result_with_text_between_images_drops_the_text_part() {
    let output = lowered_tool_result_output(ConversationItem::tool_result_with_images(
        "call_1",
        "Read 2 image files",
        vec![
            ContentPart::Image {
                url: "data:image/png;base64,aG90".into(),
            },
            ContentPart::Text {
                text: "caption between the images".into(),
            },
            ContentPart::Image {
                url: "data:image/png;base64,aW1n".into(),
            },
        ],
    ));

    assert_eq!(
        output,
        rs::FunctionCallOutput::Content(vec![
            rs::InputContent::InputText(rs::InputTextContent {
                text: "Read 2 image files".to_string(),
            }),
            rs::InputContent::InputImage(rs::InputImageContent {
                detail: rs::ImageDetail::Auto,
                file_id: None,
                image_url: Some("data:image/png;base64,aG90".to_string()),
            }),
            rs::InputContent::InputImage(rs::InputImageContent {
                detail: rs::ImageDetail::Auto,
                file_id: None,
                image_url: Some("data:image/png;base64,aW1n".to_string()),
            }),
        ]),
        "the text between the images is dropped; the images stay adjacent in input order"
    );
}

#[test]
fn responses_api_conversion_preserves_model_fingerprint() {
    use std::collections::HashMap;

    let mut metadata = HashMap::new();
    metadata.insert("system_fingerprint".into(), "fp_abc123".into());

    let response = rs::Response {
        background: None,
        billing: None,
        conversation: None,
        created_at: 0,
        completed_at: None,
        error: None,
        id: "resp_test".into(),
        incomplete_details: None,
        instructions: None,
        max_output_tokens: None,
        metadata: Some(metadata),
        model: "grok-4.5".into(),
        object: "response".into(),
        output: vec![rs::OutputItem::Message(rs::OutputMessage {
            content: vec![rs::OutputMessageContent::OutputText(
                rs::OutputTextContent {
                    text: "hello".into(),
                    annotations: vec![],
                    logprobs: None,
                },
            )],
            id: "msg_test".into(),
            role: rs::AssistantRole::Assistant,
            status: rs::OutputStatus::Completed,
        })],
        parallel_tool_calls: None,
        previous_response_id: None,
        prompt: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
        reasoning: None,
        safety_identifier: None,
        service_tier: None,
        status: rs::Status::Completed,
        temperature: None,
        text: None,
        tool_choice: None,
        tools: None,
        top_logprobs: None,
        top_p: None,
        truncation: None,
        usage: None,
    };

    let items = response_to_conversation_items(response);
    let item = items
        .into_iter()
        .next_back()
        .expect("response produces at least a trailing Assistant");
    assert_matches!(item, ConversationItem::Assistant(ref a) => {
        assert_eq!(a.model_fingerprint.as_deref(), Some("fp_abc123"));
        assert_eq!(a.model_id.as_deref(), Some("grok-4.5"));
        assert_eq!(a.content.as_ref(), "hello");
    });
}

#[test]
fn empty_reason_reasoning_only() {
    // A response with a Reasoning sibling but empty Assistant content is classified as ReasoningOnly so the retry logic resamples
    let response = ConversationResponse {
        items: vec![
            ConversationItem::Reasoning(rs::ReasoningItem {
                id: "r1".to_string(),
                summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                    text: "thinking but no text output".to_string(),
                })],
                content: None,
                encrypted_content: None,
                status: None,
            }.into()),
            ConversationItem::Assistant(AssistantItem {
                content: String::new().into(),
                tool_calls: Vec::new(),
                model_id: None,
                model_fingerprint: None,
                reasoning_effort: None,
            }),
        ],
        stop_reason: Some(StopReason::Stop),
        usage: None,
        cost_usd_ticks: None,
        message_chunks_emitted: 0,
        doom_loop_signals: Vec::new(),
        stop_message: None,
        message_id: None,
        raw_stop_reason: None,
        stop_sequence: None,
    };
    assert_eq!(
        response.empty_reason(),
        Some(crate::error::EmptyReason::ReasoningOnly)
    );
    assert!(response.is_empty());
}

#[test]
fn build_responses_input_preserves_multi_turn_ordering() {
    // 4-turn conversation where each assistant turn carries reasoning.
    // The wire-level item order must be
    // [Sys, U1, U2, U3, U4, U5, R, A1, R, A2, ...] which would shift the cache prefix every turn.
    fn r(text: &str) -> ConversationItem {
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: text.to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: text.to_string(),
            })],
            content: None,
            encrypted_content: Some(format!("enc_{text}")),
            status: None,
        }.into())
    }
    let items: Vec<ConversationItem> = vec![
        ConversationItem::system("you are helpful"),
        ConversationItem::user("u1"),
        r("r1"),
        ConversationItem::assistant("a1"),
        ConversationItem::user("u2"),
        r("r2"),
        ConversationItem::assistant("a2"),
        ConversationItem::user("u3"),
        r("r3"),
        ConversationItem::assistant("a3"),
        ConversationItem::user("u4"),
        r("r4"),
        ConversationItem::assistant("a4"),
        ConversationItem::user("u5"),
    ];

    let req = ConversationRequest::from_items(items);
    let input = super::responses::build_responses_input(&req);
    let rs::InputParam::Items(wire_items) = input else {
        panic!("expected Items input");
    };

    // Walk the wire items and verify the expected pattern.
    // Roles per wire item: System, User, Reasoning(role=Assistant), Assistant, User, Reasoning, Assistant, ...
    let kinds: Vec<&'static str> = wire_items
        .iter()
        .map(|w| match w {
            rs::InputItem::EasyMessage(m) => match m.role {
                rs::Role::System => "Sys",
                rs::Role::User => "U",
                rs::Role::Assistant => "A",
                _ => "other",
            },
            rs::InputItem::Item(rs::Item::Reasoning(_)) => "R",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "Sys", "U", "R", "A", "U", "R", "A", "U", "R", "A", "U", "R", "A", "U",
        ],
        "multi-turn ordering must preserve interleaved Reasoning ↔ Assistant per turn"
    );
}

#[test]
fn upgrade_legacy_reasoning_singular_chat_completions_text_only() {
    // Chat-completions has only text (no encrypted, no id).
    let raw = serde_json::json!({
        "type": "assistant",
        "content": "answer",
        "reasoning": {"text": "step-by-step plain reasoning"}
    });
    let mut seen = std::collections::HashSet::new();
    let siblings = upgrade_legacy_reasoning(&raw, &mut seen);
    assert_eq!(siblings.len(), 1);
    let ConversationItem::Reasoning(r) = &siblings[0] else {
        panic!("expected Reasoning sibling");
    };
    assert_eq!(r.id, "");
    assert!(r.encrypted_content.is_none());
    let rs::SummaryPart::SummaryText(s) = &r.summary[0];
    assert_eq!(s.text, "step-by-step plain reasoning");
}

#[test]
fn upgrade_legacy_reasoning_v0_chat_request_message_shape() {
    // v0 on disk: top-level role and reasoning_content
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "v0 answer",
        "reasoning_content": "v0-style plain text reasoning"
    });
    let mut seen = std::collections::HashSet::new();
    let siblings = upgrade_legacy_reasoning(&raw, &mut seen);
    assert_eq!(siblings.len(), 1);
    let ConversationItem::Reasoning(r) = &siblings[0] else {
        panic!("expected Reasoning sibling");
    };
    let rs::SummaryPart::SummaryText(s) = &r.summary[0];
    assert_eq!(s.text, "v0-style plain text reasoning");
}

#[test]
fn patch_reasoning_text_types_injects_type_discriminator() {
    // Build a request body containing a reasoning item whose nested `content[]` entries lack the `type` field (the async-openai gap)
    let mut body = serde_json::json!({
        "input": [
            {
                "type": "reasoning",
                "id": "r1",
                "content": [
                    { "text": "thinking..." },
                    { "text": "more thinking" }
                ]
            },
            {
                "type": "message",
                "role": "user",
                "content": "hi"
            }
        ]
    });
    patch_reasoning_text_types(&mut body);
    let reasoning_content = body
        .pointer("/input/0/content")
        .and_then(|v| v.as_array())
        .expect("reasoning content array");
    for item in reasoning_content {
        assert_eq!(
            item.get("type").and_then(|t| t.as_str()),
            Some("reasoning_text"),
            "every nested content item must carry the discriminator"
        );
    }
    // Untouched: the user message stays as-is.
    assert_eq!(
        body.pointer("/input/1/content").and_then(|v| v.as_str()),
        Some("hi")
    );
}

#[test]
fn patch_reasoning_text_types_preserves_existing_type() {
    let mut body = serde_json::json!({
        "input": [
            {
                "type": "reasoning",
                "id": "r1",
                "content": [
                    // Post-upstream-fix shape: discriminator already present.
                    { "type": "reasoning_text", "text": "already tagged" },
                    // A hypothetical different discriminator must NOT be clobbered.
                    { "type": "some_future_variant", "text": "future shape" },
                    // Current gap: missing type gets filled in
                    { "text": "needs tag" }
                ]
            }
        ]
    });
    patch_reasoning_text_types(&mut body);
    let content = body
        .pointer("/input/0/content")
        .and_then(|v| v.as_array())
        .expect("reasoning content array");

    // Existing discriminators preserved verbatim (no clobber).
    assert_eq!(
        content[0].get("type").and_then(|t| t.as_str()),
        Some("reasoning_text"),
    );
    assert_eq!(
        content[1].get("type").and_then(|t| t.as_str()),
        Some("some_future_variant"),
        "a non-default upstream discriminator must be left untouched",
    );
    // Only the type-less item is filled in.
    assert_eq!(
        content[2].get("type").and_then(|t| t.as_str()),
        Some("reasoning_text"),
    );

    // Object integrity: each item has exactly one `type` and its `text`.
    for item in content {
        let obj = item.as_object().expect("content item is an object");
        assert!(obj.contains_key("type") && obj.contains_key("text"));
    }
}

#[test]
fn build_responses_input_single_reasoning_sibling_lands_inline() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("u1"),
        reasoning_sibling("r_abc", "thinking", Some("enc1")),
        ConversationItem::assistant("hi"),
    ]);

    let input = input_items_json(&req);
    let summary = summarise_input(&input);

    // Expected: [system, user, reasoning, assistant]
    assert_eq!(summary.len(), 4, "got: {summary:?}");
    assert_eq!(summary[0], "system:sys");
    assert_eq!(summary[1], "user:u1");
    assert_eq!(summary[2], "reasoning:r_abc");
    assert_eq!(summary[3], "assistant:hi");

    // No placeholder strings must appear
    let body_str = serde_json::to_string(&input).unwrap();
    assert!(
        !body_str.contains("__RAW_OUTPUT_PLACEHOLDER_"),
        "no placeholder strings post-refactor"
    );

    assert_eq!(
        input[2].get("encrypted_content").and_then(|v| v.as_str()),
        Some("enc1"),
    );
}

#[test]
fn build_responses_input_multi_turn_reasoning_ordering() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("u1"),
        reasoning_sibling("r1", "think 1", Some("enc1")),
        ConversationItem::assistant("a1"),
        ConversationItem::tool_result("tc1", "result1"),
        ConversationItem::user("u2"),
        reasoning_sibling("r2", "think 2", Some("enc2")),
        ConversationItem::assistant("a2"),
        ConversationItem::tool_result("tc2", "result2"),
        ConversationItem::user("u3"),
        reasoning_sibling("r3", "think 3", Some("enc3")),
        ConversationItem::assistant("a3"),
    ]);

    let input = input_items_json(&req);
    let summary = summarise_input(&input);

    // INVARIANT 1: There must be exactly N reasoning items for N siblings
    // The pre-refactor bug produced only 1
    let reasoning_count = summary
        .iter()
        .filter(|s| s.starts_with("reasoning:"))
        .count();
    assert_eq!(
        reasoning_count, 3,
        "must have 3 reasoning items, got {reasoning_count}. Items: {summary:?}"
    );

    // INVARIANT 2: Each reasoning must be BETWEEN its corresponding user message and the NEXT user message
    // Without this check, all reasoning items bunched at the end would still pass count
    let user_positions: Vec<usize> = summary
        .iter()
        .enumerate()
        .filter(|(_, s)| s.starts_with("user:"))
        .map(|(i, _)| i)
        .collect();
    let reasoning_positions: Vec<usize> = summary
        .iter()
        .enumerate()
        .filter(|(_, s)| s.starts_with("reasoning:"))
        .map(|(i, _)| i)
        .collect();

    assert_eq!(user_positions.len(), 3);
    assert_eq!(reasoning_positions.len(), 3);

    for (i, rp) in reasoning_positions.iter().enumerate() {
        assert!(
            *rp > user_positions[i],
            "reasoning {i} at position {rp} must be after user {i} at position {}. \
                 Items: {summary:?}",
            user_positions[i]
        );
        if i + 1 < user_positions.len() {
            assert!(
                *rp < user_positions[i + 1],
                "reasoning {i} at position {rp} must be before user {} at position {}. \
                     Items: {summary:?}",
                i + 1,
                user_positions[i + 1]
            );
        }
    }

    // INVARIANT 3: encrypted_content per item is preserved 1:1.
    let mut enc_seen: Vec<&str> = Vec::new();
    for v in &input {
        if v.get("type").and_then(|t| t.as_str()) == Some("reasoning")
            && let Some(enc) = v.get("encrypted_content").and_then(|s| s.as_str())
        {
            enc_seen.push(enc);
        }
    }
    assert_eq!(enc_seen, vec!["enc1", "enc2", "enc3"]);
}

#[test]
fn backend_tool_call_position_stable() {
    let ws_a = ConversationItem::BackendToolCall(BackendToolCallItem {
        kind: BackendToolKind::WebSearch(rs::WebSearchToolCall {
            id: "ws_a".to_string(),
            status: rs::WebSearchToolCallStatus::Completed,
            action: rs::WebSearchToolCallAction::Search(rs::WebSearchActionSearch {
                query: "alpha".to_string(),
                sources: Some(vec![]),
            }),
        }),
    });
    let ws_b = ConversationItem::BackendToolCall(BackendToolCallItem {
        kind: BackendToolKind::WebSearch(rs::WebSearchToolCall {
            id: "ws_b".to_string(),
            status: rs::WebSearchToolCallStatus::Completed,
            action: rs::WebSearchToolCallAction::Search(rs::WebSearchActionSearch {
                query: "beta".to_string(),
                sources: Some(vec![]),
            }),
        }),
    });

    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("u1"),
        reasoning_sibling("r1", "think a", Some("enc_a")),
        ws_a,
        ConversationItem::assistant("a1"),
        ConversationItem::user("u2"),
        ws_b,
        ConversationItem::assistant("a2"),
    ]);

    let input = input_items_json(&req);
    let ws_items: Vec<&serde_json::Value> = input
        .iter()
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("web_search_call"))
        .collect();

    // Both backend tool calls must survive serialization.
    assert_eq!(
        ws_items.len(),
        2,
        "both web_search_call items must survive; got: {:?}",
        summarise_input(&input)
    );
    let ids: Vec<&str> = ws_items
        .iter()
        .filter_map(|v| v.get("id").and_then(|i| i.as_str()))
        .collect();
    assert_eq!(ids, vec!["ws_a", "ws_b"], "ordering preserved");
}

#[test]
fn empty_content_assistant_with_tool_calls_and_reasoning() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("u1"),
        reasoning_sibling("r1", "must call a tool", Some("enc_pre_tool")),
        ConversationItem::Assistant(AssistantItem {
            content: Arc::<str>::from(""),
            tool_calls: vec![ToolCall {
                id: Arc::<str>::from("call_1"),
                name: "read_file".to_string(),
                arguments: Arc::<str>::from("{}"),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("call_1", "file contents"),
    ]);

    let input = input_items_json(&req);
    let summary = summarise_input(&input);

    // (assistant message DROPPED by the `!a.content.is_empty()` guard in conversation_item_to_input_items) function_call_output (tool result)
    // No spurious extra reasoning items, no placeholder.
    let reasoning_count = summary
        .iter()
        .filter(|s| s.starts_with("reasoning:"))
        .count();
    assert_eq!(
        reasoning_count, 1,
        "exactly one reasoning item; got: {summary:?}"
    );
    assert!(
        summary.iter().any(|s| s == "function_call:call_1"),
        "function_call must appear; got: {summary:?}"
    );
    assert!(
        summary
            .iter()
            .any(|s| s.starts_with("type:function_call_output")),
        "function_call_output must appear; got: {summary:?}"
    );

    let body_str = serde_json::to_string(&input).unwrap();
    assert!(!body_str.contains("__RAW_OUTPUT_PLACEHOLDER_"));
}

#[test]
fn serialized_body_contains_no_placeholder_strings() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("u1"),
        reasoning_sibling("r1", "first", Some("enc1")),
        ConversationItem::assistant("a1"),
        ConversationItem::user("u2"),
        reasoning_sibling("r2", "second", Some("enc2")),
        ConversationItem::assistant("a2"),
        ConversationItem::user("u3"),
    ]);

    let cr: rs::CreateResponse = (&req).into();
    let mut body = serde_json::to_value(&cr).unwrap();
    patch_reasoning_text_types(&mut body);
    let body_str = serde_json::to_string(&body).unwrap();

    assert!(
        !body_str.contains("__RAW_OUTPUT_PLACEHOLDER_"),
        "no placeholder strings post-sibling-Reasoning refactor"
    );

    // Both reasoning items must appear inline in the input array.
    let input = body["input"].as_array().unwrap();
    let reasoning_items: Vec<&serde_json::Value> = input
        .iter()
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("reasoning"))
        .collect();
    assert_eq!(
        reasoning_items.len(),
        2,
        "both reasoning siblings must be present"
    );
}

// apex-ayl.76 (XSEARCH-REPLAY-DIALECT): x_search history must survive a turn
// boundary on every model dialect instead of replaying as an undeclared
// custom_tool_call. The flattener today emits the raw CustomToolCall carrier,
// which serializes as `{"type":"custom_tool_call",...}` — no `custom` tool is
// ever declared on any dialect (x_search rides as a raw hosted entry), so the
// item is undeclared on the wire. Acceptance (donor parity, open-grok@049664b5
// conversation.rs:4436-4446): a bounded provider-neutral placeholder message.
// RED stage A: runtime red on the current projection.
// Fixture provenance: fixtures/xsearch_replay/PROVENANCE.md (donor-derived).
#[test]
fn xsearch76_carrier_projects_to_bounded_placeholder_never_custom_tool_call() {
    let call: rs::CustomToolCall =
        serde_json::from_str(include_str!("fixtures/xsearch_replay/carrier_xs_123.json")).unwrap();
    let request = ConversationRequest::from_items(vec![
        ConversationItem::assistant("visible answer from the earlier search"),
        ConversationItem::BackendToolCall(BackendToolCallItem {
            kind: BackendToolKind::XSearch(call),
        }),
    ]);
    let input = super::test_support::input_items_json(&request);
    let replayed = &input[1];
    assert_ne!(
        replayed.get("type").and_then(serde_json::Value::as_str),
        Some("custom_tool_call"),
        "x_search history replayed as undeclared custom_tool_call (apex-ayl.76 hazard): {replayed:?}"
    );
    let golden: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/xsearch_replay/wire_placeholder_golden.json"
    ))
    .unwrap();
    assert_eq!(replayed, &golden, "donor-parity placeholder (byte-pinned)");
    assert!(
        golden["content"].as_str().unwrap().chars().count() < 128,
        "cross-provider search context must remain bounded"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// XW-EMPTYID-1 (apex-ayl.69) — first-send empty reasoning id repair.
//
// The messages-wire persist seam (sampler `src/stream/messages.rs`) persists
// every reasoning item with `id: ""`. The first cross-wire replay onto a
// strict responses target 400s on that empty id (incident 01a0b046, cell
// vxm-az). Fix (sdd-69 §2): a first-send-time JSON body patch beside
// `patch_reasoning_text_types` that synthesizes the shared xw_ grammar — the
// one .71's switch-time projector uses (conversation/projection.rs; one rule
// across goldens + L0 + send-time patch).
//
// Fixture: `fixtures/emptyid_x69/` — the 5 incident reasoning records
// verbatim + minimal portable context (PROVENANCE.md there).
//
// RED stage 1: `emptyid_replay_request_builder_emits_no_empty_reasoning_id`
// fails with a runtime assertion today. RED stage 2: the cases 2–3 that
// follow it fail E0425 (`patch_reasoning_empty_ids` not defined yet).
// GREEN: sdd-69 §4–§5.
// ─────────────────────────────────────────────────────────────────────────────

const EMPTYID_X69_PRE: &str = include_str!("fixtures/emptyid_x69/pre_switch_emptyid.json");

/// The vxm-az incident recipe through the current request-builder pipeline:
/// fixture -> `ConversationRequest` -> `rs::CreateResponse` -> serialized
/// body -> `patch_reasoning_text_types` (the in-situ patch position, the
/// call the send pipeline makes on every /responses request).
fn emptyid_x69_body() -> serde_json::Value {
    let value: serde_json::Value =
        serde_json::from_str(EMPTYID_X69_PRE).expect("fixture must be valid JSON");
    let items: Vec<ConversationItem> = serde_json::from_value(value)
        .expect("fixture records must deserialize to ConversationItem");
    let req = ConversationRequest::from_items(items);
    let cr: rs::CreateResponse = (&req).into();
    let mut body = serde_json::to_value(&cr).expect("responses request must serialize");
    patch_reasoning_text_types(&mut body);
    body
}

#[test]
fn emptyid_replay_request_builder_emits_no_empty_reasoning_id() {
    let mut body = emptyid_x69_body();
    // The fixed send pipeline (sdd-69 §2.2 placement): the empty-id repair
    // runs immediately after patch_reasoning_text_types on every /responses
    // send path. The RED stage 1 capture (this file's header) ran the
    // pre-fix pipeline the helper models — it failed here naming all 5
    // incident items; the fix makes that state unreachable pre-send.
    patch_reasoning_empty_ids(&mut body, "vxm-az");
    let input = body["input"]
        .as_array()
        .expect("responses body must carry an input array");
    let empty_id_items: Vec<usize> = input
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("reasoning")
                && item
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(str::is_empty)
        })
        .map(|(idx, _)| idx)
        .collect();
    assert!(
        empty_id_items.is_empty(),
        "reasoning items ride empty ids to the wire at input indices {empty_id_items:?}: \
         persisted id:'' must be synthesized (xw_ grammar) or dropped pre-send (apex-ayl.69)"
    );
}

/// Input indices of the reasoning items in the serialized body, in order
/// (`ord` = 0-based among reasoning items — the grammar's {ord} slot).
fn emptyid_x69_reasoning_indices(body: &serde_json::Value) -> Vec<usize> {
    body["input"]
        .as_array()
        .expect("responses body must carry an input array")
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("reasoning")
        })
        .map(|(idx, _)| idx)
        .collect()
}

#[test]
fn emptyid_synthesized_id_matches_xw_grammar_goldens() {
    let mut body = emptyid_x69_body();
    patch_reasoning_empty_ids(&mut body, "vxm-az");
    let input = body["input"]
        .as_array()
        .expect("responses body must carry an input array");
    let idx = emptyid_x69_reasoning_indices(&body);
    assert_eq!(
        idx,
        vec![2, 3, 4, 5, 6],
        "the 5 incident reasoning items, incident order"
    );
    let ids: Vec<&str> = idx
        .iter()
        .map(|&i| input[i]["id"].as_str().expect("id present post-patch"))
        .collect();
    assert_eq!(
        ids,
        vec![
            "xw_bcf9e9828796d9d08c350f8a",
            "xw_f95ed93e0a9badbaccd12afa",
            "xw_7157485b1997e6654dea85d5",
            "xw_55ab10bbffec861ca557f6e6",
            "xw_46a650fad66e55e6928d41b2",
        ],
        "outgoing ids must equal the pinned xw_ goldens (sdd-69 §2.5; 12/12 re-verified at dispatch)"
    );
}

#[test]
fn emptyid_patch_preserves_nonempty_ids_and_is_idempotent() {
    let mut body = emptyid_x69_body();
    // One vLLM-coined non-empty id (the rs_ family the lenient shim mints):
    // the patch must not rewrite it.
    {
        let input = body["input"].as_array_mut().expect("input array");
        let first = input
            .iter_mut()
            .find(|i| i.get("type").and_then(serde_json::Value::as_str) == Some("reasoning"))
            .expect("a reasoning item");
        first["id"] = serde_json::json!("rs_019x69pinned");
    }
    patch_reasoning_empty_ids(&mut body, "vxm-az");
    let input = body["input"]
        .as_array()
        .expect("responses body must carry an input array");
    let idx = emptyid_x69_reasoning_indices(&body);
    // The vLLM-coined id is byte-identical (untouched).
    assert_eq!(
        input[idx[0]]["id"],
        serde_json::json!("rs_019x69pinned"),
        "non-empty ids must ride verbatim (no regression on native responses ids)"
    );
    // The remaining 4 empty ids are synthesized with the shared grammar;
    // ord counts every reasoning item, so these equal goldens 1..5.
    let goldens = [
        "xw_bcf9e9828796d9d08c350f8a",
        "xw_f95ed93e0a9badbaccd12afa",
        "xw_7157485b1997e6654dea85d5",
        "xw_55ab10bbffec861ca557f6e6",
        "xw_46a650fad66e55e6928d41b2",
    ];
    let ids: Vec<&str> = idx[1..]
        .iter()
        .map(|&i| input[i]["id"].as_str().expect("synthesized id present"))
        .collect();
    assert_eq!(ids, goldens[1..], "ords 1..4 synthesize to goldens 1..5");
    // Idempotence: the second pass is a byte no-op.
    let first_pass = body.clone();
    patch_reasoning_empty_ids(&mut body, "vxm-az");
    assert_eq!(body, first_pass, "second patch pass must be a byte no-op");
}
