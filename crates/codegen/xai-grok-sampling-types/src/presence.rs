//! Wire-presence carriers for the anthropic-messages port (spec A0, WIREPRESENCE-1 46a).
//!
//! Frozen spec (normative authority): `codex/docs/superpowers/specs/2026-08-21-anthropic-spawn-runtime-design.md`
//! A0 block (spec L87-99; SDD §1 renders it normatively — faithful paraphrase, not verbatim).
//!
//! Two carriers, one wire semantic (SDD §4.1):
//!
//! - [`WirePresence`] — response-side carrier; states PRIVATE. For response fields whose
//!   selected rule distinguishes omission from JSON null.
//! - [`RequestPresence`] — request-side carrier; states PUBLIC `{ Omitted, Null, Value(T) }`
//!   for request fields that are BOTH optional AND nullable. The 46b builder-facing API.
//!
//! Field pattern (documented on the types; proven by `presence_goldens`):
//!
//! ```rust,ignore
//! #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
//! pub field: WirePresence<T>,
//! ```
//!
//! - Deserialize: in the field pattern, an absent member never reaches the carrier's
//!   deserializer — `#[serde(default)]` supplies Missing/Omitted. JSON `null` → Null; JSON
//!   value → `T::deserialize` → `Value(t)`, with T's own type errors propagating (checked;
//!   never silently coerced). Without `#[serde(default)]` (required field), serde 1.0.228's
//!   missing-field path routes `deserialize_option` to `visit_none`, so absence deserializes
//!   as Null — pinned in `presence_tests`, and outside the spec's modeled surface.
//! - Serialize: Missing/Omitted → member omitted by the skip predicate; Null → JSON `null`;
//!   Value(t) → `t`.
//! - Invariant (spec, test-pinned): the three states never collapse across a round trip in
//!   the field pattern. In bare (non-struct) JSON there is no encoding for "absent", so a
//!   bare Missing/Omitted serializes as `null` — the non-collapse guarantee is field-level,
//!   where absence is representable.
//!
//! 46c scope: 46b's `RequestPresence` rows unchanged (three PRESENT) + `WirePresence`
//! wired into the 12 response-side A0 fields of `MessagesResponse`/`MessagesUsage`/
//! `MessageDeltaBody`/`MessageDeltaUsage` (9 rewrites + 3 ADDs; §4.7 inventory totals
//! 3 × RequestPresence / 12 × WirePresence / 0 × Option / 6 × ABSENT).
//! Generic carrier holders need `#[serde(bound = "T: Serialize + DeserializeOwned")]` to
//! derive (E0277/E0283); all holders are concrete, so none does (46c confirmed: no bound).

use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Checked error from [`WirePresence::into_value`] when the carrier does not hold a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WirePresenceError {
    /// The carrier is in the Missing state (absent member); no value to return.
    #[error("WirePresence is Missing (absent member); no value to return")]
    Missing,
    /// The carrier is in the Null state (explicit JSON null); no value to return.
    #[error("WirePresence is Null (explicit JSON null); no value to return")]
    Null,
}

/// Three-state wire-presence carrier for RESPONSE fields whose selected rule
/// distinguishes omission from JSON null (spec A0).
///
/// States are PRIVATE by design (spec: "private-state carrier"); construct with
/// [`WirePresence::missing`] / [`WirePresence::null`] / [`WirePresence::value`] and read
/// with the accessors. The three-state distinction is defined for struct fields that use
/// the documented pattern
/// `#[serde(default, skip_serializing_if = "WirePresence::is_absent")]`; in a bare
/// (non-struct) JSON context "absent" has no encoding, so a bare Missing serializes as
/// `null` — the non-collapse guarantee is field-level, where absence is representable.
#[derive(Debug, Clone, PartialEq)]
pub struct WirePresence<T: Serialize + DeserializeOwned> {
    state: WireState<T>,
}

#[derive(Debug, Clone, PartialEq)]
enum WireState<T> {
    Missing,
    Null,
    Value(T),
}

impl<T> WirePresence<T>
where
    T: Serialize + DeserializeOwned,
{
    /// The field was absent from the JSON (or never set).
    pub const fn missing() -> Self {
        Self {
            state: WireState::Missing,
        }
    }

    /// The field was present with an explicit JSON `null`.
    pub const fn null() -> Self {
        Self {
            state: WireState::Null,
        }
    }

    /// The field was present with a typed value.
    pub const fn value(t: T) -> Self {
        Self {
            state: WireState::Value(t),
        }
    }

    /// True in the Missing state. The documented `skip_serializing_if` predicate.
    pub const fn is_absent(&self) -> bool {
        matches!(self.state, WireState::Missing)
    }

    /// True in the Missing state.
    pub const fn is_missing(&self) -> bool {
        matches!(self.state, WireState::Missing)
    }

    /// True in the Null state (explicit JSON null).
    pub const fn is_null(&self) -> bool {
        matches!(self.state, WireState::Null)
    }

    /// The typed value when Value, else None. (SDD §4.1 lists this accessor as
    /// `value() -> Option<&T>`; Rust cannot overload the `value(t)` constructor, so it
    /// is named `as_ref`.)
    pub const fn as_ref(&self) -> Option<&T> {
        match &self.state {
            WireState::Value(t) => Some(t),
            _ => None,
        }
    }

    /// Checked extraction: `Ok(t)` for Value; `Err` naming carrier + state for
    /// Missing/Null. Do not `unwrap` outside tests.
    pub fn into_value(self) -> Result<T, WirePresenceError> {
        match self.state {
            WireState::Value(t) => Ok(t),
            WireState::Missing => Err(WirePresenceError::Missing),
            WireState::Null => Err(WirePresenceError::Null),
        }
    }
}

impl<T> Default for WirePresence<T>
where
    T: Serialize + DeserializeOwned,
{
    fn default() -> Self {
        Self::missing()
    }
}

impl<T> Serialize for WirePresence<T>
where
    T: Serialize + DeserializeOwned,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.state {
            WireState::Value(t) => t.serialize(serializer),
            // Null → JSON null. A bare (non-struct) Missing has no JSON encoding other
            // than `null`; field-level omission is the skip predicate's job.
            WireState::Missing | WireState::Null => serializer.serialize_none(),
        }
    }
}

impl<'de, T> Deserialize<'de> for WirePresence<T>
where
    T: Serialize + DeserializeOwned,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Only present members reach here (absence is handled by `#[serde(default)]` in
        // the field pattern). `null` → Null; a JSON value → `T::deserialize`, with T's
        // own type errors propagating unchanged (checked; never silently coerced).
        // Same Option-via pattern as serde_helpers::double_option. serde 1.0.228's
        // missing-field path (required field, no `#[serde(default)]`) routes
        // `deserialize_option` to `visit_none`, so an absent member on a REQUIRED
        // carrier field deserializes as Null rather than a hard error — pinned in
        // `presence_tests::absent_required_carrier_field_deserializes_as_null`.
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(t) => WirePresence::value(t),
            None => WirePresence::null(),
        })
    }
}

/// Checked error from [`RequestPresence::into_value`] when the carrier does not hold a
/// value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RequestPresenceError {
    /// The carrier is in the Omitted state (no member emitted); no value to return.
    #[error("RequestPresence is Omitted (no member emitted); no value to return")]
    Omitted,
    /// The carrier is in the Null state (explicit JSON null); no value to return.
    #[error("RequestPresence is Null (explicit JSON null); no value to return")]
    Null,
}

/// Three-state wire-presence carrier for request fields that are BOTH optional AND
/// nullable (spec A0). States are PUBLIC — the 46b request builder matches on them:
/// `Omitted` emits no object member, `Null` emits the member with JSON null, and
/// `Value` emits the typed value. Use with the documented field pattern
/// `#[serde(default, skip_serializing_if = "RequestPresence::is_absent")]`.
#[derive(Debug, Clone, PartialEq)]
pub enum RequestPresence<T: Serialize + DeserializeOwned> {
    /// No object member is emitted (the default state).
    Omitted,
    /// The member is emitted with JSON `null`.
    Null,
    /// The member is emitted with the typed value.
    Value(T),
}

impl<T> RequestPresence<T>
where
    T: Serialize + DeserializeOwned,
{
    /// Omitted: no object member is emitted.
    pub const fn omitted() -> Self {
        RequestPresence::Omitted
    }

    /// Null: the member is emitted with JSON `null`.
    pub const fn null() -> Self {
        RequestPresence::Null
    }

    /// Value: the member is emitted with the typed value.
    pub const fn value(t: T) -> Self {
        RequestPresence::Value(t)
    }

    /// True in the Omitted state. The documented `skip_serializing_if` predicate.
    pub const fn is_absent(&self) -> bool {
        matches!(self, RequestPresence::Omitted)
    }

    /// True in the Omitted state.
    pub const fn is_omitted(&self) -> bool {
        matches!(self, RequestPresence::Omitted)
    }

    /// True in the Null state (explicit JSON null).
    pub const fn is_null(&self) -> bool {
        matches!(self, RequestPresence::Null)
    }

    /// The typed value when Value, else None. (SDD §4.1 lists this accessor as
    /// `value() -> Option<&T>`; Rust cannot overload the `value(t)` constructor, so it
    /// is named `as_ref`.)
    pub const fn as_ref(&self) -> Option<&T> {
        match self {
            RequestPresence::Value(t) => Some(t),
            _ => None,
        }
    }

    /// Checked extraction: `Ok(t)` for Value; `Err` naming carrier + state for
    /// Omitted/Null. Do not `unwrap` outside tests.
    pub fn into_value(self) -> Result<T, RequestPresenceError> {
        match self {
            RequestPresence::Value(t) => Ok(t),
            RequestPresence::Omitted => Err(RequestPresenceError::Omitted),
            RequestPresence::Null => Err(RequestPresenceError::Null),
        }
    }
}

impl<T> Default for RequestPresence<T>
where
    T: Serialize + DeserializeOwned,
{
    fn default() -> Self {
        Self::Omitted
    }
}

impl<T> Serialize for RequestPresence<T>
where
    T: Serialize + DeserializeOwned,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            RequestPresence::Value(t) => t.serialize(serializer),
            RequestPresence::Omitted | RequestPresence::Null => serializer.serialize_none(),
        }
    }
}

impl<'de, T> Deserialize<'de> for RequestPresence<T>
where
    T: Serialize + DeserializeOwned,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Only present members reach here (absence is handled by `#[serde(default)]` in
        // the field pattern). `null` → Null; a JSON value → `T::deserialize`, with T's
        // own type errors propagating unchanged (checked; never silently coerced).
        // Same Option-via pattern as serde_helpers::double_option (see the serde
        // 1.0.228 missing-field note on the WirePresence impl above).
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(t) => RequestPresence::value(t),
            None => RequestPresence::Null,
        })
    }
}

#[cfg(test)]
mod presence_tests {
    use super::*;
    use crate::messages::{MessagesRequest, OutputFormat};

    // ------------------------------------------------------------------
    // RED-1 (inter-stage, 46b SDD §5-1): authored ACTIVE, proven red on 25e94f5
    // (/tmp/wirepresence-red.log), ignored through 46a. 46b (this stage): un-ignored;
    // red re-captured at /tmp/wirepresence-46b-red1.log, green post-§4 wiring.
    // ------------------------------------------------------------------

    /// An explicit `"metadata": {"user_id": null}` member must survive a
    /// deserialize → re-serialize round trip. RED on 46a: `Metadata.user_id` was
    /// `Option<String>` + `skip_serializing_if = Option::is_none`, so JSON null collapsed
    /// to `None` and the member vanished (`"metadata": {}`). 46b wires
    /// `RequestPresence<String>` into the DTO and un-ignores this test.
    #[test]
    fn metadata_user_id_null_survives_round_trip() {
        let raw = r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"metadata":{"user_id":null}}"#;
        let req: MessagesRequest =
            serde_json::from_str(raw).expect("deserialize minimal request carrying null user_id");
        let out = serde_json::to_string(&req).expect("serialize round trip");
        let round: serde_json::Value = serde_json::from_str(&out).expect("re-parse round trip");
        let user_id = round.get("metadata").and_then(|metadata| metadata.get("user_id"));
        assert!(
            matches!(user_id, Some(serde_json::Value::Null)),
            "explicit `\"user_id\": null` member must survive the round trip; re-serialized bytes: {out}"
        );
    }

    // ------------------------------------------------------------------
    // Non-collapse round trips (spec invariant; SDD §4.1-3)
    // ------------------------------------------------------------------

    /// Field-pattern holder: the documented
    /// `#[serde(default, skip_serializing_if = …)]` pattern the carriers ship with.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(bound = "T: Serialize + DeserializeOwned")]
    struct RoundTripHolder<T: Serialize + DeserializeOwned> {
        #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
        field: WirePresence<T>,
    }

    fn assert_round_trip<T>(original: WirePresence<T>)
    where
        T: Clone + std::fmt::Debug + PartialEq + Serialize + DeserializeOwned,
    {
        let holder = RoundTripHolder { field: original.clone() };
        let first = serde_json::to_string(&holder).expect("serialize holder");
        let back: RoundTripHolder<T> = serde_json::from_str(&first).expect("deserialize holder");
        let second = serde_json::to_string(&back).expect("re-serialize holder");
        assert_eq!(back.field, original, "state collapsed across round trip (bytes: {first})");
        assert_eq!(second, first, "serialization is not byte-stable");
    }

    #[test]
    fn wire_presence_string_round_trip_never_collapses() {
        assert_round_trip(WirePresence::<String>::missing());
        assert_round_trip(WirePresence::<String>::null());
        assert_round_trip(WirePresence::value("user-123".to_owned()));
    }

    #[test]
    fn wire_presence_bool_round_trip_never_collapses() {
        assert_round_trip(WirePresence::<bool>::missing());
        assert_round_trip(WirePresence::<bool>::null());
        assert_round_trip(WirePresence::value(true));
    }

    #[test]
    fn wire_presence_u32_round_trip_never_collapses() {
        assert_round_trip(WirePresence::<u32>::missing());
        assert_round_trip(WirePresence::<u32>::null());
        assert_round_trip(WirePresence::value(17));
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum TaggedBlock {
        Text { text: String },
    }

    #[test]
    fn wire_presence_tagged_enum_round_trip_never_collapses() {
        assert_round_trip(WirePresence::<TaggedBlock>::missing());
        assert_round_trip(WirePresence::<TaggedBlock>::null());
        assert_round_trip(WirePresence::value(TaggedBlock::Text {
            text: "hi".to_owned(),
        }));
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Nested {
        alpha: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        beta: Option<u32>,
    }

    #[test]
    fn wire_presence_nested_struct_round_trip_never_collapses() {
        assert_round_trip(WirePresence::<Nested>::missing());
        assert_round_trip(WirePresence::<Nested>::null());
        assert_round_trip(WirePresence::value(Nested {
            alpha: "a".to_owned(),
            beta: Some(2),
        }));
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ReqRoundTripHolder {
        #[serde(default, skip_serializing_if = "RequestPresence::is_absent")]
        field: RequestPresence<String>,
    }

    #[test]
    fn request_presence_round_trip_never_collapses() {
        for original in [
            RequestPresence::omitted(),
            RequestPresence::null(),
            RequestPresence::value("high".to_owned()),
        ] {
            let holder = ReqRoundTripHolder { field: original.clone() };
            let first = serde_json::to_string(&holder).expect("serialize holder");
            let back: ReqRoundTripHolder = serde_json::from_str(&first).expect("deserialize holder");
            let second = serde_json::to_string(&back).expect("re-serialize holder");
            assert_eq!(back.field, original, "state collapsed across round trip (bytes: {first})");
            assert_eq!(second, first, "serialization is not byte-stable");
        }
    }

    /// 46b SDD §4.6-2 (qwen NIT-4): generic T-cell holder — the same field
    /// pattern as `ReqRoundTripHolder`, widened so the 46b DTO's T set (bool,
    /// u32, tagged enum, nested struct) is round-trip-pinned.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(bound = "T: Serialize + DeserializeOwned")]
    struct ReqTCellHolder<T: Serialize + DeserializeOwned> {
        #[serde(default, skip_serializing_if = "RequestPresence::is_absent")]
        field: RequestPresence<T>,
    }

    fn assert_req_round_trip<T>(original: RequestPresence<T>)
    where
        T: Clone + std::fmt::Debug + PartialEq + Serialize + DeserializeOwned,
    {
        let holder = ReqTCellHolder { field: original.clone() };
        let first = serde_json::to_string(&holder).expect("serialize holder");
        let back: ReqTCellHolder<T> = serde_json::from_str(&first).expect("deserialize holder");
        let second = serde_json::to_string(&back).expect("re-serialize holder");
        assert_eq!(back.field, original, "state collapsed across round trip (bytes: {first})");
        assert_eq!(second, first, "serialization is not byte-stable");
    }

    #[test]
    fn request_presence_bool_round_trip_never_collapses() {
        assert_req_round_trip(RequestPresence::<bool>::omitted());
        assert_req_round_trip(RequestPresence::<bool>::null());
        assert_req_round_trip(RequestPresence::value(true));
    }

    #[test]
    fn request_presence_u32_round_trip_never_collapses() {
        assert_req_round_trip(RequestPresence::<u32>::omitted());
        assert_req_round_trip(RequestPresence::<u32>::null());
        assert_req_round_trip(RequestPresence::value(42));
    }

    #[test]
    fn request_presence_tagged_enum_round_trip_never_collapses() {
        // `OutputFormat` has no PartialEq derive, so this cell pins state +
        // exact holder bytes instead of value equality.
        for (original, expect) in [
            (RequestPresence::<OutputFormat>::omitted(), r#"{}"#),
            (RequestPresence::null(), r#"{"field":null}"#),
            (
                RequestPresence::value(OutputFormat::JsonSchema {
                    schema: serde_json::json!({ "type": "object" }),
                }),
                r#"{"field":{"type":"json_schema","schema":{"type":"object"}}}"#,
            ),
        ] {
            let holder = ReqTCellHolder { field: original.clone() };
            let first = serde_json::to_string(&holder).expect("serialize holder");
            let back: ReqTCellHolder<OutputFormat> =
                serde_json::from_str(&first).expect("deserialize holder");
            let second = serde_json::to_string(&back).expect("re-serialize holder");
            assert_eq!(first, expect, "tagged-enum holder bytes drifted");
            assert_eq!(
                back.field.is_absent(),
                original.is_absent(),
                "state collapsed (bytes: {first})"
            );
            assert_eq!(
                back.field.is_null(),
                original.is_null(),
                "state collapsed (bytes: {first})"
            );
            assert_eq!(second, first, "serialization is not byte-stable");
        }
    }

    #[test]
    fn request_presence_nested_struct_round_trip_never_collapses() {
        assert_req_round_trip(RequestPresence::<Nested>::omitted());
        assert_req_round_trip(RequestPresence::<Nested>::null());
        assert_req_round_trip(RequestPresence::value(Nested {
            alpha: "a".to_owned(),
            beta: Some(2),
        }));
    }

    #[test]
    fn defaults_match_spec_states() {
        assert!(WirePresence::<String>::default().is_missing());
        assert!(RequestPresence::<String>::default().is_omitted());
    }

    #[test]
    fn accessors_distinguish_states() {
        let missing = WirePresence::<u32>::missing();
        assert!(missing.is_absent() && missing.is_missing() && !missing.is_null());
        assert_eq!(missing.as_ref(), None);
        let null = WirePresence::<u32>::null();
        assert!(!null.is_absent() && null.is_null() && !null.is_missing());
        assert_eq!(null.as_ref(), None);
        let value = WirePresence::value(9);
        assert!(!value.is_absent() && !value.is_null());
        assert_eq!(value.as_ref(), Some(&9));
        let omitted = RequestPresence::<u32>::omitted();
        assert!(omitted.is_absent() && omitted.is_omitted() && !omitted.is_null());
        assert_eq!(omitted.as_ref(), None);
        let req_value = RequestPresence::value(9);
        assert_eq!(req_value.as_ref(), Some(&9));
    }

    // ------------------------------------------------------------------
    // Checked edges (SDD §4.1-3)
    // ------------------------------------------------------------------

    #[test]
    fn explicit_null_is_representable_as_null_state() {
        // An explicit null on a Value-only-semantic field is representable — the carrier
        // never silently coerces it away.
        let holder: RoundTripHolder<u32> = serde_json::from_str(r#"{"field":null}"#).expect("parse");
        assert!(holder.field.is_null());
        let req: ReqRoundTripHolder = serde_json::from_str(r#"{"field":null}"#).expect("parse");
        assert!(req.field.is_null());
    }

    #[test]
    fn wrong_type_value_propagates_t_serde_error() {
        // A wrong-typed value must surface T's serde error — not Null, not a default.
        let bad: Result<RoundTripHolder<u32>, _> = serde_json::from_str(r#"{"field":"abc"}"#);
        let err = bad.expect_err("wrong type must not deserialize");
        assert!(err.to_string().contains("invalid type"), "got: {err}");
        let bad_req: Result<ReqRoundTripHolder, _> = serde_json::from_str(r#"{"field":1}"#);
        assert!(bad_req.is_err(), "number into String must not deserialize");
    }

    #[test]
    fn into_value_is_checked_on_missing_and_null() {
        let missing = WirePresence::<u32>::missing().into_value();
        assert!(matches!(missing, Err(WirePresenceError::Missing)));
        let null = WirePresence::<u32>::null().into_value();
        assert!(matches!(null, Err(WirePresenceError::Null)));
        assert_eq!(WirePresence::value(7).into_value(), Ok(7));
        let omitted = RequestPresence::<u32>::omitted().into_value();
        assert!(matches!(omitted, Err(RequestPresenceError::Omitted)));
        let req_null = RequestPresence::<u32>::null().into_value();
        assert!(matches!(req_null, Err(RequestPresenceError::Null)));
        assert_eq!(RequestPresence::value(7).into_value(), Ok(7));
    }

    #[test]
    fn error_display_names_carrier_and_state() {
        for message in [
            WirePresenceError::Missing.to_string(),
            WirePresenceError::Null.to_string(),
            RequestPresenceError::Omitted.to_string(),
            RequestPresenceError::Null.to_string(),
        ] {
            let names_carrier = message.contains("WirePresence") || message.contains("RequestPresence");
            let names_state = message.contains("Missing")
                || message.contains("Null")
                || message.contains("Omitted");
            assert!(names_carrier && names_state, "error must name carrier and state: {message}");
        }
    }

    #[derive(Debug, Deserialize)]
    struct NoDefaultHolder {
        #[serde(skip_serializing_if = "WirePresence::is_absent")]
        field: WirePresence<String>,
    }

    #[test]
    fn absent_required_carrier_field_deserializes_as_null() {
        // serde 1.0.228's `private::de::missing_field` deserializes a missing
        // required field through a `MissingFieldDeserializer` whose
        // `deserialize_option` arm returns `visit_none`. Because the carriers'
        // Deserialize impls route through `Option::<T>::deserialize` (house
        // `double_option` idiom), an ABSENT member on a REQUIRED carrier field
        // (no `#[serde(default)]`) deserializes as Null — not as the standard
        // required-field error. (Divergence from SDD §4.1-3's checked-edge
        // expectation, pinned here so a serde behavior change fails loudly.)
        // The documented field pattern uses `#[serde(default)]`, where absence
        // maps to Missing/Omitted via `Default` (see the round-trip tests);
        // required carrier fields are outside the spec's modeled surface
        // (optional-nullable fields are always optional).
        let holder: NoDefaultHolder = serde_json::from_str("{}").expect("serde 1.0.228 missing-field → deserialize_option → visit_none");
        assert!(holder.field.is_null());
        // Control: a non-null-accepting required field still gets the standard
        // required-field error.
        let control: Result<ControlHolder, _> = serde_json::from_str("{}");
        match control {
            Ok(holder) => {
                let _ = holder.field;
                panic!("plain required field must error, got Ok");
            }
            Err(err) => assert!(err.to_string().contains("missing field `field`"), "got: {err}"),
        }
    }

    #[derive(Debug, Deserialize)]
    struct ControlHolder {
        field: String,
    }

    #[test]
    fn metadata_user_id_value_survives_round_trip() {
        // Sanity for the 46b target shape: a Value user_id already round-trips today —
        // only the Null state collapses (RED-1).
        let raw = r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"metadata":{"user_id":"user-1"}}"#;
        let req: MessagesRequest = serde_json::from_str(raw).expect("deserialize");
        let out = serde_json::to_string(&req).expect("serialize");
        let round: serde_json::Value = serde_json::from_str(&out).expect("re-parse");
        let user_id = round
            .get("metadata")
            .and_then(|metadata| metadata.get("user_id"))
            .and_then(|value| value.as_str());
        assert_eq!(user_id, Some("user-1"));
    }

    // ------------------------------------------------------------------
    // Explicit-null rejection (spec A0 L97-98; 46b SDD §4.3): one per struct
    // touched — a wire `null` on an omission-only field is now a parse error.
    // Direct fields and the internally-tagged `ThinkingConfig` path report the
    // helper's custom message (serde 1.0.228 enum_internally.rs: variant errors
    // propagate — verified against registry source); the untagged `SystemParam`
    // path reports serde's generic fallthrough instead (enum_untagged.rs:
    // all-variants-failed, per-variant errors NOT propagated — hence its
    // Err-only assert).
    // ------------------------------------------------------------------

    #[test]
    fn request_metadata_null_wrapper_rejected() {
        let raw = r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"metadata":null}"#;
        let err = serde_json::from_str::<MessagesRequest>(raw).expect_err("null metadata must be rejected");
        assert!(
            err.to_string().contains("explicit null is not representable"),
            "error should name the rejection: {err}"
        );
    }

    #[test]
    fn request_output_config_null_wrapper_rejected() {
        let raw = r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"output_config":null}"#;
        let err = serde_json::from_str::<MessagesRequest>(raw).expect_err("null output_config must be rejected");
        assert!(
            err.to_string().contains("explicit null is not representable"),
            "error should name the rejection: {err}"
        );
    }

    #[test]
    fn request_temperature_null_rejected() {
        let raw = r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"temperature":null}"#;
        let err = serde_json::from_str::<MessagesRequest>(raw).expect_err("null temperature must be rejected");
        assert!(
            err.to_string().contains("explicit null is not representable"),
            "error should name the rejection: {err}"
        );
    }

    #[test]
    fn request_text_block_cache_control_null_rejected() {
        let raw = r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"system":[{"type":"text","text":"hi","cache_control":null}]}"#;
        assert!(
            serde_json::from_str::<MessagesRequest>(raw).is_err(),
            "null cache_control in a system block must be rejected"
        );
    }

    #[test]
    fn request_thinking_display_null_rejected() {
        let raw = r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"thinking":{"type":"adaptive","display":null}}"#;
        let err = serde_json::from_str::<MessagesRequest>(raw)
            .expect_err("null thinking.display must be rejected");
        assert!(
            err.to_string().contains("explicit null is not representable"),
            "internally-tagged path must propagate the helper's message: {err}"
        );
    }
}

#[cfg(test)]
mod presence_goldens {
    use super::*;
    use crate::messages::{
        CacheControl, CacheCreation, Message, MessageContent, MessageDeltaBody,
        MessageDeltaUsage, MessageRole, Metadata, MessagesRequest, MessagesRequestParts,
        MessagesResponse, OutputConfig, OutputFormat, OutputTokensDetails,
    };
    use serde_json::json;

    // ------------------------------------------------------------------
    // (a) DTO-level goldens (SDD §4.1-3a): omitted + value byte pins on a
    // minimal full MessagesRequest. Struct field order = deterministic
    // member order; serde_json::to_string byte equality.
    // ------------------------------------------------------------------

    const MINIMAL_REQUEST_BYTES: &str =
        r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1}"#;
    const USER_ID_OMITTED_BYTES: &str =
        r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"metadata":{}}"#;
    const USER_ID_VALUE_BYTES: &str = r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"metadata":{"user_id":"user-1"}}"#;
    const USER_ID_NULL_BYTES: &str = r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"metadata":{"user_id":null}}"#;
    const OUTPUT_CONFIG_OMITTED_BYTES: &str =
        r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"output_config":{}}"#;
    const EFFORT_VALUE_BYTES: &str = r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"output_config":{"effort":"high"}}"#;
    const EFFORT_NULL_BYTES: &str = r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"output_config":{"effort":null}}"#;
    const FORMAT_VALUE_BYTES: &str = r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"output_config":{"format":{"type":"json_schema","schema":{"type":"object"}}}}"#;
    const FORMAT_NULL_BYTES: &str = r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}],"max_tokens":1,"output_config":{"format":null}}"#;

    // 46c response-side A0 pins (spec L104-110; 46c sub-SDD §6). Shared start
    // base (the G13 base): member order = struct order; the ADDed
    // `MessagesResponse` members are LAST so the pre-46c base stays a byte
    // prefix of every pin.
    const G1_START_BASE: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1}}"#;
    const G1_START_NULL: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":null}}"#;
    const G1_START_VALUE: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":7}}"#;
    const G11_DELTA_STOP_DETAILS_OMITTED: &str = r#"{"stop_reason":"refusal"}"#;
    const G11_DELTA_STOP_DETAILS_NULL: &str = r#"{"stop_reason":"refusal","stop_details":null}"#;
    const G11_DELTA_STOP_DETAILS_VALUE: &str = r#"{"stop_reason":"refusal","stop_details":{"type":"refusal","category":"frontier_llm","explanation":"This request was blocked."}}"#;
    const G13_START_CONTAINER_NULL: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1},"container":null}"#;
    const G13_START_CONTAINER_VALUE: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1},"container":{"kind":"sandbox"}}"#;
    const G14_START_STOP_DETAILS_NULL: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1},"stop_details":null}"#;
    const G14_START_STOP_DETAILS_VALUE: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1},"stop_details":{"type":"refusal","category":"frontier_llm","explanation":"This request was blocked."}}"#;
    const G2_START_NULL: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":null}}"#;
    const G2_START_VALUE: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":500}}"#;
    const G3_START_NULL: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1,"output_tokens_details":null}}"#;
    const G3_START_VALUE: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1,"output_tokens_details":{"thinking_tokens":9}}}"#;
    const G4_START_NULL: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1,"cache_creation":null}}"#;
    const G4_START_VALUE: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1,"cache_creation":{"ephemeral_5m_input_tokens":3,"ephemeral_1h_input_tokens":0}}}"#;
    const G5_INPUT_MISSING: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"output_tokens":1}}"#;
    const G5_INPUT_NULL: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":null,"output_tokens":1}}"#;
    const G5_OUTPUT_MISSING: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1}}"#;
    const G5_OUTPUT_NULL: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":null}}"#;
    const G6_DELTA_INPUT_OMITTED: &str = r#"{"output_tokens":1}"#;
    const G6_DELTA_INPUT_NULL: &str = r#"{"output_tokens":1,"input_tokens":null}"#;
    const G6_DELTA_INPUT_VALUE: &str = r#"{"output_tokens":1,"input_tokens":42}"#;
    const G7_DELTA_CR_OMITTED: &str = r#"{"output_tokens":1}"#;
    const G7_DELTA_CR_NULL: &str = r#"{"output_tokens":1,"cache_read_input_tokens":null}"#;
    const G7_DELTA_CR_VALUE: &str = r#"{"output_tokens":1,"cache_read_input_tokens":500}"#;
    const G8_DELTA_CC_INPUT_OMITTED: &str = r#"{"output_tokens":1}"#;
    const G8_DELTA_CC_INPUT_NULL: &str = r#"{"output_tokens":1,"cache_creation_input_tokens":null}"#;
    const G8_DELTA_CC_INPUT_VALUE: &str = r#"{"output_tokens":1,"cache_creation_input_tokens":200}"#;
    const G9_DELTA_OTD_OMITTED: &str = r#"{"output_tokens":1}"#;
    const G9_DELTA_OTD_NULL: &str = r#"{"output_tokens":1,"output_tokens_details":null}"#;
    const G9_DELTA_OTD_VALUE: &str = r#"{"output_tokens":1,"output_tokens_details":{"thinking_tokens":9}}"#;
    const G10_DELTA_OUTPUT_MISSING: &str = r#"{"input_tokens":1,"cache_read_input_tokens":1,"cache_creation_input_tokens":1,"output_tokens_details":{"thinking_tokens":1}}"#;
    const G10_DELTA_OUTPUT_NULL: &str = r#"{"output_tokens":null,"input_tokens":1,"cache_read_input_tokens":1,"cache_creation_input_tokens":1,"output_tokens_details":{"thinking_tokens":1}}"#;
    const G12_DELTA_CONTAINER_OMITTED: &str = r#"{"stop_reason":"end_turn"}"#;
    const G12_DELTA_CONTAINER_NULL: &str = r#"{"stop_reason":"end_turn","container":null}"#;
    const G12_DELTA_CONTAINER_VALUE: &str = r#"{"stop_reason":"end_turn","container":{"kind":"sandbox"}}"#;
    const G15_TTL_NULL: &str = r#"{"type":"ephemeral","ttl":null}"#;
    const G15_TTL_OMITTED_BYTES: &str = r#"{"type":"ephemeral"}"#;

    fn minimal_request() -> MessagesRequest {
        minimal_request_with(None, None)
    }

    /// 47b migration: the DTO goldens set one optional member per test —
    /// construction routes through the in-crate from_parts seam (the
    /// struct fields are private to the messages module).
    fn minimal_request_with(
        metadata: Option<Metadata>,
        output_config: Option<OutputConfig>,
    ) -> MessagesRequest {
        MessagesRequest::from_parts(MessagesRequestParts {
            model: "m-1".to_owned(),
            messages: vec![Message {
                role: MessageRole::User,
                content: MessageContent::Text("hi".to_owned()),
            }],
            max_tokens: 1,
            metadata,
            output_config,
            ..Default::default()
        })
    }

    #[test]
    fn dto_golden_minimal_request_omits_all_optional_members() {
        let bytes = serde_json::to_string(&minimal_request()).expect("serialize");
        assert_eq!(bytes, MINIMAL_REQUEST_BYTES);
    }

    #[test]
    fn dto_golden_user_id_omitted_is_empty_metadata_object() {
        // Current wire truth (46a pin, byte-stable across 46b): an omitted
        // `user_id` re-serializes as an empty `metadata` object — qwen NIT-7:
        // it is the NULL row (USER_ID_NULL_BYTES) that 46b adds, not this one.
        let req = minimal_request_with(Some(Metadata { user_id: RequestPresence::omitted() }), None);
        let bytes = serde_json::to_string(&req).expect("serialize");
        assert_eq!(bytes, USER_ID_OMITTED_BYTES);
    }

    #[test]
    fn dto_golden_user_id_value_bytes() {
        let req = minimal_request_with(Some(Metadata {
            user_id: RequestPresence::value("user-1".to_owned()),
        }), None);
        let bytes = serde_json::to_string(&req).expect("serialize");
        assert_eq!(bytes, USER_ID_VALUE_BYTES);
    }

    #[test]
    fn dto_golden_user_id_null_bytes() {
        let req = minimal_request_with(Some(Metadata {
            user_id: RequestPresence::null(),
        }), None);
        let bytes = serde_json::to_string(&req).expect("serialize");
        assert_eq!(bytes, USER_ID_NULL_BYTES);
        let back: MessagesRequest =
            serde_json::from_str(USER_ID_NULL_BYTES).expect("deserialize null row");
        assert!(back.metadata().expect("metadata present").user_id.is_null());
        assert_eq!(
            serde_json::to_string(&back).expect("re-serialize"),
            USER_ID_NULL_BYTES
        );
    }

    #[test]
    fn dto_golden_effort_omitted_is_empty_output_config_object() {
        let req = minimal_request_with(None, Some(OutputConfig {
            effort: RequestPresence::omitted(),
            format: RequestPresence::omitted(),
        }));
        let bytes = serde_json::to_string(&req).expect("serialize");
        assert_eq!(bytes, OUTPUT_CONFIG_OMITTED_BYTES);
    }

    #[test]
    fn dto_golden_format_omitted_is_empty_output_config_object() {
        let req = minimal_request_with(None, Some(OutputConfig {
            effort: RequestPresence::omitted(),
            format: RequestPresence::omitted(),
        }));
        let bytes = serde_json::to_string(&req).expect("serialize");
        assert_eq!(bytes, OUTPUT_CONFIG_OMITTED_BYTES);
    }

    #[test]
    fn dto_golden_effort_value_bytes() {
        let req = minimal_request_with(None, Some(OutputConfig {
            effort: RequestPresence::value("high".to_owned()),
            format: RequestPresence::omitted(),
        }));
        let bytes = serde_json::to_string(&req).expect("serialize");
        assert_eq!(bytes, EFFORT_VALUE_BYTES);
    }

    #[test]
    fn dto_golden_effort_null_bytes() {
        let req = minimal_request_with(None, Some(OutputConfig {
            effort: RequestPresence::null(),
            format: RequestPresence::omitted(),
        }));
        let bytes = serde_json::to_string(&req).expect("serialize");
        assert_eq!(bytes, EFFORT_NULL_BYTES);
        let back: MessagesRequest =
            serde_json::from_str(EFFORT_NULL_BYTES).expect("deserialize null row");
        assert!(back.output_config().expect("output_config present").effort.is_null());
        assert_eq!(
            serde_json::to_string(&back).expect("re-serialize"),
            EFFORT_NULL_BYTES
        );
    }

    #[test]
    fn dto_golden_format_value_bytes() {
        let req = minimal_request_with(None, Some(OutputConfig {
            effort: RequestPresence::omitted(),
            format: RequestPresence::value(OutputFormat::JsonSchema {
                schema: json!({ "type": "object" }),
            }),
        }));
        let bytes = serde_json::to_string(&req).expect("serialize");
        assert_eq!(bytes, FORMAT_VALUE_BYTES);
    }

    #[test]
    fn dto_golden_format_null_bytes() {
        let req = minimal_request_with(None, Some(OutputConfig {
            effort: RequestPresence::omitted(),
            format: RequestPresence::null(),
        }));
        let bytes = serde_json::to_string(&req).expect("serialize");
        assert_eq!(bytes, FORMAT_NULL_BYTES);
        let back: MessagesRequest =
            serde_json::from_str(FORMAT_NULL_BYTES).expect("deserialize null row");
        assert!(back.output_config().expect("output_config present").format.is_null());
        assert_eq!(
            serde_json::to_string(&back).expect("re-serialize"),
            FORMAT_NULL_BYTES
        );
    }

    // ------------------------------------------------------------------
    // 46c response-side DTO goldens (spec A0 L104-110 + L4254-4260):
    // 3-state byte pins on the real response DTOs. RED-C2 (46c sub-SDD §5):
    // the byte pins are authored active on the UNCHANGED production tree —
    // the carrier state asserts (is_missing/is_null/as_ref) compile only
    // post cut A and are added with GREEN-DTO (no pin edit: the pins and
    // byte asserts below are byte-identical between red and green).
    // ------------------------------------------------------------------

    #[test]
    fn dto_golden_usage_cc_input_omitted_null_value() {
        // omitted (L109 Q8: preserve Missing — the member must vanish)
        let v: MessagesResponse = serde_json::from_str(G1_START_BASE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G1_START_BASE,
            "omitted state byte pin"
        );
        assert!(v.usage.cache_creation_input_tokens.is_missing());
        // null (L109 Q8: preserve Null — an explicit null must not collapse)
        let v: MessagesResponse = serde_json::from_str(G1_START_NULL).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G1_START_NULL,
            "null state byte pin"
        );
        assert!(v.usage.cache_creation_input_tokens.is_null());
        // value (L109 Q8: preserve Value — typed bytes unchanged)
        let v: MessagesResponse = serde_json::from_str(G1_START_VALUE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G1_START_VALUE,
            "value state byte pin"
        );
        assert_eq!(v.usage.cache_creation_input_tokens.as_ref(), Some(&7u32));
    }

    #[test]
    fn dto_golden_delta_stop_details_omitted_null_value() {
        // omitted (L110 Q9: preserve Missing — the member must vanish)
        let v: MessageDeltaBody =
            serde_json::from_str(G11_DELTA_STOP_DETAILS_OMITTED).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G11_DELTA_STOP_DETAILS_OMITTED,
            "omitted state byte pin"
        );
        assert!(v.stop_details.is_missing());
        // null (L110 Q9: preserve Null — an explicit null must not collapse into omission)
        let v: MessageDeltaBody = serde_json::from_str(G11_DELTA_STOP_DETAILS_NULL).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G11_DELTA_STOP_DETAILS_NULL,
            "null state byte pin"
        );
        assert!(v.stop_details.is_null());
        // value (L110 Q9: preserve Value — the terminal delta's own stop details)
        let v: MessageDeltaBody =
            serde_json::from_str(G11_DELTA_STOP_DETAILS_VALUE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G11_DELTA_STOP_DETAILS_VALUE,
            "value state byte pin"
        );
        let details = v.stop_details.as_ref().expect("stop_details must be Value");
        assert_eq!(details.r#type.as_deref(), Some("refusal"));
        assert_eq!(details.category.as_deref(), Some("frontier_llm"));
        assert_eq!(
            details.explanation.as_deref(),
            Some("This request was blocked.")
        );
    }

    #[test]
    fn dto_golden_start_container_omitted_null_value() {
        // omitted (L107 Q6: preserve Missing — the member must vanish)
        let v: MessagesResponse = serde_json::from_str(G1_START_BASE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G1_START_BASE,
            "omitted state byte pin"
        );
        assert!(v.container.is_missing());
        // null (L107 Q6: preserve Null — the ADDed member must not be dropped)
        let v: MessagesResponse =
            serde_json::from_str(G13_START_CONTAINER_NULL).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G13_START_CONTAINER_NULL,
            "null state byte pin"
        );
        assert!(v.container.is_null());
        // value (L107 Q6: preserve Value — the 46a mirror byte target)
        let v: MessagesResponse =
            serde_json::from_str(G13_START_CONTAINER_VALUE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G13_START_CONTAINER_VALUE,
            "value state byte pin"
        );
        assert_eq!(v.container.as_ref(), Some(&json!({"kind": "sandbox"})));
    }

    #[test]
    fn dto_golden_start_stop_details_omitted_null_value() {
        // omitted (L108 Q7: preserve Missing — the member must vanish)
        let v: MessagesResponse = serde_json::from_str(G1_START_BASE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G1_START_BASE,
            "omitted state byte pin"
        );
        assert!(v.stop_details.is_missing());
        // null (L108 Q7: preserve Null — the ADDed member must not be dropped)
        let v: MessagesResponse =
            serde_json::from_str(G14_START_STOP_DETAILS_NULL).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G14_START_STOP_DETAILS_NULL,
            "null state byte pin"
        );
        assert!(v.stop_details.is_null());
        // value (L108 Q7: preserve Value — the start event's own stop details)
        let v: MessagesResponse =
            serde_json::from_str(G14_START_STOP_DETAILS_VALUE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G14_START_STOP_DETAILS_VALUE,
            "value state byte pin"
        );
        let details = v.stop_details.as_ref().expect("stop_details must be Value");
        assert_eq!(details.r#type.as_deref(), Some("refusal"));
        assert_eq!(details.category.as_deref(), Some("frontier_llm"));
        assert_eq!(
            details.explanation.as_deref(),
            Some("This request was blocked.")
        );
    }

    #[test]
    fn dto_golden_usage_cr_input_omitted_null_value() {
        // omitted (L109 Q8: preserve Missing — the member must vanish)
        let v: MessagesResponse = serde_json::from_str(G1_START_BASE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G1_START_BASE,
            "omitted state byte pin"
        );
        assert!(v.usage.cache_read_input_tokens.is_missing());
        // null (L109 Q8: preserve Null — an explicit null must not collapse)
        let v: MessagesResponse = serde_json::from_str(G2_START_NULL).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G2_START_NULL,
            "null state byte pin"
        );
        assert!(v.usage.cache_read_input_tokens.is_null());
        // value (L109 Q8: preserve Value — typed bytes unchanged)
        let v: MessagesResponse = serde_json::from_str(G2_START_VALUE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G2_START_VALUE,
            "value state byte pin"
        );
        assert_eq!(v.usage.cache_read_input_tokens.as_ref(), Some(&500u32));
    }

    #[test]
    fn dto_golden_usage_output_tokens_details_omitted_null_value() {
        // omitted (L109 Q8: preserve Missing — the member must vanish)
        let v: MessagesResponse = serde_json::from_str(G1_START_BASE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G1_START_BASE,
            "omitted state byte pin"
        );
        assert!(v.usage.output_tokens_details.is_missing());
        // null (L109 Q8: preserve Null — an explicit null must not collapse)
        let v: MessagesResponse = serde_json::from_str(G3_START_NULL).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G3_START_NULL,
            "null state byte pin"
        );
        assert!(v.usage.output_tokens_details.is_null());
        // value (L109 Q8: preserve Value — typed bytes unchanged)
        let v: MessagesResponse = serde_json::from_str(G3_START_VALUE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G3_START_VALUE,
            "value state byte pin"
        );
        assert_eq!(
            v.usage.output_tokens_details.as_ref(),
            Some(&OutputTokensDetails { thinking_tokens: 9 })
        );
    }

    #[test]
    fn dto_golden_usage_cache_creation_omitted_null_value() {
        // omitted (L109 Q8: preserve Missing — the member must vanish)
        let v: MessagesResponse = serde_json::from_str(G1_START_BASE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G1_START_BASE,
            "omitted state byte pin"
        );
        assert!(v.usage.cache_creation.is_missing());
        // null (L109 Q8: preserve Null — an explicit null must not collapse)
        let v: MessagesResponse = serde_json::from_str(G4_START_NULL).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G4_START_NULL,
            "null state byte pin"
        );
        assert!(v.usage.cache_creation.is_null());
        // value (L109 Q8 + L4257: start-only — retains its exact start state)
        let v: MessagesResponse = serde_json::from_str(G4_START_VALUE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G4_START_VALUE,
            "value state byte pin"
        );
        assert_eq!(
            v.usage.cache_creation.as_ref(),
            Some(&CacheCreation {
                ephemeral_5m_input_tokens: 3,
                ephemeral_1h_input_tokens: 0,
            })
        );
    }

    #[test]
    fn dto_golden_usage_input_output_tokens_required() {
        // L109 Q8: input_tokens / output_tokens are REQUIRED bare u32 — absence and
        // explicit null must both fail the parse (pin against a future default/Option
        // drift).
        assert!(
            serde_json::from_str::<MessagesResponse>(G5_INPUT_MISSING).is_err(),
            "input_tokens missing must fail"
        );
        assert!(
            serde_json::from_str::<MessagesResponse>(G5_INPUT_NULL).is_err(),
            "input_tokens null must fail"
        );
        assert!(
            serde_json::from_str::<MessagesResponse>(G5_OUTPUT_MISSING).is_err(),
            "output_tokens missing must fail"
        );
        assert!(
            serde_json::from_str::<MessagesResponse>(G5_OUTPUT_NULL).is_err(),
            "output_tokens null must fail"
        );
    }

    #[test]
    fn dto_golden_delta_usage_input_omitted_null_value() {
        // omitted (L104 Q3: preserve Missing — retain the message_start value)
        let v: MessageDeltaUsage = serde_json::from_str(G6_DELTA_INPUT_OMITTED).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G6_DELTA_INPUT_OMITTED,
            "omitted state byte pin"
        );
        assert!(v.input_tokens.is_missing());
        // null (L104 Q3: preserve Null — retain the message_start value)
        let v: MessageDeltaUsage = serde_json::from_str(G6_DELTA_INPUT_NULL).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G6_DELTA_INPUT_NULL,
            "null state byte pin"
        );
        assert!(v.input_tokens.is_null());
        // value (L104 Q3: a present non-null value replaces the start value)
        let v: MessageDeltaUsage = serde_json::from_str(G6_DELTA_INPUT_VALUE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G6_DELTA_INPUT_VALUE,
            "value state byte pin"
        );
        assert_eq!(v.input_tokens.as_ref(), Some(&42u32));
    }

    #[test]
    fn dto_golden_delta_usage_cache_read_omitted_null_value() {
        // omitted (L104 Q3: preserve Missing — retain the message_start value)
        let v: MessageDeltaUsage = serde_json::from_str(G7_DELTA_CR_OMITTED).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G7_DELTA_CR_OMITTED,
            "omitted state byte pin"
        );
        assert!(v.cache_read_input_tokens.is_missing());
        // null (L104 Q3: preserve Null — retain the message_start value)
        let v: MessageDeltaUsage = serde_json::from_str(G7_DELTA_CR_NULL).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G7_DELTA_CR_NULL,
            "null state byte pin"
        );
        assert!(v.cache_read_input_tokens.is_null());
        // value (L104 Q3: a present non-null value replaces the start value)
        let v: MessageDeltaUsage = serde_json::from_str(G7_DELTA_CR_VALUE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G7_DELTA_CR_VALUE,
            "value state byte pin"
        );
        assert_eq!(v.cache_read_input_tokens.as_ref(), Some(&500u32));
    }

    #[test]
    fn dto_golden_delta_usage_cache_creation_input_omitted_null_value() {
        // omitted (L104 Q3: preserve Missing — retain the message_start value)
        let v: MessageDeltaUsage =
            serde_json::from_str(G8_DELTA_CC_INPUT_OMITTED).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G8_DELTA_CC_INPUT_OMITTED,
            "omitted state byte pin"
        );
        assert!(v.cache_creation_input_tokens.is_missing());
        // null (L104 Q3: preserve Null — retain the message_start value)
        let v: MessageDeltaUsage = serde_json::from_str(G8_DELTA_CC_INPUT_NULL).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G8_DELTA_CC_INPUT_NULL,
            "null state byte pin"
        );
        assert!(v.cache_creation_input_tokens.is_null());
        // value (L104 Q3: a present non-null value replaces the start value)
        let v: MessageDeltaUsage =
            serde_json::from_str(G8_DELTA_CC_INPUT_VALUE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G8_DELTA_CC_INPUT_VALUE,
            "value state byte pin"
        );
        assert_eq!(v.cache_creation_input_tokens.as_ref(), Some(&200u32));
    }

    #[test]
    fn dto_golden_delta_usage_output_tokens_details_omitted_null_value() {
        // omitted (L104 Q3: preserve Missing — retain the message_start value)
        let v: MessageDeltaUsage = serde_json::from_str(G9_DELTA_OTD_OMITTED).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G9_DELTA_OTD_OMITTED,
            "omitted state byte pin"
        );
        assert!(v.output_tokens_details.is_missing());
        // null (L104 Q3: preserve Null — retain the message_start value)
        let v: MessageDeltaUsage = serde_json::from_str(G9_DELTA_OTD_NULL).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G9_DELTA_OTD_NULL,
            "null state byte pin"
        );
        assert!(v.output_tokens_details.is_null());
        // value (L104 Q3: a present non-null value replaces the start value)
        let v: MessageDeltaUsage = serde_json::from_str(G9_DELTA_OTD_VALUE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G9_DELTA_OTD_VALUE,
            "value state byte pin"
        );
        assert_eq!(
            v.output_tokens_details.as_ref(),
            Some(&OutputTokensDetails { thinking_tokens: 9 })
        );
    }

    #[test]
    fn dto_golden_delta_usage_output_tokens_required() {
        // L104 Q3 + L4254 Q14: output_tokens is REQUIRED and always replaces —
        // absence and explicit null must both fail the parse.
        assert!(
            serde_json::from_str::<MessageDeltaUsage>(G10_DELTA_OUTPUT_MISSING).is_err(),
            "output_tokens missing must fail"
        );
        assert!(
            serde_json::from_str::<MessageDeltaUsage>(G10_DELTA_OUTPUT_NULL).is_err(),
            "output_tokens null must fail"
        );
    }

    #[test]
    fn dto_golden_delta_container_omitted_null_value() {
        // omitted (L105 Q4: preserve Missing — the member must vanish)
        let v: MessageDeltaBody = serde_json::from_str(G12_DELTA_CONTAINER_OMITTED).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G12_DELTA_CONTAINER_OMITTED,
            "omitted state byte pin"
        );
        assert!(v.container.is_missing());
        // null (L105 Q4: preserve Null — the ADDed member must not be dropped)
        let v: MessageDeltaBody = serde_json::from_str(G12_DELTA_CONTAINER_NULL).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G12_DELTA_CONTAINER_NULL,
            "null state byte pin"
        );
        assert!(v.container.is_null());
        // value (L105 Q4: preserve Value — the 46a mirror byte target)
        let v: MessageDeltaBody = serde_json::from_str(G12_DELTA_CONTAINER_VALUE).expect("parse");
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G12_DELTA_CONTAINER_VALUE,
            "value state byte pin"
        );
        assert_eq!(v.container.as_ref(), Some(&json!({"kind": "sandbox"})));
    }

    #[test]
    fn dto_golden_cache_control_ttl_null_lenient() {
        // §4.6 leniency pin: ttl STAYS `Option<String>` on the SHARED type — an
        // explicit null parses as None and re-serializes as omission (any future
        // strictening fails loudly here).
        let v: CacheControl = serde_json::from_str(G15_TTL_NULL).expect("parse");
        assert_eq!(v.ttl, None);
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            G15_TTL_OMITTED_BYTES,
            "null ttl re-serializes as omission"
        );
    }

    // ------------------------------------------------------------------
    // (b) Carrier-level goldens (SDD §4.1-3b): all three states pinned for
    // every spec-named field via test-only mirror structs. The null-state
    // byte pins are 46a's proof; the full-DTO null-state goldens arrive in
    // 46b when the DTO carries the carrier. Mirrors are test-only — none of
    // these fields exist on the 46a production DTO (ABSENT rows below).
    // ------------------------------------------------------------------

    macro_rules! carrier_golden {
        ($test:ident, $holder:ident, $field:ident, $wire_name:literal, $omitted:expr, $null:expr, $value:expr, $value_bytes:expr) => {
            #[test]
            fn $test() {
                let null_bytes = concat!("{", "\"", $wire_name, "\":null}");
                for (original, want, expect_absent, expect_null) in [
                    ($omitted, "{}", true, false),
                    ($null, null_bytes, false, true),
                    ($value, $value_bytes, false, false),
                ] {
                    let holder = $holder { $field: original.clone() };
                    let first = serde_json::to_string(&holder).expect("serialize mirror");
                    assert_eq!(first, want, "golden byte pin");
                    let back: $holder = serde_json::from_str(&first).expect("deserialize mirror");
                    assert_eq!(back.$field.is_absent(), expect_absent, "state: absent");
                    assert_eq!(back.$field.is_null(), expect_null, "state: null");
                    let got = serde_json::to_value(&back.$field).expect("to_value back");
                    let want_value = serde_json::to_value(&original).expect("to_value original");
                    assert_eq!(got, want_value, "state: value content");
                    let second = serde_json::to_string(&back).expect("re-serialize mirror");
                    assert_eq!(second, first, "byte-stable across round trip");
                }
            }
        };
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct UserIdMirror {
        #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
        user_id: WirePresence<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct EffortMirror {
        #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
        effort: WirePresence<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct FormatMirror {
        #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
        format: WirePresence<OutputFormat>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct ToolCacheControlMirror {
        #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
        cache_control: WirePresence<CacheControl>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct EagerInputStreamingMirror {
        #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
        eager_input_streaming: WirePresence<bool>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct ToolTypeMirror {
        #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
        r#type: WirePresence<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TopLevelCacheControlMirror {
        #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
        cache_control: WirePresence<CacheControl>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct ContainerMirror {
        #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
        container: WirePresence<serde_json::Value>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct InferenceGeoMirror {
        #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
        inference_geo: WirePresence<String>,
    }

    carrier_golden!(
        golden_user_id_omitted_null_value,
        UserIdMirror,
        user_id,
        "user_id",
        WirePresence::missing(),
        WirePresence::null(),
        WirePresence::value("user-1".to_owned()),
        r#"{"user_id":"user-1"}"#
    );

    carrier_golden!(
        golden_effort_omitted_null_value,
        EffortMirror,
        effort,
        "effort",
        WirePresence::missing(),
        WirePresence::null(),
        WirePresence::value("high".to_owned()),
        r#"{"effort":"high"}"#
    );

    carrier_golden!(
        golden_format_omitted_null_value,
        FormatMirror,
        format,
        "format",
        WirePresence::missing(),
        WirePresence::null(),
        WirePresence::value(OutputFormat::JsonSchema {
            schema: json!({ "type": "object" }),
        }),
        r#"{"format":{"type":"json_schema","schema":{"type":"object"}}}"#
    );

    carrier_golden!(
        golden_tool_cache_control_omitted_null_value,
        ToolCacheControlMirror,
        cache_control,
        "cache_control",
        WirePresence::missing(),
        WirePresence::null(),
        WirePresence::value(CacheControl::ephemeral()),
        r#"{"cache_control":{"type":"ephemeral"}}"#
    );

    carrier_golden!(
        golden_eager_input_streaming_omitted_null_value,
        EagerInputStreamingMirror,
        eager_input_streaming,
        "eager_input_streaming",
        WirePresence::missing(),
        WirePresence::null(),
        WirePresence::value(true),
        r#"{"eager_input_streaming":true}"#
    );

    carrier_golden!(
        golden_tool_type_omitted_null_value,
        ToolTypeMirror,
        r#type,
        "type",
        WirePresence::missing(),
        WirePresence::null(),
        WirePresence::value("tool".to_owned()),
        r#"{"type":"tool"}"#
    );

    carrier_golden!(
        golden_top_level_cache_control_omitted_null_value,
        TopLevelCacheControlMirror,
        cache_control,
        "cache_control",
        WirePresence::missing(),
        WirePresence::null(),
        WirePresence::value(CacheControl::ephemeral()),
        r#"{"cache_control":{"type":"ephemeral"}}"#
    );

    carrier_golden!(
        golden_container_omitted_null_value,
        ContainerMirror,
        container,
        "container",
        WirePresence::missing(),
        WirePresence::null(),
        WirePresence::value(json!({ "kind": "sandbox" })),
        r#"{"container":{"kind":"sandbox"}}"#
    );

    carrier_golden!(
        golden_inference_geo_omitted_null_value,
        InferenceGeoMirror,
        inference_geo,
        "inference_geo",
        WirePresence::missing(),
        WirePresence::null(),
        WirePresence::value("global".to_owned()),
        r#"{"inference_geo":"global"}"#
    );

    // RequestPresence mirrors for the three PRESENT request fields (addition over the
    // SDD minimum: 46b wires RequestPresence into these DTO fields, so its carrier-level
    // byte pins are pinned here, not deferred).
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct RequestUserIdMirror {
        #[serde(default, skip_serializing_if = "RequestPresence::is_absent")]
        user_id: RequestPresence<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct RequestEffortMirror {
        #[serde(default, skip_serializing_if = "RequestPresence::is_absent")]
        effort: RequestPresence<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct RequestFormatMirror {
        #[serde(default, skip_serializing_if = "RequestPresence::is_absent")]
        format: RequestPresence<OutputFormat>,
    }

    carrier_golden!(
        golden_request_user_id_omitted_null_value,
        RequestUserIdMirror,
        user_id,
        "user_id",
        RequestPresence::omitted(),
        RequestPresence::null(),
        RequestPresence::value("user-1".to_owned()),
        r#"{"user_id":"user-1"}"#
    );

    carrier_golden!(
        golden_request_effort_omitted_null_value,
        RequestEffortMirror,
        effort,
        "effort",
        RequestPresence::omitted(),
        RequestPresence::null(),
        RequestPresence::value("high".to_owned()),
        r#"{"effort":"high"}"#
    );

    carrier_golden!(
        golden_request_format_omitted_null_value,
        RequestFormatMirror,
        format,
        "format",
        RequestPresence::omitted(),
        RequestPresence::null(),
        RequestPresence::value(OutputFormat::JsonSchema {
            schema: json!({ "type": "object" }),
        }),
        r#"{"format":{"type":"json_schema","schema":{"type":"object"}}}"#
    );

    // ------------------------------------------------------------------
    // DTO coverage inventory (A1-lite source gate; SDD §4.1-3): 9 spec-named
    // fields × classification. Mechanical scan over the live messages.rs,
    // anchored per-field inside the owning struct body — a refactor that
    // moves or renames a field fails loudly (the spec's source-gate intent).
    // 46a pins the current truth: 3 × PRESENT-Option + 6 × ABSENT. 46b flips
    // the three PRESENT-Option rows to PRESENT-RequestPresence (its TDD red).
    // ------------------------------------------------------------------

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum InventoryClass {
        // 46b truth pins 0 × PRESENT-Option: no row constructs this variant
        // anymore; F5 (apex-ayl.114) flipped `Tool.cache_control` into this class —
        // the total assert keeps the variant in the truth table.
        #[allow(dead_code)]
        PresentOption,
        PresentRequestPresence,
        PresentWirePresence,
        Absent,
    }

    struct InventoryRow {
        spec_field: &'static str,
        struct_name: &'static str,
        field: &'static str,
        /// Exact field-declaration line, for PRESENT-* rows.
        exact_decl: Option<&'static str>,
        expected: InventoryClass,
    }

    fn struct_body<'a>(src: &'a str, name: &str) -> Option<&'a str> {
        let marker = format!("pub struct {name} {{");
        let start = src.find(&marker)?;
        let rest = &src[start + marker.len() - 1..];
        let mut depth = 0i32;
        for (idx, byte) in rest.bytes().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&rest[..=idx]);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn has_field_decl(body: &str, field: &str) -> bool {
        body.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with(&format!("pub {field}:"))
                || trimmed.starts_with(&format!("pub r#{field}:"))
        })
    }

    #[test]
    fn dto_coverage_inventory_pins_46c_truth() {
        let src = include_str!("messages.rs");
        let rows: &[InventoryRow] = &[
            InventoryRow {
                spec_field: "Metadata.user_id",
                struct_name: "Metadata",
                field: "user_id",
                exact_decl: Some("pub user_id: RequestPresence<String>,"),
                expected: InventoryClass::PresentRequestPresence,
            },
            InventoryRow {
                spec_field: "OutputConfig.effort",
                struct_name: "OutputConfig",
                field: "effort",
                exact_decl: Some("pub effort: RequestPresence<String>,"),
                expected: InventoryClass::PresentRequestPresence,
            },
            InventoryRow {
                spec_field: "OutputConfig.format",
                struct_name: "OutputConfig",
                field: "format",
                exact_decl: Some("pub format: RequestPresence<OutputFormat>,"),
                expected: InventoryClass::PresentRequestPresence,
            },
            // F5 (apex-ayl.114): per-tool cache breakpoint landed — row flips
            // Absent → PRESENT-Option (46-series same-commit truth discipline).
            // MGW F1 (apex-ayl.115): the flat ToolParam struct became the
            // untagged Custom/Server union — the 46c truth re-derives the
            // Tool.* rows against `ToolCustom` (the renamed pre-cut flat
            // struct; same-commit flip, totals unchanged).
            InventoryRow {
                spec_field: "Tool.cache_control",
                struct_name: "ToolCustom",
                field: "cache_control",
                exact_decl: Some("pub cache_control: Option<CacheControl>,"),
                expected: InventoryClass::PresentOption,
            },
            InventoryRow {
                spec_field: "Tool.eager_input_streaming",
                struct_name: "ToolCustom",
                field: "eager_input_streaming",
                exact_decl: None,
                expected: InventoryClass::Absent,
            },
            InventoryRow {
                spec_field: "Tool.type",
                struct_name: "ToolCustom",
                field: "type",
                exact_decl: None,
                expected: InventoryClass::Absent,
            },
            InventoryRow {
                spec_field: "top-level cache_control",
                struct_name: "MessagesRequest",
                field: "cache_control",
                exact_decl: None,
                expected: InventoryClass::Absent,
            },
            InventoryRow {
                spec_field: "container",
                struct_name: "MessagesRequest",
                field: "container",
                exact_decl: None,
                expected: InventoryClass::Absent,
            },
            InventoryRow {
                spec_field: "inference_geo",
                struct_name: "MessagesRequest",
                field: "inference_geo",
                exact_decl: None,
                expected: InventoryClass::Absent,
            },
            // 46c response-side A0 closure (spec L103-111): 12 × PRESENT-WirePresence
            // (9 carrier rewrites + 3 ADDs; §4.7 of the 46c sub-SDD).
            InventoryRow {
                spec_field: "message_start.message.usage.cache_creation_input_tokens",
                struct_name: "MessagesUsage",
                field: "cache_creation_input_tokens",
                exact_decl: Some("pub cache_creation_input_tokens: WirePresence<u32>,"),
                expected: InventoryClass::PresentWirePresence,
            },
            InventoryRow {
                spec_field: "message_start.message.usage.cache_read_input_tokens",
                struct_name: "MessagesUsage",
                field: "cache_read_input_tokens",
                exact_decl: Some("pub cache_read_input_tokens: WirePresence<u32>,"),
                expected: InventoryClass::PresentWirePresence,
            },
            InventoryRow {
                spec_field: "message_start.message.usage.output_tokens_details",
                struct_name: "MessagesUsage",
                field: "output_tokens_details",
                exact_decl: Some("pub output_tokens_details: WirePresence<OutputTokensDetails>,"),
                expected: InventoryClass::PresentWirePresence,
            },
            InventoryRow {
                spec_field: "message_start.message.usage.cache_creation",
                struct_name: "MessagesUsage",
                field: "cache_creation",
                exact_decl: Some("pub cache_creation: WirePresence<CacheCreation>,"),
                expected: InventoryClass::PresentWirePresence,
            },
            InventoryRow {
                spec_field: "message_delta.usage.input_tokens",
                struct_name: "MessageDeltaUsage",
                field: "input_tokens",
                exact_decl: Some("pub input_tokens: WirePresence<u32>,"),
                expected: InventoryClass::PresentWirePresence,
            },
            InventoryRow {
                spec_field: "message_delta.usage.cache_read_input_tokens",
                struct_name: "MessageDeltaUsage",
                field: "cache_read_input_tokens",
                exact_decl: Some("pub cache_read_input_tokens: WirePresence<u32>,"),
                expected: InventoryClass::PresentWirePresence,
            },
            InventoryRow {
                spec_field: "message_delta.usage.cache_creation_input_tokens",
                struct_name: "MessageDeltaUsage",
                field: "cache_creation_input_tokens",
                exact_decl: Some("pub cache_creation_input_tokens: WirePresence<u32>,"),
                expected: InventoryClass::PresentWirePresence,
            },
            InventoryRow {
                spec_field: "message_delta.usage.output_tokens_details",
                struct_name: "MessageDeltaUsage",
                field: "output_tokens_details",
                exact_decl: Some("pub output_tokens_details: WirePresence<OutputTokensDetails>,"),
                expected: InventoryClass::PresentWirePresence,
            },
            InventoryRow {
                spec_field: "message_delta.delta.stop_details",
                struct_name: "MessageDeltaBody",
                field: "stop_details",
                exact_decl: Some("pub stop_details: WirePresence<StopDetails>,"),
                expected: InventoryClass::PresentWirePresence,
            },
            InventoryRow {
                spec_field: "message_delta.delta.container",
                struct_name: "MessageDeltaBody",
                field: "container",
                exact_decl: Some("pub container: WirePresence<serde_json::Value>,"),
                expected: InventoryClass::PresentWirePresence,
            },
            InventoryRow {
                spec_field: "message_start.message.container",
                struct_name: "MessagesResponse",
                field: "container",
                exact_decl: Some("pub container: WirePresence<serde_json::Value>,"),
                expected: InventoryClass::PresentWirePresence,
            },
            InventoryRow {
                spec_field: "message_start.message.stop_details",
                struct_name: "MessagesResponse",
                field: "stop_details",
                exact_decl: Some("pub stop_details: WirePresence<StopDetails>,"),
                expected: InventoryClass::PresentWirePresence,
            },
        ];
        let mut present_option = 0usize;
        let mut present_request_presence = 0usize;
        let mut present_wire_presence = 0usize;
        let mut absent = 0usize;
        for row in rows {
            let body = struct_body(src, row.struct_name).unwrap_or_else(|| {
                panic!(
                    "A1-lite inventory gate: struct `{}` moved or renamed in messages.rs — re-derive the 46c classification table",
                    row.struct_name
                )
            });
            let present = match row.exact_decl {
                Some(decl) => body.lines().any(|line| line.trim() == decl),
                None => has_field_decl(body, row.field),
            };
            let matches = match row.expected {
                InventoryClass::PresentOption | InventoryClass::PresentRequestPresence
                | InventoryClass::PresentWirePresence => present,
                InventoryClass::Absent => !present,
            };
            assert!(
                matches,
                "A1-lite inventory gate: spec field `{}` (struct `{}`, field `{}`) expected {:?} but source says present={present} — the 46c truth table moved, or the declaration was renamed/moved/reformatted (re-derive against spec A0 L87-114, response half)",
                row.spec_field, row.struct_name, row.field, row.expected
            );
            match row.expected {
                InventoryClass::PresentOption => present_option += 1,
                InventoryClass::PresentRequestPresence => present_request_presence += 1,
                InventoryClass::PresentWirePresence => present_wire_presence += 1,
                InventoryClass::Absent => absent += 1,
            }
        }
        assert_eq!(
            present_request_presence,
            3,
            "46c truth: 3 × PRESENT-RequestPresence (46b, unchanged)"
        );
        assert_eq!(present_wire_presence, 12, "46c truth: 12 × PRESENT-WirePresence");
        assert_eq!(
            present_option,
            1,
            "46c truth: 1 × PRESENT-Option (F5: Tool.cache_control)"
        );
        assert_eq!(
            absent,
            5,
            "46c truth: 5 × ABSENT (46a request-side, minus F5 Tool.cache_control)"
        );
    }

    // ------------------------------------------------------------------
    // 56a (CITATIONS-1) RECORDED-ABSENT source gate: the output
    // `TextBlock.citations` wire surface (frozen spec r23 L106 row Q5) is
    // recorded ABSENT at base b745593 (registry entry CITATIONS-1,
    // grok/plans/citations/citations-registry.md). Fails if 56b or any
    // refactor adds the surface without flipping the registry entry
    // `modeled_by` in the same commit (46-series truth-table discipline).
    // ------------------------------------------------------------------

    /// Brace-delimited body starting at the last `{` of `marker` — the
    /// `struct_body` idiom generalized to enum/variant bodies (the 46c
    /// machinery only reaches `pub struct`; 56a T4 source gate).
    fn brace_body_after<'a>(src: &'a str, marker: &str) -> Option<&'a str> {
        let start = src.find(marker)?;
        let rest = &src[start + marker.len() - 1..];
        let mut depth = 0i32;
        for (idx, byte) in rest.bytes().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&rest[..=idx]);
                    }
                }
                _ => {}
            }
        }
        None
    }

    #[test]
    fn citations_surface_recorded_absent_pins_56() {
        let src = include_str!("messages.rs");

        // (i) `ContentBlock::Text` body = { text, cache_control } only:
        // no `citations` member (46c R18 / ledger 13:48Z OQ-4).
        let enum_body = brace_body_after(src, "pub enum ContentBlock {")
            .expect("RECORDED-ABSENT pin: ContentBlock enum must be brace-delimited");
        let text_body = brace_body_after(enum_body, "    Text {")
            .expect("RECORDED-ABSENT pin: ContentBlock::Text variant must be brace-delimited");
        assert!(
            !text_body
                .lines()
                .any(|line| line.trim_start().starts_with("citations:")),
            "RECORDED-ABSENT (56a): ContentBlock::Text must not gain a `citations` member \
             until registry entry CITATIONS-1 flips modeled_by in the same commit (56b)"
        );

        // (ii) `KNOWN_DELTA_SUBTYPES` = exactly the 4 quoted literals;
        // no `citation_delta` row (the R1 forward-compat table is 56b's to change).
        let known_marker = "const KNOWN_DELTA_SUBTYPES: &[&str] = &[";
        let known_start = src
            .find(known_marker)
            .expect("RECORDED-ABSENT pin: KNOWN_DELTA_SUBTYPES must exist");
        let known_rest = &src[known_start + known_marker.len()..];
        let known_body = &known_rest[..known_rest
            .find("];")
            .expect("RECORDED-ABSENT pin: array must be closed")];
        let entries: Vec<&str> = known_body
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(
            entries,
            [
                "\"text_delta\",",
                "\"input_json_delta\",",
                "\"thinking_delta\",",
                "\"signature_delta\",",
            ],
            "RECORDED-ABSENT (56a): KNOWN_DELTA_SUBTYPES must stay exactly the 4 quoted \
             literals (no `citation_delta`) until registry entry CITATIONS-1 flips \
             modeled_by in the same commit (56b)"
        );

        // (iii) No `TextCitation` type anywhere in the wire module.
        assert!(
            !src.contains("TextCitation"),
            "RECORDED-ABSENT (56a): `TextCitation` must not appear in messages.rs until \
             registry entry CITATIONS-1 flips modeled_by in the same commit (56b)"
        );

        // (iv) `StreamDelta` = exactly 4 tagged variants; no citation variant.
        let sd_body = brace_body_after(src, "pub enum StreamDelta {")
            .expect("RECORDED-ABSENT pin: StreamDelta enum must be brace-delimited");
        let variant_count = sd_body
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                trimmed.chars().next().is_some_and(|c: char| c.is_ascii_uppercase())
                    && trimmed.contains(" { ")
                    && trimmed.ends_with("},")
            })
            .count();
        assert_eq!(
            variant_count, 4,
            "RECORDED-ABSENT (56a): StreamDelta must stay exactly 4 variants (no citation \
             variant) until registry entry CITATIONS-1 flips modeled_by in the same \
             commit (56b)"
        );
    }
}
