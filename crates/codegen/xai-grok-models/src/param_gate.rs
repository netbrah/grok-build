//! Build-time derived param-gate constants (apex-ayl.130
//! ZC-PARAMSCHEMA-GATE-1, P1).
//!
//! `build.rs` derives these from
//! `crates/codegen/xai-grok-shell/config.schema.json` — the single source of
//! truth (the sibling Python gate reads the same schema). Param drift
//! between [`crate::DefaultModelEntry`] and the schema fails the build.
include!(concat!(env!("OUT_DIR"), "/param_gate_generated.rs"));
