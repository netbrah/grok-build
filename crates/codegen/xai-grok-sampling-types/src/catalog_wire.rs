//! Infer first-class catalog wire metadata when `/v1/models` omits it.
//!
//! LiteLLM-style catalogs (and many OpenAI-compatible proxies) emit `{id, object,
//! owned_by}` only. Without a local classification, every row collapses to
//! Chat Completions and OpenAI reasoning models never reach `/v1/responses`.
//!
//! The slug-only resolvers ([`catalog_family`], [`infer_api_backend`],
//! [`infer_reasoning_efforts`]) are **slug-keyed fallbacks**: they classify
//! from the model slug alone and cannot see the row's explicit fields. The
//! "explicit fields always win" contract is enforced at the config seam
//! (xai-grok-shell) by the row-aware resolvers below
//! ([`resolve_family`], [`resolve_api_backend`], [`resolve_reasoning_efforts`]),
//! which consult the row's explicit `model_family` / `api_backend` /
//! reasoning menu before any slug inference:
//! row field > inference > endpoint defaults > built-in defaults.

use crate::{ApiBackend, ReasoningEffort, ReasoningEffortOption};

/// Coarse provider family for a catalog slug. Used to pick a wire and to
/// keep xAI-only hosted tools (`x_search`) off OpenAI Responses routes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogFamily {
    Xai,
    OpenAi,
    Anthropic,
    Other,
}

impl CatalogFamily {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Xai => "xai",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Other => "other",
        }
    }
}

/// Classify a model id or catalog key. Matching is case-insensitive and uses
/// the slug, never the host URL: an OpenAI model on a corporate proxy is still
/// OpenAI on the Responses wire.
pub fn catalog_family(model_id: &str) -> CatalogFamily {
    let id = model_id.to_ascii_lowercase();
    if id.starts_with("grok") {
        CatalogFamily::Xai
    } else if id.starts_with("gpt-") || is_openai_o_series_slug(&id) {
        CatalogFamily::OpenAi
    } else if id.starts_with("claude") {
        CatalogFamily::Anthropic
    } else {
        CatalogFamily::Other
    }
}

/// OpenAI o-series reasoning slugs (`o1-preview`, `o3`, `o4-mini`, ...):
/// an `o` immediately followed by a digit.
fn is_openai_o_series_slug(id: &str) -> bool {
    id.starts_with('o') && id.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
}

/// Backend to use when the catalog omitted `api_backend`.
///
/// Grok and current OpenAI agent models run Responses. Claude stays on Chat
/// Completions until the native `/v1/messages` port lands. Unknown slugs keep
/// the historical Chat Completions default.
pub fn infer_api_backend(model_id: &str) -> ApiBackend {
    match catalog_family(model_id) {
        // Grok rows are Responses-native (baked catalog).
        CatalogFamily::Xai => ApiBackend::Responses,
        // gpt-5+ and the o-series are agent models; gpt-4.1/gpt-4o were the
        // last 4.x releases served on the Responses API. Earlier 3.x/4.x
        // chat models and everything else stay Chat Completions.
        CatalogFamily::OpenAi if openai_responses_slug(model_id) => ApiBackend::Responses,
        CatalogFamily::OpenAi | CatalogFamily::Anthropic | CatalogFamily::Other => {
            ApiBackend::ChatCompletions
        }
    }
}

fn openai_responses_slug(model_id: &str) -> bool {
    let id = model_id.to_ascii_lowercase();
    if is_openai_o_series_slug(&id) {
        return true;
    }
    let rest = id.strip_prefix("gpt-").unwrap_or("");
    matches!(rest, "4o") || rest.starts_with("4.1") || rest.starts_with(['5', '6', '7', '8', '9'])
}

/// Reasoning-effort menu for catalogs that omit one. Empty means "do not
/// advertise effort" (legacy chat models, embeddings, Claude until Messages).
pub fn infer_reasoning_efforts(model_id: &str) -> Vec<ReasoningEffortOption> {
    // (menu, default) per family. The OpenAI menu is only advertised on the
    // Responses wire; the Grok menu mirrors the baked grok-4.6 catalog row.
    let (menu, default): ([ReasoningEffort; 4], ReasoningEffort) = match catalog_family(model_id) {
        CatalogFamily::Xai => (
            [
                ReasoningEffort::Xhigh,
                ReasoningEffort::High,
                ReasoningEffort::Medium,
                ReasoningEffort::Low,
            ],
            ReasoningEffort::High,
        ),
        CatalogFamily::OpenAi if infer_api_backend(model_id) == ApiBackend::Responses => (
            [
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::Xhigh,
            ],
            ReasoningEffort::Medium,
        ),
        _ => return Vec::new(),
    };
    menu.into_iter()
        .map(|value| effort_option(value, value == default))
        .collect()
}

/// Map an explicit row `model_family` string to a family, case-insensitively.
/// `None` and unmappable strings mean "no explicit evidence" — the caller
/// falls back to slug inference.
fn parse_explicit_family(explicit: Option<&str>) -> Option<CatalogFamily> {
    let family = explicit?.to_ascii_lowercase();
    match family.as_str() {
        "xai" => Some(CatalogFamily::Xai),
        // "codex" is the provider-family string the sampler's Responses
        // dialect gate (`xai-grok-sampler::provider`) keys on; both spellings
        // denote the OpenAI GPT provider family in this stack.
        "codex" | "openai" => Some(CatalogFamily::OpenAi),
        "anthropic" => Some(CatalogFamily::Anthropic),
        _ => None,
    }
}

/// Resolve the provider family for a catalog row: the row's explicit
/// `model_family` wins when mappable; otherwise the slug-keyed fallback
/// [`catalog_family`] decides.
pub fn resolve_family(explicit_family: Option<&str>, model_id: &str) -> CatalogFamily {
    parse_explicit_family(explicit_family).unwrap_or_else(|| catalog_family(model_id))
}

/// Resolve the API backend for a catalog row: an explicit `api_backend` wins
/// outright; otherwise the slug-keyed fallback [`infer_api_backend`] decides.
pub fn resolve_api_backend(explicit_backend: Option<ApiBackend>, model_id: &str) -> ApiBackend {
    explicit_backend.unwrap_or_else(|| infer_api_backend(model_id))
}

/// Resolve the reasoning-effort menu for a catalog row: a non-empty explicit
/// menu wins; otherwise the slug-keyed fallback [`infer_reasoning_efforts`]
/// decides (empty when the slug advertises no menu).
pub fn resolve_reasoning_efforts(
    explicit_efforts: Option<&[ReasoningEffortOption]>,
    model_id: &str,
) -> Vec<ReasoningEffortOption> {
    match explicit_efforts {
        Some(efforts) if !efforts.is_empty() => efforts.to_vec(),
        _ => infer_reasoning_efforts(model_id),
    }
}

fn effort_option(value: ReasoningEffort, default: bool) -> ReasoningEffortOption {
    let id = value.as_str().to_string();
    let mut label = id.clone();
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    label.push_str(" Effort");
    ReasoningEffortOption {
        id,
        value,
        label,
        description: None,
        default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grok_and_openai_agent_models_use_responses() {
        for id in [
            "grok-4.6",
            "grok-4.5",
            "gpt-5",
            "gpt-5.4",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5-codex",
            "gpt-5.1-codex-max",
            "gpt-4.1",
            "gpt-4o",
            "o3",
            "o4-mini",
            "o1-preview",
        ] {
            assert_eq!(
                infer_api_backend(id),
                ApiBackend::Responses,
                "{id} should be first-class Responses"
            );
        }
    }

    #[test]
    fn legacy_openai_chat_and_non_agent_slugs_stay_on_chat_completions() {
        for id in [
            "gpt-4",
            "gpt-4-turbo",
            "gpt-3.5-turbo",
            "gpt-35-turbo-0301",
            "text-embedding-3-large",
            "gemini-3.5-flash",
            "glm-5.2",
            "qwen3.8-27b",
        ] {
            assert_eq!(
                infer_api_backend(id),
                ApiBackend::ChatCompletions,
                "{id} should remain Chat Completions"
            );
        }
    }

    #[test]
    fn claude_is_not_inferred_onto_messages() {
        for id in ["claude-sonnet-5", "claude-opus-4.6", "claude-haiku-4.5"] {
            assert_eq!(
                infer_api_backend(id),
                ApiBackend::ChatCompletions,
                "{id}: /v1/messages is pinned; do not infer it from the slug"
            );
            assert_eq!(catalog_family(id), CatalogFamily::Anthropic);
        }
    }

    #[test]
    fn family_is_slug_not_url() {
        assert_eq!(catalog_family("gpt-5.4"), CatalogFamily::OpenAi);
        assert_eq!(catalog_family("GPT-5.6-SOL"), CatalogFamily::OpenAi);
        assert_eq!(catalog_family("grok-4.6"), CatalogFamily::Xai);
        assert_eq!(catalog_family("Grok-4.5"), CatalogFamily::Xai);
        assert_eq!(catalog_family("claude-sonnet-5"), CatalogFamily::Anthropic);
        assert_eq!(catalog_family("gemini-3.5-flash"), CatalogFamily::Other);
    }

    #[test]
    fn openai_and_grok_get_reasoning_menus() {
        let openai = infer_reasoning_efforts("gpt-5.4");
        let values: Vec<_> = openai.iter().map(|o| o.value).collect();
        assert_eq!(
            values,
            [
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::Xhigh,
            ]
        );
        assert!(
            openai
                .iter()
                .any(|o| o.default && o.value == ReasoningEffort::Medium)
        );

        let grok = infer_reasoning_efforts("grok-4.6");
        assert!(grok.iter().any(|o| o.value == ReasoningEffort::Xhigh));
        assert!(
            grok.iter()
                .any(|o| o.default && o.value == ReasoningEffort::High)
        );

        assert!(infer_reasoning_efforts("gpt-3.5-turbo").is_empty());
        assert!(infer_reasoning_efforts("claude-sonnet-5").is_empty());
        assert!(infer_reasoning_efforts("text-embedding-3-small").is_empty());
    }

    /// The inferred Grok menu mirrors the baked `grok-4.6` catalog row
    /// (xai-grok-models/default_models.json) so live-hydrated Grok rows
    /// advertise the same tiers in the same order.
    #[test]
    fn grok_menu_mirrors_baked_grok_4_6_row() {
        let grok = infer_reasoning_efforts("grok-4.6");
        let values: Vec<_> = grok.iter().map(|o| o.value).collect();
        assert_eq!(
            values,
            [
                ReasoningEffort::Xhigh,
                ReasoningEffort::High,
                ReasoningEffort::Medium,
                ReasoningEffort::Low,
            ]
        );
        assert!(
            grok.iter()
                .any(|o| o.default && o.value == ReasoningEffort::High)
        );
    }
    // ==================== P2.0: row-aware resolvers ====================
    // The resolve_* trio consults the row's explicit field before any slug
    // inference; with the field absent (or empty/unmappable) the slug-keyed
    // fallback below still applies.

    /// Explicit `model_family` overrides a contradicting slug inference.
    #[test]
    fn resolve_family_explicit_wins_over_slug() {
        // Brief examples, case-insensitive mapping.
        assert_eq!(
            resolve_family(Some("xai"), "gpt-5.6-sol"),
            CatalogFamily::Xai,
            "explicit xai wins over the gpt slug"
        );
        assert_eq!(
            resolve_family(Some("codex"), "grok-4.6"),
            CatalogFamily::OpenAi,
            "codex is the OpenAI-family row string"
        );
        assert_eq!(
            resolve_family(Some("openai"), "grok-4.6"),
            CatalogFamily::OpenAi,
            "openai maps to the OpenAi family"
        );
        assert_eq!(
            resolve_family(Some("anthropic"), "gpt-5.6-sol"),
            CatalogFamily::Anthropic,
            "explicit anthropic wins over the gpt slug"
        );
        assert_eq!(
            resolve_family(Some("XAI"), "gpt-5.6-sol"),
            CatalogFamily::Xai
        );
        assert_eq!(
            resolve_family(Some("Codex"), "grok-4.6"),
            CatalogFamily::OpenAi
        );
    }

    /// No explicit family — or an unmappable one — falls back to the slug.
    #[test]
    fn resolve_family_absent_or_unmappable_uses_slug() {
        assert_eq!(resolve_family(None, "grok-4.6"), CatalogFamily::Xai);
        assert_eq!(resolve_family(None, "gpt-5.6-sol"), CatalogFamily::OpenAi);
        assert_eq!(
            resolve_family(None, "claude-sonnet-5"),
            CatalogFamily::Anthropic
        );
        assert_eq!(
            resolve_family(Some("glm"), "claude-sonnet-5"),
            CatalogFamily::Anthropic,
            "an unmappable explicit family defers to the slug"
        );
        assert_eq!(
            resolve_family(Some("xai"), "gemini-3.5-flash"),
            CatalogFamily::Xai,
            "a mappable explicit family wins over an Other slug"
        );
    }

    /// Explicit `api_backend` overrides a contradicting slug inference.
    #[test]
    fn resolve_api_backend_explicit_wins_over_slug() {
        assert_eq!(
            resolve_api_backend(Some(ApiBackend::ChatCompletions), "grok-4.6"),
            ApiBackend::ChatCompletions,
            "explicit chat completions wins over the grok slug"
        );
        assert_eq!(
            resolve_api_backend(Some(ApiBackend::Messages), "gpt-5.6-sol"),
            ApiBackend::Messages,
            "explicit messages wins over the gpt slug"
        );
    }

    /// No explicit backend — the slug-keyed fallback still applies.
    #[test]
    fn resolve_api_backend_absent_uses_slug() {
        assert_eq!(resolve_api_backend(None, "grok-4.6"), ApiBackend::Responses);
        assert_eq!(
            resolve_api_backend(None, "gpt-5.6-sol"),
            ApiBackend::Responses
        );
        assert_eq!(
            resolve_api_backend(None, "gpt-4"),
            ApiBackend::ChatCompletions
        );
        assert_eq!(
            resolve_api_backend(None, "claude-sonnet-5"),
            ApiBackend::ChatCompletions
        );
    }

    /// A non-empty explicit menu overrides a contradicting slug inference.
    #[test]
    fn resolve_reasoning_efforts_explicit_wins_over_slug() {
        let low = vec![effort_option(ReasoningEffort::Low, true)];
        assert_eq!(
            resolve_reasoning_efforts(Some(&low), "grok-4.6"),
            low,
            "explicit [Low] wins over the grok menu"
        );
        let single = vec![effort_option(ReasoningEffort::High, false)];
        assert_eq!(
            resolve_reasoning_efforts(Some(&single), "gpt-5.4"),
            single,
            "explicit [High] wins over the openai menu"
        );
    }

    /// No explicit menu (or an empty one) — the slug-keyed fallback applies.
    #[test]
    fn resolve_reasoning_efforts_absent_or_empty_uses_slug() {
        assert_eq!(
            resolve_reasoning_efforts(None, "grok-4.6"),
            infer_reasoning_efforts("grok-4.6")
        );
        assert_eq!(
            resolve_reasoning_efforts(None, "gpt-5.4"),
            infer_reasoning_efforts("gpt-5.4")
        );
        assert_eq!(
            resolve_reasoning_efforts(Some(&[]), "gpt-5.4"),
            infer_reasoning_efforts("gpt-5.4"),
            "an empty explicit menu is not a menu"
        );
        assert!(
            resolve_reasoning_efforts(None, "claude-sonnet-5").is_empty(),
            "claude slugs advertise no menu until Messages lands"
        );
    }
}
