//! Self-contained HTML reference for the `~/.grok/config.toml` surface.
//!
//! Donor: the codex config-schema pattern via apex-hw0 — renders from the
//! same canonicalized, default-stripped schema value the JSON fixture uses
//! (`super::config_schema_value`), so the two artifacts can never drift
//! independently.
//!
//! Content (bead apex-33z — completeness over brevity):
//! - header: title, schema content hash, regen command, provenance;
//! - all root properties (type summary + description + definition link);
//! - the `[model.<id>]` ConfigModelOverride reference (the centerpiece):
//!   every field with cardinality, rendered type (enum-like refs as chips,
//!   `$ref`s as anchor links, format/minimum/maximum on integers, object/
//!   array children expanded one level) and the verbatim schema description
//!   (the doc comments carry the operating semantics);
//! - the enum appendix: every `enum` definition, every oneOf-of-const
//!   vocabulary, and the ReasoningEffortOption menu-row shape;
//! - the `[endpoints]` EndpointsConfig fleet defaults;
//! - a definition index (anchor home for every `$ref` target);
//! - a tiny inline vanilla-JS substring filter over field names.
//!
//! Self-contained: no external assets, no CDN, no network refs, one inline
//! `<script>`, std-only string building (no new cargo dependencies).
//!
//! Determinism (the golden `config_reference_html_matches` depends on it):
//! sorted traversal (the value comes from `canonicalize`, which sorts every
//! object key), no timestamps, no environment reads. The output self-
//! identifies the schema it was rendered from via an fnv1a-64 content hash
//! of the schema value (FNV-1a rather than `DefaultHasher` because the
//! latter's algorithm is not guaranteed stable across toolchains).
//!
//! Regenerate the committed reference:
//! ```sh
//! cargo run -p xai-grok-shell --bin config-schema-write -- --html
//! ```

use std::collections::HashSet;
use std::path::Path;

use serde_json::{Map, Value};

use super::config_schema_value;

/// Render the config reference as a self-contained HTML string.
pub fn render_config_reference_html() -> std::io::Result<String> {
    let value = config_schema_value();
    let hash = fnv1a64_hex(
        &serde_json::to_string(&value)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?,
    );
    let root = value.as_object().expect("schema root must be a JSON object");
    let defs = root
        .get("definitions")
        .and_then(Value::as_object)
        .expect("schema must carry /definitions");
    let title = root.get("title").and_then(Value::as_str).unwrap_or("Config");
    let props = root
        .get("properties")
        .and_then(Value::as_object)
        .expect("schema must carry root /properties");
    let cmo = defs.get("ConfigModelOverride").expect(
        "ConfigModelOverride definition (the model-row centerpiece) must exist",
    );
    let endpoints = defs
        .get("EndpointsConfig")
        .expect("EndpointsConfig definition must exist");

    // Anchor homes: every enum-like vocabulary def is documented in the enum
    // appendix (the id lives on its appendix row), so the definition index —
    // the anchor home for everything else — must not re-anchor them.
    let anchored: HashSet<String> = defs
        .keys()
        .filter(|k| is_enum_like(&defs[*k]))
        .cloned()
        .chain(std::iter::once("ReasoningEffortOption".to_string()))
        .collect();

    let sections = [
        section_root(props, root.get("required").unwrap_or(&Value::Null), defs),
        section_model(cmo, defs),
        section_enums(defs),
        section_endpoints(endpoints, defs),
        section_def_index(defs, &anchored),
    ]
    .concat();

    // Sequential literal replacement (not `format!`, which requires a string
    // literal first argument — DOC is a const). All ten placeholders are
    // unique tokens; `{css}`/`{sections}`/`{js}` are substituted last so
    // their contents can never be re-scanned as placeholders.
    let nmodel = cmo
        .get("properties")
        .and_then(Value::as_object)
        .map(|p| p.len().to_string())
        .unwrap_or_default();
    let nendpoints = endpoints
        .get("properties")
        .and_then(Value::as_object)
        .map(|p| p.len().to_string())
        .unwrap_or_default();
    let mut html = DOC.to_string();
    html = html.replace("{title}", &esc(title));
    html = html.replace("{hash}", &hash);
    html = html.replace("{nroot}", &props.len().to_string());
    html = html.replace("{nmodel}", &nmodel);
    html = html.replace("{nenum}", &anchored.len().to_string());
    html = html.replace("{nendpoints}", &nendpoints);
    html = html.replace("{ndefs}", &defs.len().to_string());
    html = html.replace("{css}", CSS);
    html = html.replace("{sections}", &sections);
    html = html.replace("{js}", JS);
    Ok(html)
}

/// Write the config reference HTML to disk.
pub fn write_config_reference_html(out_path: &Path) -> std::io::Result<()> {
    let html = render_config_reference_html()?;
    std::fs::write(out_path, html)
}

// ------------------------------------------------------------------ sections

fn section_root(
    props: &Map<String, Value>,
    required: &Value,
    defs: &Map<String, Value>,
) -> String {
    let required_list = required.as_array().cloned().unwrap_or_default();
    let mut rows = String::new();
    for (name, spec) in props {
        let req = required_list.iter().any(|r| r.as_str() == Some(name.as_str()));
        let name_cell = match ref_of(spec) {
            Some(d) => format!(
                "<code><a class=\"ref\" href=\"#def-{d}\">{n}</a></code>",
                d = esc(&d),
                n = esc(name),
            ),
            None => format!("<code>{}</code>", esc(name)),
        };
        // Prefer the property's own description; fall back to the referenced
        // definition's description (data-driven, never invented).
        let desc = spec
            .get("description")
            .and_then(Value::as_str)
            .or_else(|| {
                ref_of(spec)
                    .and_then(|d| defs.get(&d))
                    .and_then(|d| d.get("description").and_then(Value::as_str))
            });
        rows.push_str(&format!(
            "<tr class=\"f\" data-name=\"{n}\"><th>{name}</th><td>{req}</td><td class=\"type\">{t}</td><td class=\"desc\">{d}</td></tr>\n",
            n = esc(name),
            name = name_cell,
            req = req_badge(req),
            t = type_cell(spec, defs),
            d = desc_or_dash(desc),
        ));
    }
    format!(
        "<section id=\"root\"><h2>root properties ({n})</h2>\n<table><thead><tr><th>property</th><th>required</th><th>type</th><th>description</th></tr></thead><tbody>\n{rows}</tbody></table></section>\n",
        n = props.len(),
        rows = rows,
    )
}

fn section_model(cmo: &Value, defs: &Map<String, Value>) -> String {
    let props = cmo
        .get("properties")
        .and_then(Value::as_object)
        .expect("ConfigModelOverride must carry properties");
    let required_list = cmo.get("required").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut rows = String::new();
    for (name, spec) in props {
        let req = required_list.iter().any(|r| r.as_str() == Some(name.as_str()));
        rows.push_str(&format!(
            "<tr class=\"f\" data-name=\"{n}\"><th><code>{n}</code></th><td>{req}</td><td class=\"type\">{t}</td><td class=\"desc\">{d}</td></tr>\n",
            n = esc(name),
            req = req_badge(req),
            t = type_cell(spec, defs),
            d = desc_or_dash(spec.get("description").and_then(Value::as_str)),
        ));
    }
    format!(
        "<section id=\"model\"><h2><code>[model.&lt;id&gt;]</code> override rows — <a class=\"ref\" href=\"#def-ConfigModelOverride\">ConfigModelOverride</a> ({n} fields)</h2>\n{lead}<table><thead><tr><th>field</th><th>required</th><th>type</th><th>description (verbatim schema text)</th></tr></thead><tbody>\n{rows}</tbody></table></section>\n",
        n = props.len(),
        lead = cmo.get("description").and_then(Value::as_str).map(desc_block).unwrap_or_default(),
        rows = rows,
    )
}

fn section_enums(defs: &Map<String, Value>) -> String {
    let enum_defs: Vec<&String> = defs
        .keys()
        .filter(|k| defs[*k].get("enum").is_some())
        .collect();
    let const_defs: Vec<&String> = defs.keys().filter(|k| is_const_oneof(&defs[*k])).collect();
    let mut out = String::new();
    out.push_str(&format!(
        "<section id=\"enums\"><h2>enum appendix ({n})</h2>\n",
        n = enum_defs.len() + const_defs.len()
            + usize::from(defs.get("ReasoningEffortOption").is_some()),
    ));
    if !enum_defs.is_empty() {
        out.push_str("<h3>string enums</h3>\n<table><thead><tr><th>definition</th><th>description</th><th>values</th></tr></thead><tbody>\n");
        for k in &enum_defs {
            let d = &defs[*k];
            let chips = d
                .get("enum")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(|v| format!("<span class=\"chip\">{}</span>", esc(v)))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            out.push_str(&format!(
                "<tr id=\"def-{k}\"><th><code><a class=\"ref\" href=\"#def-{k}\">{k}</a></code></th><td class=\"desc\">{d}</td><td>{chips}</td></tr>\n",
                k = esc(k),
                d = desc_or_dash(d.get("description").and_then(Value::as_str)),
                chips = chips,
            ));
        }
        out.push_str("</tbody></table>\n");
    }
    if !const_defs.is_empty() {
        out.push_str("<h3>const vocabularies (oneOf of const)</h3>\n<table><thead><tr><th>definition</th><th>description</th><th>values</th></tr></thead><tbody>\n");
        for k in &const_defs {
            let d = &defs[*k];
            let branches = d.get("oneOf").and_then(Value::as_array).cloned().unwrap_or_default();
            let cell = if branches.iter().any(|b| b.get("description").is_some()) {
                format!(
                    "<ul class=\"constlist\">{}</ul>",
                    branches
                        .iter()
                        .map(|b| {
                            let v = b.get("const").and_then(Value::as_str).unwrap_or_default();
                            match b.get("description").and_then(Value::as_str) {
                                Some(dd) => format!("<li><span class=\"chip\">{v}</span> <span class=\"desc\">{dd}</span></li>", v = esc(v), dd = esc(dd)),
                                None => format!("<li><span class=\"chip\">{}</span></li>", esc(v)),
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("")
                )
            } else {
                branches
                    .iter()
                    .filter_map(|b| b.get("const").and_then(Value::as_str))
                    .map(|v| format!("<span class=\"chip\">{}</span>", esc(v)))
                    .collect::<Vec<_>>()
                    .join("")
            };
            out.push_str(&format!(
                "<tr id=\"def-{k}\"><th><code><a class=\"ref\" href=\"#def-{k}\">{k}</a></code></th><td class=\"desc\">{d}</td><td>{cell}</td></tr>\n",
                k = esc(k),
                d = desc_or_dash(d.get("description").and_then(Value::as_str)),
                cell = cell,
            ));
        }
        out.push_str("</tbody></table>\n");
    }
    if let Some(d) = defs.get("ReasoningEffortOption") {
        out.push_str(&rer_option_block(d, defs));
    }
    out.push_str("</section>\n");
    out
}

fn rer_option_block(d: &Value, defs: &Map<String, Value>) -> String {
    let props = d.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
    let required_list = d.get("required").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut rows = String::new();
    for (name, spec) in &props {
        let req = required_list.iter().any(|r| r.as_str() == Some(name.as_str()));
        rows.push_str(&format!(
            "<tr class=\"f\" data-name=\"{n}\"><th><code>{n}</code></th><td class=\"type\">{t}</td><td>{req}</td><td class=\"desc\">{d}</td></tr>\n",
            n = esc(name),
            t = type_cell(spec, defs),
            req = req_badge(req),
            d = desc_or_dash(spec.get("description").and_then(Value::as_str)),
        ));
    }
    format!(
        "<h3 id=\"def-ReasoningEffortOption\"><code><a class=\"ref\" href=\"#def-ReasoningEffortOption\">ReasoningEffortOption</a></code> — selectable menu-row shape</h3>\n{lead}<table><thead><tr><th>field</th><th>type</th><th>required</th><th>description</th></tr></thead><tbody>\n{rows}</tbody></table>\n",
        lead = desc_block(d.get("description").and_then(Value::as_str).unwrap_or("")),
        rows = rows,
    )
}

fn section_endpoints(d: &Value, defs: &Map<String, Value>) -> String {
    let props = d.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut rows = String::new();
    for (name, spec) in &props {
        rows.push_str(&format!(
            "<tr class=\"f\" data-name=\"{n}\"><th><code>{n}</code></th><td class=\"type\">{t}</td><td class=\"desc\">{d}</td></tr>\n",
            n = esc(name),
            t = type_cell(spec, defs),
            d = desc_or_dash(spec.get("description").and_then(Value::as_str)),
        ));
    }
    format!(
        "<section id=\"endpoints\"><h2><code>[endpoints]</code> fleet defaults — <a class=\"ref\" href=\"#def-EndpointsConfig\">EndpointsConfig</a> ({n} fields)</h2>\n{lead}<table><thead><tr><th>field</th><th>type</th><th>description</th></tr></thead><tbody>\n{rows}</tbody></table></section>\n",
        n = props.len(),
        lead = d.get("description").and_then(Value::as_str).map(desc_block).unwrap_or_default(),
        rows = rows,
    )
}

fn section_def_index(defs: &Map<String, Value>, anchored: &HashSet<String>) -> String {
    let mut rows = String::new();
    let mut n = 0;
    for (k, d) in defs {
        if anchored.contains(k) {
            continue;
        }
        n += 1;
        rows.push_str(&format!(
            "<tr id=\"def-{k}\"><th><code>{k}</code></th><td>{shape}</td><td class=\"desc\">{d}</td></tr>\n",
            k = esc(k),
            shape = type_summary(d),
            d = desc_or_dash(d.get("description").and_then(Value::as_str)),
        ));
    }
    format!(
        "<section id=\"defs\"><h2>definition index ({n} — the {a} anchored vocabulary definitions live in the enum appendix)</h2>\n<table><thead><tr><th>definition</th><th>shape</th><th>description</th></tr></thead><tbody>\n{rows}</tbody></table></section>\n",
        n = n,
        a = anchored.len(),
        rows = rows,
    )
}

// ------------------------------------------------------------- type rendering

/// Rich type cell for tables: `$ref`s become anchor links (with chips when the
/// target is enum-like), arrays/objects expand one level, scalars carry their
/// format/minimum/maximum. `Option<T>` shapes (type `[T, null]` or
/// anyOf `[T, null]`) render the core type plus a `?` nullability marker.
fn type_cell(spec: &Value, defs: &Map<String, Value>) -> String {
    let (core, nullable) = decompose_nullable(spec);
    let mut s = render_core(&core, defs);
    if nullable {
        s.push_str("<span class=\"optmark\" title=\"nullable (Option)\">?</span>");
    }
    s
}

/// One-line plain-text shape for the definition index.
fn type_summary(spec: &Value) -> String {
    if spec == &Value::Bool(true) {
        return "any".into();
    }
    if let Some(name) = ref_of(spec) {
        return name;
    }
    if let Some(branches) = spec
        .get("anyOf")
        .or_else(|| spec.get("oneOf"))
        .and_then(Value::as_array)
    {
        let parts: Vec<String> = branches
            .iter()
            .filter(|b| !is_null_branch(b))
            .map(type_summary)
            .collect();
        let s = parts.join(" | ");
        return if branches.iter().any(is_null_branch) {
            s + "?"
        } else {
            s
        };
    }
    if let Some(ts) = spec.get("type").and_then(Value::as_array) {
        let mut parts = Vec::new();
        let mut nullable = false;
        for t in ts {
            match t.as_str() {
                Some("null") => nullable = true,
                Some(o) => parts.push(o.to_string()),
                None => {}
            }
        }
        let s = parts.join("|");
        return if nullable { s + "?" } else { s };
    }
    match spec.get("type").and_then(Value::as_str) {
        Some("array") => {
            let mut s = "array".to_string();
            if let Some(items) = spec.get("items") {
                s.push_str(" of ");
                s.push_str(&type_summary(items));
            }
            s
        }
        Some("object") => {
            let mut s = "object".to_string();
            if let Some(ap) = spec.get("additionalProperties") {
                if ap != &Value::Bool(true) {
                    s.push_str(&format!(" map&mdash;{}", type_summary(ap)));
                }
            }
            s
        }
        Some(t) => {
            let mut s = t.to_string();
            if let Some(f) = spec.get("format").and_then(Value::as_str) {
                s.push_str(&format!(" ({f})"));
            }
            if let Some(m) = spec.get("minimum") {
                if m.is_number() {
                    s.push_str(&format!(" min {m}"));
                }
            }
            if let Some(m) = spec.get("maximum") {
                if m.is_number() {
                    s.push_str(&format!(" max {m}"));
                }
            }
            s
        }
        None => {
            if spec.get("const").is_some() {
                "const".into()
            } else if let Some(a) = spec.get("enum").and_then(Value::as_array) {
                format!("enum ({})", a.len())
            } else if spec.get("properties").is_some() {
                "object".into()
            } else if spec.get("items").is_some() {
                "array".into()
            } else {
                "any".into()
            }
        }
    }
}

fn render_core(spec: &Value, defs: &Map<String, Value>) -> String {
    if spec == &Value::Bool(true) {
        return "<span class=\"t\">any</span>".into();
    }
    if let Some(name) = ref_of(spec) {
        let mut s = format!(
            "<a class=\"ref\" href=\"#def-{n}\">{n}</a>",
            n = esc(&name)
        );
        if let Some(d) = defs.get(&name) {
            if let Some(chips) = enum_chips(d) {
                s.push_str(&chips);
            }
        }
        return s;
    }
    if let Some(branches) = spec
        .get("anyOf")
        .or_else(|| spec.get("oneOf"))
        .and_then(Value::as_array)
    {
        let parts: Vec<String> = branches
            .iter()
            .map(|b| {
                if is_null_branch(b) {
                    "<span class=\"t\">null</span>".to_string()
                } else {
                    render_core(b, defs)
                }
            })
            .collect();
        return format!("<span class=\"t\">{}</span>", parts.join("<span class=\"t\"> &amp; </span>"));
    }
    match spec.get("type").and_then(Value::as_str) {
        Some("array") => {
            let mut s = String::from("<span class=\"t\">array of</span> ");
            if let Some(items) = spec.get("items") {
                s.push_str(&render_core(items, defs));
            }
            s
        }
        Some("object") => {
            let mut s = String::from("<span class=\"t\">object</span>");
            if let Some(ap) = spec.get("additionalProperties") {
                if ap == &Value::Bool(true) {
                    s.push_str(" <span class=\"t\">(free-form)</span>");
                } else {
                    s.push_str(" <span class=\"t\">map&mdash;</span>");
                    s.push_str(&render_core(ap, defs));
                }
            } else if let Some(p) = spec.get("properties").and_then(Value::as_object) {
                if !p.is_empty() {
                    let names = p
                        .keys()
                        .map(|k| format!("<code>{}</code>", esc(k)))
                        .collect::<Vec<_>>()
                        .join(" ");
                    s.push_str(&format!(" <span class=\"t\">{{</span> {names} <span class=\"t\">}}</span>"));
                }
            }
            s
        }
        Some(t) => {
            let mut s = format!("<span class=\"t\">{}</span>", esc(t));
            if let Some(f) = spec.get("format").and_then(Value::as_str) {
                s.push_str(&format!(" <code>{}</code>", esc(f)));
            }
            if let Some(m) = spec.get("minimum") {
                if m.is_number() {
                    s.push_str(&format!(" <span class=\"t\">min {m}</span>"));
                }
            }
            if let Some(m) = spec.get("maximum") {
                if m.is_number() {
                    s.push_str(&format!(" <span class=\"t\">max {m}</span>"));
                }
            }
            s
        }
        None => {
            if let Some(c) = spec.get("const") {
                let v = c.as_str().map(esc).unwrap_or_else(|| esc(&c.to_string()));
                format!("<span class=\"t\">const</span> <code>{v}</code>")
            } else if spec.get("enum").is_some() {
                "<span class=\"t\">enum</span>".into()
            } else if spec.get("properties").is_some() {
                "<span class=\"t\">object</span>".into()
            } else if spec.get("items").is_some() {
                "<span class=\"t\">array</span>".into()
            } else {
                "<span class=\"t\">any</span>".into()
            }
        }
    }
}

/// Split off JSON-Schema nullability: `type: [T, null]` and
/// `anyOf: [T, {type: null}]` both render as the core type plus a `?`.
fn decompose_nullable(spec: &Value) -> (Value, bool) {
    if let Some(ts) = spec.get("type").and_then(Value::as_array) {
        if ts.len() == 2 {
            let non_null: Vec<&Value> = ts.iter().filter(|t| t.as_str() != Some("null")).collect();
            if non_null.len() == 1 {
                let mut core = spec.clone();
                if let Value::Object(m) = &mut core {
                    m.insert("type".into(), non_null[0].clone());
                }
                return (core, true);
            }
        }
    }
    if let Some(branches) = spec.get("anyOf").and_then(Value::as_array) {
        if branches.len() == 2 {
            if is_null_branch(&branches[0]) {
                return (branches[1].clone(), true);
            }
            if is_null_branch(&branches[1]) {
                return (branches[0].clone(), true);
            }
        }
    }
    (spec.clone(), false)
}

/// The definition name behind a `$ref` (direct, or a single `allOf` wrap —
/// the shape schemars emits for most struct-typed properties).
fn ref_of(spec: &Value) -> Option<String> {
    if let Some(r) = spec.get("$ref").and_then(Value::as_str) {
        return r.strip_prefix("#/definitions/").map(str::to_string);
    }
    if let Some(all) = spec.get("allOf").and_then(Value::as_array) {
        if all.len() == 1 {
            return ref_of(&all[0]);
        }
    }
    None
}

/// True for definitions rendered in the enum appendix: a literal `enum` or a
/// oneOf of string consts.
fn is_enum_like(d: &Value) -> bool {
    d.get("enum").is_some() || is_const_oneof(d)
}

fn is_const_oneof(d: &Value) -> bool {
    d.get("oneOf")
        .and_then(Value::as_array)
        .map(|b| !b.is_empty() && b.iter().all(|x| x.get("const").and_then(Value::as_str).is_some()))
        .unwrap_or(false)
}

fn enum_chips(d: &Value) -> Option<String> {
    if let Some(a) = d.get("enum").and_then(Value::as_array) {
        let chips = a
            .iter()
            .filter_map(|v| v.as_str())
            .map(|v| format!("<span class=\"chip\">{}</span>", esc(v)))
            .collect::<Vec<_>>();
        if !chips.is_empty() {
            return Some(chips.join(""));
        }
    }
    if is_const_oneof(d) {
        let chips = d
            .get("oneOf")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|b| b.get("const").and_then(Value::as_str))
            .map(|v| format!("<span class=\"chip\">{}</span>", esc(v)))
            .collect::<Vec<_>>();
        if !chips.is_empty() {
            return Some(chips.join(""));
        }
    }
    None
}

fn is_null_branch(b: &Value) -> bool {
    b.get("type").and_then(Value::as_str) == Some("null")
}

// ------------------------------------------------------------------ helpers

fn req_badge(req: bool) -> &'static str {
    if req {
        "<span class=\"req\">required</span>"
    } else {
        "<span class=\"opt\">optional</span>"
    }
}

fn desc_block(d: &str) -> String {
    format!("<p class=\"lead\">{}</p>\n", desc_html(d))
}

fn desc_or_dash(d: Option<&str>) -> String {
    match d {
        Some(s) if !s.trim().is_empty() => desc_html(s),
        _ => "<span class=\"dash\">—</span>".into(),
    }
}

fn desc_html(d: &str) -> String {
    esc(d).replace('\n', "<br>")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// FNV-1a 64-bit over the compact serialization of the schema value — the
/// version marker. Stable by construction (fixed algorithm), unlike
/// `DefaultHasher`, whose output must not be relied on across toolchains.
fn fnv1a64_hex(bytes: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

// ------------------------------------------------------------------ template

const CSS: &str = r#"
:root { --bg:#fafafa; --fg:#1a1a1a; --muted:#666; --line:#ddd; --accent:#0b6bcb; --chip:#e8f0fe; --chipfg:#0b5cad; }
* { box-sizing: border-box; }
body { margin:0; background:var(--bg); color:var(--fg); font:15px/1.5 -apple-system, "Segoe UI", Helvetica, Arial, sans-serif; }
code { font-family:ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size:13px; background:#f0f0f0; padding:1px 5px; border-radius:4px; }
header { padding:24px 32px 12px; max-width:1200px; margin:0 auto; }
h1 { margin:0 0 4px; font-size:24px; }
.sub { color:var(--muted); margin:2px 0; }
.ver, .regen, .prov { margin:2px 0; font-size:13.5px; color:var(--muted); }
nav.toc { position:sticky; top:0; z-index:9; background:#fff; border-top:1px solid var(--line); border-bottom:1px solid var(--line); padding:8px 32px; display:flex; gap:14px; align-items:center; flex-wrap:wrap; }
nav.toc a { color:var(--accent); text-decoration:none; font-size:13.5px; }
nav.toc a:hover { text-decoration:underline; }
#q { margin-left:auto; padding:5px 10px; border:1px solid var(--line); border-radius:6px; font-size:13.5px; width:240px; }
main { max-width:1200px; margin:0 auto; padding:16px 32px 48px; }
section { margin-top:28px; }
h2 { font-size:19px; border-bottom:2px solid var(--line); padding-bottom:6px; }
h3 { font-size:15.5px; margin:18px 0 6px; }
table { border-collapse:collapse; width:100%; background:#fff; }
th, td { border:1px solid var(--line); padding:6px 10px; vertical-align:top; text-align:left; }
thead th { background:#f2f2f2; font-size:12px; text-transform:uppercase; letter-spacing:.03em; color:var(--muted); }
tr.f th { background:#fbfbfb; white-space:nowrap; }
.chip { display:inline-block; background:var(--chip); color:var(--chipfg); border:1px solid #c5d8f7; border-radius:9px; padding:0 8px; margin:1px 2px; font-family:ui-monospace, Menlo, monospace; font-size:12px; }
a.ref { color:var(--accent); text-decoration:none; }
a.ref:hover { text-decoration:underline; }
.t { color:#444; font-size:13px; }
.optmark { color:var(--muted); font-weight:600; }
.opt, .req { font-size:11.5px; border-radius:8px; padding:0 7px; border:1px solid; white-space:nowrap; }
.opt { color:var(--muted); border-color:var(--line); background:#f5f5f5; }
.req { color:#8a3b00; border-color:#e6b98a; background:#fdf1e3; }
.dash { color:#bbb; }
.desc { max-width:640px; }
.lead { color:#333; }
ul.constlist { list-style:none; margin:2px 0; padding:0; }
ul.constlist li { margin:2px 0; }
footer { max-width:1200px; margin:0 auto; padding:12px 32px 32px; color:var(--muted); font-size:12.5px; border-top:1px solid var(--line); }
"#;

const JS: &str = r#"
(function () {
  var box = document.getElementById('q');
  var rows = Array.prototype.slice.call(document.querySelectorAll('tr.f'));
  box.addEventListener('input', function () {
    var q = box.value.toLowerCase();
    for (var i = 0; i < rows.length; i++) {
      var name = (rows[i].getAttribute('data-name') || '').toLowerCase();
      rows[i].style.display = (!q || name.indexOf(q) !== -1) ? '' : 'none';
    }
  });
})();
"#;

const DOC: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} — config.toml reference (xai-grok-shell)</title>
<style>{css}</style>
</head>
<body>
<header>
<h1>config.toml reference</h1>
<p class="sub">xai-grok-shell · agent <code>{title}</code> surface + <code>[model.&lt;id&gt;]</code> override rows · JSON Schema draft-07, rendered from the live types</p>
<p class="ver">content hash <code>fnv1a-64 {hash}</code> · {nroot} root properties · {nmodel} model-row fields · {nenum} enum definitions · {nendpoints} endpoint fields · {ndefs} definitions</p>
<p class="regen">regenerate: <code>cargo run -p xai-grok-shell --bin config-schema-write -- --html</code></p>
<p class="prov">provenance: bead <code>apex-33z</code> CONFIG-REFERENCE-HTML-1 · donor pattern: codex config-schema golden via apex-hw0 (<code>config_schema.rs</code>)</p>
</header>
<nav class="toc">
<a href="#root">root ({nroot})</a>
<a href="#model">model rows ({nmodel})</a>
<a href="#enums">enums ({nenum})</a>
<a href="#endpoints">endpoints ({nendpoints})</a>
<a href="#defs">definitions</a>
<input id="q" type="search" placeholder="filter field names…" autocomplete="off">
</nav>
<main>
{sections}
</main>
<footer>Rendered deterministically from the live schema: sorted traversal, no timestamps, std-only, self-contained (no external assets). Self-identifies via fnv1a-64 content hash <code>{hash}</code> over the canonicalized, default-stripped schema JSON. Guarded by the golden <code>agent::config_schema_tests::config_reference_html_matches</code>.</footer>
<script>{js}</script>
</body>
</html>
"##;
