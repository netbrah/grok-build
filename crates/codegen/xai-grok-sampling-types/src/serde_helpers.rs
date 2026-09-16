use serde::{Deserialize, Deserializer};

pub fn empty_string_as_none<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

/// Deserialize `Option<Option<T>>`: absent (`None`) leaves, `null` (`Some(None)`) clears, a value sets.
/// Requires `#[serde(default, deserialize_with = "…")]`.
/// 3-state generalization: `presence::RequestPresence`/`WirePresence` (null is a first-class
/// state); `rejecting_null` below is the inverse (null rejected).
pub fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

/// Deserialize an omission-only `Option<T>` field that REJECTS an explicit
/// JSON `null` (spec A0, L97-98: optional non-nullable request fields
/// "continue to use an omission-only typed option and reject explicit
/// null"). An absent member never reaches this fn (serde's `#[serde(default)]`
/// / implicit `None` handles absent `Option` fields — the helper only sees
/// present members); a present `null` is an error; a present value
/// deserializes through `T`. Requires
/// `#[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null")]`.
/// Generalization note: the 3-state carrier case (null is a first-class
/// state) is `presence::RequestPresence` / `presence::WirePresence` — the
/// inverse of this helper; `double_option` above is its 2-state sibling
/// (reverse-linked from its doc).
pub fn rejecting_null<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    match Option::<T>::deserialize(deserializer)? {
        Some(v) => Ok(Some(v)),
        None => Err(serde::de::Error::custom(
            "explicit null is not representable on this omission-only field (spec A0: optional non-nullable rejects explicit null)",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Field-pattern holder per `rejecting_null`'s doc: `#[serde(default,
    /// deserialize_with = …)]` — an absent member routes through `default`
    /// (never the helper); a present `null` is an error.
    #[derive(Debug, PartialEq, Deserialize)]
    struct RejectHolder {
        #[serde(default, deserialize_with = "super::rejecting_null")]
        value: Option<String>,
    }

    /// One non-String T to keep the where-clause honest.
    #[derive(Debug, PartialEq, Deserialize)]
    struct RejectHolderU32 {
        #[serde(default, deserialize_with = "super::rejecting_null")]
        value: Option<u32>,
    }

    #[test]
    fn rejecting_null_missing_is_none() {
        let holder: RejectHolder = serde_json::from_str("{}").expect("absent member must not error");
        assert_eq!(holder.value, None);
    }

    #[test]
    fn rejecting_null_explicit_null_is_rejected() {
        let err = serde_json::from_str::<RejectHolder>(r#"{"value":null}"#)
            .expect_err("explicit null must be rejected");
        assert!(
            err.to_string().contains("explicit null is not representable"),
            "error should name the rejection: {err}"
        );
    }

    #[test]
    fn rejecting_null_value_passes_through() {
        let holder: RejectHolder =
            serde_json::from_str(r#"{"value":"hi"}"#).expect("present value must deserialize");
        assert_eq!(holder.value, Some("hi".to_owned()));
    }

    #[test]
    fn rejecting_null_generic_t() {
        let holder: RejectHolderU32 =
            serde_json::from_str(r#"{"value":42}"#).expect("present u32 must deserialize");
        assert_eq!(holder.value, Some(42));
        let absent: RejectHolderU32 =
            serde_json::from_str("{}").expect("absent member must not error");
        assert_eq!(absent.value, None);
        let err = serde_json::from_str::<RejectHolderU32>(r#"{"value":null}"#)
            .expect_err("explicit null must be rejected on non-String T");
        assert!(err.to_string().contains("explicit null is not representable"));
    }
}
