//! Regenerate the committed config schema fixture (`config.schema.json`).
//!
//! See `src/agent/config_schema.rs` for the mechanism and surface model.
//! Donor: codex `codex-rs/config-schema` (`codex-write-config-schema`).
//!
//! Usage:
//!   cargo run -p xai-grok-shell --bin config-schema-write            # crate root
//!   cargo run -p xai-grok-shell --bin config-schema-write -- -o OUT  # custom path

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut out: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-o" | "--out" => {
                out = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("missing value for {arg}"))?,
                ))
            }
            other if other.starts_with('-') => anyhow::bail!("unknown argument: {other}"),
            other => out = Some(PathBuf::from(other)),
        }
    }
    let out_path =
        out.unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config.schema.json"));
    xai_grok_shell::agent::config_schema::write_config_schema(&out_path)?;
    eprintln!("wrote {}", out_path.display());
    Ok(())
}
