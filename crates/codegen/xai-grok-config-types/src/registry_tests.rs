use super::*;
use crate::flags::ConfigSource;
use std::collections::BTreeMap;
use strum::IntoEnumIterator;

#[test]
fn every_feature_has_one_row_with_its_own_spellings() {
    for feature in Feature::iter() {
        assert_eq!(
            FEATURES.iter().filter(|spec| spec.id == feature).count(),
            1,
            "{feature:?} needs exactly one row in FEATURES",
        );
    }
    assert_eq!(
        Feature::iter().count(),
        FEATURES.len(),
        "the enum and FEATURES disagree: a variant has no row, or a row has no variant",
    );

    for spec in FEATURES {
        assert_eq!(spec.path, format!("features.{}", spec.key));
        assert_eq!(
            FEATURES.iter().filter(|s| s.key == spec.key).count(),
            1,
            "{} is on two rows",
            spec.key,
        );
        assert_eq!(
            FEATURES.iter().filter(|s| s.env == spec.env).count(),
            1,
            "{} is read by two features",
            spec.env,
        );
    }
}

/// Product decisions, stated here rather than read back out of the table.
#[test]
fn registered_settings() {
    let rows: BTreeMap<_, _> = FEATURES
        .iter()
        .map(|spec| (spec.key, (spec.env, spec.default_enabled)))
        .collect();
    assert_eq!(
        rows,
        BTreeMap::from([
            ("session_search", ("GROK_SESSION_SEARCH", true)),
            ("lsp_tools", ("GROK_LSP_TOOLS", false)),
            ("web_fetch", ("GROK_WEB_FETCH", false)),
            ("session_recap", ("GROK_SESSION_RECAP", true)),
            ("ask_user_question", ("GROK_ASK_USER_QUESTION", true)),
            ("voice_mode", ("GROK_VOICE_MODE", true)),
            ("write_file", ("GROK_WRITE_FILE", true)),
            ("feedback", ("GROK_FEEDBACK_ENABLED", true)),
            ("feedback_trace_card", ("GROK_FEEDBACK_TRACE_CARD", false)),
            ("turn_summary", ("GROK_TURN_SUMMARY", true)),
            ("cancel_rewind", ("GROK_CANCEL_REWIND", true)),
            (
                "compaction_verbatim_input",
                ("GROK_COMPACTION_VERBATIM_INPUT", true),
            ),
            ("two_pass_compaction", ("GROK_TWO_PASS_COMPACTION", true)),
            ("remote_compaction_v2", ("GROK_REMOTE_COMPACTION_V2", true)),
            ("backend_tools", ("GROK_BACKEND_SEARCH", true)),
            ("auto_wake", ("GROK_AUTO_WAKE", true)),
            (
                "subagent_worktree_snapshot",
                ("GROK_SUBAGENT_WORKTREE_SNAPSHOT", false),
            ),
            (
                "active_agent_messages",
                ("GROK_ACTIVE_AGENT_MESSAGES", false),
            ),
            (
                "repo_status_in_system_prompt",
                ("GROK_REPO_STATUS_IN_SYSTEM_PROMPT", true),
            ),
            ("dock", ("GROK_DOCK", false)),
            ("terminal_theme", ("GROK_TERMINAL_THEME", false)),
            ("multi_agent_v2", ("GROK_MULTI_AGENT_V2", false)),
        ]),
    );
}

/// A row wired to a neighbour's field type-checks, so each case sets one field and a wrong projection reads nothing.
#[test]
fn every_registered_feature_reads_its_own_remote_setting() {
    for spec in FEATURES {
        let value = !spec.default_enabled;
        let mut settings = RemoteSettings::default();
        match spec.id {
            Feature::SessionSearch => settings.session_search = Some(value),
            Feature::LspTools => settings.lsp_tools_enabled = Some(value),
            Feature::WebFetch => settings.web_fetch_enabled = Some(value),
            Feature::SessionRecap => settings.session_recap = Some(value),
            Feature::AskUserQuestion => settings.ask_user_question_enabled = Some(value),
            Feature::VoiceMode => settings.voice_mode_enabled = Some(value),
            Feature::WriteFile => settings.write_file_enabled = Some(value),
            Feature::Feedback => settings.feedback_enabled = Some(value),
            Feature::FeedbackTraceCard => settings.feedback_trace_card_enabled = Some(value),
            Feature::TurnSummary => settings.turn_summary = Some(value),
            Feature::CancelRewind => settings.cancel_rewind_enabled = Some(value),
            Feature::CompactionVerbatimInput => settings.compaction_verbatim_input = Some(value),
            Feature::TwoPassCompaction => settings.two_pass_compaction_enabled = Some(value),
            // No remote tier: pin / env / config over the default, mirroring the OG resolver
            // (open-grok@240c99c9 crates/codegen/xai-grok-shell/src/agent/config.rs
            // `resolve_remote_compaction_v2`).
            Feature::RemoteCompactionV2 => {
                assert!(spec.remote.is_none(), "{} grew a remote tier", spec.key);
                continue;
            }
            Feature::AutoWake => settings.auto_wake_enabled = Some(value),
            Feature::SubagentWorktreeSnapshot => {
                settings.subagent_worktree_snapshot_enabled = Some(value)
            }
            Feature::ActiveAgentMessages => settings.active_agent_messages_enabled = Some(value),
            Feature::RepoStatusInSystemPrompt => {
                settings.repo_status_in_system_prompt = Some(value)
            }
            Feature::Dock => settings.dock_enabled = Some(value),
            Feature::TerminalTheme => settings.terminal_theme_enabled = Some(value),
            // No remote tier: the proxy catalog carries no capability field;
            // the per-model row lives on the model entry (MA-3, spec Q1.1/Q1.2).
            Feature::MultiAgentV2 => {
                assert!(spec.remote.is_none(), "{} grew a remote tier", spec.key);
                continue;
            }
            // The one row with no remote tier, stated as such rather than as a projection that reads nothing
            Feature::BackendTools => {
                assert!(spec.remote.is_none(), "{} grew a remote tier", spec.key);
                continue;
            }
        }

        let resolved = spec.id.resolve(FeatureSources {
            remote: spec.id.remote_value(Some(&settings)),
            ..Default::default()
        });
        assert_eq!(
            resolved.source,
            ConfigSource::Remote,
            "{} reads no remote",
            spec.key
        );
        assert_eq!(
            resolved.value, value,
            "{} reads another feature's setting",
            spec.key,
        );
    }
}

#[test]
fn pin_outranks_env_outranks_config_outranks_remote_outranks_default() {
    let pinned = Feature::SessionSearch.resolve(FeatureSources {
        pin: Some(true),
        env: Some(false),
        config: Some(false),
        remote: Some(false),
    });
    assert!(pinned.value);
    assert_eq!(pinned.source, ConfigSource::Requirement);

    let from_env = Feature::SessionSearch.resolve(FeatureSources {
        env: Some(true),
        config: Some(false),
        remote: Some(false),
        ..Default::default()
    });
    assert!(from_env.value);
    assert_eq!(from_env.source, ConfigSource::Env);

    let configured = Feature::SessionSearch.resolve(FeatureSources {
        config: Some(true),
        remote: Some(false),
        ..Default::default()
    });
    assert!(configured.value);
    assert_eq!(configured.source, ConfigSource::Config);

    let remote = Feature::SessionSearch.resolve(FeatureSources {
        remote: Some(false),
        ..Default::default()
    });
    assert!(!remote.value);
    assert_eq!(remote.source, ConfigSource::Remote);

    let fallback = Feature::SessionSearch.resolve(FeatureSources::default());
    assert!(fallback.value);
    assert_eq!(fallback.source, ConfigSource::Default);
}

/// Provenance: open-grok@240c99c9 crates/codegen/xai-grok-shell/src/agent/config.rs:11366 :: resolve_remote_compaction_v2_precedence (adapted: the worktree resolves every registry row through the one shared `Feature::resolve` ladder, so the ported assertions run against `FeatureSources` instead of a per-key resolver and process env; the OG `OPENGROK_REMOTE_COMPACTION_V2` env is spelled `GROK_REMOTE_COMPACTION_V2` in the registry row)
#[test]
fn remote_compaction_v2_precedence() {
    let resolved = Feature::RemoteCompactionV2.resolve(FeatureSources::default());
    assert!(resolved.value, "current Codex compaction is default-on");
    assert_eq!(resolved.source, ConfigSource::Default);

    let config_off = Feature::RemoteCompactionV2.resolve(FeatureSources {
        config: Some(false),
        ..Default::default()
    });
    assert!(!config_off.value);
    assert_eq!(config_off.source, ConfigSource::Config);

    let env_wins = Feature::RemoteCompactionV2.resolve(FeatureSources {
        env: Some(true),
        config: Some(false),
        ..Default::default()
    });
    assert!(env_wins.value, "env override wins over config");
    assert_eq!(env_wins.source, ConfigSource::Env);
}

/// MA-3 (ledger L559-562): the v2 multi-agent gate ships dark. The spec
/// (item9-v2-multiagent-spec.md Q1.1, G2) requires the precedence test to
/// assert the ladder direction: pin beats env.
#[test]
fn multi_agent_v2_precedence() {
    let resolved = Feature::MultiAgentV2.resolve(FeatureSources::default());
    assert!(!resolved.value, "v2 multi-agent ships dark");
    assert_eq!(resolved.source, ConfigSource::Default);

    let pin_wins = Feature::MultiAgentV2.resolve(FeatureSources {
        pin: Some(true),
        env: Some(false),
        ..Default::default()
    });
    assert!(pin_wins.value, "pin beats env");
    assert_eq!(pin_wins.source, ConfigSource::Requirement);

    let env_wins = Feature::MultiAgentV2.resolve(FeatureSources {
        env: Some(true),
        config: Some(false),
        ..Default::default()
    });
    assert!(env_wins.value, "env beats config");
    assert_eq!(env_wins.source, ConfigSource::Env);

    let config_on = Feature::MultiAgentV2.resolve(FeatureSources {
        config: Some(true),
        ..Default::default()
    });
    assert!(config_on.value);
    assert_eq!(config_on.source, ConfigSource::Config);

    // The kill-switch direction: env off beats config on.
    let env_off = Feature::MultiAgentV2.resolve(FeatureSources {
        env: Some(false),
        config: Some(true),
        ..Default::default()
    });
    assert!(!env_off.value);
    assert_eq!(env_off.source, ConfigSource::Env);
}
#[test]
fn off_reason_names_the_setting_that_turned_it_off() {
    let on = Feature::SessionSearch.off_reason(FeatureSources {
        env: Some(true),
        ..Default::default()
    });
    assert_eq!(on, None);

    for (sources, reason) in [
        (
            FeatureSources {
                pin: Some(false),
                ..Default::default()
            },
            "a requirements.toml pin or an MDM policy",
        ),
        (
            FeatureSources {
                env: Some(false),
                ..Default::default()
            },
            "the GROK_SESSION_SEARCH environment variable",
        ),
        (
            FeatureSources {
                config: Some(false),
                ..Default::default()
            },
            "the session_search key in config.toml or managed_config.toml",
        ),
        (
            FeatureSources {
                remote: Some(false),
                ..Default::default()
            },
            "a remote setting",
        ),
    ] {
        assert_eq!(
            Feature::SessionSearch.off_reason(sources).as_deref(),
            Some(reason),
        );
    }

    // `lsp_tools` is the default-off row, so nothing has to set it.
    assert_eq!(
        Feature::LspTools
            .off_reason(FeatureSources::default())
            .as_deref(),
        Some("the default"),
    );
}
