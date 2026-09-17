//! Shared reasoning-effort dropdown levels for `/model` and `/effort`.

use xai_grok_shell::sampling::types::{ReasoningEffort, ReasoningEffortOption};

use crate::slash::command::ArgItem;

/// Effort levels in the built-in fallback menu (strongest first).
/// `none`/`minimal` are still accepted by `ReasoningEffort::from_str` for power users.
pub(crate) const EFFORT_LEVELS: &[ReasoningEffort] =
    xai_grok_shell::sampling::types::LEGACY_REASONING_EFFORTS;

pub(crate) fn effort_description(level: ReasoningEffort) -> &'static str {
    match level {
        ReasoningEffort::None => "No reasoning",
        ReasoningEffort::Minimal => "Minimal reasoning",
        ReasoningEffort::Low => "Faster, lighter reasoning",
        ReasoningEffort::Medium => "Balanced reasoning",
        ReasoningEffort::High => "Heavy reasoning",
        ReasoningEffort::Xhigh => "Extended reasoning",
        ReasoningEffort::Max => "Maximum reasoning",
        ReasoningEffort::Ultra => "Ultra reasoning with automatic delegation",
    }
}

/// The built-in menu used when the server sends no `reasoningEfforts`.
/// Reproduces the historical rows: labels are the lowercase level (via `Display`), descriptions from `effort_description`.
/// The active row is matched by value against the session effort at render time, so `default` is left unset here.
pub(crate) fn legacy_effort_options() -> Vec<ReasoningEffortOption> {
    EFFORT_LEVELS
        .iter()
        .map(|&level| ReasoningEffortOption {
            id: level.as_ref().to_string(),
            value: level,
            label: level.to_string(),
            description: Some(effort_description(level).to_string()),
            default: false,
        })
        .collect()
}

/// Build effort rows for autocomplete from a per-model option list. `match_text` gets an `a `/`b `/…` sort prefix
/// so the matcher's alphabetical tiebreak preserves the option order.
pub(crate) fn build_effort_arg_items(
    options: &[ReasoningEffortOption],
    current_effort: Option<ReasoningEffort>,
    mark_active: bool,
    insert_text_for: impl Fn(&ReasoningEffortOption) -> String,
) -> Vec<ArgItem> {
    options
        .iter()
        .enumerate()
        .map(|(idx, option)| {
            let active = mark_active && current_effort == Some(option.value);
            let active_suffix = if active { " (active)" } else { "" };
            let insert_text = insert_text_for(option);
            // Sort-key prefix: 'a' for top row, 'b' for next, etc
            // Only affects matcher tiebreak ordering, never rendered
            let sort_prefix = char::from(b'a' + idx as u8);
            ArgItem {
                display: format!("{}{active_suffix}", option.label),
                match_text: format!("{sort_prefix} {insert_text}"),
                insert_text,
                description: option.description.clone().unwrap_or_default(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// EFFORT-SEAM-1 (apex-ayl.59) test 6 (pager half): the built-in menu consumes the shared
    /// `LEGACY_REASONING_EFFORTS` const in order — single owner, no private copy (B3).
    #[test]
    fn legacy_menu_consumes_shared_const_in_order() {
        let const_levels: &[ReasoningEffort] =
            xai_grok_shell::sampling::types::LEGACY_REASONING_EFFORTS;
        assert_eq!(EFFORT_LEVELS, const_levels);
        let options = legacy_effort_options();
        assert_eq!(options.len(), const_levels.len());
        for (option, &level) in options.iter().zip(const_levels.iter()) {
            assert_eq!(option.value, level);
            assert_eq!(option.id, level.as_ref().to_string());
            assert!(option.description.is_some());
            assert!(!option.default);
        }
    }
}
