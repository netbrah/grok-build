//! First-party API-key environment primitives.
//!
//! Only the env-key checks the auth subsystem itself needs live here. The ACP
//! `auth_methods` list-building surface (`build_auth_methods`,
//! `AuthMethodsBuildInputs`, `should_advertise_xai_api_key`, ...) stays in
//! `xai_grok_shell::agent::auth_method`, which depends on shell's `ModelEntry`.

/// Env var that, when set, advertises `xai.api_key` as a viable auth method.
///
/// Kept as a constant so test code and the production check stay in sync.
pub const XAI_API_KEY_ENV_VAR: &str = "XAI_API_KEY";

/// Legacy env var name.
/// Checked as a fallback when `XAI_API_KEY` is not set, so existing deployments that use the old name keep working.
pub const LEGACY_XAI_API_KEY_ENV_VAR: &str = "GROK_CODE_XAI_API_KEY";

/// APEX zero-config deploy env var (ZC-APEX-FEATURE-1, apex-ayl.127):
/// checked LAST in the `read_xai_api_key_env` chain — the no-config
/// fall-through for APEX deploy builds that ship `APEX_LLM_PROXY_KEY` and
/// nothing else. Unconditional (not feature-gated): it is part of the
/// xai-native credential surface.
pub const APEX_LLM_PROXY_KEY_ENV_VAR: &str = "APEX_LLM_PROXY_KEY";

/// Read the API key from the environment.
///
/// Checks `XAI_API_KEY` first, then falls back to the legacy
/// `GROK_CODE_XAI_API_KEY` for backward compatibility, then to the APEX
/// zero-config deploy key `APEX_LLM_PROXY_KEY` (ZC-APEX-FEATURE-1,
/// apex-ayl.127) — the no-config fall-through.
pub fn read_xai_api_key_env() -> Result<String, std::env::VarError> {
    std::env::var(XAI_API_KEY_ENV_VAR)
        .or_else(|_| std::env::var(LEGACY_XAI_API_KEY_ENV_VAR))
        .or_else(|_| std::env::var(APEX_LLM_PROXY_KEY_ENV_VAR))
}

/// Returns `true` if any of `XAI_API_KEY`, `GROK_CODE_XAI_API_KEY`, or
/// `APEX_LLM_PROXY_KEY` is set.
pub fn has_xai_api_key_env() -> bool {
    read_xai_api_key_env().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use xai_grok_test_support::EnvGuard;

    fn clear_key_env() -> Vec<EnvGuard> {
        [
            XAI_API_KEY_ENV_VAR,
            LEGACY_XAI_API_KEY_ENV_VAR,
            APEX_LLM_PROXY_KEY_ENV_VAR,
        ]
        .iter()
        .map(|key| EnvGuard::unset(key))
        .collect()
    }

    /// ZC-APEX-FEATURE-1 (apex-ayl.127): the no-config fall-through arm —
    /// with only `APEX_LLM_PROXY_KEY` set (no `XAI_API_KEY`, no
    /// `GROK_CODE_XAI_API_KEY`), the chain resolves it.
    #[test]
    #[serial]
    fn read_xai_api_key_env_falls_through_to_apex_llm_proxy_key() {
        let _guards = clear_key_env();
        let apex = EnvGuard::set(APEX_LLM_PROXY_KEY_ENV_VAR, "apex-key");
        assert_eq!(read_xai_api_key_env().as_deref(), Ok("apex-key"));
        drop(apex);
        assert!(read_xai_api_key_env().is_err());
    }

    /// ZC-APEX-FEATURE-1 (apex-ayl.127): legacy priority — `XAI_API_KEY`
    /// beats `APEX_LLM_PROXY_KEY`, and `GROK_CODE_XAI_API_KEY` beats
    /// `APEX_LLM_PROXY_KEY`.
    #[test]
    #[serial]
    fn read_xai_api_key_env_keeps_legacy_priority_over_apex() {
        let _guards = clear_key_env();
        let apex = EnvGuard::set(APEX_LLM_PROXY_KEY_ENV_VAR, "apex-key");
        let xai = EnvGuard::set(XAI_API_KEY_ENV_VAR, "xai-key");
        assert_eq!(read_xai_api_key_env().as_deref(), Ok("xai-key"));
        drop(xai);
        let legacy = EnvGuard::set(LEGACY_XAI_API_KEY_ENV_VAR, "legacy-key");
        assert_eq!(read_xai_api_key_env().as_deref(), Ok("legacy-key"));
        drop(legacy);
        assert_eq!(
            read_xai_api_key_env().as_deref(),
            Ok("apex-key"),
            "with only the APEX key left, arm 3 must resolve it"
        );
        drop(apex);
    }
}
