//! Regenerate the committed config schema fixture (`config.schema.json`).
//!
//! See `src/agent/config_schema.rs` for the mechanism and surface model.
//! Donor: codex `codex-rs/config-schema` (`codex-write-config-schema`).
//!
//! Usage:
//!   cargo run -p xai-grok-shell --bin config-schema-write             # JSON, crate root
//!   cargo run -p xai-grok-shell --bin config-schema-write -- -o OUT   # JSON, custom path
//!   cargo run -p xai-grok-shell --bin config-schema-write -- --html   # HTML, crate root
//!   cargo run -p xai-grok-shell --bin config-schema-write -- --html P # HTML, custom path
//!
//! The no-flag JSON mode is unchanged (apex-33z); `--html [PATH]` renders the
//! self-contained `config.reference.html` reference instead (combine with
//! `-o` to write both).

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut json_out: Option<PathBuf> = None;
    let mut html_out: Option<PathBuf> = None;
    let mut want_html = false;
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-o" | "--out" => {
                json_out = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("missing value for {arg}"))?,
                ))
            }
            "--html" => {
                want_html = true;
                // Optional value: `--html` alone uses the default location.
                if let Some(next) = args.peek() {
                    if !next.starts_with('-') {
                        html_out = Some(PathBuf::from(next.clone()));
                        args.next();
                    }
                }
            }
            other if other.starts_with('-') => anyhow::bail!("unknown argument: {other}"),
            other => json_out = Some(PathBuf::from(other)),
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut wrote: Vec<String> = Vec::new();
    // No-flag invocations keep the original JSON-only behavior byte-for-byte.
    if json_out.is_some() || !want_html {
        let out_path =
            json_out.unwrap_or_else(|| manifest.join("config.schema.json"));
        xai_grok_shell::agent::config_schema::write_config_schema(&out_path)?;
        wrote.push(out_path.display().to_string());
    }
    if want_html {
        let out_path = html_out.unwrap_or_else(|| manifest.join("config.reference.html"));
        xai_grok_shell::agent::config_schema::html::write_config_reference_html(&out_path)?;
        wrote.push(out_path.display().to_string());
    }
    for path in &wrote {
        eprintln!("wrote {path}");
    }
    Ok(())
}
