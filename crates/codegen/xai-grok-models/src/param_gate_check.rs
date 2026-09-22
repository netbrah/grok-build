// P1 param-gate shared logic (apex-ayl.130 ZC-PARAMSCHEMA-GATE-1).
//
// Compiled into two contexts:
//   - `build.rs` — the compile-time gate (param drift = build failure);
//   - the `#[cfg(test)]` selftest — runs the same logic against real and
//     mutated schemas and asserts the expected failures.
//
// Depends on `std` + `serde_json` only, so both contexts compile it.
//
// Source of truth: `crates/codegen/xai-grok-shell/config.schema.json`
// (the sibling Python gate, apex-ayl.128, reads the same schema).

use serde_json::Value;

/// The struct whose fields the gate pins against the schema.
pub const STRUCT_NAME: &str = "DefaultModelEntry";

/// Struct fields allowed WITHOUT a `ConfigModelOverride` schema definition:
/// - `id`: row identity — config rows are keyed by the `[model.<id>]` table
///   name, so the schema has no `id` property;
/// - `max_input_tokens` / `max_output_tokens`: generated cap aliases (the
///   proxy's `/model_group/info` naming, see the `DefaultModelEntry` doc) —
///   the schema intentionally defines no aliases.
pub const STRUCT_ONLY_ALLOWED: &[&str] = &["id", "max_input_tokens", "max_output_tokens"];

/// `ConfigModelOverride` schema properties intentionally NOT modeled by
/// `DefaultModelEntry` (the config-only surface: credentials are forbidden
/// on baked rows, transport/sampling fields are resolver-level config).
///
/// Maintenance contract (fail-closed drift, apex-ayl.126): when a new
/// `[model.<id>]` field lands in `ConfigModelOverride`, either model it in
/// `DefaultModelEntry` (if the catalog row should carry it) or classify it
/// here with a reason. Unclassified additions fail the build.
pub const SCHEMA_ONLY_ALLOWED: &[&str] = &[
    // Credential / identity — never baked (Python gate forbids in overlays).
    "api_key",
    "auth_provider",
    "env_key",
    "mtls_cert_dir",
    // Transport / routing — session-level config, not row curation.
    "agent_type",
    "api_base_url",
    "base_url",
    "env_http_headers",
    "model_provider",
    "query_params",
    // Sampling / session behavior — config.toml resolver level.
    "disable_parallel_tool_use",
    "stop_sequences",
    "stream_tool_calls",
    "temperature",
    // Messages-wire replay policy (MSGW-THINKREPLAY-1, apex-ayl.108.1) — per-user
    // closed-set knob {None, "off"}; no baked per-model default (SDD D2: no
    // model-specific behavior in the harness; the row key resolves in config).
    "thinking_replay",
    "tools_cache_breakpoint",
    "top_k",
    "top_p",
    "use_concise",
    // Retry / limit tuning.
    "inference_idle_timeout_secs",
    "max_retries",
    "rate_limit_retry_threshold",
    "subagent_rate_limit_max_attempts",
    // Display / visibility.
    "hidden",
    "show_model_fingerprint",
    "supported_in_api",
    // F1 server tools / MCP (apex-ayl.115) — config-only surface.
    "mcp_servers",
    "mcp_toolset_server",
    "server_tools",
];

/// `struct ∩ schema` members excluded from the curation-required set:
/// - `model`: row identity (the config table name);
/// - `description`: free-form prose, explicitly optional in curation;
/// - `context_window` / `max_completion_tokens`: C-class generated caps —
///   the proxy is truth (the Python gate forbids them on on-proxy overlay
///   rows, `FORBIDDEN_OVERLAY_FIELDS`).
pub const CATALOG_REQUIRED_EXCLUDES: &[&str] =
    &["model", "description", "context_window", "max_completion_tokens"];

/// Outcome of a gate run: every drift error (empty = pass) + the derived
/// constants (single source of truth = the schema, no hardcoded copies).
pub struct GateReport {
    pub errors: Vec<String>,
    /// `DefaultModelEntry` fields, declaration order.
    pub struct_fields: Vec<String>,
    /// `ConfigModelOverride` properties, sorted.
    pub schema_properties: Vec<String>,
    /// Curation-required fields = (struct ∩ schema) −
    /// `CATALOG_REQUIRED_EXCLUDES`, struct declaration order.
    pub required_fields: Vec<String>,
    /// Canonical effort values = `#/definitions/ReasoningEffort.enum`,
    /// schema order.
    pub effort_values: Vec<String>,
}

/// `ConfigModelOverride` property names (sorted), or a schema-shape error.
pub fn schema_override_properties(schema_text: &str) -> Result<Vec<String>, String> {
    let schema: Value = serde_json::from_str(schema_text)
        .map_err(|e| format!("config.schema.json: invalid JSON: {e}"))?;
    let props = schema
        .get("definitions")
        .and_then(|d| d.get("ConfigModelOverride"))
        .and_then(|c| c.get("properties"))
        .and_then(|p| p.as_object())
        .ok_or_else(|| {
            "config.schema.json: missing #/definitions/ConfigModelOverride/properties".to_string()
        })?;
    let mut names: Vec<String> = props.keys().cloned().collect();
    names.sort();
    Ok(names)
}

/// `#/definitions/ReasoningEffort.enum` values, schema order.
pub fn schema_effort_values(schema_text: &str) -> Result<Vec<String>, String> {
    let schema: Value = serde_json::from_str(schema_text)
        .map_err(|e| format!("config.schema.json: invalid JSON: {e}"))?;
    let entries = schema
        .get("definitions")
        .and_then(|d| d.get("ReasoningEffort"))
        .and_then(|r| r.get("enum"))
        .and_then(|e| e.as_array())
        .ok_or_else(|| "config.schema.json: missing #/definitions/ReasoningEffort/enum".to_string())?;
    entries
        .iter()
        .map(|v| v.as_str().map(str::to_string))
        .collect::<Option<Vec<String>>>()
        .ok_or_else(|| "config.schema.json: ReasoningEffort.enum entries must be strings".to_string())
}

/// `DefaultModelEntry` field names from `src/lib.rs` source, declaration
/// order. Strict and fail-loud: a missing struct or an unparseable body
/// line is an error — the gate must never silently pass.
pub fn parse_struct_fields(lib_text: &str) -> Result<Vec<String>, String> {
    let mut fields: Vec<String> = Vec::new();
    let mut in_struct = false;
    let mut depth = 0usize;
    for line in lib_text.lines() {
        let trimmed = line.trim();
        if !in_struct {
            if trimmed.starts_with(&format!("pub struct {STRUCT_NAME}"))
                && trimmed.ends_with('{')
            {
                in_struct = true;
                depth = 1;
            }
            continue;
        }
        for ch in trimmed.chars() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(fields);
                    }
                }
                _ => {}
            }
        }
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("///")
            || trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with('(')
            || trimmed.starts_with(')')
        {
            continue;
        }
        let rest = trimmed
            .strip_prefix("pub")
            .ok_or_else(|| format!("param-gate: unexpected line in {STRUCT_NAME}: {trimmed}"))?;
        let rest = rest.trim_start();
        let rest = rest.strip_prefix("(crate)").unwrap_or(rest);
        let rest = rest.trim_start();
        let ident = rest
            .split(|c: char| c == ':' || c.is_whitespace())
            .next()
            .unwrap_or_default();
        let first = ident.chars().next();
        let valid = first.is_some_and(|c| c.is_ascii_lowercase() || c == '_')
            && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(format!("param-gate: unexpected line in {STRUCT_NAME}: {trimmed}"));
        }
        fields.push(ident.to_string());
    }
    Err(format!(
        "param-gate: {STRUCT_NAME} struct not found (or unterminated) in src/lib.rs — the gate cannot verify drift"
    ))
}

/// Distinct keys across `default_models.json` `models[*]` rows, sorted.
pub fn baked_row_keys(baked_text: &str) -> Result<Vec<String>, String> {
    let baked: Value =
        serde_json::from_str(baked_text).map_err(|e| format!("default_models.json: invalid JSON: {e}"))?;
    let models = baked
        .get("models")
        .and_then(|m| m.as_array())
        .ok_or_else(|| "default_models.json: missing 'models' array".to_string())?;
    let mut keys = std::collections::BTreeSet::new();
    for row in models {
        if let Some(obj) = row.as_object() {
            for key in obj.keys() {
                keys.insert(key.clone());
            }
        }
    }
    Ok(keys.into_iter().collect())
}

/// Run the full P1 gate. Empty `errors` = pass. All drift is reported, not
/// just the first hit (both directions, per the contract).
pub fn run_gate(schema_text: &str, lib_text: &str, baked_text: &str) -> GateReport {
    let mut errors: Vec<String> = Vec::new();

    // Record the shape error and continue with an empty set: the recorded
    // error already fails the gate, and empty sets add no false positives.
    fn collect<T: Default>(errors: &mut Vec<String>, result: Result<T, String>) -> T {
        match result {
            Ok(value) => value,
            Err(e) => {
                errors.push(e);
                T::default()
            }
        }
    }

    let struct_fields = collect(&mut errors, parse_struct_fields(lib_text));
    let schema_properties = collect(&mut errors, schema_override_properties(schema_text));
    let effort_values = collect(&mut errors, schema_effort_values(schema_text));
    let baked_keys = collect(&mut errors, baked_row_keys(baked_text));

    // Direction A: every struct field needs a schema definition (or a
    // documented exception). A new struct field without one = build failure.
    for field in &struct_fields {
        if !schema_properties.contains(field) && !STRUCT_ONLY_ALLOWED.contains(&field.as_str()) {
            errors.push(format!(
                "param-gate: {STRUCT_NAME} field '{field}' has no definition in config.schema.json #/definitions/ConfigModelOverride/properties — add the schema property (and regenerate the schema) or classify it in STRUCT_ONLY_ALLOWED with a reason"
            ));
        }
    }
    // Direction B: every schema property is either modeled by the struct or
    // explicitly classified config-only. A new schema property without a
    // struct field = build failure (fail-closed curation decision).
    for prop in &schema_properties {
        if !struct_fields.contains(prop) && !SCHEMA_ONLY_ALLOWED.contains(&prop.as_str()) {
            errors.push(format!(
                "param-gate: config.schema.json ConfigModelOverride property '{prop}' has no {STRUCT_NAME} field — model it in the struct or classify it in SCHEMA_ONLY_ALLOWED with a reason"
            ));
        }
    }
    // Direction C: baked row keys must be modeled by the struct (serde would
    // silently drop unknown keys — the offline fallback must not lose them)
    // and schema-defined (the row surface is the config surface).
    for key in &baked_keys {
        if !struct_fields.contains(key) {
            errors.push(format!(
                "param-gate: default_models.json row key '{key}' is not modeled by {STRUCT_NAME} — serde would silently drop it from the baked catalog"
            ));
        }
        if !schema_properties.contains(key) && !STRUCT_ONLY_ALLOWED.contains(&key.as_str()) {
            errors.push(format!(
                "param-gate: default_models.json row key '{key}' has no definition in config.schema.json ConfigModelOverride/properties"
            ));
        }
    }

    // Derived constants (the single-source-of-truth artifacts):
    // REQUIRED_FIELDS = (struct ∩ schema) − CATALOG_REQUIRED_EXCLUDES,
    // EFFORT_VALUES = #/definitions/ReasoningEffort.enum.
    let mut required_fields: Vec<String> = Vec::new();
    for field in &struct_fields {
        if schema_properties.contains(field)
            && !CATALOG_REQUIRED_EXCLUDES.contains(&field.as_str())
        {
            required_fields.push(field.clone());
        }
    }

    GateReport {
        errors,
        struct_fields,
        schema_properties,
        required_fields,
        effort_values,
    }
}
