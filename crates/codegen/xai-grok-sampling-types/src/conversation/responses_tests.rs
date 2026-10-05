use super::responses::{
    MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES, SearchAdmission, TOOL_SEARCH_DEFAULT_LIMIT,
    ToolSearchExecution, ToolSearchSource, ToolSearchSourceListing,
    extra_tool_entries_with_declaration, tool_search_declaration_entry, tool_search_description,
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
/// (`conversation.rs:41-50` against `codex-rs/utils/string/src/lib.rs:13-26`). So the bytes the
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
/// `tool_search, web_search, x_search`. The splice of those entries into the
/// serialized body's top-level `tools` array is the sampler's
/// (`xai-grok-sampler/src/client.rs:952`, `splice_extra_tool_entries`), and a wire-order
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
/// Production passes a declaration only on an admitted route: the three Responses body sites
/// call `extra_tool_entries_for_route` (`client.rs:2734`, `client.rs:3500`, `client.rs:3597`),
/// which forwards one only when `SearchAdmission::admitted()`; `extra_tool_entries` itself stays
/// declaration-less (`responses.rs:818`). Those bodies are pinned in the sampler, and this
/// test pins the raw-JSON channel the declaration travels on, so it holds whichever way the
/// wiring goes. The top-level splice is the sampler's own (`client.rs:952`).
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
/// Production passes a declaration only on an admitted route: `extra_tool_entries` passes
/// `declaration: None` (`responses.rs:819`), and the three Responses body sites call
/// `extra_tool_entries_for_route` (`client.rs:2734`, `client.rs:3500`, `client.rs:3597`), which
/// forwards one only when `SearchAdmission::admitted()`. This test pins the raw-JSON channel
/// itself, so it holds whichever way the wiring goes. The top-level splice is the sampler's own
/// (`client.rs:952`). The end-to-end proof is owed by apex-waj.9 and apex-waj.20.
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
/// `extra_tool_entries` passes `declaration: None`, which is the branch this test
/// pins; the admitted branch is reached through `extra_tool_entries_for_route`
/// (`client.rs:2734`, `client.rs:3500`, `client.rs:3597`) and is pinned in the sampler, so
/// nothing here depends on that wiring. The top-level splice a declaration would ride is the
/// sampler's own (`client.rs:952` `splice_extra_tool_entries`); the live arm is apex-waj.20.
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
            phase: None,
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
        prompt_cache_options: None,
        prompt_cache_diagnostics: None,
        moderation: None,
    };

    let items = response_to_conversation_items(response).expect("the projection succeeds for this response");
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
            namespace: None,
            caller: None,
            r#async: None,
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
        prompt_cache_options: None,
        prompt_cache_diagnostics: None,
        moderation: None,
    };

    let items = response_to_conversation_items(response_with_fc).expect("the projection succeeds for this response");
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
            mode: None,
            context: None,
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
        prompt_cache_options: None,
        prompt_cache_diagnostics: None,
        moderation: None,
    };

    let items = response_to_conversation_items(response).expect("the projection succeeds for this response");
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
    assert_eq!(fco.call_id.as_deref(), Some("call_1"));
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
    assert_eq!(fco_items[0].call_id.as_deref(), Some("call_1"));
    assert_eq!(fco_items[1].call_id.as_deref(), Some("call_2"));
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
                id: Some("reasoning_enc".to_string()),
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
                phase: None,
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
        prompt_cache_options: None,
        prompt_cache_diagnostics: None,
        moderation: None,
    };

    // Exercise the flat-list path: reasoning lives as a sibling
    let items = response_to_conversation_items(response).expect("the projection succeeds for this response");
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
                id: Some("reasoning_only_enc".to_string()),
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
                phase: None,
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
        prompt_cache_options: None,
        prompt_cache_diagnostics: None,
        moderation: None,
    };

    // Flat-list path: reasoning sibling carries the encrypted blob, empty summary maps to an empty `Vec<SummaryPart>`
    let items = response_to_conversation_items(response).expect("the projection succeeds for this response");
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
            id: Some("r1".to_string()),
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
            id: Some(String::new()),
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
        Some(rs::ToolChoiceParam::Option(rs::ToolChoiceOptions::Auto))
    );

    // Test Required
    let req = ConversationRequest::from_items(vec![ConversationItem::user("test")])
        .with_tool_choice(ConversationToolChoice::Required);
    let responses_req: rs::CreateResponse = (&req).into();
    assert_matches!(
        responses_req.tool_choice,
        Some(rs::ToolChoiceParam::Option(rs::ToolChoiceOptions::Required))
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
        function_outputs.contains(&Some("call_1".to_string())),
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
            id: Some("rs_1".to_string()),
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
    assert_eq!(fco_items[0].call_id.as_deref(), Some("call_1"));

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
    assert_eq!(outputs[0].call_id.as_deref(), Some("call_1"));
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
                prompt_cache_breakpoint: None,
            }),
            rs::InputContent::InputImage(rs::InputImageContent {
                detail: rs::ImageDetail::Auto,
                file_id: None,
                image_url: Some("data:image/png;base64,iVBOR".to_string()),
                prompt_cache_breakpoint: None,
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
                prompt_cache_breakpoint: None,
            }),
            rs::InputContent::InputImage(rs::InputImageContent {
                detail: rs::ImageDetail::Auto,
                file_id: None,
                image_url: Some("data:image/png;base64,iVBOR".to_string()),
                prompt_cache_breakpoint: None,
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
                prompt_cache_breakpoint: None,
            }),
            rs::InputContent::InputImage(rs::InputImageContent {
                detail: rs::ImageDetail::Auto,
                file_id: None,
                image_url: Some("data:image/png;base64,aG90".to_string()),
                prompt_cache_breakpoint: None,
            }),
            rs::InputContent::InputImage(rs::InputImageContent {
                detail: rs::ImageDetail::Auto,
                file_id: None,
                image_url: Some("data:image/png;base64,aW1n".to_string()),
                prompt_cache_breakpoint: None,
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
                prompt_cache_breakpoint: None,
            }),
            rs::InputContent::InputImage(rs::InputImageContent {
                detail: rs::ImageDetail::Auto,
                file_id: None,
                image_url: Some("data:image/png;base64,aG90".to_string()),
                prompt_cache_breakpoint: None,
            }),
            rs::InputContent::InputImage(rs::InputImageContent {
                detail: rs::ImageDetail::Auto,
                file_id: None,
                image_url: Some("data:image/png;base64,aW1n".to_string()),
                prompt_cache_breakpoint: None,
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
            phase: None,
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
        prompt_cache_options: None,
        prompt_cache_diagnostics: None,
        moderation: None,
    };

    let items = response_to_conversation_items(response).expect("the projection succeeds for this response");
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
                id: Some("r1".to_string()),
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
            id: Some(text.to_string()),
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
    assert_eq!(r.id.as_deref(), Some(""));
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
            status: rs::WebSearchCallStatus::Completed,
            action: Some(rs::WebSearchToolCallAction::Search(rs::WebSearchActionSearch {
                query: Some("alpha".to_string()),
                queries: None,
                sources: Some(vec![]),
            })),
        }),
    });
    let ws_b = ConversationItem::BackendToolCall(BackendToolCallItem {
        kind: BackendToolKind::WebSearch(rs::WebSearchToolCall {
            id: "ws_b".to_string(),
            status: rs::WebSearchCallStatus::Completed,
            action: Some(rs::WebSearchToolCallAction::Search(rs::WebSearchActionSearch {
                query: Some("beta".to_string()),
                queries: None,
                sources: Some(vec![]),
            })),
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

// The discovery-item arms of this seam are owned by the
// `HTS-DECODE-CARRIER (apex-mrmq)` section at the end of this file: the seam
// maps a complete `tool_search_call` + `tool_search_output` pair onto
// `ConversationItem::Discovery` and refuses only the classes that genuinely
// cannot be represented.

// ─── ITEM O3 / bead apex-waj.35: the route-keyed entry point ───────────────

/// The admitted half of [`extra_tool_entries_for_route`]: a route whose admission holds leads the
/// raw-JSON channel with the `tool_search` declaration, ahead of every hosted entry. Placement is
/// the whole contract this crate can see — the top-level splice into the serialized body's `tools`
/// array is the sampler's (`client.rs` `splice_extra_tool_entries`), and its own tests pin that the
/// declaration reaches `tools[0]` even when the body already carried typed function tools, while the
/// hosted entries go on the end.
///
/// This is the seam the item exists to open: `extra_tool_entries` passes `declaration: None`, so an
/// un-admitted route gets the declaration-less channel.
/// An admitted route for the channel tests below, spelled as named fields. These tests are about
/// the bytes this module emits, not about how either signal is computed: the fixture spells the
/// pair directly. [`SearchAdmission::for_row`] is the construction path production uses.
const ADMITTED_ROUTE: SearchAdmission = SearchAdmission {
    supports_search_tool: true,
    has_searchable_tools: true,
};
/// The three ways a route is NOT admitted. Named as field-named
/// literals rather than through a positional constructor: `SearchAdmission::admitted()`
/// folds the two signals with a symmetric `&&`, so `new(a, b)` and `new(b, a)` would both
/// compile and a swap would be invisible here — the literal names which signal is closing
/// the route. No `new(bool, bool)` exists and none was added for that reason; the one
/// positional constructor this head does have, [`SearchAdmission::for_row`], takes the second
/// signal as a typed tool surface rather than a bool, which is what makes its argument order
/// unswappable.
const NOTHING_SEARCHABLE: SearchAdmission = SearchAdmission {
    supports_search_tool: true,
    has_searchable_tools: false,
};
const ROW_DECLINES: SearchAdmission = SearchAdmission {
    supports_search_tool: false,
    has_searchable_tools: true,
};
const NEITHER_SIGNAL: SearchAdmission = SearchAdmission {
    supports_search_tool: false,
    has_searchable_tools: false,
};

#[test]
fn admitted_route_leads_the_channel_with_the_declaration() {
    let hosted = [
        HostedTool::WebSearch { options: None },
        HostedTool::XSearch { options: None },
    ];
    let entries = extra_tool_entries_for_route(&hosted, Some(ADMITTED_ROUTE));
    assert_eq!(
        entries
            .iter()
            .map(|e| e["type"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["tool_search", "web_search", "x_search"],
        "the declaration leads, then the hosted tools in the request's own order"
    );

    // A route with no hosted tools still gets the declaration: the typed body carries no `tools`
    // key in that shape, so the raw-JSON channel is its only way onto the wire.
    let alone = extra_tool_entries_for_route(&[], Some(ADMITTED_ROUTE));
    assert_eq!(
        alone.len(),
        1,
        "the declaration alone is a one-entry channel"
    );
    assert_eq!(alone[0]["type"], serde_json::json!("tool_search"));
}

/// The un-admitted half, and the blast-radius guarantee: every route that is not admitted — no
/// admission at all (`ConversationRequest::search_admission` is `None`, which only a request
/// hand-built outside either writer takes — both writers install `Some(..)`), a row that does
/// not advertise the contract, or a row that advertises it with nothing searchable — must
/// produce the bytes `extra_tool_entries` produces today, in the same order, with no
/// declaration anywhere in the slice. Compared as serialized bytes rather than `Value` equality
/// so an inserted or re-keyed entry cannot cancel itself out under serde_json's order-blind
/// object compare.
#[test]
fn unadmitted_route_entries_stay_byte_identical() {
    let hosted = [
        HostedTool::WebSearch { options: None },
        HostedTool::XSearch { options: None },
    ];
    let today = serde_json::to_string(&extra_tool_entries(&hosted)).expect("entries serialize");
    for decline in [
        None,
        Some(NEITHER_SIGNAL),
        Some(NOTHING_SEARCHABLE),
        Some(ROW_DECLINES),
    ] {
        let got = extra_tool_entries_for_route(&hosted, decline);
        assert_eq!(
            serde_json::to_string(&got).expect("entries serialize"),
            today,
            "a route that is not admitted must not change one byte of the channel: {decline:?}"
        );
        assert!(
            !got.iter()
                .any(|e| e["type"].as_str() == Some("tool_search")),
            "an un-admitted route must not advertise the declaration: {decline:?}"
        );
    }
    assert!(
        extra_tool_entries_for_route(&[], None).is_empty(),
        "no hosted tools and no admission must stay an empty channel, so the sampler's splice \
         remains a no-op and the body grows no `tools` key"
    );
}

/// Ruling `map/RULINGS-o1o5.md` §D5, pinned on the emitted bytes. The live strict row
/// `gpt-5.6-sol` (Azure via the proxy) 400s `execution: "server"` whenever the declaration carries
/// a `description` or `parameters`, and `tool_search_declaration_entry` emits both
/// unconditionally, so `Server` here would 400 every request on that row. `Client` + both fields
/// is the donor form that returns 200 and mints a `tool_search_call`. Nothing in the types
/// prevents assembling the 400ing pair, which is exactly why this assertion reads the entry the
/// function emitted.
///
/// The determinism half is the cache-cost clause of the item: the declaration is model-visible
/// inside the request-level `tools[]`, so it must not churn per request.
#[test]
fn admitted_route_declaration_is_client_executed() {
    let entries = extra_tool_entries_for_route(&[], Some(ADMITTED_ROUTE));
    let declaration = &entries[0];
    assert_eq!(
        declaration["execution"],
        serde_json::json!("client"),
        "ruling D5: an admitted route sends client execution, or the strict row 400s every request"
    );
    assert!(
        declaration["description"].is_string() && declaration["parameters"].is_object(),
        "the donor form that the live row accepts carries both fields: {declaration:?}"
    );
    assert_eq!(
        json_keys(declaration),
        ["type", "execution", "description", "parameters"],
        "the declaration keeps the donor key order on its way to the wire"
    );
    assert_declaration_fixed_half_is_donor_exact(declaration);
    // The production path's `limit`, tied to the constant the producer passes. The donor bytes are
    // already pinned at 8 by `assert_declaration_fixed_half_is_donor_exact` above, so a constant
    // that moved reddens there; this assertion names the constant in the failure instead of
    // leaving the next seat to find it from a description-text diff.
    let limit_description = declaration["parameters"]["properties"]["limit"]["description"]
        .as_str()
        .expect("the limit description is a string");
    assert_eq!(
        limit_description,
        format!("Maximum number of tools to return. Defaults to {TOOL_SEARCH_DEFAULT_LIMIT}."),
        "the declaration the route emits documents the constant, not a literal"
    );
    assert_eq!(
        serde_json::to_string(&entries).expect("entries serialize"),
        serde_json::to_string(&extra_tool_entries_for_route(&[], Some(ADMITTED_ROUTE)))
            .expect("entries serialize"),
        "the declaration is byte-stable across producer calls — not churn the cached prefix"
    );
}

// ─── HTS-DECODE-CARRIER / bead apex-mrmq: the discovery decode seam ────────
//
// Provider-shaped discovery item bytes, driven through the real
// [`response_to_conversation_items`]. The four base fixtures are the item objects
// the sampler's stream tests bank from live SSE captures —
// `xai-grok-sampler/src/stream/responses.rs:2143` (client `tool_search_call`
// done copy), `:2146` (server `tool_search_call`), `:2149` (server
// `tool_search_output`) and `:2152` (the terminal client call) at this head —
// with the client answer item written in the same shape (the live corpus holds
// no client-executed answer: `ratchet-capture/_recon-fixtures.md` §6b). Both
// join shapes the module recognises are therefore covered: the keyed client
// pair (`call_id` on both halves) and the keyless server pair (`call_id`
// absent on both halves, adjacent in `output`).
//
// The two `_BODY` fixtures below add the third observed shape: the same hosted
// pair as it arrives in a RESPONSE BODY, where the provider sends `call_id`,
// `created_by` and two `tools[]` keys explicitly as null.

/// A server-executed `tool_search_call`: no `call_id` key at all.
const HOSTED_CALL_ITEM: &str = r#"{"id":"tsc_0c1a30c585eeaaf9016ac01db8650081949aff9bff5698a20a","type":"tool_search_call","status":"completed","arguments":{"paths":["lookup_shipping_eta"]},"execution":"server"}"#;

/// The server-executed answer to [`HOSTED_CALL_ITEM`], its `tools` entry in
/// the captured shape (`defer_loading`, `output_schema` and `strict` included).
const HOSTED_OUTPUT_ITEM: &str = r#"{"id":"tso_0c1a30c585eeaaf9016ac01db86afc81949af526e214666b827","type":"tool_search_output","status":"completed","execution":"server","tools":[{"type":"function","defer_loading":true,"description":"Look up the shipping ETA for an order ID by order ID.","name":"lookup_shipping_eta","output_schema":null,"parameters":{"type":"object","properties":{"order_id":{"type":"string"}},"required":["order_id"],"additionalProperties":false},"strict":true}]}"#;

/// The [`HOSTED_CALL_ITEM`] pair as a RESPONSE BODY delivers it, not an SSE frame:
/// every optional key is present with value null. Verbatim from
/// `plans/harness/hosted-tool-search/captures/2026-09-25-wire-grounding/wire_resp_20260925T062640Z_R1_SOL_HOSTED.json`
/// (`output[1]`), the shape 8 of 8 discovery items in that corpus's four
/// hosted-search files actually take.
const HOSTED_CALL_ITEM_BODY: &str = r#"{"id":"tsc_0ce980d5c6afd41f016ab61423e6ec81908939d7d041618fb1","arguments":{"paths":["lookup_shipping_eta"]},"call_id":null,"execution":"server","status":"completed","type":"tool_search_call","created_by":null}"#;

/// The answer half for [`HOSTED_CALL_ITEM_BODY`], same capture (`output[2]`).
const HOSTED_OUTPUT_ITEM_BODY: &str = r#"{"id":"tso_0ce980d5c6afd41f016ab61424031481908982e2c788dcc429","call_id":null,"execution":"server","status":"completed","tools":[{"name":"lookup_shipping_eta","parameters":{"type":"object","properties":{"order_id":{"type":"string"}},"required":["order_id"],"additionalProperties":false},"strict":true,"type":"function","allowed_callers":null,"defer_loading":true,"description":"Look up the shipping ETA for an order ID.","output_schema":null}],"type":"tool_search_output","created_by":null}"#;

/// A client-executed `tool_search_call` whose answer the harness owns.
const CLIENT_CALL_ITEM: &str = r#"{"id":"tsc_08f7abc693f02e74016ac02242e2fc8190b1842f1acf6b72fc","arguments":{"query":"shipping ETA lookup by order ID","limit":5},"call_id":"call_KGrhHQ8F7MeagVDbKnGlq6vv","execution":"client","status":"completed","type":"tool_search_call"}"#;

/// The client-executed answer to [`CLIENT_CALL_ITEM`], joined by `call_id`.
const CLIENT_OUTPUT_ITEM: &str = r#"{"id":"tso_08f7abc693f02e74016ac02242e2fc8190b1842f1acf6b72fd","type":"tool_search_output","status":"completed","execution":"client","call_id":"call_KGrhHQ8F7MeagVDbKnGlq6vv","tools":[{"type":"function","name":"lookup_shipping_eta","parameters":{"type":"object","properties":{"order_id":{"type":"string"}},"required":["order_id"]}}]}"#;

/// A completed response envelope carrying raw item JSON in `output`.
fn response_of(output: &[&str]) -> rs::Response {
    let output: Vec<serde_json::Value> = output
        .iter()
        .map(|raw| serde_json::from_str(raw).expect("fixture item is valid JSON"))
        .collect();
    let wire = serde_json::json!({
        "id": "resp_mrmq",
        "object": "response",
        "created_at": 0u64,
        "status": "completed",
        "model": "gpt-5.6-sol",
        "output": output,
    });
    serde_json::from_value(wire).expect("0.42.1 models this response shape")
}

/// The discovery carriers a decode produced, in item order.
fn discovery_carriers(items: &[ConversationItem]) -> Vec<&tool_search::ToolSearchItem> {
    items
        .iter()
        .filter_map(ConversationItem::discovery)
        .collect()
}

/// The pair law is what this seam now enforces, so a complete pair must decode
/// onto two `Discovery` items — the server-executed (keyless, adjacent) shape
/// the hosted-search captures hold. Emission order is asserted against a
/// `Reasoning` sibling because the flattened order is what the next turn replays
/// byte for byte.
#[test]
fn a_hosted_discovery_pair_decodes_onto_discovery_items_in_emission_order() {
    let reasoning = r#"{"type":"reasoning","id":"rs_mrmq_1","summary":[]}"#;
    let items = response_to_conversation_items(response_of(&[
        reasoning,
        HOSTED_CALL_ITEM,
        HOSTED_OUTPUT_ITEM,
    ]))
    .expect("a complete discovery pair is not a refusal condition");

    let carriers = discovery_carriers(&items);
    assert_eq!(
        carriers.len(),
        2,
        "one carrier per discovery item, no duplicate: {items:?}"
    );
    assert_eq!(
        carriers
            .iter()
            .map(|c| c.kind())
            .collect::<Vec<tool_search::ToolSearchKind>>(),
        [
            tool_search::ToolSearchKind::Call,
            tool_search::ToolSearchKind::Output,
        ],
        "the pair lands in emission order"
    );
    assert_eq!(
        items
            .iter()
            .position(|i| matches!(i, ConversationItem::Reasoning(_))),
        Some(0),
        "the reasoning sibling keeps its slot ahead of the pair"
    );
    assert!(
        matches!(items.last(), Some(ConversationItem::Assistant(_))),
        "the trailing Assistant is still last: {items:?}"
    );
    let order = items
        .iter()
        .map(|i| match i {
            ConversationItem::Reasoning(_) => "reasoning",
            ConversationItem::Discovery { item } => match item.kind() {
                tool_search::ToolSearchKind::Call => "call",
                tool_search::ToolSearchKind::Output => "output",
            },
            ConversationItem::Assistant(_) => "assistant",
            _ => "other",
        })
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        ["reasoning", "call", "output", "assistant"],
        "flattening must preserve emission order, not group the pair"
    );
}

/// The keyed client shape pairs on `call_id`, not on adjacency.
#[test]
fn a_keyed_client_discovery_pair_decodes_onto_discovery_items() {
    let items =
        response_to_conversation_items(response_of(&[CLIENT_CALL_ITEM, CLIENT_OUTPUT_ITEM]))
            .expect("a keyed pair is a representable item");
    let carriers = discovery_carriers(&items);
    assert_eq!(carriers.len(), 2, "one carrier per half: {items:?}");
    for carrier in &carriers {
        assert_eq!(
            carrier.call_id(),
            Some("call_KGrhHQ8F7MeagVDbKnGlq6vv"),
            "the join key is copied, never rewritten"
        );
    }
    assert_eq!(
        carriers[0].query(),
        Some("shipping ETA lookup by order ID"),
        "the call's query survives the typed round-trip"
    );
    assert_eq!(
        carriers[1].tools().len(),
        1,
        "the loaded definition survives the typed round-trip"
    );
    assert_eq!(
        carriers[1].text_summary(),
        "[tool_search results] 1 tool",
        "the bounded summary form of the answer"
    );
}

/// Two provider-minted pairs in one response. Every half is keyless, so order is the
/// only join available, and the module that owns discovery pairing reads this shape
/// as TWO closed groups — `discovery_groups`' unkeyed FIFO
/// (`conversation/tool_search.rs:2239-2256`) — which is also the reading
/// `snap_index_over_discovery_pairs` (`conversation/tool_search.rs:2417`) uses to
/// keep both pairs atomic across every history cut. Refusing it here while the cut
/// funnel called it two pairs would cost a whole turn (the caller maps the `Err` to
/// `SamplingEvent::Failed`, `xai-grok-sampler/src/stream/responses.rs:823-829`) over
/// a shape the transcript is ready to hold, so both provider orderings are pinned as
/// decodes. A batch that does NOT balance stays refused —
/// `a_discovery_half_pair_is_refused_and_names_the_missing_half` pins `call, call,
/// output`.
#[test]
fn a_keyless_discovery_batch_decodes_every_closed_pair() {
    let second_call = HOSTED_CALL_ITEM.replace(
        "tsc_0c1a30c585eeaaf9016ac01db8650081949aff9bff5698a20a",
        "tsc_mrmq_second_call",
    );
    let second_output = HOSTED_OUTPUT_ITEM.replace(
        "tso_0c1a30c585eeaaf9016ac01db86afc81949af526e214666b827",
        "tso_mrmq_second_output",
    );
    for (label, order) in [
        (
            "call, call, output, output",
            [
                HOSTED_CALL_ITEM,
                second_call.as_str(),
                HOSTED_OUTPUT_ITEM,
                second_output.as_str(),
            ],
        ),
        (
            "call, output, call, output",
            [
                HOSTED_CALL_ITEM,
                HOSTED_OUTPUT_ITEM,
                second_call.as_str(),
                second_output.as_str(),
            ],
        ),
    ] {
        let items = response_to_conversation_items(response_of(&order))
            .unwrap_or_else(|error| panic!("{label}: two closed keyless pairs must map: {error}"));
        let carriers = discovery_carriers(&items);
        assert_eq!(
            carriers.len(),
            4,
            "one carrier per half, {label}: {items:?}"
        );
        assert_eq!(
            carriers
                .iter()
                .map(|c| c.kind().item_type())
                .collect::<Vec<&str>>(),
            order
                .iter()
                .map(|raw| if raw.contains(r#""type":"tool_search_call""#) {
                    "tool_search_call"
                } else {
                    "tool_search_output"
                })
                .collect::<Vec<&str>>(),
            "{label}: every half must keep its own slot, not be re-grouped into pairs"
        );
        let mut ids = carriers
            .iter()
            .map(|c| c.id().expect("fixture ids are non-empty"))
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            4,
            "four different provider items, not one pair mapped twice: {ids:?}"
        );
    }
}

/// The keyless join runs over the response's discovery halves, not over its raw
/// `output` array: a `reasoning` row between the two halves — the shape the SDK
/// itself emits, since reasoning items land wherever the provider put them — must
/// not separate a complete pair. Pinned as a decode rather than argued in the law's
/// doc, because a grouping that walked the whole `items` vector looking for a
/// neighbour would pass every physically adjacent fixture in this file and still
/// split a real response.
#[test]
fn a_non_discovery_item_between_the_halves_does_not_split_the_keyless_pair() {
    let reasoning = r#"{"type":"reasoning","id":"rs_mrmq_between","summary":[]}"#;
    let items = response_to_conversation_items(response_of(&[
        HOSTED_CALL_ITEM,
        reasoning,
        HOSTED_OUTPUT_ITEM,
    ]))
    .expect("a non-discovery row between the halves does not separate the pair");
    assert_eq!(
        discovery_carriers(&items).len(),
        2,
        "both halves of the separated pair are in the IR: {items:?}"
    );
    assert_eq!(
        items
            .iter()
            .filter(|i| matches!(i, ConversationItem::Reasoning(_)))
            .count(),
        1,
        "the row between them survives too — the pair was joined around it, not by dropping it"
    );

    // Control: the same three items with the row outside the pair, which every other
    // fixture here already assumes.
    let control = response_to_conversation_items(response_of(&[
        reasoning,
        HOSTED_CALL_ITEM,
        HOSTED_OUTPUT_ITEM,
    ]))
    .expect("the unseparated pair decodes");
    assert_eq!(
        discovery_carriers(&control).len(),
        2,
        "the control shape is unaffected: {control:?}"
    );
}

/// The shape set this seam denies, pinned case by case. Dropping one half silently
/// is the A-26 loss the carrier variant exists to prevent and committing one half
/// alone is what PLAN:946's keep-or-drop-together law forbids, so the answer is a
/// refusal that names the missing half.
///
/// The last two rows are COMPLETE pairs by count, and they are refused on purpose:
/// a key disagreement is not a pair (`call_KGrh…` and a keyless output answer two
/// different searches as far as this seam can tell), and a keyless batch with more
/// calls than answers leaves the owner's FIFO (`conversation/tool_search.rs:2239-2256`)
/// holding a call that nothing can close. PLAN:946's remedy for both is
/// the repair pass (T15) or its removal set, neither of which lives in this crate —
/// `enforce_discovery_pair_law`'s doc names the cost, which is the whole response.
#[test]
fn a_discovery_half_pair_is_refused_and_names_the_missing_half() {
    for (label, items, expected) in [
        ("call alone", &[HOSTED_CALL_ITEM][..], "tool_search_output"),
        (
            "output alone",
            &[HOSTED_OUTPUT_ITEM][..],
            "tool_search_call",
        ),
        (
            "keyed call alone",
            &[CLIENT_CALL_ITEM][..],
            "tool_search_output",
        ),
        (
            "keyed call answered by a keyless output",
            &[CLIENT_CALL_ITEM, HOSTED_OUTPUT_ITEM][..],
            "tool_search_output",
        ),
        (
            "keyless batch: call, call, output",
            &[HOSTED_CALL_ITEM, HOSTED_CALL_ITEM, HOSTED_OUTPUT_ITEM][..],
            "tool_search_output",
        ),
    ] {
        let err = response_to_conversation_items(response_of(items))
            .err()
            .unwrap_or_else(|| panic!("{label}: a pair this seam cannot join must not commit"));
        let msg = err.to_string();
        assert!(
            msg.contains(expected),
            "{label}: the refusal must name the missing {expected}: {msg}"
        );
    }
}

/// The keyed join is a KEY test, not "an opposite kind exists somewhere in the
/// response": a call and an output carrying different `call_id` values answer two
/// different searches, and reading them as one pair is not what PLAN:946's law
/// says. A keyless half may not adopt a keyed opposite either — the unkeyed FIFO
/// skips items that carry a key (`conversation/tool_search.rs:2243-2244`), so the
/// hosted pair and the client pair do not merge into one. Both halves of the positive case are
/// pinned too: the keyed join is order-blind, which is the property the doc on
/// [`enforce_discovery_pair_law`] claims when it says it takes the KEY as the unit.
#[test]
fn discovery_halves_only_pair_with_the_same_join_key() {
    let foreign_key_output = CLIENT_OUTPUT_ITEM.replace(
        "call_KGrhHQ8F7MeagVDbKnGlq6vv",
        "call_MISMATCHED00000000000000000",
    );
    let err = response_to_conversation_items(response_of(&[CLIENT_CALL_ITEM, &foreign_key_output]))
        .expect_err("two halves under different call_id are not a pair");
    let msg = err.to_string();
    assert!(
        msg.contains("call_KGrhHQ8F7MeagVDbKnGlq6vv"),
        "the refusal must name the key it could not answer: {msg}"
    );

    let err = response_to_conversation_items(response_of(&[HOSTED_CALL_ITEM, CLIENT_OUTPUT_ITEM]))
        .expect_err("a keyless call must not adopt a keyed output as its answer");
    assert!(
        err.to_string()
            .contains("<none: the keyless quadrant joins on order>"),
        "the refusal must say the half was keyless: {err}"
    );

    let items =
        response_to_conversation_items(response_of(&[CLIENT_OUTPUT_ITEM, CLIENT_CALL_ITEM]))
            .expect("a keyed pair pairs in either emission order");
    assert_eq!(
        discovery_carriers(&items).len(),
        2,
        "order is not a pairing condition for a keyed pair: {items:?}"
    );

    // CONDITIONAL PIN — re-open this the moment a hosted-dialect capture lands.
    // The keyless join IS ordered: with no key there is nothing to identify the
    // pair except PLAN:946's "must follow" placement, so an output that runs ahead
    // of its call is not an answer to it. The rule is the owner's unkeyed FIFO
    // (`conversation/tool_search.rs`, `discovery_groups`'s
    // "the NEXT null-key output" pass), so ORDER is required and adjacency is not —
    // `a_non_discovery_item_between_the_halves_does_not_split_the_keyless_pair`
    // pins the difference. The ORDER itself is a reading of the two
    // banked live frames (`xai-grok-sampler/src/stream/responses.rs:2146` carries
    // `output_index: 1`, its output at `:2149` carries 2), not wire evidence that
    // the inverted order never happens. Two consequences are recorded here because
    // both are this test's doing, not the plan's: a complete-but-inverted pair
    // loses the WHOLE turn including the assistant text, which is stricter than
    // PLAN:946's remedy ("restore from the durable record, or drop both" — T12/T15
    // own those), and relaxing this arm to drop-both would need that repair pass to
    // exist first. If a capture ever shows a keyless pair in the other order, the
    // rule, this pin and the deviation note in `enforce_discovery_pair_law` all
    // change together.
    let err = response_to_conversation_items(response_of(&[HOSTED_OUTPUT_ITEM, HOSTED_CALL_ITEM]))
        .expect_err("a keyless output is not answered by a call that comes after it");
    assert!(
        err.to_string().contains("tool_search_call"),
        "the inverted keyless pair must be refused as a missing call: {err}"
    );
}

/// Idempotence and carrier survival: the same response decoded twice produces the
/// same items, and the pair round-trips through the store form (`Serialize` /
/// `Deserialize` over `raw`) without a second copy or a synthesised id.
#[test]
fn decoding_the_same_response_twice_yields_identical_carriers_with_no_synthesised_ids() {
    let response = response_of(&[HOSTED_CALL_ITEM, HOSTED_OUTPUT_ITEM]);
    let first = response_to_conversation_items(response.clone())
        .expect("the pair decodes")
        .into_iter()
        .map(|item| serde_json::to_string(&item).expect("item serialises"))
        .collect::<Vec<_>>();
    let second = response_to_conversation_items(response)
        .expect("the pair decodes again")
        .into_iter()
        .map(|item| serde_json::to_string(&item).expect("item serialises"))
        .collect::<Vec<_>>();
    assert_eq!(first, second, "decode is idempotent across passes");
    assert_eq!(
        first
            .iter()
            .filter(|line| line.contains("\"tool_search_output\""))
            .count(),
        1,
        "no duplicate output carrier: {first:?}"
    );

    // Store round-trip: `chat_history.jsonl` re-enters through `Deserialize`,
    // which re-runs `ToolSearchItem::from_wire` on the same bytes.
    let items =
        response_to_conversation_items(response_of(&[HOSTED_CALL_ITEM, HOSTED_OUTPUT_ITEM]))
            .expect("the pair decodes");
    for item in items.iter().filter(|i| i.discovery().is_some()) {
        let json = serde_json::to_string(item).expect("carrier serialises");
        let back: ConversationItem = serde_json::from_str(&json).expect("carrier re-enters");
        let raw_out = item.discovery().expect("item is a carrier").raw();
        let raw_back = back
            .discovery()
            .expect("round-tripped item is a carrier")
            .raw();
        assert_eq!(raw_out, raw_back, "store round-trip is a byte round-trip");
        assert!(
            !json.contains("tsc_synthetic") && !json.contains("tso_synthetic"),
            "the decode mints nothing: {json}"
        );
    }
}

/// Opaque artifacts are handles, not content (wire invariant 6). At this seam that
/// is three checkable things: the `tsc_*` / `tso_*` ids ride through verbatim; the
/// decode ATTACHES nothing — no key on a carrier carries a non-null value the
/// provider item did not already carry under that key, so one half can never
/// inherit the other's handle; and the normalisations the typed round trip does
/// make are pinned as bytes instead of believed.
///
/// The normalisations are the SDK's, not this seam's, and one of them is a real loss: an
/// `encrypted_content` sent on a discovery item is dropped by the deserializer before
/// [`response_to_conversation_items`] is ever reached, so on this path the carrier keeps the
/// SDK's typed projection rather than the provider's bytes. `ToolSearchItem`'s `raw` field
/// doc owns that analysis (`conversation/tool_search.rs:460-477`); it is not restated here.
/// Recovering the frame bytes belongs to the sampler's stream layer, outside this cut, so the
/// loss is ASSERTED here rather than fixed — when a raw-frame path lands, this assertion
/// reddens and the carrier's claim becomes true again.
#[test]
fn decoded_carriers_copy_provider_handles_and_attach_no_foreign_encrypted_content() {
    let call_with_carrier = HOSTED_CALL_ITEM.replace(
        ",\"execution\":\"server\"}",
        ",\"execution\":\"server\",\"encrypted_content\":\"litellm_enc:mrmq\"}",
    );
    let items =
        response_to_conversation_items(response_of(&[&call_with_carrier, HOSTED_OUTPUT_ITEM]))
            .expect("the pair decodes");
    let carriers = discovery_carriers(&items);
    assert_eq!(carriers.len(), 2, "one carrier per half: {items:?}");
    let sent: Vec<serde_json::Value> = [&call_with_carrier, HOSTED_OUTPUT_ITEM]
        .into_iter()
        .map(|raw| serde_json::from_str(raw).expect("fixture is valid JSON"))
        .collect();

    assert_eq!(
        carriers[0].id(),
        Some("tsc_0c1a30c585eeaaf9016ac01db8650081949aff9bff5698a20a"),
        "the call's item id is copied verbatim"
    );
    assert_eq!(
        carriers[1].id(),
        Some("tso_0c1a30c585eeaaf9016ac01db86afc81949af526e214666b827"),
        "the output's item id is copied verbatim"
    );
    // No half gains a value the other carried: compare each carrier's scalar keys
    // against the item it was built from. Nested values normalise internally (the
    // `output_schema` case below) and are pinned separately.
    for (carrier, sent) in carriers.iter().zip(sent.iter()) {
        let sent = sent.as_object().expect("fixture is an object");
        let stored = carrier.raw().as_object().expect("carrier is an object");
        for (key, value) in stored {
            if value.is_object() || value.is_array() {
                continue;
            }
            match sent.get(key) {
                Some(was) if !was.is_object() && !was.is_array() => {
                    assert_eq!(was, value, "`{key}` was rewritten between wire and carrier")
                }
                // A key the provider item did not carry may not appear at all: the
                // SDK's one addition on this shape (an explicit null `call_id`) is
                // undone inside `discovery_carrier`, and the nested key set is held
                // to the provider's by
                // `the_codex_splice_replays_the_provider_item_key_set`.
                None => panic!(
                    "the decode added a key `{key}` that no provider item carried: {}",
                    carrier.raw()
                ),
                Some(_) => {}
            }
        }
    }
    assert_eq!(
        carriers[1].raw().get("encrypted_content"),
        None,
        "the output half must not inherit the call half's handle"
    );
    assert_eq!(
        carriers[0].raw().get("encrypted_content"),
        None,
        "FIDELITY LOSS, pinned: the SDK models no `encrypted_content` on a discovery item, so \
         the provider's value never reaches the carrier (see this test's doc)"
    );
    assert!(
        !items
            .last()
            .expect("assistant")
            .text_content()
            .contains("litellm_enc:"),
        "the assistant text must not carry the opaque field"
    );
    assert_eq!(
        carriers[0].raw().get("call_id"),
        None,
        "the fixture omits `call_id`; the typed round trip would re-emit it as null \
         (async-openai-0.42.1 response.rs:151 has no skip_serializing_if) and \
         `discovery_carrier` drops that key so the stored bytes keep the provider's key set"
    );
    assert_eq!(
        carriers[1].raw().get("call_id"),
        None,
        "the output half is normalised identically (async-openai-0.42.1 response.rs:189)"
    );
    assert_eq!(
        carriers[1].raw()["tools"][0].get("output_schema"),
        None,
        "the provider wrote `\"output_schema\":null` at this index; the typed round trip drops \
         it (async-openai-0.42.1 response.rs:1386-1387 is skip_serializing_if)"
    );
    assert_eq!(
        carriers[1].call_id(),
        None,
        "a re-emitted null still reads as no key"
    );
}

/// §6.7: the model-visible fragment this decode can author is the bounded
/// summary, never the payload. The payload's own size is the provider's (a
/// `tool_search_output` carries whole tool definitions) and the seam neither
/// truncates it nor claims it is bounded.
#[test]
fn the_bounded_summary_form_stays_bounded_however_large_the_mapped_payload_is() {
    let wide_output = format!(
        r#"{{"id":"tso_wide_1","type":"tool_search_output","status":"completed","execution":"server","tools":[{}]}}"#,
        (0..40)
            .map(|i| format!(
                r#"{{"type":"function","name":"mrmq_tool_{i}","description":"{}","parameters":{{"type":"object"}}}}"#,
                "a description long enough to matter for the cached prefix. ".repeat(3)
            ))
            .collect::<Vec<_>>()
            .join(","),
    );
    let wide_call = HOSTED_CALL_ITEM.replace("lookup_shipping_eta", &"path_".repeat(400));
    let items = response_to_conversation_items(response_of(&[&wide_call, &wide_output]))
        .expect("a wide pair is still a complete pair");
    let carriers = discovery_carriers(&items);
    let payload_len: usize = carriers
        .iter()
        .map(|c| c.estimated_model_visible_len())
        .sum();
    let summary_len: usize = carriers.iter().map(|c| c.text_summary().len()).sum();
    assert!(
        payload_len > 4_000,
        "fixture must actually be wide to mean anything: {payload_len}"
    );
    assert!(
        summary_len < 1_000,
        "the summary form this seam can author is bounded; got {summary_len} bytes over a \
         {payload_len}-byte payload"
    );
}

/// §6.5's "no empty item ids" as a checked property of this arm rather than an
/// argued one. The decision itself is `enforce_discovery_pair_law`'s doc: an `id`
/// the provider wrote as `""` is that provider's byte, and this seam's only two
/// alternatives are to rewrite it (wire invariant 6 forbids) or to fail the turn for
/// a value the corpus does not contain — the sweep at
/// `conversation/tool_search.rs:566` measures 0 instances of `"id": ""` across
/// `captures/` and `ratchet-capture/fixtures/`. So the byte is KEPT and the handle is
/// not advertised. The Compaction arm earlier in the same loop writes its `id` key only
/// when non-empty (`conversation/responses.rs:98-100`) because that arm assembles its
/// raw from typed fields and therefore owns those bytes; this arm does not own them.
///
/// What this pin does NOT certify is wire ACCEPTANCE, and the next lane must not
/// read it as that. The kept byte is model-visible on the admitting rows: the Codex
/// replay arm splices `raw()` verbatim into every later request
/// (`conversation.rs:2699`) and all three rows the bake ships ON are
/// `model_family: "codex"` (`xai-grok-models/default_models.json:365`, `:420`,
/// `:528`). No capture in this estate carries a discovery item with an empty `id`
/// and this lane made no provider call, so whether any boundary accepts `"id": ""`
/// on a `tool_search_call`/`tool_search_output` is UNVERIFIED (§6.8; ruling D10
/// still in force). The house remedy for an empty id on the other carrier is the
/// send-time patch pass (`patch_reasoning_empty_ids`, `conversation/responses.rs`),
/// which has no discovery arm — adding one would rewrite a provider byte with no
/// wire evidence either way, so it stays unverified and recorded rather than fixed
/// blind.
#[test]
fn an_empty_provider_item_id_is_kept_in_the_bytes_and_never_advertised_as_a_handle() {
    let empty_id_call = HOSTED_CALL_ITEM.replace(
        "\"id\":\"tsc_0c1a30c585eeaaf9016ac01db8650081949aff9bff5698a20a\"",
        "\"id\":\"\"",
    );
    let empty_id_output = HOSTED_OUTPUT_ITEM.replace(
        "\"id\":\"tso_0c1a30c585eeaaf9016ac01db86afc81949af526e214666b827\"",
        "\"id\":\"\"",
    );
    let items = response_to_conversation_items(response_of(&[&empty_id_call, &empty_id_output]))
        .expect("an empty id is the provider's own byte, not a refusal condition");
    let carriers = discovery_carriers(&items);
    assert_eq!(carriers.len(), 2, "both halves still map: {items:?}");
    for carrier in &carriers {
        assert_eq!(
            carrier.id(),
            None,
            "an empty id identifies nothing, so it must not be handed out as a handle: {}",
            carrier.raw()
        );
        assert_eq!(
            carrier.raw().get("id"),
            Some(&serde_json::Value::String(String::new())),
            "the byte is retained — the store keeps it and the Codex arm splices it; nothing \
             was minted in its place: {}",
            carrier.raw()
        );
    }
}

/// Key paths present in the provider's item but absent from the spliced one, and the
/// reverse, walking objects and arrays. Value equality is deliberately not compared:
/// the question this pin answers is which KEYS the provider sees again.
fn key_path_diff(
    provider: &serde_json::Value,
    spliced: &serde_json::Value,
    path: &str,
    dropped: &mut Vec<String>,
    added: &mut Vec<String>,
) {
    match (provider, spliced) {
        (serde_json::Value::Object(provider), serde_json::Value::Object(spliced)) => {
            for (key, value) in provider {
                let here = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match spliced.get(key) {
                    Some(inner) => key_path_diff(value, inner, &here, dropped, added),
                    None => dropped.push(here),
                }
            }
            for key in spliced.keys() {
                if !provider.contains_key(key) {
                    added.push(if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    });
                }
            }
        }
        (serde_json::Value::Array(provider), serde_json::Value::Array(spliced)) => {
            for (index, (p, s)) in provider.iter().zip(spliced.iter()).enumerate() {
                key_path_diff(p, s, &format!("{path}[{index}]"), dropped, added);
            }
        }
        _ => {}
    }
}

/// The carrier's bytes are model-visible, not inert storage: `conversation.rs:2699`
/// splices `Discovery`'s `raw()` verbatim into the next request on the Codex dialect,
/// and all three rows the bake advertises on are `model_family: "codex"`
/// (`xai-grok-models/default_models.json:365`, `:420`, `:528`). The decode builds
/// those bytes by re-serializing the SDK's closed structs, so this drives the banked
/// hosted pair through the seam and diffs what the next turn would send against the
/// provider's OWN item JSON, at every depth.
///
/// Two rules follow from that diff and both are asserted per half: the splice adds NO
/// key the provider never sent — the case this pin exists for is `"call_id": null`,
/// which `ToolSearchCall::call_id` (`async-openai-0.42.1/src/types/responses/response.rs:151`)
/// and `ToolSearchOutput::call_id` (`:189`) re-emit for an omitted key and which
/// `discovery_carrier` removes — and every key it DROPS is named below. A third
/// normalization, in either direction, on either half, reddens this test instead of
/// reaching a wire nobody captured.
///
/// Both observed provider shapes run through it. The banked SSE frames omit every
/// optional key; the banked response bodies send them explicitly as null, so the two
/// cases lose DIFFERENT keys and each list is named. Key-set equality is what the
/// response-body case can prove; resolving present-null to absent is a deliberate
/// choice whose whole cost is the `call_id` line in its drop list.
///
/// What it cannot show is ACCEPTANCE. Key-set equality with the provider's own item is
/// a necessary condition, not a captured 200: this lane made no provider call, so
/// replaying a server-executed pair on the Codex dialect stays UNVERIFIED per row class
/// and §6.8 / ruling D10 stay open.
#[test]
fn the_codex_splice_replays_the_provider_item_key_set() {
    // `tools[0].output_schema` is the provider's `"output_schema": null` eaten by
    // `FunctionTool::output_schema`'s `skip_serializing_if` (same SDK file,
    // :1386-1387), `tools[0].allowed_callers` the same for `allowed_callers`
    // (`:1390`), `created_by` the same for `created_by` (`:159-160`, `:197-198`);
    // none can be restored here without inventing a key the provider may not have
    // sent, so each is named as a loss rather than silently shipped. `call_id` is
    // the one this crate removes by hand (`conversation/responses.rs:251-255`).
    // One case: the shape's label, its two provider item fixtures, and the key paths
    // the Codex splice loses from each half, in the provider's own key order.
    type SpliceCase = (
        &'static str,
        [&'static str; 2],
        [&'static [&'static str]; 2],
    );
    let cases: [SpliceCase; 2] = [
        (
            "stream frame",
            [HOSTED_CALL_ITEM, HOSTED_OUTPUT_ITEM],
            [&[], &["tools[0].output_schema"]],
        ),
        (
            "response body",
            [HOSTED_CALL_ITEM_BODY, HOSTED_OUTPUT_ITEM_BODY],
            [
                &["call_id", "created_by"],
                &[
                    "call_id",
                    "tools[0].allowed_callers",
                    "tools[0].output_schema",
                    "created_by",
                ],
            ],
        ),
    ];
    for (shape, fixtures, expected_drops) in cases {
        let provider_items: Vec<serde_json::Value> = fixtures
            .iter()
            .map(|raw| serde_json::from_str(raw).expect("fixture is the provider's own item JSON"))
            .collect();
        let items = response_to_conversation_items(response_of(&fixtures))
            .expect("the banked hosted pair decodes");
        let splices = ConversationRequest::from_items(items)
            .raw_responses_input_replacements(ResponsesReplayDialect::Codex);
        assert_eq!(
            splices.len(),
            provider_items.len(),
            "{shape}: one Codex splice per discovery half, and nothing else: {splices:?}"
        );

        for ((provider, splice), drops) in provider_items
            .iter()
            .zip(splices.iter())
            .zip(expected_drops)
        {
            let (mut missing, mut added) = (Vec::new(), Vec::new());
            key_path_diff(provider, &splice.value, "", &mut missing, &mut added);
            assert!(
                added.is_empty(),
                "the Codex splice would send keys the provider never sent for this {shape} item: \
                 {added:?} (provider item: {provider}; spliced: {})",
                splice.value
            );
            assert_eq!(
                missing, drops,
                "the Codex splice's dropped-key set moved for {shape} {} (spliced: {}); a new \
                 loss needs a ruling, not a widened list here",
                provider["type"], splice.value
            );
        }
    }
}

/// Ruling `map/RULINGS-o1o5.md` §D10 R4 (bead `apex-mrmq`, deliverable 3), held where
/// the campaign's test authority can actually see it: this file runs under
/// `cargo test --release -p xai-grok-sampling-types --lib`, which the §5 GATE does
/// execute, while `xai-grok-models` is not one of its eight packages — a bake check
/// the pre-commit authority never runs is prose. `xai-grok-models` carries the
/// companion pin (`no_baked_admitting_row_outlives_the_decode_seam`,
/// `xai-grok-models/src/lib.rs`) which reads the same invariant through the
/// `include_str!`-baked `DEFAULT_MODELS_JSON` const; the two are the same law read at
/// the two ends of the dependency edge. The row SET stays owned by
/// `catalog_flags_parse_and_default_off` (`xai-grok-models/src/lib.rs:178-180`); this
/// test owns only the coupling between that set and the decode seam.
///
/// It reads the rows from the same `default_models.json` the bake embeds via
/// `include_str!` (read from disk rather than through that crate because
/// `xai-grok-models` depends on this one, so the edge cannot run the other way),
/// reads the `execution` the admitted route ACTUALLY declares off
/// [`extra_tool_entries_for_route`] rather than assuming one, and drives both provider
/// answer shapes through the real [`response_to_conversation_items`]:
///
/// * UNCONDITIONALLY (whenever the admitting set is non-empty) the complete
///   server-executed pair. This is the mandated positive assertion — the items must
///   LAND — and it cannot be gated on what this head declares, because the decode seam
///   does not branch on the route: the pair shape is already live on these rows. The
///   banked live frames are a `"execution":"server"` `tool_search_call` at
///   `output_index` 1 and its `tool_search_output` at 2, from `"model":"gpt-5.6-sol"`
///   (`xai-grok-sampler/src/stream/responses.rs:2146`, `:2149`), and gpt-5.6-sol is one
///   of the three ON rows. A refusal that merely changed its text would pass a
///   probe this route's declaration gated.
/// * A `client`-declared route — what this head emits, ruling D5
///   (`admitted_route_declaration_is_client_executed`) — additionally gets a lone
///   call, because the answer half is the harness's to author and apex-waj.57 has not
///   landed. The seam refuses that, and that IS the D10 hazard, so each row carrying
///   it is named below with the bead that owes the fix. A new ON row that is not
///   named reddens this test: the curate breaks, not a user's turn. When the answer
///   arm lands the lone call starts mapping and the stale-pin branch reddens until the
///   list is emptied. This is a second, separately-attributed check, not an
///   alternative to the first.
#[test]
fn no_baked_admitting_row_drives_a_shape_the_decode_seam_refuses() {
    /// Rows that advertise hosted search while the harness cannot answer the call it
    /// invites. The second field is the bead that owes the answer half.
    const ROWS_ADMITTING_WITH_NO_ANSWER_HALF: &[(&str, &str)] = &[
        ("gpt-5.6-luna", "apex-waj.57"),
        ("gpt-5.6-sol", "apex-waj.57"),
        ("gpt-5.6-terra", "apex-waj.57"),
    ];
    /// The catalog source the bake embeds through
    /// `pub const DEFAULT_MODELS_JSON: &str = include_str!("../default_models.json")`
    /// (`xai-grok-models/src/lib.rs:20`); `build.rs` only declares the
    /// `cargo:rerun-if-changed=default_models.json` trigger (`build.rs:62`) and runs
    /// the unrelated param gate — it does not write the catalog. A sibling path, not a
    /// dependency: `xai-grok-models` depends on this crate
    /// (`xai-grok-models/Cargo.toml`), so no edge back exists.
    const BAKED_ROWS: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../xai-grok-models/default_models.json"
    );

    let bake = std::fs::read_to_string(BAKED_ROWS).unwrap_or_else(|error| {
        panic!("{BAKED_ROWS} is the catalog source the bake ships: {error}")
    });
    let read: serde_json::Value =
        serde_json::from_str(&bake).expect("the baked catalog source parses");
    let admitting: Vec<&str> = read["models"]
        .as_array()
        .expect("the catalog source has a models array")
        .iter()
        .filter(|row| row["supports_search_tool"] == serde_json::json!(true))
        // id-less rows fall back to the wire slug, as the models crate's own pin does,
        // so no row can leave the counted set uncounted.
        .map(|row| {
            row["id"]
                .as_str()
                .or_else(|| row["model"].as_str())
                .expect("a catalog row carries an id or a model slug")
        })
        .collect();

    // The positive assertion deliverable 3 mandates, run whenever there is a row to
    // protect — NOT gated on the route's declared `execution`. The seam maps this pair
    // for every row regardless of the declaration, so gating it on the declaration
    // would leave the arm that proves the item LANDS unreachable at this head, and a
    // decode reverted to any refusal would stay green while every ON row died
    // mid-search.
    if !admitting.is_empty() {
        let items = response_to_conversation_items(response_of(&[
            HOSTED_CALL_ITEM,
            HOSTED_OUTPUT_ITEM,
        ]))
        .unwrap_or_else(|error| {
            panic!(
                "the bake carries supports_search_tool: true on {admitting:?}, but the decode \
                 seam refused the discovery pair that shape answers with: {error}. Land the \
                 mapping, or apply D10 R3 and curate those rows off."
            )
        });
        let kinds: Vec<&str> = discovery_carriers(&items)
            .iter()
            .map(|carrier| carrier.kind().item_type())
            .collect();
        assert_eq!(
            kinds,
            ["tool_search_call", "tool_search_output"],
            "an admitting row must get both halves into the IR, or the turn dies \
             mid-search on {admitting:?}"
        );
    }

    let declaration = extra_tool_entries_for_route(&[], Some(ADMITTED_ROUTE))
        .first()
        .cloned()
        .expect("the admitted route leads the channel with the tool_search declaration");
    let declared = declaration["execution"]
        .as_str()
        .expect("the declaration names its execution")
        .to_string();

    // The second, separately-attributed check: the shape THIS route's declaration
    // actually invites. The seam does not branch on the slug, so one probe answers for
    // the whole set and the rows are enumerated only to attribute the pin.
    match declared.as_str() {
        // Nothing further to check: the provider mints both halves and the
        // unconditional probe above already proved they land.
        "server" => {}
        // The harness owes the answer, so the provider's answer is a lone call.
        "client" => {
            let probe = response_to_conversation_items(response_of(&[CLIENT_CALL_ITEM]));
            for row in admitting.iter().copied() {
                let error = match &probe {
                    Ok(_) => panic!(
                        "stale pin: the seam now maps the lone client-executed call that \
                         {row:?}'s route invites, so its entry in \
                         ROWS_ADMITTING_WITH_NO_ANSWER_HALF is false — remove it, and re-check \
                         whether ruling D10 can be re-ruled."
                    ),
                    Err(error) => error,
                };
                let msg = error.to_string();
                let owner = ROWS_ADMITTING_WITH_NO_ANSWER_HALF
                    .iter()
                    .find(|(pinned, _)| *pinned == row)
                    .map(|(_, owner)| *owner)
                    .unwrap_or_else(|| {
                        panic!(
                            "{row} advertises hosted search on a client-executed route and the \
                             decode seam refuses the answer that route returns ({msg}). D10 R4 \
                             makes that a user's turn, not a test failure: land the answer arm, \
                             or apply D10 R3 and curate the row off. Adding the row to \
                             ROWS_ADMITTING_WITH_NO_ANSWER_HALF is not a fix — that list records \
                             hazards the coordinator has already ruled on."
                        )
                    });
                assert!(
                    msg.contains("tool_search_output"),
                    "{row} (answer owed by {owner}): the refusal must name the missing half: {msg}"
                );
            }
        }
        other => panic!(
            "the admitted route declares execution {other:?}, which this test has no provider \
             answer shape for; add that shape deliberately rather than falling through"
        ),
    }

    for (pinned, owner) in ROWS_ADMITTING_WITH_NO_ANSWER_HALF {
        assert!(
            admitting.contains(pinned),
            "stale pin: {pinned:?} (answer owed by {owner}) is listed as an admitting row with \
             no answer half, but the bake no longer carries it ON — delete the entry so the list \
             keeps meaning what it says"
        );
    }
}

// ─── HTS-DEFERRED-LOWER / bead apex-waj.85: deferred-aware lowering ────────
//
// Plan Task 7 (`docs/superpowers/plans/2026-09-25-s3a-responses-native-tool-search.md`):
// on the admitted route, a tool whose `exposure` is `Deferred` rides the wire with
// `defer_loading: true` — the deviation the 2026-10-02T210154Z capture pinned as the
// one that makes the provider fire the search — and every other cell stays
// byte-identical to the pre-lowering output (plan scope rule: the un-admitted route
// does not learn that a tool was withheld).

/// One Immediate and one Deferred tool, with the route's admission set per cell.
fn deferred_lowering_request(admission: Option<SearchAdmission>) -> ConversationRequest {
    let mut req =
        ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_tools(vec![
            ToolSpec {
                name: "read_file".to_string(),
                description: None,
                parameters: serde_json::json!({"type": "object"}),
                exposure: ToolExposure::Immediate,
            },
            ToolSpec {
                name: "server__deploy".to_string(),
                description: Some("Deploy the server".to_string()),
                parameters: serde_json::json!({"type": "object"}),
                exposure: ToolExposure::Deferred,
            },
        ]);
    req.search_admission = admission;
    req
}

/// Acceptance 1, firing cell: a deferred tool on the admitted route rides the wire
/// WITHHELD — `defer_loading: true` on the entry itself, which the capture shows is
/// the deviation under which the provider mints the `tool_search_call`. The tool
/// stays in the array: the lowering withholds, it does not drop.
#[test]
fn admitted_route_deferred_tool_rides_defer_loading_true() {
    let req = deferred_lowering_request(Some(ADMITTED_ROUTE));
    let body: rs::CreateResponse = (&req).into();
    let wire = serde_json::to_value(&body.tools).expect("tools serialize");
    assert_eq!(wire[0]["name"], serde_json::json!("read_file"));
    assert_eq!(wire[1]["name"], serde_json::json!("server__deploy"));
    assert_eq!(
        wire[1]["defer_loading"],
        serde_json::json!(true),
        "a deferred tool on the admitted route must ride withheld: {wire:?}"
    );
    assert_eq!(
        json_keys(&wire[1]),
        ["type", "name", "parameters", "description", "defer_loading"],
        "the withheld entry keeps the function-tool key order with defer_loading last"
    );
}

/// Acceptance 1, non-firing cell: the Immediate tool beside a withheld one carries
/// no `defer_loading` key at all — `None` skips the key, so its bytes do not move.
#[test]
fn admitted_route_immediate_tool_carries_no_defer_loading() {
    let req = deferred_lowering_request(Some(ADMITTED_ROUTE));
    let body: rs::CreateResponse = (&req).into();
    let wire = serde_json::to_value(&body.tools).expect("tools serialize");
    assert!(
        wire[0].get("defer_loading").is_none(),
        "the Immediate tool is not withheld: {wire:?}"
    );
}

/// Plan scope rule: an un-admitted route does not act on `exposure`, so its lowered
/// tools are byte-identical to the pre-lowering output — the Deferred tool included.
#[test]
fn unadmitted_route_lowering_stays_byte_identical_to_today() {
    let req = deferred_lowering_request(None);
    let body: rs::CreateResponse = (&req).into();
    let wire = serde_json::to_string(&body.tools).expect("tools serialize");
    assert_eq!(
        wire,
        r#"[{"type":"function","name":"read_file","parameters":{"type":"object"}},{"type":"function","name":"server__deploy","parameters":{"type":"object"},"description":"Deploy the server"}]"#,
        "no admission: the withheld tool is only a concept the admitted route acts on"
    );
}

/// Admitted, but nothing is deferred: the route admits yet withholds nothing, so the
/// array is byte-identical to the same surface on the un-admitted route.
#[test]
fn admitted_route_without_deferred_tools_moves_no_byte() {
    let mut req =
        ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_tools(vec![
            ToolSpec {
                name: "read_file".to_string(),
                description: None,
                parameters: serde_json::json!({"type": "object"}),
                exposure: ToolExposure::default(),
            },
        ]);
    req.search_admission = Some(ADMITTED_ROUTE);
    let body: rs::CreateResponse = (&req).into();
    let wire = serde_json::to_string(&body.tools).expect("tools serialize");
    assert_eq!(
        wire, r#"[{"type":"function","name":"read_file","parameters":{"type":"object"}}]"#,
        "admission withholds nothing: an Immediate-only array must not move a byte"
    );
}
