use super::support::*;
use super::*;
use futures_util::StreamExt;
use serde_json::{Value, json};
use xai_grok_test_support::{MockInferenceServer, ScriptedResponse, SseEvent};
use xai_grok_tools::implementations::grok_build::monitor::types::{
    MonitorEventBuffer, MonitorEventNotification,
};

const I7YX_ALLOWED_ENV_NAMES: [&str; 22] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "DEVELOPER_DIR",
    "SDKROOT",
    "DYLD_LIBRARY_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "__CF_USER_TEXT_ENCODING",
    "CARGO_TARGET_DIR",
    "TMPDIR",
    "CARGO_BUILD_JOBS",
    "CARGO_INCREMENTAL",
    "RUST_MIN_STACK",
    "GROK_SHELL_BUNDLE_RG_PATH",
    "GROK_TOOLS_BUNDLE_RG_PATH",
    "CARGO_NET_OFFLINE",
];

fn i7yx_present_parent_names() -> Vec<String> {
    let mut names = Vec::new();
    for name in I7YX_ALLOWED_ENV_NAMES {
        if std::env::var_os(name).is_some() {
            names.push(name.to_owned());
        }
    }
    names.sort_unstable();
    names
}

fn i7yx_expected_child_names(route: I7yxChildRoute, parent_names: &[String]) -> Vec<String> {
    let mut names = parent_names.to_vec();
    names.extend([
        "GROK_HOME".to_owned(),
        "GROK_TEST_I7YX_ENV_NAMES_CHILD".to_owned(),
    ]);
    let legacy_name = match route {
        I7yxChildRoute::K0 => Some("GROK_TEST_K0_SHELL_CHILD"),
        I7yxChildRoute::K1 => Some("GROK_TEST_K1_SHELL_CHILD"),
        I7yxChildRoute::PendingV => Some("K2LG_PENDING_CHILD"),
        I7yxChildRoute::NativeN => Some("K2LG_N_CHILD"),
        I7yxChildRoute::SamplerProbe => Some("K2LG_SOURCE_PROBE"),
        I7yxChildRoute::SamplerRun => None,
    };
    if let Some(name) = legacy_name {
        names.push(name.to_owned());
    }
    names.sort_unstable();
    names
}

fn i7yx_assert_child_hygiene(route: I7yxChildRoute, stdout: &[u8], parent_names: &[String]) {
    let record = i7yx_read_child_names(stdout, route)
        .unwrap_or_else(|error| panic!("{}", i7yx_record_diagnostic(error, stdout)));
    println!(
        "\nI7YX_PARENT_NAMES {}",
        json!({"route": route, "names": parent_names})
    );
    let payload = serde_json::to_string(&record).expect("I7YX validated names encoding");
    println!("I7YX_CHILD_NAMES {payload}");
    let expected = i7yx_expected_child_names(route, parent_names);
    assert_eq!(
        record.names, expected,
        "I7YX names equality route={route:?}"
    );
    assert!(record.detached, "I7YX detached predicate route={route:?}");
    println!(
        "I7YX_CHILD_CHECK {}",
        json!({"route": route, "names_pass": true, "detached_pass": true})
    );
}

fn i7yx_shell_child_command(
    route: I7yxChildRoute,
    home: &std::path::Path,
) -> (std::process::Command, Vec<String>) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.env_clear();
    let parent_names = i7yx_present_parent_names();
    for name in I7YX_ALLOWED_ENV_NAMES {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let (marker, legacy_name, legacy_value) = match route {
        I7yxChildRoute::K0 => (
            "K0",
            "GROK_TEST_K0_SHELL_CHILD",
            "session::acp_session::native_messages_replay_tests::messages_no_plan_preserves_pin_shell",
        ),
        I7yxChildRoute::K1 => (
            "K1",
            "GROK_TEST_K1_SHELL_CHILD",
            "session::acp_session::native_messages_replay_tests::messages_replay_plan_checked_on_all_sends",
        ),
        I7yxChildRoute::PendingV => ("PENDING_V", "K2LG_PENDING_CHILD", "1"),
        I7yxChildRoute::NativeN => ("NATIVE_N", "K2LG_N_CHILD", "1"),
        I7yxChildRoute::SamplerProbe | I7yxChildRoute::SamplerRun => {
            panic!("I7YX shell constructor route rejected")
        }
    };
    command
        .env("GROK_HOME", home)
        .env("GROK_TEST_I7YX_ENV_NAMES_CHILD", marker)
        .env(legacy_name, legacy_value);
    (command, parent_names)
}

const I7YX_CHILD_NAMES_PREFIX: &[u8] = b"I7YX_CHILD_NAMES ";
const I7YX_MAX_NAMES: usize = 256;
const I7YX_MAX_NAME_BYTES: usize = 128;
const I7YX_MAX_RECORD_BYTES: usize = 200_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum I7yxChildRoute {
    K0,
    K1,
    PendingV,
    NativeN,
    SamplerProbe,
    SamplerRun,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct I7yxChildNames {
    route: I7yxChildRoute,
    names: Vec<String>,
    pid: u32,
    pgid: u32,
    detached: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum I7yxRecordError {
    Bounds,
    Malformed,
    NamesOrder,
    ProcessFacts,
    Missing,
    Duplicate,
    Io(std::io::ErrorKind),
}

fn i7yx_validate_child_names(record: &I7yxChildNames) -> std::result::Result<(), I7yxRecordError> {
    if record.names.len() > I7YX_MAX_NAMES
        || record
            .names
            .iter()
            .any(|name| name.is_empty() || name.len() > I7YX_MAX_NAME_BYTES)
    {
        return Err(I7yxRecordError::Bounds);
    }
    if record.names.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(I7yxRecordError::NamesOrder);
    }
    if record.pid == 0 || record.detached != (record.pid == record.pgid) {
        return Err(I7yxRecordError::ProcessFacts);
    }
    Ok(())
}

fn i7yx_decode_child_names(payload: &[u8]) -> std::result::Result<I7yxChildNames, I7yxRecordError> {
    if payload.len() > I7YX_MAX_RECORD_BYTES {
        return Err(I7yxRecordError::Bounds);
    }
    let record: I7yxChildNames =
        serde_json::from_slice(payload).map_err(|_| I7yxRecordError::Malformed)?;
    i7yx_validate_child_names(&record)?;
    Ok(record)
}

fn i7yx_read_child_names(
    stdout: &[u8],
    route: I7yxChildRoute,
) -> std::result::Result<I7yxChildNames, I7yxRecordError> {
    let mut matching = None;
    for line in stdout.split(|byte| *byte == b'\n') {
        if let Some(payload) = line.strip_prefix(I7YX_CHILD_NAMES_PREFIX) {
            let record = i7yx_decode_child_names(payload)?;
            if record.route == route {
                if matching.is_some() {
                    return Err(I7yxRecordError::Duplicate);
                }
                matching = Some(record);
            }
        }
    }
    matching.ok_or(I7yxRecordError::Missing)
}

fn i7yx_record_diagnostic(error: I7yxRecordError, bytes: &[u8]) -> String {
    let count = bytes.len();
    let digest = native_content_digest(bytes);
    format!("I7YX record rejected: kind={error:?}; bytes={count}; sha256={digest}")
}

fn i7yx_output_diagnostic(route: I7yxChildRoute, output: &std::process::Output) -> String {
    let child_exit = output.status.code();
    #[cfg(unix)]
    let child_signal = {
        use std::os::unix::process::ExitStatusExt as _;
        output.status.signal()
    };
    #[cfg(not(unix))]
    let child_signal: Option<i32> = None;
    let stdout_bytes = output.stdout.len();
    let stderr_bytes = output.stderr.len();
    let stdout_sha256 = native_content_digest(&output.stdout);
    let stderr_sha256 = native_content_digest(&output.stderr);
    format!(
        "I7YX route={route:?}; child_exit={child_exit:?}; child_signal={child_signal:?}; stdout_bytes={stdout_bytes}; stdout_sha256={stdout_sha256}; stderr_bytes={stderr_bytes}; stderr_sha256={stderr_sha256}"
    )
}

fn i7yx_report_child_names() {
    let Some(marker) = std::env::var_os("GROK_TEST_I7YX_ENV_NAMES_CHILD") else {
        return;
    };
    let route = match marker.to_str() {
        Some("K0") => I7yxChildRoute::K0,
        Some("K1") => I7yxChildRoute::K1,
        Some("PENDING_V") => I7yxChildRoute::PendingV,
        Some("NATIVE_N") => I7yxChildRoute::NativeN,
        Some("SAMPLER_PROBE") => I7yxChildRoute::SamplerProbe,
        Some("SAMPLER_RUN") => I7yxChildRoute::SamplerRun,
        _ => panic!("I7YX child marker rejected"),
    };
    let mut names = Vec::new();
    for (name, _) in std::env::vars_os() {
        let Ok(name) = name.into_string() else {
            panic!("I7YX child name encoding rejected");
        };
        assert!(
            names.len() < I7YX_MAX_NAMES && name.len() <= I7YX_MAX_NAME_BYTES,
            "I7YX child name bounds rejected"
        );
        names.push(name);
    }
    names.sort_unstable();
    #[cfg(unix)]
    let (pid, pgid) = (
        u32::try_from(nix::unistd::getpid().as_raw()).expect("I7YX positive child pid"),
        u32::try_from(nix::unistd::getpgrp().as_raw()).expect("I7YX positive child process group"),
    );
    #[cfg(not(unix))]
    let (pid, pgid) = (std::process::id(), 0);
    let record = I7yxChildNames {
        route,
        names,
        pid,
        pgid,
        detached: pid == pgid,
    };
    assert!(
        i7yx_validate_child_names(&record).is_ok(),
        "I7YX child report rejected"
    );
    let payload = serde_json::to_string(&record).expect("I7YX child report encoding");
    assert!(
        payload.len() <= I7YX_MAX_RECORD_BYTES,
        "I7YX child report byte bounds rejected"
    );
    println!("I7YX_CHILD_NAMES {payload}");
}

#[test]
fn i7yx_names_parser_contract() {
    let record = I7yxChildNames {
        route: I7yxChildRoute::K0,
        names: vec!["HOME".into(), "PATH".into()],
        pid: 17,
        pgid: 17,
        detached: true,
    };
    let payload = serde_json::to_vec(&record).unwrap();
    let framed = [I7YX_CHILD_NAMES_PREFIX, payload.as_slice(), b"\n"].concat();
    assert_eq!(
        i7yx_read_child_names(&framed, I7yxChildRoute::K0),
        Ok(record.clone())
    );
    let bounded = I7yxChildNames {
        names: (0..256).map(|index| format!("N{index:03}")).collect(),
        ..record.clone()
    };
    assert_eq!(
        i7yx_decode_child_names(&serde_json::to_vec(&bounded).unwrap()),
        Ok(bounded)
    );
    let longest = I7yxChildNames {
        names: vec!["N".repeat(128)],
        ..record.clone()
    };
    assert_eq!(
        i7yx_decode_child_names(&serde_json::to_vec(&longest).unwrap()),
        Ok(longest)
    );
    let mut invalid = vec![b"{".to_vec()];
    for names in [
        vec!["PATH".into(), "HOME".into()],
        vec!["HOME".into(), "HOME".into()],
        vec!["N".repeat(129)],
        (0..257).map(|index| format!("N{index:03}")).collect(),
    ] {
        invalid.push(
            serde_json::to_vec(&I7yxChildNames {
                names,
                ..record.clone()
            })
            .unwrap(),
        );
    }
    let mut unknown_route = serde_json::to_value(&record).unwrap();
    unknown_route["route"] = serde_json::json!("UNKNOWN_ROUTE");
    invalid.push(serde_json::to_vec(&unknown_route).unwrap());
    let mut value_field = serde_json::to_value(&record).unwrap();
    value_field["value"] = serde_json::json!("i7yx-canned-token-v1");
    invalid.push(serde_json::to_vec(&value_field).unwrap());
    for input in invalid {
        let error = i7yx_decode_child_names(&input).expect_err("I7YX invalid record accepted");
        let diagnostic = i7yx_record_diagnostic(error, &input);
        assert!(
            !diagnostic.contains("i7yx-canned-token-v1"),
            "I7YX parser diagnostic exposed payload"
        );
        assert!(
            diagnostic.contains(&native_content_digest(&input)),
            "I7YX parser diagnostic missing digest"
        );
    }
    let duplicate = [framed.as_slice(), framed.as_slice()].concat();
    assert_eq!(
        i7yx_read_child_names(&duplicate, I7yxChildRoute::K0),
        Err(I7yxRecordError::Duplicate)
    );
    assert_eq!(
        i7yx_read_child_names(&framed, I7yxChildRoute::K1),
        Err(I7yxRecordError::Missing)
    );
}

#[cfg(unix)]
#[test]
fn i7yx_output_diagnostic_contract() {
    use std::os::unix::process::ExitStatusExt as _;
    let output = std::process::Output {
        status: std::process::ExitStatus::from_raw(101 << 8),
        stdout: b"i7yx-canned-fault-payload-v1".to_vec(),
        stderr: b"i7yx-canned-token-v1".to_vec(),
    };
    let diagnostic = i7yx_output_diagnostic(I7yxChildRoute::K0, &output);
    assert!(
        !diagnostic.contains("i7yx-canned-fault-payload-v1"),
        "I7YX status diagnostic exposed stdout"
    );
    assert!(
        !diagnostic.contains("i7yx-canned-token-v1"),
        "I7YX status diagnostic exposed stderr"
    );
    assert!(
        diagnostic.contains("child_exit=Some(101)"),
        "I7YX status diagnostic missing child exit"
    );
    assert!(
        diagnostic.contains(&native_content_digest(&output.stdout)),
        "I7YX status diagnostic missing stdout digest"
    );
    assert!(
        diagnostic.contains(&native_content_digest(&output.stderr)),
        "I7YX status diagnostic missing stderr digest"
    );
}

#[test]
fn i7yx_proof_forwarding_preserves_original_bytes() {
    let digest = native_content_digest(b"I7YX unit proof");
    let stamp = format!(
        "K2LG_BUILD {{ \"sampler\": \"{digest}\", \"validation\": \"{digest}\", \"fixture\": \"{digest}\" }}\r\n"
    );
    let arm = b"K2LG_ARM {\"label\":\"unit\",\"passed\":true}\n";
    let final_arm = b"K2LG_ARM {\"label\":\"last\",\"passed\":true}";
    let actor = b"K1_SHELL_CASE actor capture=true valid builds=1 http_delta=1 ends=1\r\n";
    let direct = b"K1_SHELL_CASE direct missing=false http_delta=1\n";
    let final_direct = b"K1_SHELL_CASE compact-helper missing=true http_delta=0";
    for (route, records) in [
        (
            I7yxChildRoute::K1,
            vec![actor.as_slice(), direct, actor.as_slice(), final_direct],
        ),
        (
            I7yxChildRoute::PendingV,
            vec![stamp.as_bytes(), arm, stamp.as_bytes(), final_arm],
        ),
        (
            I7yxChildRoute::NativeN,
            vec![stamp.as_bytes(), arm, stamp.as_bytes(), final_arm],
        ),
        (I7yxChildRoute::K0, vec![]),
    ] {
        let expected = records.concat();
        let input = [b"running 1 test\n".as_slice(), expected.as_slice()].concat();
        let mut forwarded = Vec::new();
        i7yx_forward_proof_records(&input, route, &mut forwarded).unwrap();
        assert_eq!(
            forwarded.len(),
            expected.len(),
            "I7YX proof byte count changed"
        );
        assert_eq!(
            native_content_digest(&forwarded),
            native_content_digest(&expected),
            "I7YX original proof framing changed"
        );
    }
}

#[test]
fn i7yx_proof_forwarding_rejects_unknown_prefixes() {
    for (route, input) in [
        (
            I7yxChildRoute::K0,
            b"K0_PIN_H {\"value\":\"i7yx-canned-token-v1\"}\n".as_slice(),
        ),
        (
            I7yxChildRoute::K1,
            b"K1_UNKNOWN {\"value\":\"i7yx-canned-token-v1\"}\n".as_slice(),
        ),
        (
            I7yxChildRoute::PendingV,
            b"K2LG_UNKNOWN {\"value\":\"i7yx-canned-token-v1\"}\n".as_slice(),
        ),
        (
            I7yxChildRoute::NativeN,
            b"K2LG_UNKNOWN {\"value\":\"i7yx-canned-token-v1\"}\n".as_slice(),
        ),
    ] {
        let mut forwarded = Vec::new();
        let error = i7yx_forward_proof_records(input, route, &mut forwarded)
            .expect_err("I7YX unrecognized proof accepted");
        assert!(
            forwarded.is_empty(),
            "I7YX unrecognized proof was forwarded"
        );
        let diagnostic = i7yx_record_diagnostic(error, input);
        assert!(
            !diagnostic.contains("i7yx-canned-token-v1"),
            "I7YX proof diagnostic exposed payload"
        );
        assert!(
            diagnostic.contains(&native_content_digest(input)),
            "I7YX proof diagnostic missing digest"
        );
    }
}

#[test]
fn i7yx_proof_forwarding_rejects_unsafe_records() {
    for route in [I7yxChildRoute::PendingV, I7yxChildRoute::NativeN] {
        for input in [
            b"K2LG_BUILD {broken}\n".as_slice(),
            b"K2LG_ARM {\"label\":\"unit\",\"passed\":true,\"value\":\"i7yx-canned-token-v1\"}\n".as_slice(),
            b"K2LG_ARM {\"label\":\"unit\",\"passed\":true,\"passed\":false}\n".as_slice(),
            b"K2LG_BUILD {\"sampler\":\"bad\",\"validation\":\"bad\",\"fixture\":\"bad\"}\n".as_slice(),
            b"K2LG_N {\"producer\":\"unit\",\"native_agents_enabled_for\":{\"value\":\"i7yx-canned-token-v1\"}}\n".as_slice(),
        ] {
            let mut forwarded = Vec::new();
            let error = i7yx_forward_proof_records(input, route, &mut forwarded)
                .expect_err("I7YX invalid proof accepted");
            assert!(forwarded.is_empty(), "I7YX invalid proof was forwarded");
            let diagnostic = i7yx_record_diagnostic(error, input);
            assert!(!diagnostic.contains("i7yx-canned-token-v1"), "I7YX proof diagnostic exposed payload");
            assert!(diagnostic.contains(&native_content_digest(input)), "I7YX proof diagnostic missing digest");
        }
    }
}

#[test]
fn i7yx_proof_forwarding_validates_k1_schema() {
    for input in [
        b"K1_SHELL_CASE actor capture=other valid builds=1 http_delta=1 ends=1\n".as_slice(),
        b"K1_SHELL_CASE actor capture=true unknown-case builds=1 http_delta=1 ends=1\n".as_slice(),
        b"K1_SHELL_CASE actor capture=true valid builds=secret http_delta=1 ends=1\n".as_slice(),
        b"K1_SHELL_CASE direct missing=false http_delta=1 value=i7yx-canned-token-v1\n".as_slice(),
        b"K1_SHELL_CASE unknown-route missing=false http_delta=1\n".as_slice(),
        b"K1_SHELL_CASE direct missing=false http_delta=-1\n".as_slice(),
    ] {
        let mut forwarded = Vec::new();
        let error = i7yx_forward_proof_records(input, I7yxChildRoute::K1, &mut forwarded)
            .expect_err("I7YX invalid K1 proof accepted");
        assert!(forwarded.is_empty(), "I7YX invalid K1 proof was forwarded");
        let diagnostic = i7yx_record_diagnostic(error, input);
        assert!(
            !diagnostic.contains("i7yx-canned-token-v1"),
            "I7YX K1 proof diagnostic exposed payload"
        );
        assert!(
            diagnostic.contains(&native_content_digest(input)),
            "I7YX K1 proof diagnostic missing digest"
        );
    }
}

struct I7yxUniqueProofValue(serde_json::Value);

impl<'de> serde::Deserialize<'de> for I7yxUniqueProofValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct ProofVisitor;
        impl<'de> serde::de::Visitor<'de> for ProofVisitor {
            type Value = I7yxUniqueProofValue;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("proof JSON with unique object keys")
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(I7yxUniqueProofValue(serde_json::Value::Null))
            }
            fn visit_bool<E: serde::de::Error>(
                self,
                value: bool,
            ) -> std::result::Result<Self::Value, E> {
                Ok(I7yxUniqueProofValue(serde_json::Value::Bool(value)))
            }
            fn visit_i64<E: serde::de::Error>(
                self,
                value: i64,
            ) -> std::result::Result<Self::Value, E> {
                Ok(I7yxUniqueProofValue(serde_json::Value::Number(
                    value.into(),
                )))
            }
            fn visit_u64<E: serde::de::Error>(
                self,
                value: u64,
            ) -> std::result::Result<Self::Value, E> {
                Ok(I7yxUniqueProofValue(serde_json::Value::Number(
                    value.into(),
                )))
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Err(E::custom("floating proof scalar rejected"))
            }
            fn visit_str<E: serde::de::Error>(
                self,
                value: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(I7yxUniqueProofValue(serde_json::Value::String(
                    value.to_owned(),
                )))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                value: String,
            ) -> std::result::Result<Self::Value, E> {
                Ok(I7yxUniqueProofValue(serde_json::Value::String(value)))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(I7yxUniqueProofValue(value)) = sequence.next_element()? {
                    values.push(value);
                }
                Ok(I7yxUniqueProofValue(serde_json::Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut object: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((key, I7yxUniqueProofValue(value))) =
                    object.next_entry::<String, I7yxUniqueProofValue>()?
                {
                    if values.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate proof key rejected"));
                    }
                }
                Ok(I7yxUniqueProofValue(serde_json::Value::Object(values)))
            }
        }
        deserializer.deserialize_any(ProofVisitor)
    }
}

fn i7yx_proof_digest(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn i7yx_proof_scalar(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::String(_) => true,
        serde_json::Value::Number(number) => number.as_u64().is_some(),
        serde_json::Value::Array(values) => values.iter().all(i7yx_proof_scalar),
        serde_json::Value::Object(_) => false,
    }
}

fn i7yx_proof_object(value: &serde_json::Value, allowed: &[&str], required: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.keys().all(|key| allowed.contains(&key.as_str()))
            && required.iter().all(|key| object.contains_key(*key))
    })
}

fn i7yx_proof_field_type(key: &str, value: &serde_json::Value) -> bool {
    match key {
        "route"
        | "case"
        | "producer"
        | "credit"
        | "window"
        | "status"
        | "time_bound"
        | "reason"
        | "row"
        | "api_backend"
        | "arm"
        | "closure"
        | "W_TE"
        | "observer"
        | "site"
        | "order"
        | "first_met"
        | "enqueue"
        | "later_failure"
        | "resident_after"
        | "repair"
        | "path"
        | "selected_shell_test" => value.as_str().is_some(),
        "input_in_history"
        | "input_in_custody"
        | "input_resident"
        | "input_transmitted"
        | "source_unchanged"
        | "controlled_test_stop"
        | "post_closure_credit"
        | "K2LG_kill"
        | "L16_credit"
        | "input_queued"
        | "turn_completion_credit"
        | "NativeAgentsEnabled"
        | "send_message_tool"
        | "plain_append_equivalent"
        | "repair_before"
        | "prune_after"
        | "session_turn_active"
        | "exchange_live_at_release"
        | "delivery_reported"
        | "constructed_defensive"
        | "identical_bytes"
        | "matched_control_equal_after_first_ordinary_push"
        | "bound_client_preparation"
        | "captured_headers_equal"
        | "credential_values_printed"
        | "bridge_source_matched"
        | "child_success" => value.as_bool().is_some(),
        "executions" | "ops_after_closure" | "ops" | "native_ops" | "control_ops" => value
            .as_array()
            .is_some_and(|values| values.iter().all(|value| value.as_str().is_some())),
        _ => value.as_u64().is_some(),
    }
}

fn i7yx_validate_proof_value(kind: &str, value: &serde_json::Value) -> bool {
    let (allowed, required): (&[&str], &[&str]) = match kind {
        "K2LG_BUILD" => (
            &["sampler", "validation", "fixture"],
            &["sampler", "validation", "fixture"],
        ),
        "K2LG_N_BUILD" => (
            &["session_setup", "interjection", "fixture"],
            &["session_setup", "interjection", "fixture"],
        ),
        "K2LG_ARM" | "K2LG_N_ARM" => (&["label", "passed", "assertion"], &["label", "passed"]),
        "K2LG_V" => (
            &[
                "route",
                "case",
                "producer",
                "builds",
                "continuation_http",
                "executions",
                "body_digest",
                "credit",
                "http_delta",
                "expected_digest",
                "client_executions",
                "window",
                "status",
                "time_bound",
                "input_in_history",
                "input_in_custody",
                "input_resident",
                "input_transmitted",
                "source_unchanged",
                "controlled_test_stop",
                "post_closure_credit",
                "K2LG_kill",
                "L16_credit",
                "input_queued",
                "turn_completion_credit",
                "reason",
            ],
            &["route"],
        ),
        "K2LG_V_SOURCE" => (
            &[
                "case",
                "source_unchanged",
                "HTTP_B",
                "native_usage",
                "defensive_http_delta",
            ],
            &[
                "case",
                "source_unchanged",
                "HTTP_B",
                "native_usage",
                "defensive_http_delta",
            ],
        ),
        "K2LG_V_TRANSPORT" => (
            &[
                "route",
                "case",
                "bound_client_preparation",
                "captured_headers_equal",
                "credential_values_printed",
            ],
            &[
                "route",
                "case",
                "bound_client_preparation",
                "captured_headers_equal",
                "credential_values_printed",
            ],
        ),
        "K2LG_P" => (
            &[
                "producer",
                "source_stamp",
                "selected_shell_test",
                "bridge_source_matched",
                "child_success",
            ],
            &[
                "producer",
                "source_stamp",
                "selected_shell_test",
                "bridge_source_matched",
                "child_success",
            ],
        ),
        "K2LG_N" => {
            let Some(fields) = i7yx_native_proof_fields(value) else {
                return false;
            };
            (fields, fields)
        }
        _ => return false,
    };
    if !i7yx_proof_object(value, allowed, required) {
        return false;
    }
    let Some(object) = value.as_object() else {
        return false;
    };
    if kind == "K2LG_V" && !object.contains_key("case") {
        let gap_fields = [
            "route",
            "window",
            "status",
            "reason",
            "K2LG_kill",
            "L16_credit",
        ];
        if !i7yx_proof_object(value, &gap_fields, &gap_fields) {
            return false;
        }
    }
    if matches!(kind, "K2LG_BUILD" | "K2LG_N_BUILD") {
        return object.values().all(i7yx_proof_digest);
    }
    if matches!(kind, "K2LG_ARM" | "K2LG_N_ARM") {
        return object["label"].as_str().is_some()
            && match object["passed"].as_bool() {
                Some(true) => !object.contains_key("assertion"),
                Some(false) => object
                    .get("assertion")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|assertion| assertion.contains("K2LG boundary")),
                None => false,
            };
    }
    object.iter().all(|(key, field)| match key.as_str() {
        "source_stamp" => {
            i7yx_proof_object(
                field,
                &["sampler", "validation", "fixture"],
                &["sampler", "validation", "fixture"],
            ) && field
                .as_object()
                .is_some_and(|stamp| stamp.values().all(i7yx_proof_digest))
        }
        "body_digest" => field.is_null() || i7yx_proof_digest(field),
        "expected_digest" => i7yx_proof_digest(field),
        "native_usage" => field.as_array().is_some_and(|usage| {
            usage.iter().all(|entry| {
                i7yx_proof_object(
                    entry,
                    &[
                        "request",
                        "response_occurrence",
                        "http_dispatches",
                        "outcome",
                    ],
                    &[
                        "request",
                        "response_occurrence",
                        "http_dispatches",
                        "outcome",
                    ],
                ) && entry["request"].as_str().is_some()
                    && i7yx_proof_scalar(&entry["response_occurrence"])
                    && entry["http_dispatches"].as_u64().is_some()
                    && entry["outcome"].as_str().is_some()
            })
        }),
        "native_agents_enabled_for" => {
            i7yx_proof_object(
                field,
                &[
                    "subagents_enabled",
                    "subagent_depth",
                    "multi_agent_v2_feature",
                    "model_row",
                ],
                &[
                    "subagents_enabled",
                    "subagent_depth",
                    "multi_agent_v2_feature",
                    "model_row",
                ],
            ) && field.as_object().is_some_and(|flags| {
                flags.iter().all(|(name, flag)| {
                    if name == "subagent_depth" {
                        flag.as_u64().is_some()
                    } else {
                        flag.as_bool().is_some()
                    }
                })
            })
        }
        "per_session" => {
            i7yx_proof_object(
                field,
                &[
                    "agent_wide_flag_after_sender_turn",
                    "receiver_prefix_resident",
                ],
                &[
                    "agent_wide_flag_after_sender_turn",
                    "receiver_prefix_resident",
                ],
            ) && field
                .as_object()
                .is_some_and(|flags| flags.values().all(|flag| flag.as_bool().is_some()))
        }
        "submit_revisions" => field.as_array().is_some_and(|revisions| {
            revisions.iter().all(|revision| {
                i7yx_proof_object(
                    revision,
                    &["history_revision", "persistence_ops"],
                    &["history_revision", "persistence_ops"],
                ) && revision
                    .as_object()
                    .is_some_and(|fields| fields.values().all(|number| number.as_u64().is_some()))
            })
        }),
        _ => i7yx_proof_field_type(key, field),
    })
}

fn i7yx_native_proof_fields(value: &serde_json::Value) -> Option<&'static [&'static str]> {
    if let Some(producer) = value.get("producer").and_then(serde_json::Value::as_str) {
        return match producer {
            "host_gate" => Some(&[
                "producer",
                "native_agents_enabled_for",
                "NativeAgentsEnabled",
                "send_message_tool",
                "row",
                "api_backend",
            ]),
            "send_message->coordinator->ShellChildRuntime->AgentMessage->message_item->resident" => {
                Some(&["producer", "call", "status", "resident"])
            }
            _ => None,
        };
    }
    match value.get("arm").and_then(serde_json::Value::as_str)? {
        "N1" => Some(&[
            "arm",
            "closure",
            "continuation_http",
            "post_release_rounds",
            "native_occurrences",
            "per_session",
            "W_TE",
        ]),
        "N6/i" => Some(&[
            "arm",
            "release_op_index",
            "closure_appends",
            "ops_after_closure",
            "plain_append_equivalent",
            "native_persists",
            "repair_before",
            "prune_after",
            "submit_revisions",
        ]),
        "N6/iv" => Some(&["arm", "observer", "captured_items", "turn_appends"]),
        "N1/turn-end" => Some(&[
            "arm",
            "site",
            "session_turn_active",
            "exchange_live_at_release",
            "released",
        ]),
        "N2" => Some(&["arm", "native_occurrences", "order", "persist_appends"]),
        "N3" => Some(&[
            "arm",
            "first_met",
            "enqueue",
            "resident_after_failures",
            "delivery_reported",
        ]),
        "N4" => Some(&[
            "arm",
            "accepted_prefix",
            "later_failure",
            "resident_after",
            "constructed_defensive",
        ]),
        "N5" => Some(&[
            "arm",
            "http_attempts_for_round",
            "identical_bytes",
            "native_appends",
            "native_persists",
        ]),
        "N6/ii-cancel-during-client" => Some(&[
            "arm",
            "site",
            "matched_control_equal_after_first_ordinary_push",
            "ops",
        ]),
        "N6/ii-cancel-after-results" => Some(&[
            "arm",
            "site",
            "repair",
            "matched_control_equal_after_first_ordinary_push",
            "ops",
        ]),
        "N6/ii-error" => Some(&[
            "arm",
            "site",
            "matched_control_equal_after_first_ordinary_push",
            "native_ops",
            "control_ops",
        ]),
        "N6/iii" => Some(&[
            "arm",
            "seeded_turns",
            "cleared_before",
            "cleared_after_native",
            "cleared_after_next_prompt",
        ]),
        "N6/inactive-no-plan" => Some(&["arm", "path", "native_appends"]),
        _ => None,
    }
}

fn i7yx_proof_decimal(field: Option<&str>, prefix: &str) -> bool {
    field
        .and_then(|field| field.strip_prefix(prefix))
        .is_some_and(|digits| {
            !digits.is_empty()
                && digits.bytes().all(|byte| byte.is_ascii_digit())
                && digits.parse::<u64>().is_ok()
        })
}

fn i7yx_validate_k1_proof(line: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(line) else {
        return false;
    };
    let mut fields = text.split_ascii_whitespace();
    if fields.next() != Some("K1_SHELL_CASE") {
        return false;
    }
    let Some(route) = fields.next() else {
        return false;
    };
    let valid = if route == "actor" {
        matches!(fields.next(), Some("capture=true" | "capture=false"))
            && matches!(
                fields.next(),
                Some(
                    "valid"
                        | "missing-plan"
                        | "stale-request"
                        | "stale-context"
                        | "wrong-banked-body"
                        | "install-B-retain-A"
                )
            )
            && i7yx_proof_decimal(fields.next(), "builds=")
            && i7yx_proof_decimal(fields.next(), "http_delta=")
            && i7yx_proof_decimal(fields.next(), "ends=")
    } else {
        matches!(
            route,
            "direct" | "collect" | "non-streaming" | "compact-helper"
        ) && matches!(fields.next(), Some("missing=true" | "missing=false"))
            && i7yx_proof_decimal(fields.next(), "http_delta=")
    };
    valid && fields.next().is_none()
}

fn i7yx_forward_proof_records<W: std::io::Write>(
    stdout: &[u8],
    route: I7yxChildRoute,
    writer: &mut W,
) -> std::result::Result<(), I7yxRecordError> {
    for line in stdout.split_inclusive(|byte| *byte == b'\n') {
        let candidate = match route {
            I7yxChildRoute::K0 => line.starts_with(b"K0_PIN_H "),
            I7yxChildRoute::K1 => line.starts_with(b"K1_"),
            I7yxChildRoute::PendingV | I7yxChildRoute::NativeN => line.starts_with(b"K2LG_"),
            I7yxChildRoute::SamplerProbe | I7yxChildRoute::SamplerRun => false,
        };
        if !candidate {
            continue;
        }
        if line.len() > 1_048_576 {
            return Err(I7yxRecordError::Bounds);
        }
        let valid = if route == I7yxChildRoute::K1 {
            i7yx_validate_k1_proof(line)
        } else if route == I7yxChildRoute::K0 {
            // The signed base has no K0_PIN_H producer or accepted schema.
            false
        } else {
            let split = line
                .iter()
                .position(|byte| *byte == b' ')
                .ok_or(I7yxRecordError::Malformed)?;
            let kind =
                std::str::from_utf8(&line[..split]).map_err(|_| I7yxRecordError::Malformed)?;
            let I7yxUniqueProofValue(value) = serde_json::from_slice(&line[split + 1..])
                .map_err(|_| I7yxRecordError::Malformed)?;
            i7yx_validate_proof_value(kind, &value)
        };
        if !valid {
            return Err(I7yxRecordError::Malformed);
        }
        writer
            .write_all(line)
            .map_err(|error| I7yxRecordError::Io(error.kind()))?;
    }
    Ok(())
}

fn i7yx_print_proof_records(stdout: &[u8], route: I7yxChildRoute) {
    let mut forwarded = Vec::new();
    i7yx_forward_proof_records(stdout, route, &mut forwarded)
        .unwrap_or_else(|error| panic!("{}", i7yx_record_diagnostic(error, stdout)));
    let text = std::str::from_utf8(&forwarded)
        .unwrap_or_else(|_| panic!("I7YX validated proof encoding rejected"));
    print!("{text}");
}

const K0_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
struct K0IdleObserver {
    count: std::cell::Cell<usize>,
    notification: tokio::sync::Notify,
}

#[async_trait::async_trait(?Send)]
impl xai_agent_lifecycle::LocalSessionLifecycleContributor for K0IdleObserver {
    async fn on_session_idle(&self, _: &xai_agent_lifecycle::SessionIdleInput) {
        self.count.set(self.count.get() + 1);
        self.notification.notify_one();
    }
}

struct K0Shell {
    actor: Arc<SessionActor>,
    events: Option<tokio::sync::mpsc::UnboundedReceiver<SessionEvent>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    idle: std::rc::Rc<K0IdleObserver>,
    _workspace: tempfile::TempDir,
    _model_home: tempfile::TempDir,
}

impl Drop for K0Shell {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl K0Shell {
    async fn new(server: &MockInferenceServer, with_memory: bool) -> Self {
        let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway_task = tokio::task::spawn_local(async move {
            while let Some(message) = gateway_rx.recv().await {
                if let xai_acp_lib::AcpClientMessage::SessionNotification(args) = message {
                    let _ = args.response_tx.send(Ok(()));
                }
            }
        });
        let (persistence_tx, mut persistence_rx) = tokio::sync::mpsc::unbounded_channel();
        let persistence_task = tokio::task::spawn_local(async move {
            while let Some(message) = persistence_rx.recv().await {
                if let PersistenceMsg::FlushAndAck { respond_to } = message {
                    let _ = respond_to.send(Ok(()));
                }
            }
        });
        let (mut actor, events) =
            create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await;
        let workspace = tempfile::tempdir().unwrap();
        actor.hook_resolved_workspace_root = workspace.path().to_string_lossy().into_owned();
        actor
            .last_live_orphan_reconcile
            .set(Some(std::time::Instant::now()));
        actor.tool_context.monitor_event_buffer = Some(MonitorEventBuffer::new());
        actor.max_retries = 0;
        actor.mcp_reminder_mode = McpReminderMode::Full;
        let (manager, model_home) = manager_with_entries();
        let mut entry = entry_with_search_flag("test", false);
        entry.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        entry.info.context_window = std::num::NonZeroU64::new(256_000).unwrap();
        entry.api_base_url = Some(server.url());
        entry.api_key = Some("test-key".into());
        manager.insert_test_entry("test", entry);
        manager.set_current_model_id(acp::ModelId::new("test"));
        actor.models_manager = manager;
        let agent = {
            let old = actor.agent.borrow();
            let mut policy = old.compaction_policy().clone();
            policy.two_pass_enabled = false;
            policy.remote_compaction_v2 = false;
            policy.wall_clock_budget_secs = 25;
            xai_grok_agent::Agent::new(
                old.definition().clone(),
                old.prompt_context().clone(),
                old.system_prompt().to_string(),
                old.tool_bridge().clone(),
                old.reminder_policy().clone(),
                policy,
                vec![],
                false,
            )
        };
        actor.agent.replace(agent);
        actor.memory.flush_config.enabled = false;
        actor.memory.dream_config.enabled = false;
        actor.memory.save_on_end = false;
        if with_memory {
            let root = workspace.path().join("memory-v2");
            let storage = crate::session::memory::MemoryStorage::new_for_mode(
                std::path::Path::new("/test/k0-workspace"),
                Some(&root),
                crate::config::MemoryMode::V2,
            );
            storage.ensure_initialized().unwrap();
            actor.memory.configured_mode = Some(crate::config::MemoryMode::V2);
            actor.memory.storage.replace(Some(storage));
        }
        let idle = std::rc::Rc::new(K0IdleObserver::default());
        let mut extensions = xai_agent_lifecycle::LocalExtensionRegistryBuilder::default();
        extensions.session_lifecycle_contributor(idle.clone());
        actor.extension_registry = extensions.build();
        let mut config = actor.chat_state_handle.get_sampling_config().await.unwrap();
        config.base_url = server.url();
        config.model = "test".into();
        config.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        config.max_completion_tokens = Some(4096);
        config.max_retries = Some(0);
        actor.chat_state_handle.update_sampling_config(config);
        let mut credentials = actor.chat_state_handle.get_credentials().await;
        credentials.api_key = Some("test-key".into());
        actor.chat_state_handle.update_credentials(credentials);
        let (sampler_tx, mut sampler_rx) = tokio::sync::mpsc::unbounded_channel();
        actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
            xai_grok_sampler::SamplerConfig {
                base_url: server.url(),
                model: "test".into(),
                api_key: Some("test-key".into()),
                api_backend: xai_grok_sampler::ApiBackend::Messages,
                max_completion_tokens: Some(4096),
                max_retries: Some(0),
                ..Default::default()
            },
            xai_grok_sampler::RetryPolicy {
                max_retries: 0,
                ..Default::default()
            },
            sampler_tx,
        );
        actor
            .workspace_ops
            .bind_local_session(
                &actor.session_id_string(),
                actor.tool_context.cwd.as_path().to_path_buf(),
                actor.tool_context.hunk_tracker_handle.clone(),
                actor.agent.borrow().tool_bridge().toolset(),
                None,
            )
            .unwrap();
        let actor = Arc::new(actor);
        let drainer = actor.clone();
        let sampler_task = tokio::task::spawn_local(async move {
            while let Some(event) = sampler_rx.recv().await {
                drainer.handle_sampling_event(event).await;
            }
        });
        Self {
            actor,
            events: Some(events),
            tasks: vec![gateway_task, persistence_task, sampler_task],
            idle,
            _workspace: workspace,
            _model_home: model_home,
        }
    }

    fn buffer(&self, task_id: &str, text: &str) {
        self.actor.tool_context.monitor_event_buffer.as_ref().unwrap().push_capped(
            MonitorEventNotification {
                task_id: task_id.into(), event_text: format!("<monitor-event description=\"k0\" task_id=\"{task_id}\">\n{text}\n</monitor-event>"),
                owner_session_id: Some(self.actor.session_id_string()),
            }, 32,
        );
    }

    async fn install_running(&mut self, prompt: &str) {
        let pending = tokio::task::spawn_local(std::future::pending::<()>());
        let epoch = self.actor.turn_report.start_next_turn();
        self.actor.state.lock().await.running_task = Some(AgentTask::new_at_epoch(
            prompt,
            epoch,
            pending.abort_handle(),
        ));
        self.actor
            .state
            .lock()
            .await
            .pending_inputs
            .push_back(user_item(prompt, "k0"));
        *self.actor.current_prompt_id.lock().unwrap() = Some(prompt.into());
        self.actor
            .session_turn_active
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.tasks.push(pending);
    }

    fn start_loop(&mut self) -> tokio::sync::mpsc::UnboundedSender<SessionCommand> {
        let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let (chat_tx, chat_rx) = tokio::sync::mpsc::unbounded_channel();
        let actor = self.actor.clone();
        let events = self.events.take().expect("one session loop");
        self.tasks.push(tokio::task::spawn_local(async move {
            let _chat_tx = chat_tx;
            super::run_session(
                actor,
                command_rx,
                chat_rx,
                events,
                None,
                Arc::new(parking_lot::Mutex::new(
                    xai_grok_workspace::file_system::CodebaseIndexManager::new(),
                )),
                std::path::PathBuf::from("/tmp"),
                crate::session::fs_watch::FsWatchCapabilities::none(),
            )
            .await;
        }));
        commands
    }
}

fn k0_frames(blocks: Vec<Value>, stop_reason: &str) -> Vec<Value> {
    let mut frames = vec![json!({"type":"message_start","message":{
        "id":"msg_k0","type":"message","role":"assistant","model":"test","content":[],
        "stop_reason":null,"usage":{"input_tokens":10,"output_tokens":0}
    }})];
    for (index, block) in blocks.into_iter().enumerate() {
        frames.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        frames.push(json!({"type":"content_block_stop","index":index}));
    }
    frames.push(json!({"type":"message_delta","delta":{"stop_reason":stop_reason},"usage":{"input_tokens":10,"output_tokens":5}}));
    frames.push(json!({"type":"message_stop"}));
    frames
}

fn k0_script(blocks: Vec<Value>, stop_reason: &str) -> ScriptedResponse {
    ScriptedResponse::sse(
        k0_frames(blocks, stop_reason)
            .into_iter()
            .map(|frame| SseEvent::data(frame.to_string()))
            .collect(),
    )
}

async fn k0_seed_decoded_discovery(actor: &SessionActor) {
    let raw = futures_util::stream::iter(
        k0_frames(
            vec![json!({
                "type":"server_tool_use","id":"srvtoolu_shell_k0","name":"tool_search_tool_regex",
                "input":{"query":"shell preservation"}
            })],
            "end_turn",
        )
        .into_iter()
        .map(|frame| {
            Ok::<_, xai_grok_sampling_types::SamplingError>(serde_json::from_value(frame).unwrap())
        }),
    )
    .boxed();
    let decoded = xai_grok_sampler::stream_messages(raw, None, "k0-seed".into(), K0_TIMEOUT);
    futures_util::pin_mut!(decoded);
    let mut found = false;
    while let Some(event) = decoded.next().await {
        if let xai_grok_sampler::SamplingEvent::Completed { response, .. } = event {
            for item in response.items {
                if matches!(item, ConversationItem::Discovery { .. }) {
                    found = true;
                }
                actor.chat_state_handle.push_model_output(item);
            }
        }
    }
    assert!(
        found,
        "real Messages decoder supplies the pending Discovery"
    );
    assert!(
        actor
            .chat_state_handle
            .get_conversation()
            .await
            .iter()
            .any(|item| matches!(item, ConversationItem::Discovery { .. }))
    );
}

async fn k0_monitor_observations() -> Value {
    let server = MockInferenceServer::start().await.unwrap();
    let mut observations = serde_json::Map::new();
    for mode in [
        "completion",
        "user-start",
        "idle",
        "cancel",
        "rewind",
        "drop",
    ] {
        let mut shell = K0Shell::new(&server, false).await;
        shell
            .actor
            .chat_state_handle
            .push_user_message(ConversationItem::user("keep the pending search"));
        k0_seed_decoded_discovery(&shell.actor).await;
        shell.buffer("watch-k0", "monitor payload");
        match mode {
            "completion" => {
                shell.install_running("completion-k0").await;
                shell.actor.state.lock().await.notifications_suppressed = true;
                let epoch = shell.actor.turn_report.epoch();
                let commands = shell.start_loop();
                let (processed, processed_rx) = tokio::sync::oneshot::channel();
                commands
                    .send(SessionCommand::InjectTurnCompletion {
                        prompt_id: "completion-k0".into(),
                        epoch,
                        result: Box::new(crate::session::commands::ok_end_turn(0, None)),
                        elapsed_ms: Some(0),
                        processed,
                    })
                    .unwrap();
                tokio::time::timeout(K0_TIMEOUT, processed_rx)
                    .await
                    .unwrap()
                    .unwrap();
                tokio::time::timeout(K0_TIMEOUT, shell.idle.notification.notified())
                    .await
                    .expect("post-drain idle contributor is the completion barrier");
                assert!(shell.idle.count.get() > 0);
            }
            "user-start" => {
                assert!(shell.actor.consume_deferred_completions().await.is_empty());
            }
            "idle" => {
                shell
                    .actor
                    .goal_harness_enabled
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                shell
                    .actor
                    .goal_turn_task_ids
                    .lock()
                    .insert("watch-k0".into());
                let (completion_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                shell
                    .actor
                    .clone()
                    .maybe_drain_notifications(completion_tx)
                    .await;
                shell.actor.emit_session_idle_if_idle().await;
                tokio::time::timeout(K0_TIMEOUT, shell.idle.notification.notified())
                    .await
                    .unwrap();
            }
            "cancel" | "rewind" => {
                shell.install_running("cancel-k0").await;
                if mode == "rewind" {
                    shell.actor.state.lock().await.rewindable = true;
                }
                let outcome = shell
                    .actor
                    .cancel_running_task(crate::session::CancelOptions {
                        cancel_subagents: false,
                        kill_background_tasks: false,
                        user_initiated: true,
                        trigger: Some(crate::session::CancelTrigger::CtrlC),
                        history: if mode == "rewind" {
                            crate::session::CancelHistoryDisposition::RewindIfNoOutput {
                                prompt_id: Some("cancel-k0".into()),
                            }
                        } else {
                            crate::session::CancelHistoryDisposition::Keep
                        },
                    })
                    .await;
                assert!(
                    outcome.turn_stopped,
                    "{mode} cancels immediately without a native plan"
                );
            }
            "drop" => {
                shell.actor.state.lock().await.notifications_suppressed = true;
                shell
                    .actor
                    .state
                    .lock()
                    .await
                    .pending_notifications
                    .push(PendingNotification {
                        prompt_id: "monitor-pending-k0".into(),
                        prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(
                            "pending monitor",
                        ))],
                        priority: NotificationPriority::Next,
                        source: NotificationSource::MonitorEvent {
                            task_id: "watch-k0".into(),
                        },
                    });
                shell
                    .actor
                    .state
                    .lock()
                    .await
                    .pending_notifications
                    .push(PendingNotification {
                        prompt_id: "monitor-completed-k0".into(),
                        prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(
                            "task completion",
                        ))],
                        priority: NotificationPriority::Later,
                        source: NotificationSource::MonitorCompleted {
                            task_id: "watch-k0".into(),
                        },
                    });
                let commands = shell.start_loop();
                commands
                    .send(SessionCommand::DropMonitorNotifications {
                        task_id: "watch-k0".into(),
                    })
                    .unwrap();
                let (respond_to, response_rx) = tokio::sync::oneshot::channel();
                commands
                    .send(SessionCommand::SnapshotClientHooks { respond_to })
                    .unwrap();
                tokio::time::timeout(K0_TIMEOUT, response_rx)
                    .await
                    .unwrap()
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let state = shell.actor.state.lock().await;
        let pending: Vec<_> = state
            .pending_notifications
            .iter()
            .map(|notification| {
                json!({
                    "source": match &notification.source {
                        NotificationSource::MonitorEvent { .. } => "monitor_event",
                        NotificationSource::MonitorCompleted { .. } => "monitor_completed",
                        NotificationSource::BashTaskCompleted { .. } => "bash_completed",
                    },
                    "task": notification.source.task_id(),
                    "blocks": notification.prompt_blocks,
                })
            })
            .collect();
        let buffer_len = shell
            .actor
            .tool_context
            .monitor_event_buffer
            .as_ref()
            .unwrap()
            .len();
        assert_eq!(buffer_len, 0, "{mode} consumes the owned monitor buffer");
        observations.insert(
            mode.into(),
            json!({
                "pending": pending, "buffer_len": buffer_len,
                "running": state.running_task.is_some(),
                "idle_observed": shell.idle.count.get() > 0,
            }),
        );
    }
    assert_eq!(
        server.messages_request_count(),
        0,
        "suppressed or consumed monitors never start an unsolicited turn"
    );
    Value::Object(observations)
}

#[derive(Debug, Clone)]
struct K0ReadTool {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    admission: tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>,
}

impl xai_grok_tools::types::tool_metadata::ToolMetadata for K0ReadTool {
    fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
        xai_grok_tools::types::tool::ToolKind::Read
    }
    fn tool_namespace(&self) -> xai_grok_tools::types::tool::ToolNamespace {
        xai_grok_tools::types::tool::ToolNamespace::GrokBuild
    }
    fn description_template(&self) -> &str {
        "Read the offline account fixture"
    }
}

impl xai_tool_runtime::Tool for K0ReadTool {
    type Args = Value;
    type Output = xai_grok_tools::types::output::ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("k0_read").unwrap()
    }
    fn description(
        &self,
        _: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new("k0_read", "Read the offline account fixture")
    }
    async fn run(
        &self,
        _: xai_tool_runtime::ToolCallContext,
        _: Value,
    ) -> Result<Self::Output, xai_tool_runtime::ToolError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut output = xai_grok_tools::types::output::MCPOutput::okay_output(
            "k0_read".into(),
            "k0".into(),
            "account fixture".into(),
        );
        if call == 0 {
            let (ack, received) = tokio::sync::oneshot::channel();
            self.admission.send(ack).unwrap();
            tokio::time::timeout(K0_TIMEOUT, received)
                .await
                .unwrap()
                .unwrap();
            use base64::Engine as _;
            let image = image::ImageBuffer::from_pixel(32, 32, image::Rgba([80u8, 60, 40, 255]));
            let mut png = Vec::new();
            image
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            output
                .extracted_images
                .push(xai_grok_tools::util::base64_images::ExtractedImage {
                    data: base64::engine::general_purpose::STANDARD.encode(png),
                    mime_type: "image/png".into(),
                });
        }
        Ok(xai_grok_tools::types::output::ToolOutput::MCP(output))
    }
}

use xai_grok_tools::implementations::grok_build::task::coordinator as k0_coordinator;
use xai_grok_tools::implementations::grok_build::task::types as k0_task;

type K0Delivery = (
    k0_task::ActiveAgentMessageDelivery,
    tokio::sync::oneshot::Sender<k0_coordinator::ActiveMessageAdmission>,
);

#[derive(Clone)]
struct K0SteerControl(tokio::sync::mpsc::UnboundedSender<K0Delivery>);

impl k0_coordinator::ChildControl for K0SteerControl {
    type ProgressFuture = std::future::Ready<k0_coordinator::SubagentProgress>;
    fn progress(&self) -> Self::ProgressFuture {
        std::future::ready(k0_coordinator::SubagentProgress::default())
    }
    fn send_active_message(
        &self,
        delivery: k0_task::ActiveAgentMessageDelivery,
    ) -> k0_coordinator::SendBoxFuture<k0_coordinator::ActiveMessageAdmission> {
        let (reply, received) = tokio::sync::oneshot::channel();
        self.0.send((delivery, reply)).unwrap();
        Box::pin(async move { received.await.unwrap() })
    }
    fn cancel(&self) {}
}

struct K0SteerRunner {
    control: K0SteerControl,
    started: std::rc::Rc<tokio::sync::Notify>,
}

impl k0_coordinator::ChildRunner for K0SteerRunner {
    type Control = K0SteerControl;
    type CompletionData = ();
    type RunFuture = k0_coordinator::LocalBoxFuture<k0_coordinator::ChildRunOutput<()>>;
    type ValidateFuture = std::future::Pending<k0_task::SubagentValidateTypeOutcome>;
    type DescribeFuture = std::future::Pending<k0_task::SubagentDescribeOutcome>;

    fn run(&self, request: k0_coordinator::ChildRunRequest<Self::Control>) -> Self::RunFuture {
        let control = self.control.clone();
        let started = self.started.clone();
        Box::pin(async move {
            assert!(
                request
                    .reporter
                    .started(k0_coordinator::StartedChild {
                        child_session_id: "test-actor".into(),
                        persona: None,
                        resumed_from: None,
                        child_cwd: "/test/k0-workspace".into(),
                        worktree_path: None,
                        effective_model_id: "test".into(),
                        definition_background: false,
                        control,
                    })
                    .await
            );
            started.notify_one();
            std::future::pending().await
        })
    }
    fn validate_type(&self, _: String, _: String) -> Self::ValidateFuture {
        std::future::pending()
    }
    fn describe_type(&self, _: String, _: Option<String>, _: String) -> Self::DescribeFuture {
        std::future::pending()
    }
    fn supports_wake(&self) -> bool {
        false
    }
    fn on_completed(
        &self,
        _: k0_coordinator::ChildCompletion<()>,
        published: Box<dyn FnOnce() + Send>,
    ) {
        published();
    }
}

struct K0Abort(tokio::task::AbortHandle);
impl Drop for K0Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn k0_admit_steer(actor: &Arc<SessionActor>) {
    use xai_grok_tools::implementations::grok_build::task::backend::{
        ChannelBackend, SubagentBackend,
    };
    let (deliveries, mut delivery_rx) = tokio::sync::mpsc::unbounded_channel();
    let started = std::rc::Rc::new(tokio::sync::Notify::new());
    let runner = K0SteerRunner {
        control: K0SteerControl(deliveries),
        started: started.clone(),
    };
    let (commands, receiver) = k0_coordinator::SubagentCoordinator::<K0SteerRunner>::channel();
    let task = tokio::task::spawn_local(
        k0_coordinator::SubagentCoordinator::from_channel(receiver, runner, Default::default())
            .run(),
    );
    let _coordinator_guard = K0Abort(task.abort_handle());
    let (result_tx, _result_rx) = tokio::sync::oneshot::channel();
    commands
        .send(k0_task::SubagentEvent::Spawn(
            k0_task::SubagentSpawnRequest {
                request: Box::new(k0_task::SubagentRequest {
                    id: "k0-child".into(),
                    prompt: "unused in-process runner".into(),
                    description: "Steer fixture".into(),
                    subagent_type: "fixture".into(),
                    parent_session_id: "parent-k0".into(),
                    parent_prompt_id: None,
                    resume_from: None,
                    cwd: None,
                    runtime_overrides: Default::default(),
                    run_in_background: true,
                    surface_completion: false,
                    await_to_completion: false,
                    fork_context: false,
                    context: Default::default(),
                    owner: Default::default(),
                    cancel_token: Default::default(),
                    spawn_root: Default::default(),
                }),
                result_tx,
                registered_tx: None,
            },
        ))
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, started.notified())
        .await
        .unwrap();
    let actor = actor.clone();
    let delivery_task = tokio::task::spawn_local(async move {
        let (delivery, respond_to) = delivery_rx.recv().await.unwrap();
        let (receipts, mut receipt_rx) = tokio::sync::mpsc::channel(1);
        let (completion, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
        actor
            .admit_parent_agent_message(
                delivery,
                receipts,
                xai_grok_telemetry::TelemetryCtx::new(
                    "parent-k0".into(),
                    Arc::new(tokio::sync::Mutex::new(0)),
                ),
                respond_to,
                completion,
            )
            .await;
        receipt_rx
            .recv()
            .await
            .expect("Steer admission reserves its receipt");
    });
    let _delivery_guard = K0Abort(delivery_task.abort_handle());
    let backend = ChannelBackend::for_coordinator_session(commands, "parent-k0");
    let outcome = tokio::time::timeout(
        K0_TIMEOUT,
        backend.send_active_message(
            k0_task::ActiveAgentMessageRequest::try_new_with_operation(
                "k0-child",
                "parent steer k0",
                k0_task::ActiveAgentMessageOperation::Steer,
            )
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, k0_task::ActiveAgentMessageOutcome::Accepted { .. }),
        "real Steer admission: {outcome:?}"
    );
    delivery_task.await.unwrap();
}

async fn k0_loop_observations() -> Value {
    let server = MockInferenceServer::start().await.unwrap();
    let mut shell = K0Shell::new(&server, true).await;
    shell
        .actor
        .chat_state_handle
        .push_user_message(ConversationItem::user("old account request"));
    k0_seed_decoded_discovery(&shell.actor).await;
    shell.install_running("k0-main").await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (admission, mut admission_rx) = tokio::sync::mpsc::unbounded_channel();
    shell.actor.agent.borrow().tool_bridge().register_mcp_tools(
        "k0_read".into(), K0ReadTool { calls: calls.clone(), admission },
        Some(json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]})),
    ).unwrap();
    let actor = shell.actor.clone();
    shell.tasks.push(tokio::task::spawn_local(async move {
        let ack = admission_rx
            .recv()
            .await
            .expect("real tool dispatch reaches admission barrier");
        actor.pending_interjections.push(PendingInterjection {
            text: "human interjection k0".into(),
            attachments: Vec::new(),
        });
        k0_admit_steer(&actor).await;
        actor
            .pending_skill_reminders
            .lock()
            .push(ConversationItem::system_reminder("skill reminder k0"));
        actor
            .mcp_announcements
            .lock()
            .fingerprints
            .insert("departed-k0".into(), (1, 2, 3));
        actor
            .mcp_reminder_dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
        actor
            .tool_context
            .monitor_event_buffer
            .as_ref()
            .unwrap()
            .push_capped(
                MonitorEventNotification {
                    task_id: "loop-monitor-k0".into(),
                    event_text: "loop-top monitor k0".into(),
                    owner_session_id: Some(actor.session_id_string()),
                },
                32,
            );
        ack.send(()).unwrap();
    }));
    for ordinal in 0..4 {
        server.enqueue_response("/v1/messages", k0_script(vec![json!({
            "type":"tool_use","id":format!("client-k0-{ordinal}"),"name":"k0_read","input":{"query":"account"}
        })], "tool_use"));
    }
    server.enqueue_response(
        "/v1/messages",
        k0_script(
            vec![json!({"type":"text","text":"k0 complete"})],
            "end_turn",
        ),
    );
    let outcome = tokio::time::timeout(
        K0_TIMEOUT,
        shell.actor.handle_prompt(
            "k0-main",
            vec![acp::ContentBlock::Text(acp::TextContent::new(
                "continue the account request",
            ))],
            PromptMode::Agent,
            None,
            None,
            None,
            None,
            true,
            false,
            None,
            None,
            None,
        ),
    )
    .await
    .expect("bounded real no-plan shell turn");
    assert!(
        outcome.is_ok(),
        "no-plan tool-round path succeeds: {outcome:?}"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        4,
        "tool dispatch: outcome={outcome:?}; requests={:?}; history={:?}",
        server.request_bodies(),
        shell.actor.chat_state_handle.get_conversation().await
    );
    assert_eq!(
        server.messages_request_count(),
        5,
        "stationarity nudge reaches the next request"
    );
    assert!(shell.actor.pending_interjections.is_empty());
    assert!(shell.actor.pending_skill_reminders.lock().is_empty());
    assert_eq!(
        shell
            .actor
            .tool_context
            .monitor_event_buffer
            .as_ref()
            .unwrap()
            .len(),
        0
    );
    assert!(
        !shell
            .actor
            .mcp_reminder_dirty
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    let history = shell.actor.chat_state_handle.get_conversation().await;
    let bodies = server.request_bodies();
    let observation = json!({
        "bodies": bodies,
        "tool_calls": calls.load(std::sync::atomic::Ordering::SeqCst),
        "memory_injection_count": shell.actor.memory.injection_count.load(std::sync::atomic::Ordering::Relaxed),
        "memory_context_injected": shell.actor.memory.context_injected.load(std::sync::atomic::Ordering::Relaxed),
        "discovery_survives": history.iter().any(|item| matches!(item, ConversationItem::Discovery { .. })),
        "interjections_empty": shell.actor.pending_interjections.is_empty(),
        "skills_empty": shell.actor.pending_skill_reminders.lock().is_empty(),
        "monitor_empty": shell.actor.tool_context.monitor_event_buffer.as_ref().unwrap().is_empty(),
        "mcp_dirty": shell.actor.mcp_reminder_dirty.load(std::sync::atomic::Ordering::Relaxed),
    });
    k0_normalize_paths(observation, shell._workspace.path())
}

fn k0_normalize_paths(value: Value, workspace: &std::path::Path) -> Value {
    match value {
        Value::String(text) => {
            Value::String(text.replace(&workspace.to_string_lossy().to_string(), "<k0-workspace>"))
        }
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| k0_normalize_paths(value, workspace))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, k0_normalize_paths(value, workspace)))
                .collect(),
        ),
        value => value,
    }
}

// Match the pinned chat-state identity lookup, including its 1024-byte Unix buffer.
fn k0_expected_host_user_id() -> String {
    #[cfg(unix)]
    let identity = {
        // SAFETY: getpwuid_r receives live writable buffers; its C string is copied before they drop.
        let name = unsafe {
            let mut buffer = [0u8; 1024];
            let mut passwd: libc::passwd = std::mem::zeroed();
            let mut result = std::ptr::null_mut();
            let rc = libc::getpwuid_r(
                libc::geteuid(),
                &mut passwd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            );
            if rc == 0 && !passwd.pw_name.is_null() {
                std::ffi::CStr::from_ptr(passwd.pw_name)
                    .to_str()
                    .ok()
                    .map(str::to_owned)
            } else {
                None
            }
        };
        name.or_else(|| std::env::var("USER").ok())
            .or_else(|| std::env::var("LOGNAME").ok())
    };
    #[cfg(not(unix))]
    let identity = std::env::var("USERNAME")
        .ok()
        .map(|name| match std::env::var("USERDOMAIN") {
            Ok(domain) => format!("{domain}\\{name}"),
            Err(_) => name,
        });
    let identity = identity.expect("K0 host identity resolves through passwd or environment");
    use sha2::Digest as _;
    format!(
        "{:x}",
        sha2::Sha256::digest(format!("grok-responses:metadata-user-id:v1:{identity}"))
    )
}

fn k0_normalize_user_id(mut observation: Value, expected_user_id: &str) -> Value {
    for body in observation["loop"]["bodies"]
        .as_array_mut()
        .expect("K0 loop request bodies")
    {
        let user_id = body
            .pointer_mut("/metadata/user_id")
            .expect("K0 request metadata.user_id is present");
        assert_eq!(
            user_id.as_str(),
            Some(expected_user_id),
            "K0 H metadata.user_id matches the account"
        );
        *user_id = json!("<k0-host-user-id>");
    }
    observation
}

async fn k0_compaction_observations() -> Value {
    let server = MockInferenceServer::start().await.unwrap();
    let shell = K0Shell::new(&server, false).await;
    shell
        .actor
        .chat_state_handle
        .replace_conversation(vec![ConversationItem::system(
            "Preserve the account investigation.",
        )]);
    shell
        .actor
        .chat_state_handle
        .push_user_message(ConversationItem::user("old account request"));
    k0_seed_decoded_discovery(&shell.actor).await;
    for _ in 0..8 {
        shell
            .actor
            .chat_state_handle
            .push_user_message(ConversationItem::user(
                "Preserve the account query and its constraints. ".repeat(40),
            ));
        shell
            .actor
            .chat_state_handle
            .push_model_output(ConversationItem::assistant(
                "The account investigation remains incomplete. ".repeat(40),
            ));
    }
    shell
        .actor
        .chat_state_handle
        .push_user_message(ConversationItem::user("continue after compaction"));
    let before = shell.actor.chat_state_handle.get_conversation().await;
    shell.actor.chat_state_handle.record_token_usage(80_000);
    let mut binding = shell
        .actor
        .chat_state_handle
        .capture_transport_binding()
        .await
        .unwrap();
    let error = xai_grok_sampler::SamplingErrorInfo {
        kind: xai_grok_sampler::SamplingErrorKind::Api,
        status_code: Some(400),
        message: "prompt is too long".into(),
        is_retryable: false,
        retry_after_secs: None,
        should_retry: None,
        error_code: None,
        model_metadata: Some(xai_grok_sampling_types::ResponseModelMetadata {
            context_window: Some(50_000),
            max_completion_tokens: None,
            models_etag: None,
        }),
        empty_response_context: None,
        doom_loop_triggers: None,
        doom_loop_aborted_at_chunk: None,
        credential: xai_grok_sampling_types::SentCredential::Sent,
    };
    assert!(shell.actor.should_compact_on_error(&error).await);
    server.set_response(format!(
        "<summary>{}</summary>",
        "Retain the account lookup, user constraints, and pending investigation. ".repeat(20)
    ));
    let outcome = tokio::time::timeout(
        K0_TIMEOUT,
        shell.actor.handle_sampling_failure(
            error,
            &mut binding,
            0,
            transient_state(0, false),
            false,
            TurnParkState::Fresh,
        ),
    )
    .await
    .expect("bounded no-plan retry compaction");
    assert!(
        matches!(outcome, Ok(SamplerFailureRecovery::CompactAndResubmit)),
        "no-plan compaction keeps its recovery arm: {:?}",
        outcome.as_ref().err()
    );
    let after = shell.actor.chat_state_handle.get_conversation().await;
    assert_ne!(
        before.len(),
        after.len(),
        "real compaction replaces history"
    );
    assert!(
        server.messages_request_count() > 0,
        "real compaction uses the controlled Messages transport"
    );
    json!({
        "recovery": "CompactAndResubmit",
        "compaction_count": shell.actor.compaction.count.load(std::sync::atomic::Ordering::Relaxed),
        "before_items": before.len(), "after_items": after.len(),
        "discovery_before": before.iter().filter(|item| matches!(item, ConversationItem::Discovery { .. })).count(),
        "discovery_after": after.iter().filter(|item| matches!(item, ConversationItem::Discovery { .. })).count(),
        "compaction_requests": server.messages_request_count(),
    })
}

fn k0_isolated_shell_test() -> bool {
    const NAME: &str =
        "session::acp_session::native_messages_replay_tests::messages_no_plan_preserves_pin_shell";
    const CHILD: &str = "GROK_TEST_K0_SHELL_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(NAME) {
        return false;
    }
    let home = tempfile::tempdir().unwrap();
    let (mut command, parent_names) = i7yx_shell_child_command(I7yxChildRoute::K0, home.path());
    command
        .args(["--exact", NAME, "--nocapture"])
        .stdin(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut command);
    let output = command.output().unwrap();
    i7yx_assert_child_hygiene(I7yxChildRoute::K0, &output.stdout, &parent_names);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "isolated K0 shell failed: {}",
        i7yx_output_diagnostic(I7yxChildRoute::K0, &output)
    );
    assert!(
        stdout.contains("running 1 test") && stdout.contains(&format!("test {NAME} ... ok")),
        "exact isolated selection: {}",
        i7yx_output_diagnostic(I7yxChildRoute::K0, &output)
    );
    i7yx_print_proof_records(&output.stdout, I7yxChildRoute::K0);
    true
}

#[tokio::test(flavor = "current_thread")]
async fn messages_no_plan_preserves_pin_shell() {
    i7yx_report_child_names();
    if k0_isolated_shell_test() {
        return;
    }
    tokio::task::LocalSet::new().run_until(async {
        let observed = json!({
            "loop": k0_loop_observations().await,
            "monitors": k0_monitor_observations().await,
            "compaction": k0_compaction_observations().await,
        });
        // Frozen from the isolated bb3a3f50 shell observation.
        let expected: Value = serde_json::from_str(r###"{"compaction":{"after_items":4,"before_items":21,"compaction_count":1,"compaction_requests":1,"discovery_after":0,"discovery_before":1,"recovery":"CompactAndResubmit"},"loop":{"bodies":[{"max_tokens":4096,"messages":[{"content":[{"text":"old account request","type":"text"}],"role":"user"},{"content":[{"cache_control":{"type":"ephemeral"},"text":"continue the account request","type":"text"}],"role":"user"}],"metadata":{"user_id":"3bb0043ea6227e7cb2e918da9aaa6dd78d5a9847c9aef422a9b814ad4776b61c"},"model":"test","stream":true,"system":[{"cache_control":{"type":"ephemeral"},"text":"<memory-context>\n## Global memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/global`\n\n# Global memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n## Workspace memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/workspaces/k0-workspace-244ae27a`\n\n# Workspace memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n</memory-context>","type":"text"}],"tools":[{"description":"Read the offline account fixture","input_schema":{"properties":{"query":{"type":"string"}},"required":["query"],"type":"object"},"name":"k0_read"}]},{"max_tokens":4096,"messages":[{"content":[{"text":"old account request","type":"text"}],"role":"user"},{"content":[{"cache_control":{"type":"ephemeral"},"text":"continue the account request","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-0","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"content":"account fixture","tool_use_id":"client-k0-0","type":"tool_result"},{"text":"[Image extracted from tool result above]","type":"text"},{"source":{"data":"iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAqElEQVR4Ae3AA6AkWZbG8f937o3IzKdyS2Oubdu2bdu2bdu2bWmMnpZKr54yMyLu+Xa3anqmhztr1a++9as/2vz3ofLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/734R5tVAjS+u+QnAAAAAElFTkSuQmCC","media_type":"image/png","type":"base64"},"type":"image"},{"text":"The user sent a message while you were working:\n<user_query>\nhuman interjection k0\n</user_query>\nMake sure to complete any unfinished tasks from previous turns.","type":"text"},{"text":"[Message authored by another agent; not a human request or approval.]","type":"text"},{"text":"parent steer k0","type":"text"},{"text":"skill reminder k0","type":"text"},{"text":"<system-reminder>\n<monitor-event task_id=\"loop-monitor-k0\">\n[event] loop-top monitor k0\n</monitor-event>\n</system-reminder>","type":"text"},{"cache_control":{"type":"ephemeral"},"text":"<system-reminder>\nAll MCP servers have disconnected.\nTo use MCP tools, you MUST call `` first to retrieve the tool's input schema before calling ``. NEVER guess parameter names — always use the exact schema returned by ``.\n</system-reminder>","type":"text"}],"role":"user"}],"metadata":{"user_id":"3bb0043ea6227e7cb2e918da9aaa6dd78d5a9847c9aef422a9b814ad4776b61c"},"model":"test","stream":true,"system":[{"cache_control":{"type":"ephemeral"},"text":"<memory-context>\n## Global memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/global`\n\n# Global memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n## Workspace memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/workspaces/k0-workspace-244ae27a`\n\n# Workspace memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n</memory-context>","type":"text"}],"tools":[{"description":"Read the offline account fixture","input_schema":{"properties":{"query":{"type":"string"}},"required":["query"],"type":"object"},"name":"k0_read"}]},{"max_tokens":4096,"messages":[{"content":[{"text":"old account request","type":"text"}],"role":"user"},{"content":[{"text":"continue the account request","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-0","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"content":"account fixture","tool_use_id":"client-k0-0","type":"tool_result"},{"text":"[Image extracted from tool result above]","type":"text"},{"source":{"data":"iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAqElEQVR4Ae3AA6AkWZbG8f937o3IzKdyS2Oubdu2bdu2bdu2bWmMnpZKr54yMyLu+Xa3anqmhztr1a++9as/2vz3ofLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/734R5tVAjS+u+QnAAAAAElFTkSuQmCC","media_type":"image/png","type":"base64"},"type":"image"},{"text":"The user sent a message while you were working:\n<user_query>\nhuman interjection k0\n</user_query>\nMake sure to complete any unfinished tasks from previous turns.","type":"text"},{"text":"[Message authored by another agent; not a human request or approval.]","type":"text"},{"text":"parent steer k0","type":"text"},{"text":"skill reminder k0","type":"text"},{"text":"<system-reminder>\n<monitor-event task_id=\"loop-monitor-k0\">\n[event] loop-top monitor k0\n</monitor-event>\n</system-reminder>","type":"text"},{"cache_control":{"type":"ephemeral"},"text":"<system-reminder>\nAll MCP servers have disconnected.\nTo use MCP tools, you MUST call `` first to retrieve the tool's input schema before calling ``. NEVER guess parameter names — always use the exact schema returned by ``.\n</system-reminder>","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-1","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"cache_control":{"type":"ephemeral"},"content":"account fixture","tool_use_id":"client-k0-1","type":"tool_result"}],"role":"user"}],"metadata":{"user_id":"3bb0043ea6227e7cb2e918da9aaa6dd78d5a9847c9aef422a9b814ad4776b61c"},"model":"test","stream":true,"system":[{"cache_control":{"type":"ephemeral"},"text":"<memory-context>\n## Global memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/global`\n\n# Global memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n## Workspace memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/workspaces/k0-workspace-244ae27a`\n\n# Workspace memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n</memory-context>","type":"text"}],"tools":[{"description":"Read the offline account fixture","input_schema":{"properties":{"query":{"type":"string"}},"required":["query"],"type":"object"},"name":"k0_read"}]},{"max_tokens":4096,"messages":[{"content":[{"text":"old account request","type":"text"}],"role":"user"},{"content":[{"text":"continue the account request","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-0","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"content":"account fixture","tool_use_id":"client-k0-0","type":"tool_result"},{"text":"[Image extracted from tool result above]","type":"text"},{"source":{"data":"iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAqElEQVR4Ae3AA6AkWZbG8f937o3IzKdyS2Oubdu2bdu2bdu2bWmMnpZKr54yMyLu+Xa3anqmhztr1a++9as/2vz3ofLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/734R5tVAjS+u+QnAAAAAElFTkSuQmCC","media_type":"image/png","type":"base64"},"type":"image"},{"text":"The user sent a message while you were working:\n<user_query>\nhuman interjection k0\n</user_query>\nMake sure to complete any unfinished tasks from previous turns.","type":"text"},{"text":"[Message authored by another agent; not a human request or approval.]","type":"text"},{"text":"parent steer k0","type":"text"},{"text":"skill reminder k0","type":"text"},{"text":"<system-reminder>\n<monitor-event task_id=\"loop-monitor-k0\">\n[event] loop-top monitor k0\n</monitor-event>\n</system-reminder>","type":"text"},{"text":"<system-reminder>\nAll MCP servers have disconnected.\nTo use MCP tools, you MUST call `` first to retrieve the tool's input schema before calling ``. NEVER guess parameter names — always use the exact schema returned by ``.\n</system-reminder>","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-1","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"cache_control":{"type":"ephemeral"},"content":"account fixture","tool_use_id":"client-k0-1","type":"tool_result"}],"role":"user"},{"content":[{"id":"client-k0-2","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"cache_control":{"type":"ephemeral"},"content":"account fixture","tool_use_id":"client-k0-2","type":"tool_result"}],"role":"user"}],"metadata":{"user_id":"3bb0043ea6227e7cb2e918da9aaa6dd78d5a9847c9aef422a9b814ad4776b61c"},"model":"test","stream":true,"system":[{"cache_control":{"type":"ephemeral"},"text":"<memory-context>\n## Global memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/global`\n\n# Global memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n## Workspace memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/workspaces/k0-workspace-244ae27a`\n\n# Workspace memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n</memory-context>","type":"text"}],"tools":[{"description":"Read the offline account fixture","input_schema":{"properties":{"query":{"type":"string"}},"required":["query"],"type":"object"},"name":"k0_read"}]},{"max_tokens":4096,"messages":[{"content":[{"text":"old account request","type":"text"}],"role":"user"},{"content":[{"text":"continue the account request","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-0","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"content":"account fixture","tool_use_id":"client-k0-0","type":"tool_result"},{"text":"[Image extracted from tool result above]","type":"text"},{"source":{"data":"iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAqElEQVR4Ae3AA6AkWZbG8f937o3IzKdyS2Oubdu2bdu2bdu2bWmMnpZKr54yMyLu+Xa3anqmhztr1a++9as/2vz3ofLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/734R5tVAjS+u+QnAAAAAElFTkSuQmCC","media_type":"image/png","type":"base64"},"type":"image"},{"text":"The user sent a message while you were working:\n<user_query>\nhuman interjection k0\n</user_query>\nMake sure to complete any unfinished tasks from previous turns.","type":"text"},{"text":"[Message authored by another agent; not a human request or approval.]","type":"text"},{"text":"parent steer k0","type":"text"},{"text":"skill reminder k0","type":"text"},{"text":"<system-reminder>\n<monitor-event task_id=\"loop-monitor-k0\">\n[event] loop-top monitor k0\n</monitor-event>\n</system-reminder>","type":"text"},{"text":"<system-reminder>\nAll MCP servers have disconnected.\nTo use MCP tools, you MUST call `` first to retrieve the tool's input schema before calling ``. NEVER guess parameter names — always use the exact schema returned by ``.\n</system-reminder>","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-1","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"content":"account fixture","tool_use_id":"client-k0-1","type":"tool_result"}],"role":"user"},{"content":[{"id":"client-k0-2","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"cache_control":{"type":"ephemeral"},"content":"account fixture","tool_use_id":"client-k0-2","type":"tool_result"}],"role":"user"},{"content":[{"id":"client-k0-3","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"content":"account fixture","tool_use_id":"client-k0-3","type":"tool_result"},{"cache_control":{"type":"ephemeral"},"text":"<system-reminder>\nYou have called the same tool (`k0_read`) with the exact same arguments 4 times in a row — you appear to be stuck in a polling loop. Stop repeating this call. If you are waiting on a long-running job or command, use a background task, or run a single `sleep` and then check once — do not poll in a tight loop. If you cannot make progress, stop and tell the user what you are waiting for. This turn will be halted automatically if the identical call keeps repeating.\n</system-reminder>","type":"text"}],"role":"user"}],"metadata":{"user_id":"3bb0043ea6227e7cb2e918da9aaa6dd78d5a9847c9aef422a9b814ad4776b61c"},"model":"test","stream":true,"system":[{"cache_control":{"type":"ephemeral"},"text":"<memory-context>\n## Global memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/global`\n\n# Global memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n## Workspace memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/workspaces/k0-workspace-244ae27a`\n\n# Workspace memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n</memory-context>","type":"text"}],"tools":[{"description":"Read the offline account fixture","input_schema":{"properties":{"query":{"type":"string"}},"required":["query"],"type":"object"},"name":"k0_read"}]}],"discovery_survives":true,"interjections_empty":true,"mcp_dirty":false,"memory_context_injected":true,"memory_injection_count":2,"monitor_empty":true,"skills_empty":true,"tool_calls":4},"monitors":{"cancel":{"buffer_len":0,"idle_observed":false,"pending":[{"blocks":[{"text":"<monitor-event description=\"k0\" task_id=\"watch-k0\">\nmonitor payload\n</monitor-event>","type":"text"}],"source":"monitor_event","task":"watch-k0"}],"running":false},"completion":{"buffer_len":0,"idle_observed":true,"pending":[{"blocks":[{"text":"<monitor-event description=\"k0\" task_id=\"watch-k0\">\nmonitor payload\n</monitor-event>","type":"text"}],"source":"monitor_event","task":"watch-k0"}],"running":false},"drop":{"buffer_len":0,"idle_observed":false,"pending":[{"blocks":[{"text":"task completion","type":"text"}],"source":"monitor_completed","task":"watch-k0"}],"running":false},"idle":{"buffer_len":0,"idle_observed":true,"pending":[],"running":false},"rewind":{"buffer_len":0,"idle_observed":false,"pending":[{"blocks":[{"text":"<monitor-event description=\"k0\" task_id=\"watch-k0\">\nmonitor payload\n</monitor-event>","type":"text"}],"source":"monitor_event","task":"watch-k0"}],"running":false},"user-start":{"buffer_len":0,"idle_observed":false,"pending":[{"blocks":[{"text":"<monitor-event description=\"k0\" task_id=\"watch-k0\">\nmonitor payload\n</monitor-event>","type":"text"}],"source":"monitor_event","task":"watch-k0"}],"running":false}}}"###).unwrap();
        let observed = k0_normalize_user_id(observed, &k0_expected_host_user_id());
        let captured_user_id = expected["loop"]["bodies"][0]["metadata"]["user_id"]
            .as_str()
            .expect("frozen K0 capture account")
            .to_owned();
        let expected = k0_normalize_user_id(expected, &captured_user_id);
        assert_eq!(observed, expected, "K0 H parent shell parity");
    }).await;
}

use xai_grok_sampling_types::conversation::native_search_history::*;
use xai_grok_sampling_types::conversation::projection::{
    Boundary, DiscoveryOrigin, NativeHistoryAction, NativeQualification, NativeReplayDialect,
    NativeReplayDomain, NativeReplayShape, TargetRoute,
};
use xai_grok_sampling_types::request_validation::NativeHistoryRequest;

fn native_content_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

fn k1_prepare(
    client: &SamplingClient,
    mut request: ConversationRequest,
    endpoint: &str,
    streaming: bool,
) -> ConversationRequest {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use xai_grok_sampling_types::ConversationItem as Item;
    use xai_grok_sampling_types::conversation::tool_search::ToolSearchItem;
    use serde_json::{Value, json};

    let mut blobs = BTreeMap::<String, Arc<[u8]>>::new();
    let mut put = |kind: &str, value: Value| {
        let bytes = serde_json::to_vec(&value).unwrap();
        let reference = NativeBlobRef {
            kind: kind.into(),
            byte_len: bytes.len() as u64,
            digest: native_content_digest(&bytes),
        };
        blobs.insert(reference.digest.clone(), bytes.into());
        reference
    };
    let source_coordinate = |block| NativeSourceCoordinate {
        lineage: "k1-lineage".into(),
        producing_binding: "k1-issued-A".into(),
        turn: 1,
        item: 0,
        response: 0,
        block,
        submember: None,
        occurrence: block,
    };
    let raw = [
        json!({"type":"server_tool_use","id":"srvtoolu_k1","name":"tool_search_tool_regex","input":{"query":"private fixture query"}}),
        json!({"type":"tool_search_tool_result","tool_use_id":"srvtoolu_k1","content":{"type":"tool_search_tool_search_result","tool_references":[]}}),
    ];
    let offset = request.items.len();
    let mut original = request.items.clone();
    original.extend(raw.iter().map(|value| Item::Discovery {
        item: ToolSearchItem::from_wire(value.clone()).unwrap(),
    }));
    original.push(Item::user("Continue after search."));
    let input_history_bytes: Arc<[u8]> = serde_json::to_vec(&original).unwrap().into();
    let history = NativeHistoryBoundary {
        items: original.len() as u64,
        digest: native_content_digest(&input_history_bytes),
    };
    let origin = put(
        "producing_binding",
        json!({"request":"k1-issued-A","generation":17,"origin":{"status":"Unknown"}}),
    );
    let source = NativeSource {
        schema_version: 2,
        lineage: "k1-lineage".into(),
        source_revision: 3,
        producing_bindings: vec![NativeProducingBinding {
            request: "k1-issued-A".into(),
            generation: Some(17),
            origin: json!({"status":"Unknown"}),
            origin_ref: origin.clone(),
        }],
        responses: vec![NativeResponseBoundary {
            occurrence: 0,
            request: "k1-issued-A".into(),
            lease: 1,
            attempt: 1,
            continuation: 0,
            provider_response_id: Some(json!("msg_k1")),
            source_stop_reason: Some("end_turn".into()),
            block_start: 0,
            block_end: 2,
            stream_complete: true,
            status: NativeResponseStatus::Accepted,
        }],
        ordered_blocks: raw
            .iter()
            .enumerate()
            .map(|(index, value)| NativeRawBlock {
                coordinates: source_coordinate(index as u64),
                wire_index: index as u32,
                arrival_ordinal: index as u64,
                block: put("raw_block", value.clone()),
                partial: None,
                state: NativeBlockState::ClosedFaithful,
                eligible_ref: None,
                eligible_unavailable: None,
            })
            .collect(),
        eligible_snapshots: vec![],
        older_sources: vec![],
    };
    let source_ref = put(
        "source",
        serde_json::to_value(NativeSourceV2::from_parts(source.clone(), vec![])).unwrap(),
    );
    let active_ref = put(
        "active",
        serde_json::to_value(NativeActive::default()).unwrap(),
    );
    let mut ledger = NativeLedger {
        source_digest: source_ref.digest.clone(),
        active_digest: active_ref.digest.clone(),
        groups: vec![NativeGroupRecord {
            key: "k1-completed".into(),
            family: "Messages".into(),
            execution: "server".into(),
            origin_ref: origin.clone(),
            validated_join: Some("srvtoolu_k1".into()),
            keyless_server_fifo: false,
            members: (0..2).map(source_coordinate).collect(),
            duplicate_observations: vec![],
            state: "Completed".into(),
        }],
        accounting: (0..2)
            .map(|index| NativeViewLink {
                source: source_coordinate(index),
                source_digest: source_ref.digest.clone(),
                view_item: Some(offset as u64 + index),
                view_digest: Some(native_content_digest(
                    &serde_json::to_vec(&original[offset + index as usize]).unwrap(),
                )),
                role: "Discovery".into(),
                source_subrange: None,
            })
            .collect(),
        ..Default::default()
    };
    ledger
        .lifecycle
        .groups
        .insert("k1-completed".into(), "Completed".into());
    let ledger_ref = put(
        "ledger",
        serde_json::to_value(NativeLedgerV2::from_parts(ledger.clone(), vec![])).unwrap(),
    );
    let record = NativeSearchHistoryV1 {
        schema_version: 2,
        policy_revision: 1,
        reader_floor: 2,
        limits_revision: 1,
        lineage: NativeLineage {
            id: "k1-lineage".into(),
            source_revision: 3,
            source_digest: source_ref.digest.clone(),
        },
        generation: 5,
        high_water: 7,
        source: source_ref,
        active: active_ref,
        ledger: ledger_ref,
        history_boundary: history.clone(),
        exchange_snapshot: None,
        previous_checkpoint: None,
        capacity: BTreeMap::new(),
    };
    let (normalized, _) = client.finalize_conversation_messages(&request).unwrap();
    request = normalized;
    let model = request.model.clone().unwrap();
    let admission = request
        .search_admission
        .unwrap_or(xai_grok_sampling_types::SearchAdmission {
            supports_search_tool: false,
            has_searchable_tools: false,
        });
    request.search_admission = Some(admission);
    let domain = NativeReplayDomain {
        backend: ApiBackend::Messages,
        dialect: NativeReplayDialect::Messages,
        model: model.clone(),
        row: model.clone(),
        provider: "offline".into(),
        deployment: "offline".into(),
        deployment_revision: "1".into(),
        endpoint: endpoint.into(),
        principal: "fixture".into(),
    };
    let route = TargetRoute::new(ApiBackend::Messages, Boundary::Vertex, admission);
    let selected: Vec<_> = (0..2).map(source_coordinate).collect();
    let identity = NativeIdentityExtension::Revision2 {
        eligible_member_links: vec![],
        eligible_member_accounting: vec![],
    };
    let authorship = (0..2)
        .map(|index| (index, NativeIdAuthorship::Provider))
        .collect();
    let core = prepare_native_history_core(
        &NativePureHistory {
            record: &record,
            source: &source,
            ledger: &ledger,
            identity: &identity,
            blobs: &blobs,
            authorship: &authorship,
        },
        &NativeProjectionInputs {
            target_model_id: &model,
            target: &route,
            tools: &request.tools,
            current: None,
            selected: &selected,
            origin_domains: &Default::default(),
            pending: &Default::default(),
            prior_f: None,
        },
        |_| {
            (
                DiscoveryOrigin {
                    boundary: Boundary::Vertex,
                    owner_model_id: None,
                },
                NativeQualification {
                    source: None,
                    target: Some(domain.clone()),
                    request: Some(domain.clone()),
                    original_admission: admission,
                    shape: NativeReplayShape {
                        family: NativeReplayFamily::MessagesServer,
                        keyless_server: false,
                        output_id_present: false,
                        completed: true,
                        result_error: false,
                        referenced_members: 0,
                    },
                    receipt: None,
                    intact_membership: true,
                    current_backing: false,
                    faithful_identity: true,
                },
            )
        },
    )
    .unwrap();
    assert_eq!(core.decision.groups.len(), 1);
    assert_eq!(core.decision.groups[0].action, NativeHistoryAction::F);
    assert_eq!(core.accounting.qualification, core.decision.qualification);
    assert_eq!(core.accounting.raw_members.len(), 2);
    assert!(core.accounting.references.is_empty());
    assert_eq!(core.decision.members.len(), 2);
    assert_eq!(core.accounting.groups.demoted, 1);
    assert_eq!(core.accounting.members.demoted, 2);
    assert_eq!(
        core.decision.members[0].neutral.as_ref().unwrap().kind,
        NativeNeutralKind::Call
    );
    assert_eq!(
        core.decision.members[1].neutral.as_ref().unwrap().kind,
        NativeNeutralKind::Result
    );
    assert!(core.decision.members.iter().all(|member| {
        member
            .neutral
            .as_ref()
            .is_some_and(|text| text.text.len() <= 128)
    }));
    assert!(
        core.decision
            .members
            .iter()
            .map(|member| member.neutral.as_ref().unwrap().text.len())
            .sum::<usize>()
            <= 256
    );
    let mut replacements = BTreeMap::new();
    for member in &core.decision.members {
        let link = ledger
            .accounting
            .iter()
            .find(|link| link.source == member.source)
            .unwrap();
        assert!(member.raw.is_none());
        let output = member
            .neutral
            .as_ref()
            .map(|neutral| Item::assistant(neutral.text.clone()));
        assert!(
            replacements
                .insert(link.view_item.unwrap() as usize, output)
                .is_none()
        );
    }
    request.items = original
        .into_iter()
        .enumerate()
        .flat_map(|(index, item)| replacements.remove(&index).unwrap_or(Some(item)))
        .collect();
    assert!(replacements.is_empty());
    let (normalized, streaming_carrier) = client.finalize_conversation_messages(&request).unwrap();
    request = normalized;
    let encoded = if streaming {
        streaming_carrier
    } else {
        xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(
            &xai_grok_sampling_types::build_messages_request(&request),
        )
        .unwrap()
    };
    let carrier = NativeFinalCarrier::capture(encoded.as_bytes().to_vec().into()).unwrap();
    let declarations: Vec<Value> = carrier
        .declarations()
        .map(|bytes| serde_json::from_slice(bytes).unwrap())
        .unwrap_or_default();
    let identities = declarations
        .into_iter()
        .enumerate()
        .filter_map(|(index, declaration)| {
            let schema_value = declaration.get("input_schema")?;
            let schema_bytes: Arc<[u8]> = serde_json::to_vec(schema_value).unwrap().into();
            let schema = NativeBlobRef {
                kind: "eligible_schema".into(),
                byte_len: schema_bytes.len() as u64,
                digest: native_content_digest(&schema_bytes),
            };
            let snapshot = NativeEligibleSnapshot {
                version: 1,
                namespace_kind: NativeNamespaceKind::GrokBuild,
                qualified_namespace: "grok_build".into(),
                child_name: declaration["name"].as_str().unwrap().into(),
                schema: schema.clone(),
                binding: origin.clone(),
            };
            let provenance_bytes: Arc<[u8]> = serde_json::to_vec(&snapshot).unwrap().into();
            Some(NativeDeclarationIdentity {
                declaration_index: index as u64,
                child_index: None,
                namespace_kind: snapshot.namespace_kind,
                qualified_namespace: snapshot.qualified_namespace,
                child_name: snapshot.child_name,
                declaration,
                schema,
                schema_bytes,
                provenance: NativeBlobRef {
                    kind: "eligible_snapshot".into(),
                    byte_len: provenance_bytes.len() as u64,
                    digest: native_content_digest(&provenance_bytes),
                },
                provenance_bytes,
                binding_bytes: Some(blobs[&origin.digest].clone()),
            })
        })
        .collect();
    let context = NativeHistoryRequestContext::captured(
        NativeHistoryRequirement::Selected,
        NativeHistoryCapture {
            history,
            history_revision: 11,
            lineage: record.lineage.clone(),
            native_generation: 5,
            high_water: 7,
            projection_revision: 0,
            strict_append_sequence: 13,
            binding_generation: 17,
            install_generation: 19,
            issued_request: "k1-issued-A".into(),
            target: domain,
            execution: NativeHistoryExecution::Server,
            original_admission: admission,
            versions: NativeHistoryVersions {
                schema: 2,
                policy: 1,
                projection: 1,
                reader: 2,
                limits: 1,
            },
        },
        NativeHistoryWriteAuthority::SendOnly,
    );
    let plan = NativeHistoryPlan::checked(
        context.clone(),
        NativeHistoryPlanInput {
            ledger_bytes: blobs[&record.ledger.digest].clone(),
            record,
            input_history_bytes,
            lowering_request: &request,
            post_history_bytes: serde_json::to_vec(&request.items).unwrap().into(),
            carrier,
            identities,
            qualification: core.accounting.qualification.clone(),
            core,
        },
    )
    .unwrap();
    assert_eq!(plan.readiness(), &NativePlanReadiness::Tentative);
    assert_eq!(
        validate_native_history_plan(&context, Some(&plan), plan.carrier()),
        Err(NativePlanRefusal::NotReady)
    );
    let plan = plan.send_only().unwrap();
    assert_eq!(
        validate_native_history_plan(&context, Some(&plan), plan.carrier()),
        Ok(NativeHistoryReady::SendOnly)
    );
    let body: Value = serde_json::from_slice(encoded.as_bytes()).unwrap();
    assert_eq!(body["max_tokens"], request.max_output_tokens.unwrap());
    assert_eq!(body.get("stream"), streaming.then_some(&Value::Bool(true)));
    request.encoded = streaming.then_some(encoded);
    request.native_history = Some(NativeHistoryRequest::new(context, Some(plan)));
    request
}

use xai_grok_sampler::{ApiBackend, SamplingClient, SamplerConfig};

struct K1NativePersistence {
    ordinary: xai_chat_state::NullChatPersistence,
    capture: bool,
}

impl xai_chat_state::ChatPersistence for K1NativePersistence {
    fn persist_message(&mut self, item: &ConversationItem) {
        self.ordinary.persist_message(item);
    }
    fn persist_working_directory_switch_and_ack(
        &mut self,
        item: &ConversationItem,
    ) -> tokio::sync::oneshot::Receiver<
        Result<xai_chat_state::StrictAppendAck, xai_chat_state::StrictAppendError>,
    > {
        self.ordinary.persist_working_directory_switch_and_ack(item)
    }
    fn replace_history(&mut self, items: &[ConversationItem]) {
        self.ordinary.replace_history(items);
    }
    fn replace_history_for_strip_and_ack(
        &mut self,
        items: &[ConversationItem],
    ) -> tokio::sync::oneshot::Receiver<std::io::Result<()>> {
        self.ordinary.replace_history_for_strip_and_ack(items)
    }
    fn flush(&mut self) {
        self.ordinary.flush();
    }
    fn read_native_history(
        &mut self,
    ) -> tokio::sync::oneshot::Receiver<
        Result<xai_chat_state::persistence::native::NativeRead, NativeUnavailable>,
    > {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(if self.capture {
            Ok(xai_chat_state::persistence::native::NativeRead {
                lineage: "test-actor".into(),
                checkpoint: None,
            })
        } else {
            Err(NativeUnavailable::Persistence)
        });
        rx
    }
}

struct K1Shell {
    actor: Arc<SessionActor>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    evidence: std::rc::Rc<std::cell::RefCell<Vec<(bool, u64)>>>,
    _workspace: tempfile::TempDir,
    _model_home: tempfile::TempDir,
}

impl K1Shell {
    async fn new(base_url: &str, capture: bool) -> Self {
        let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway = tokio::task::spawn_local(async move {
            while let Some(message) = gateway_rx.recv().await {
                if let xai_acp_lib::AcpClientMessage::SessionNotification(args) = message {
                    let _ = args.response_tx.send(Ok(()));
                }
            }
        });
        let (persistence_tx, mut persistence_rx) = tokio::sync::mpsc::unbounded_channel();
        let persistence = tokio::task::spawn_local(async move {
            while let Some(message) = persistence_rx.recv().await {
                if let PersistenceMsg::FlushAndAck { respond_to } = message {
                    let _ = respond_to.send(Ok(()));
                }
            }
        });
        let (mut actor, _events) = create_test_actor_with_chat_persistence(
            0,
            256_000,
            85,
            gateway_tx,
            persistence_tx,
            Box::new(K1NativePersistence {
                ordinary: xai_chat_state::NullChatPersistence,
                capture,
            }),
        )
        .await;
        let workspace = tempfile::tempdir().unwrap();
        actor.hook_resolved_workspace_root = workspace.path().to_string_lossy().into_owned();
        actor
            .last_live_orphan_reconcile
            .set(Some(std::time::Instant::now()));
        actor.max_retries = 0;
        actor.transient_retry_enabled = false;
        actor.mcp_reminder_mode = McpReminderMode::Full;
        let (manager, model_home) = manager_with_entries();
        let mut entry = entry_with_search_flag("test", false);
        entry.info.api_backend = ApiBackend::Messages;
        entry.info.context_window = std::num::NonZeroU64::new(256_000).unwrap();
        entry.api_base_url = Some(base_url.into());
        entry.api_key = Some("test-key".into());
        manager.insert_test_entry("test", entry);
        manager.set_current_model_id(acp::ModelId::new("test"));
        actor.models_manager = manager;
        actor.memory.flush_config.enabled = false;
        actor.memory.dream_config.enabled = false;
        actor.memory.save_on_end = false;
        let agent = {
            let old = actor.agent.borrow();
            let mut policy = old.compaction_policy().clone();
            policy.two_pass_enabled = false;
            policy.remote_compaction_v2 = false;
            xai_grok_agent::Agent::new(
                old.definition().clone(),
                old.prompt_context().clone(),
                old.system_prompt().to_string(),
                old.tool_bridge().clone(),
                old.reminder_policy().clone(),
                policy,
                vec![],
                false,
            )
        };
        actor.agent.replace(agent);
        let mut config = actor.chat_state_handle.get_sampling_config().await.unwrap();
        config.base_url = base_url.into();
        config.model = "test".into();
        config.api_backend = ApiBackend::Messages;
        config.max_completion_tokens = Some(4096);
        config.max_retries = Some(0);
        actor.chat_state_handle.update_sampling_config(config);
        let mut credentials = actor.chat_state_handle.get_credentials().await;
        credentials.api_key = Some("test-key".into());
        actor.chat_state_handle.update_credentials(credentials);
        let (events, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
            k1_shell_config(base_url),
            xai_grok_sampler::RetryPolicy {
                max_retries: 0,
                ..Default::default()
            },
            events,
        );
        actor
            .workspace_ops
            .bind_local_session(
                &actor.session_id_string(),
                actor.tool_context.cwd.as_path().to_path_buf(),
                actor.tool_context.hunk_tracker_handle.clone(),
                actor.agent.borrow().tool_bridge().toolset(),
                None,
            )
            .unwrap();
        let actor = Arc::new(actor);
        let drainer = actor.clone();
        let evidence = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let observed = evidence.clone();
        let sampler = tokio::task::spawn_local(async move {
            while let Some(event) = event_rx.recv().await {
                if let xai_grok_sampler::SamplingEvent::NativeAttemptEvidence { evidence, .. } =
                    &event
                {
                    observed.borrow_mut().push(match evidence {
                        NativeEvidenceEvent::Attempt(attempt) => (false, attempt.http_dispatches),
                        NativeEvidenceEvent::End => (true, 0),
                    });
                }
                drainer.handle_sampling_event(event).await;
            }
        });
        Self {
            actor,
            tasks: vec![gateway, persistence, sampler],
            evidence,
            _workspace: workspace,
            _model_home: model_home,
        }
    }

    async fn finish(self) {
        assert_eq!(self.actor.sampler_handle.active_count().await, 0);
        for task in self.tasks {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
    }
}

fn k1_shell_config(base_url: &str) -> SamplerConfig {
    SamplerConfig {
        base_url: base_url.into(),
        model: "test".into(),
        api_key: Some("test-key".into()),
        api_backend: ApiBackend::Messages,
        max_completion_tokens: Some(4096),
        max_retries: Some(0),
        ..Default::default()
    }
}

async fn k1_shell_http() -> (
    String,
    Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    tokio::task::JoinHandle<()>,
) {
    let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = bodies.clone();
    let router = axum::Router::new().route(
        "/v1/messages",
        axum::routing::post(move |body: axum::body::Bytes| {
            let captured = captured.clone();
            async move {
                let value: Value = serde_json::from_slice(&body).unwrap();
                captured.lock().unwrap().push(body.to_vec());
                let (content_type, response) = if value.get("stream") == Some(&Value::Bool(true)) {
                    (
                        "text/event-stream",
                        k0_frames(
                            vec![json!({"type":"text","text":"K1 complete"})],
                            "end_turn",
                        )
                        .into_iter()
                        .map(|event| format!("data: {event}\n\n"))
                        .collect::<String>(),
                    )
                } else {
                    (
                        "application/json",
                        json!({"id":"msg_k1","type":"message","role":"assistant","model":"test",
                    "content":[{"type":"text","text":"K1 complete"}],"stop_reason":"end_turn",
                    "stop_sequence":null,"usage":{"input_tokens":5,"output_tokens":3}})
                        .to_string(),
                    )
                };
                axum::response::Response::builder()
                    .header("content-type", content_type)
                    .body(axum::body::Body::from(response))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, bodies, server)
}
async fn k1_shell_actor_case(
    base_url: &str,
    bodies: &Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    capture: bool,
    case: &str,
) {
    let shell = K1Shell::new(base_url, capture).await;
    shell
        .actor
        .chat_state_handle
        .push_user_message(ConversationItem::user("K1 bound shell request"));
    let before = bodies.lock().unwrap().len();
    let builds = std::cell::Cell::new(0usize);
    let expected = std::cell::RefCell::new(Vec::new());
    let mut prepare = |request: &mut ConversationRequest,
                       binding: &xai_chat_state::RequestTransportBinding| {
        builds.set(builds.get() + 1);
        assert_eq!(binding.config.base_url, base_url);
        assert_eq!(binding.config.api_backend, ApiBackend::Messages);
        let client = SamplingClient::new(k1_shell_config(&binding.config.base_url)).unwrap();
        *request = k1_prepare(
            &client,
            request.clone(),
            &format!("{base_url}/messages"),
            true,
        );
        *expected.borrow_mut() = request.encoded.as_ref().unwrap().as_bytes().to_vec();
        match case {
            "valid" | "install-B-retain-A" => {}
            "missing-plan" => request.native_history.as_mut().unwrap().invalidate_plan(),
            "stale-request" => request.max_output_tokens = Some(4097),
            "stale-context" => {
                let native = request.native_history.as_ref().unwrap();
                let mut context = native.context().capture().clone();
                context.history_revision += 1;
                request.native_history = Some(NativeHistoryRequest::new(
                    NativeHistoryRequestContext::captured(
                        NativeHistoryRequirement::Selected,
                        context,
                        NativeHistoryWriteAuthority::SendOnly,
                    ),
                    native.plan().cloned(),
                ));
            }
            "wrong-banked-body" => {
                let mut changed = request.clone();
                changed.max_output_tokens = Some(4098);
                request.encoded = Some(client.encode_conversation_messages(&changed).unwrap());
            }
            _ => unreachable!(),
        }
        if case == "install-B-retain-A" {
            let mut next = binding.config.clone();
            next.model = "installed-B".into();
            next.base_url = "http://127.0.0.1:1/not-A".into();
            shell.actor.chat_state_handle.update_sampling_config(next);
        }
        Ok(())
    };
    let mut salvage = super::length_salvage::LengthSalvage::new(None);
    let mut sampling = Default::default();
    let result = tokio::time::timeout(
        K0_TIMEOUT,
        shell
            .actor
            .process_conversation_turn_for_native_history_test(
                "k1-shell-turn",
                None,
                None,
                None,
                &mut salvage,
                &mut sampling,
                &mut prepare,
            ),
    )
    .await
    .expect("K1 shell turn terminates");
    assert_eq!(builds.get(), 1, "one real bound build; no fixture rebuild");
    let success = matches!(case, "valid" | "install-B-retain-A");
    {
        let bodies = bodies.lock().unwrap();
        if success {
            assert!(
                matches!(result, Ok(TurnOutcome::Completed { .. })),
                "K1 shell positive: {:?}",
                result.err()
            );
            assert_eq!(bodies.len(), before + 1);
            assert_eq!(
                bodies[before],
                *expected.borrow(),
                "K1 exact actual shell HTTP bytes"
            );
        } else {
            assert_eq!(
                bodies.len(),
                before,
                "K1 shell forbidden HTTP: capture={capture}/{case}"
            );
            assert!(
                result.is_err(),
                "K1 shell typed refusal surfaces through normal completion"
            );
        }
    }
    let evidence = shell.evidence.borrow();
    assert_eq!(
        evidence.iter().filter(|(end, _)| *end).count(),
        usize::from(capture)
    );
    assert_eq!(
        evidence
            .iter()
            .map(|(_, dispatches)| dispatches)
            .sum::<u64>(),
        u64::from(capture && success)
    );
    println!(
        "K1_SHELL_CASE actor capture={capture} {case} builds={} http_delta={} ends={}",
        builds.get(),
        bodies.lock().unwrap().len() - before,
        evidence.iter().filter(|(end, _)| *end).count()
    );
    drop(evidence);
    shell.finish().await;
}

async fn k1_shell_direct_cases(base_url: &str, bodies: &Arc<std::sync::Mutex<Vec<Vec<u8>>>>) {
    let client = SamplingClient::new(k1_shell_config(base_url)).unwrap();
    for route in ["direct", "collect", "non-streaming", "compact-helper"] {
        let prepared = k1_prepare(
            &client,
            ConversationRequest::from_items(vec![ConversationItem::user("K1 direct shell")]),
            &format!("{base_url}/messages"),
            route != "non-streaming",
        );
        let expected = prepared
            .native_history
            .as_ref()
            .unwrap()
            .plan()
            .unwrap()
            .carrier()
            .bytes()
            .to_vec();
        for missing in [false, true] {
            let mut request = prepared.clone();
            request.encoded = None;
            if missing {
                request.native_history.as_mut().unwrap().invalidate_plan();
            }
            let before = bodies.lock().unwrap().len();
            let result: Result<(), String> = tokio::time::timeout(K0_TIMEOUT, async {
                match route {
                    "direct" => {
                        let (mut stream, _) = client.conversation_stream_messages(request).await.map_err(|error| error.to_string())?;
                        while let Some(event) = stream.next().await { event.map_err(|error| error.to_string())?; }
                        Ok(())
                    }
                    "collect" => client.conversation_collect(request).await.map(|_| ()).map_err(|error| error.to_string()),
                    "non-streaming" => client.conversation_messages(request).await.map(|_| ()).map_err(|error| error.to_string()),
                    "compact-helper" => {
                        let (mut stream, _) = crate::session::helpers::session_compact::send_compact_messages_request(
                            &client, request, &tokio_util::sync::CancellationToken::new(),
                        ).await.map_err(|error| format!("{error:?}"))?;
                        while let Some(event) = stream.next().await { event.map_err(|error| error.to_string())?; }
                        Ok(())
                    }
                    _ => unreachable!(),
                }
            }).await.expect("K1 shell direct transport terminates");
            let bodies = bodies.lock().unwrap();
            if missing {
                assert_eq!(
                    bodies.len(),
                    before,
                    "K1 shell direct missing proof refuses {route}"
                );
                assert!(result.is_err());
            } else {
                assert!(result.is_ok(), "K1 shell {route}: {result:?}");
                assert_eq!(bodies.len(), before + 1);
                assert_eq!(bodies[before], expected);
            }
            println!(
                "K1_SHELL_CASE {route} missing={missing} http_delta={}",
                bodies.len() - before
            );
        }
    }
}

fn k1_isolated_shell_test() -> bool {
    const NAME: &str = "session::acp_session::native_messages_replay_tests::messages_replay_plan_checked_on_all_sends";
    const CHILD: &str = "GROK_TEST_K1_SHELL_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(NAME) {
        return false;
    }
    let home = tempfile::tempdir().unwrap();
    let (mut command, parent_names) = i7yx_shell_child_command(I7yxChildRoute::K1, home.path());
    command
        .args(["--exact", NAME, "--nocapture"])
        .stdin(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut command);
    let output = command.output().unwrap();
    i7yx_assert_child_hygiene(I7yxChildRoute::K1, &output.stdout, &parent_names);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "isolated K1 shell failed: {}",
        i7yx_output_diagnostic(I7yxChildRoute::K1, &output)
    );
    assert!(stdout.contains("running 1 test") && stdout.contains(&format!("test {NAME} ... ok")));
    i7yx_print_proof_records(&output.stdout, I7yxChildRoute::K1);
    true
}

#[tokio::test(flavor = "current_thread")]
async fn messages_replay_plan_checked_on_all_sends() {
    if k2lg_selector_prelude().await {
        return;
    }
    const CHILD_SELECTOR: &str = "session::acp_session::native_messages_replay_tests::messages_replay_plan_checked_on_all_sends";
    if std::env::var("GROK_TEST_K1_SHELL_CHILD").as_deref() != Ok(CHILD_SELECTOR) {
        k2lg_pending_isolated().await;
    }
    if k1_isolated_shell_test() {
        return;
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, bodies, server) = k1_shell_http().await;
            for capture in [false, true] {
                for case in [
                    "valid",
                    "missing-plan",
                    "stale-request",
                    "stale-context",
                    "wrong-banked-body",
                    "install-B-retain-A",
                ] {
                    k1_shell_actor_case(&base_url, &bodies, capture, case).await;
                }
            }
            k1_shell_direct_cases(&base_url, &bodies).await;
            server.abort();
            assert!(server.await.unwrap_err().is_cancelled());
        })
        .await;
}


use xai_grok_sampling_types::conversation::native_search_history as k2_history;
use xai_grok_sampling_types::conversation::projection as k2_replay;

type K2Checkpoint = xai_chat_state::persistence::native::NativeCheckpoint;

#[derive(Clone, Debug)]
struct K2ClientTool {
    name: &'static str,
    executions: Arc<std::sync::Mutex<Vec<String>>>,
    entered: tokio::sync::mpsc::UnboundedSender<String>,
    release: Arc<tokio::sync::Semaphore>,
}

impl xai_grok_tools::types::tool_metadata::ToolMetadata for K2ClientTool {
    fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
        xai_grok_tools::types::tool::ToolKind::Other
    }
    fn tool_namespace(&self) -> xai_grok_tools::types::tool::ToolNamespace {
        xai_grok_tools::types::tool::ToolNamespace::MCP
    }
    fn description_template(&self) -> &str {
        "Bounded offline K2 client tool"
    }
}

impl xai_tool_runtime::Tool for K2ClientTool {
    type Args = Value;
    type Output = xai_grok_tools::types::output::ToolOutput;
    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(self.name).unwrap()
    }
    fn description(
        &self,
        _: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(self.name, "Bounded offline K2 client tool")
    }
    async fn run(
        &self,
        _: xai_tool_runtime::ToolCallContext,
        arguments: Value,
    ) -> Result<Self::Output, xai_tool_runtime::ToolError> {
        self.executions.lock().unwrap().push(self.name.into());
        self.entered.send(self.name.into()).unwrap();
        self.release.acquire().await.unwrap().forget();
        Ok(xai_grok_tools::types::output::ToolOutput::Text(format!(
            "{} actual result: {}",
            self.name, arguments["value"]
        ).into()))
    }
}

struct K2ReplaySession {
    actor: Arc<SessionActor>,
    persistence: crate::session::persistence::PersistenceHandle,
    directory: tempfile::TempDir,
    _models: tempfile::TempDir,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    executions: Arc<std::sync::Mutex<Vec<String>>>,
    entered: tokio::sync::mpsc::UnboundedReceiver<String>,
    releases: [Arc<tokio::sync::Semaphore>; 2],
    source_acks: tokio::sync::mpsc::UnboundedReceiver<k2_history::NativeResponseStatus>,
}

impl Drop for K2ReplaySession {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl K2ReplaySession {
    async fn new(server: &MockInferenceServer) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config = xai_grok_sampler::SamplerConfig {
            base_url: server.url(),
            model: "k2-replay".into(),
            api_key: Some("nonsecret-k2-key".into()),
            api_backend: xai_grok_sampler::ApiBackend::Messages,
            max_completion_tokens: Some(4096),
            max_retries: Some(0),
            ..Default::default()
        };
        let info = crate::session::info::Info {
            id: acp::SessionId::new(format!(
                "k2-{}",
                directory.path().file_name().unwrap().to_string_lossy()
            )),
            cwd: directory.path().to_string_lossy().into_owned(),
        };
        let persistence = crate::session::persistence::new_with_explicit_dir(
            &info,
            directory.path().to_path_buf(),
            acp::ModelId::new("k2-replay"),
            xai_grok_sampler::SamplingClient::new(config.clone()).unwrap(),
            "k2-replay".into(),
            crate::session::persistence::ExplicitSessionOpen::New {
                identity: None,
                next_trace_turn: None,
            },
        )
        .await
        .unwrap();
        let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway = tokio::task::spawn_local(async move {
            while let Some(message) = gateway_rx.recv().await {
                if let xai_acp_lib::AcpClientMessage::SessionNotification(arguments) = message {
                    let _ = arguments.response_tx.send(Ok(()));
                }
            }
        });
        let (mut actor, _) = create_test_actor_with_chat_persistence(
            0,
            256_000,
            85,
            gateway_tx,
            persistence.tx.clone(),
            Box::new(
                crate::session::chat_persistence::ChannelChatPersistence::new(
                    persistence.tx.clone(),
                ),
            ),
        )
        .await;
        actor.session_info = info;
        actor.attach_non_interactive.set(true);
        actor.max_retries = 0;
        actor.compaction.compaction_mode = xai_chat_state::CompactionMode::Summary;
        {
            let mut slot = actor.agent.borrow_mut();
            let mut policy = slot.compaction_policy().clone();
            policy.two_pass_enabled = false;
            policy.remote_compaction_v2 = false;
            policy.memory_flush_enabled = false;
            *slot = xai_grok_agent::Agent::new(
                slot.definition().clone(),
                slot.prompt_context().clone(),
                slot.system_prompt().to_string(),
                slot.tool_bridge().clone(),
                slot.reminder_policy().clone(),
                policy,
                Vec::new(),
                true,
            );
        }
        actor.memory.flush_config.enabled = false;
        actor.memory.dream_config.enabled = false;
        actor.memory.save_on_end = false;
        let (manager, models) = manager_with_entries();
        let mut entry = entry_with_search_flag("k2-replay", true);
        entry.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        entry.api_base_url = Some(server.url());
        entry.api_key = Some("nonsecret-k2-key".into());
        manager.insert_test_entry("k2-replay", entry);
        manager.set_current_model_id(acp::ModelId::new("k2-replay"));
        actor.models_manager = manager;
        let mut binding = actor
            .chat_state_handle
            .capture_transport_binding()
            .await
            .unwrap();
        binding.config.base_url = server.url();
        binding.config.model = "k2-replay".into();
        binding.config.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        binding.config.supports_search_tool = true;
        binding.config.max_completion_tokens = Some(4096);
        binding.config.max_retries = Some(0);
        binding.credentials.api_key = Some("nonsecret-k2-key".into());
        binding.credentials.auth_type = xai_chat_state::AuthType::ApiKey;
        actor
            .chat_state_handle
            .install_transport_binding(binding)
            .await
            .unwrap();
        let executions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (entered_tx, entered) = tokio::sync::mpsc::unbounded_channel();
        let releases = [
            Arc::new(tokio::sync::Semaphore::new(16)),
            Arc::new(tokio::sync::Semaphore::new(16)),
        ];
        for (index, name) in ["alpha__lookup", "zeta__fetch"].into_iter().enumerate() {
            actor.agent.borrow().tool_bridge().register_mcp_tools(name.into(), K2ClientTool { name, executions: executions.clone(), entered: entered_tx.clone(), release: releases[index].clone() }, Some(json!({"type":"object","description":"Exact K2 original schema","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false}))).unwrap();
        }
        {
            let mut mcp = actor.mcp_state.lock().await;
            mcp.finish_init();
            mcp.complete_init();
        }
        actor
            .workspace_ops
            .bind_local_session(
                &actor.session_id_string(),
                actor.tool_context.cwd.as_path().to_path_buf(),
                actor.tool_context.hunk_tracker_handle.clone(),
                actor.agent.borrow().tool_bridge().toolset(),
                None,
            )
            .unwrap();
        let (sampler_tx, mut sampler_rx) = tokio::sync::mpsc::unbounded_channel();
        actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
            config,
            xai_grok_sampler::RetryPolicy {
                max_retries: 0,
                ..Default::default()
            },
            sampler_tx,
        );
        let actor = Arc::new(actor);
        let drainer = actor.clone();
        let (source_tx, source_acks) = tokio::sync::mpsc::unbounded_channel();
        let sampler = tokio::task::spawn_local(async move {
            while let Some(event) = sampler_rx.recv().await {
                let status = match &event {
                    xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                        evidence: k2_history::NativeEvidenceEvent::Attempt(attempt),
                        ..
                    } => Some(attempt.status),
                    _ => None,
                };
                drainer.handle_sampling_event(event).await;
                if let Some(status) = status {
                    source_tx.send(status).unwrap();
                }
            }
        });
        Self {
            actor,
            persistence,
            directory,
            _models: models,
            tasks: vec![gateway, sampler],
            executions,
            entered,
            releases,
            source_acks,
        }
    }

    fn domain(&self, server: &MockInferenceServer) -> k2_replay::NativeReplayDomain {
        k2_replay::NativeReplayDomain {
            backend: xai_grok_sampling_types::ApiBackend::Messages,
            dialect: k2_replay::NativeReplayDialect::Messages,
            model: "k2-replay".into(),
            row: "k2-replay".into(),
            provider: "offline".into(),
            deployment: "loopback".into(),
            deployment_revision: "1".into(),
            endpoint: format!("{}/messages", server.url()),
            principal: "nonsecret-fixture".into(),
        }
    }

    async fn turn(
        &self,
        requirement: k2_history::NativeHistoryRequirement,
        server: &MockInferenceServer,
        label: &str,
    ) -> Result<TurnOutcome, acp::Error> {
        self.actor
            .chat_state_handle
            .push_user_message_and_ack(ConversationItem::user(label))
            .await;
        let selected = requirement == k2_history::NativeHistoryRequirement::Selected;
        self.actor
            .session_turn_active
            .store(selected, std::sync::atomic::Ordering::SeqCst);
        let result = super::switch_projection::HISTORY_S3_TEST_DOMAIN
            .scope(
                self.domain(server),
                super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
                    requirement,
                    self.actor.process_conversation_turn_with_recovery(
                        label,
                        None,
                        None,
                        None,
                        &mut super::length_salvage::LengthSalvage::new(None),
                        &mut Default::default(),
                    ),
                ),
            )
            .await;
        self.actor
            .session_turn_active
            .store(false, std::sync::atomic::Ordering::SeqCst);
        result
    }

    async fn reload(&self) -> (K2Checkpoint, Vec<ConversationItem>) {
        use crate::session::storage::StorageAdapter;
        let (respond_to, flushed) = tokio::sync::oneshot::channel();
        self.persistence
            .tx
            .send(PersistenceMsg::FlushAndAck { respond_to })
            .unwrap();
        flushed.await.unwrap().unwrap();
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
            self.directory.path().to_path_buf(),
        );
        let full = adapter
            .load_session(&self.actor.session_info)
            .await
            .unwrap();
        let light = adapter
            .load_session_without_updates(&self.actor.session_info)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&full.chat_history).unwrap(),
            serde_json::to_value(&light.chat_history).unwrap(),
            "K2 full/light disk readers agree"
        );
        let disk = adapter
            .read_native_history(&self.actor.session_info)
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        let live = self
            .actor
            .chat_state_handle
            .read_native_history()
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        assert_eq!(
            disk.source, live.source,
            "K2 actor source is the reloaded source"
        );
        assert_eq!(
            disk.ledger, live.ledger,
            "K2 actor ledger is the reloaded ledger"
        );
        assert_eq!(
            disk.bytes.as_ref(),
            std::fs::read(self.directory.path().join("native_history/current.json")).unwrap()
        );
        disk.matches_history(&full.chat_history).unwrap();
        ((*disk).clone(), full.chat_history)
    }
}

fn k2_script(frames: Vec<Value>) -> ScriptedResponse {
    ScriptedResponse::sse(
        frames
            .into_iter()
            .map(|frame| SseEvent::data(frame.to_string()))
            .collect(),
    )
}

fn k2_frames(id: &str, blocks: Vec<(Value, Vec<Value>)>, stop: &str) -> Vec<Value> {
    let mut frames = vec![
        json!({"type":"message_start","message":{"id":id,"type":"message","role":"assistant","model":"k2-replay","content":[],"stop_reason":null,"usage":{"input_tokens":17,"output_tokens":0}}}),
    ];
    for (index, (block, deltas)) in blocks.into_iter().enumerate() {
        frames.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        for delta in deltas {
            frames.push(json!({"type":"content_block_delta","index":index,"delta":delta}));
        }
        frames.push(json!({"type":"content_block_stop","index":index}));
    }
    frames.extend([json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"input_tokens":17,"output_tokens":7}}),json!({"type":"message_stop"})]);
    frames
}

fn k2_call(id: &str, caller: Option<Value>) -> (Value, Vec<Value>) {
    let mut block =
        json!({"type":"server_tool_use","id":id,"name":"tool_search_tool_regex","input":{}});
    if let Some(caller) = caller {
        block["caller"] = caller;
    }
    (
        block,
        vec![json!({"type":"input_json_delta","partial_json":"{\"query\":\"exact K2 search\"}"})],
    )
}

fn k2_result(id: &str, name: &str) -> (Value, Vec<Value>) {
    (
        json!({"type":"tool_search_tool_result","tool_use_id":id,"content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":name}]}}),
        Vec::new(),
    )
}

async fn k2_seed(session: &mut K2ReplaySession, server: &MockInferenceServer) {
    server.enqueue_response(
        "/v1/messages",
        k2_script(k2_frames(
            "msg_k2_seed",
            vec![
                k2_call("srvtoolu_k2_seed", None),
                k2_result("srvtoolu_k2_seed", "alpha__lookup"),
                (json!({"type":"text","text":"K2 seed complete"}), Vec::new()),
            ],
            "end_turn",
        )),
    );
    session
        .turn(
            k2_history::NativeHistoryRequirement::Inactive,
            server,
            "seed exactly one completed native group",
        )
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv())
            .await
            .unwrap(),
        Some(k2_history::NativeResponseStatus::Accepted)
    );
    assert!(session.source_acks.try_recv().is_err());
    let (checkpoint, _) = session.reload().await;
    assert_eq!(
        checkpoint
            .ledger
            .groups
            .iter()
            .map(|group| group.state.as_str())
            .collect::<Vec<_>>(),
        vec!["Completed"],
        "K2 Selected scenarios start after the H-S3F-R6 Inactive seed"
    );
    assert_eq!(server.messages_request_count(), 1);
}

fn k2_citation(text: &str, start: u64) -> Value {
    json!({"type":"char_location","cited_text":text,"document_index":0,"document_title":null,"file_id":null,"start_char_index":start,"end_char_index":start+1})
}

fn k2_ordered_frames() -> Vec<Value> {
    k2_frames(
        "msg_k2_ordered",
        vec![
            (
                json!({"type":"text","text":"missing citations"}),
                Vec::new(),
            ),
            (
                json!({"type":"text","text":"null citations","citations":null}),
                Vec::new(),
            ),
            (
                json!({"type":"text","text":"ordered citations","citations":[]}),
                vec![
                    json!({"type":"citations_delta","citation":k2_citation("first",0)}),
                    json!({"type":"citations_delta","citation":k2_citation("second",1)}),
                ],
            ),
            k2_call("srvtoolu_k2_s1", None),
            (
                json!({"type":"thinking","thinking":""}),
                vec![
                    json!({"type":"thinking_delta","thinking":"ordered reasoning"}),
                    json!({"type":"signature_delta","signature":"opaque-k2-signature"}),
                ],
            ),
            k2_call(
                "srvtoolu_k2_s2",
                Some(json!({"type":"direct","retained":"K2 caller"})),
            ),
            k2_result("srvtoolu_k2_s2", "zeta__fetch"),
            (json!({"type":"text","text":"between results"}), Vec::new()),
            k2_result("srvtoolu_k2_s1", "alpha__lookup"),
            (json!({"type":"text","text":"ordered suffix"}), Vec::new()),
        ],
        "end_turn",
    )
}

#[derive(Clone, Copy)]
enum K2Receipt {
    Matching,
    None,
    WrongShape,
    Foreign,
}

fn k2_core(
    checkpoint: &K2Checkpoint,
    request: &ConversationRequest,
    domain: &k2_replay::NativeReplayDomain,
    receipt: K2Receipt,
) -> Result<k2_history::NativeHistoryCore, k2_history::NativeHistoryRefusal> {
    use k2_history::*;
    use k2_replay::*;
    let selected: Vec<_> = checkpoint
        .source
        .ordered_blocks
        .iter()
        .map(|block| block.coordinates.clone())
        .collect();
    let authorship = checkpoint
        .source
        .ordered_blocks
        .iter()
        .map(|block| (block.coordinates.occurrence, NativeIdAuthorship::Provider))
        .collect();
    let target = TargetRoute::new(
        xai_grok_sampling_types::ApiBackend::Messages,
        Boundary::Vertex,
        request.search_admission.unwrap(),
    );
    let origins = checkpoint
        .source
        .producing_bindings
        .iter()
        .map(|binding| (binding.origin_ref.digest.clone(), domain.clone()))
        .collect();
    prepare_native_history_core(
        &NativePureHistory {
            record: &checkpoint.record,
            source: &checkpoint.source,
            ledger: &checkpoint.ledger,
            identity: &checkpoint.identity,
            blobs: &checkpoint.blobs,
            authorship: &authorship,
        },
        &NativeProjectionInputs {
            target_model_id: "k2-replay",
            target: &target,
            tools: &request.tools,
            current: request.native_eligible_inputs.as_deref(),
            selected: &selected,
            origin_domains: &origins,
            pending: &Default::default(),
            prior_f: None,
        },
        |group| {
            let result = group
                .members
                .iter()
                .filter_map(|coordinate| {
                    checkpoint
                        .source
                        .ordered_blocks
                        .iter()
                        .find(|block| &block.coordinates == coordinate)
                })
                .map(|block| {
                    serde_json::from_slice::<Value>(&checkpoint.blobs[&block.block.digest]).unwrap()
                })
                .find(|raw| raw["type"] == "tool_search_tool_result")
                .unwrap();
            let shape = NativeReplayShape {
                family: NativeReplayFamily::MessagesServer,
                keyless_server: false,
                output_id_present: false,
                completed: true,
                result_error: false,
                referenced_members: native_candidate_references(&result).unwrap().len() as u64,
            };
            let mut receipt_shape = shape.clone();
            let mut receipt_target = domain.clone();
            if matches!(receipt, K2Receipt::WrongShape) {
                receipt_shape.referenced_members += 1;
            }
            if matches!(receipt, K2Receipt::Foreign) {
                receipt_target.row = "foreign-K2-row".into();
            }
            (
                DiscoveryOrigin {
                    boundary: Boundary::Vertex,
                    owner_model_id: Some("k2-replay".into()),
                },
                NativeQualification {
                    source: Some(domain.clone()),
                    target: Some(domain.clone()),
                    request: Some(domain.clone()),
                    original_admission: request.search_admission.unwrap(),
                    shape,
                    receipt: (!matches!(receipt, K2Receipt::None)).then_some(NativeShapeReceipt {
                        target: receipt_target,
                        shape: receipt_shape,
                        receipt_id: "K2-synthetic-offline-receipt".into(),
                    }),
                    intact_membership: true,
                    current_backing: true,
                    faithful_identity: true,
                },
            )
        },
    )
}

async fn k2_shared_request(
    session: &K2ReplaySession,
    server: &MockInferenceServer,
    checkpoint: &K2Checkpoint,
    history: Vec<ConversationItem>,
) -> (ConversationRequest, k2_history::NativeHistoryRequestContext) {
    use k2_history::*;
    session
        .actor
        .session_turn_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let owner = session
        .actor
        .chat_state_handle
        .register_native_history_owner(
            session.actor.session_id_string(),
            "K2-shared-boundary".into(),
            session.domain(server),
            NativeProjectionOwner::OwningPrimary,
            session.actor.session_turn_active.clone(),
        )
        .await
        .unwrap();
    let (prepared, _) = session.actor.prepare_native_tool_candidates_timed().await;
    let bundle = session.actor.prepared_tool_bundle(&prepared);
    let projection = super::sampler_turn::BridgeDeclarationProjection::prepared(
        bundle,
        prepared
            .mcp_specs
            .iter()
            .map(|spec| spec.name.clone())
            .collect(),
    );
    let candidates = projection
        .candidates()
        .with_history_entry(xai_chat_state::NativeHistoryEntry::captured(
            NativeHistoryRequirement::Selected,
            xai_chat_state::NativeHistoryEntryIntent::OwningPrimary(owner.token()),
        ))
        .unwrap();
    let (mut request, _) = session
        .actor
        .chat_state_handle
        .build_request_with_binding(
            candidates,
            None,
            false,
            None,
            session.actor.session_id_string(),
            "K2-shared-boundary".into(),
        )
        .await
        .unwrap();
    let capture = request
        .native_bound_preparation
        .as_ref()
        .unwrap()
        .context()
        .unwrap()
        .capture()
        .clone();
    request.items = history;
    checkpoint.matches_history(&request.items).unwrap();
    let context = NativeHistoryRequestContext::captured(
        NativeHistoryRequirement::Selected,
        capture,
        NativeHistoryWriteAuthority::SendOnly,
    );
    drop(owner);
    session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    (request, context)
}

fn k2_projected_request(
    checkpoint: &K2Checkpoint,
    mut request: ConversationRequest,
    context: &k2_history::NativeHistoryRequestContext,
    core: &k2_history::NativeHistoryCore,
) -> ConversationRequest {
    let mut replacements = std::collections::BTreeMap::new();
    for member in &core.decision.members {
        let link = checkpoint
            .ledger
            .accounting
            .iter()
            .find(|link| link.source == member.source)
            .unwrap();
        if let Some(index) = link.view_item {
            let item = member.raw.as_ref().map(|raw| ConversationItem::Discovery { item: xai_grok_sampling_types::conversation::tool_search::ToolSearchItem::from_wire(raw.clone()).unwrap() })
                .or_else(|| member.neutral.as_ref().map(|neutral| ConversationItem::assistant(neutral.text.clone())));
            replacements.insert(index as usize, item);
        }
    }
    request.items = request
        .items
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| replacements.get(&index).cloned().unwrap_or(Some(item)))
        .collect();
    request.native_history = Some(
        xai_grok_sampling_types::request_validation::NativeHistoryRequest::new(
            context.clone(),
            None,
        ),
    );
    request
}

fn k2_shared_plan(
    checkpoint: &K2Checkpoint,
    request: &ConversationRequest,
    context: k2_history::NativeHistoryRequestContext,
    core: k2_history::NativeHistoryCore,
    carrier: k2_history::NativeFinalCarrier,
) -> Result<k2_history::NativeHistoryPlan, k2_history::NativePlanRefusal> {
    use k2_history::*;
    use xai_chat_state::persistence::native::digest;
    let binding: Arc<[u8]> = serde_json::to_vec(&json!({"request":context.capture().issued_request,"generation":context.capture().binding_generation,"origin":{"status":"Captured"}})).unwrap().into();
    let binding_ref = NativeBlobRef {
        kind: "producing_binding".into(),
        byte_len: binding.len() as u64,
        digest: digest(&binding),
    };
    let declarations: Vec<Value> = carrier
        .declarations()
        .map(|bytes| serde_json::from_slice(bytes).unwrap())
        .unwrap_or_default();
    let identities = declarations
        .iter()
        .enumerate()
        .filter_map(|(index, declaration)| {
            let schema = declaration.get("input_schema")?;
            let name = declaration["name"].as_str().unwrap();
            let entry = request.native_eligible_inputs.as_ref().and_then(|inputs| {
                inputs
                    .entries()
                    .iter()
                    .find(|entry| entry.declaration().name == name)
            });
            let (kind, namespace, child) = if let Some(entry) = entry {
                (
                    entry.namespace_kind(),
                    entry.qualified_namespace().to_owned(),
                    entry.child_name().to_owned(),
                )
            } else {
                assert_eq!(name, "DeferredToolPlaceholder");
                (
                    NativeNamespaceKind::GrokBuild,
                    "grok_build".into(),
                    name.into(),
                )
            };
            let schema_bytes: Arc<[u8]> = serde_json::to_vec(schema).unwrap().into();
            let schema_ref = NativeBlobRef {
                kind: "eligible_schema".into(),
                byte_len: schema_bytes.len() as u64,
                digest: digest(&schema_bytes),
            };
            let snapshot = NativeEligibleSnapshot {
                version: 1,
                namespace_kind: kind,
                qualified_namespace: namespace.clone(),
                child_name: child.clone(),
                schema: schema_ref.clone(),
                binding: binding_ref.clone(),
            };
            let provenance_bytes: Arc<[u8]> = serde_json::to_vec(&snapshot).unwrap().into();
            Some(NativeDeclarationIdentity {
                declaration_index: index as u64,
                child_index: None,
                namespace_kind: kind,
                qualified_namespace: namespace,
                child_name: child,
                declaration: declaration.clone(),
                schema: schema_ref,
                schema_bytes,
                provenance: NativeBlobRef {
                    kind: "eligible_snapshot".into(),
                    byte_len: provenance_bytes.len() as u64,
                    digest: digest(&provenance_bytes),
                },
                provenance_bytes,
                binding_bytes: Some(binding.clone()),
            })
        })
        .collect();
    let finalization =
        NativeFinalizationEvidence::checked_native_messages(request, &core, carrier.clone())?;
    NativeHistoryPlan::checked_finalized(
        context,
        NativeHistoryPlanInput {
            record: checkpoint.record.clone(),
            input_history_bytes: serde_json::to_vec(&request.items).unwrap().into(),
            ledger_bytes: checkpoint.blobs[&checkpoint.record.ledger.digest].clone(),
            lowering_request: request,
            post_history_bytes: serde_json::to_vec(&request.items).unwrap().into(),
            carrier,
            identities,
            qualification: core.accounting.qualification.clone(),
            core,
        },
        finalization,
    )?
    .send_only()
}

fn k2_source_blocks(checkpoint: &K2Checkpoint, response_id: &str) -> Vec<Value> {
    let response = checkpoint
        .source
        .responses
        .iter()
        .find(|response| {
            response
                .provider_response_id
                .as_ref()
                .and_then(Value::as_str)
                == Some(response_id)
        })
        .unwrap();
    checkpoint.source.ordered_blocks[response.block_start as usize..response.block_end as usize]
        .iter()
        .map(|block| serde_json::from_slice(&checkpoint.blobs[&block.block.digest]).unwrap())
        .collect()
}

fn k2_body_blocks(body: &Value) -> Vec<Value> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .cloned()
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn messages_ordered_replay_survives_checkpoint() {
    use k2_history::*;
    tokio::task::LocalSet::new().run_until(async {
        let server = MockInferenceServer::start().await.unwrap();
        let mut session = K2ReplaySession::new(&server).await;
        k2_seed(&mut session, &server).await;
        server.enqueue_response("/v1/messages", k2_script(k2_ordered_frames()));
        session.turn(NativeHistoryRequirement::Selected, &server, "K2 ordered real decode and checkpoint").await.unwrap();
        assert_eq!(tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv()).await.unwrap(), Some(NativeResponseStatus::Accepted));
        assert!(session.source_acks.try_recv().is_err());
        assert_eq!(server.messages_request_count(), 2);
        let (checkpoint, history) = session.reload().await;
        let mut s1 = k2_call("srvtoolu_k2_s1", None).0;
        s1["input"] = json!({"query":"exact K2 search"});
        let mut s2 = k2_call("srvtoolu_k2_s2", Some(json!({"type":"direct","retained":"K2 caller"}))).0;
        s2["input"] = json!({"query":"exact K2 search"});
        let expected = vec![
            json!({"type":"text","text":"missing citations"}),
            json!({"type":"text","text":"null citations","citations":null}),
            json!({"type":"text","text":"ordered citations","citations":[k2_citation("first",0),k2_citation("second",1)]}),
            s1.clone(), json!({"type":"thinking","thinking":"ordered reasoning","signature":"opaque-k2-signature"}),
            s2.clone(), k2_result("srvtoolu_k2_s2", "zeta__fetch").0,
            json!({"type":"text","text":"between results"}), k2_result("srvtoolu_k2_s1", "alpha__lookup").0,
            json!({"type":"text","text":"ordered suffix"}),
        ];
        assert_eq!(k2_source_blocks(&checkpoint, "msg_k2_ordered"), expected, "K2 real delta assembly and ordered raw fields survive actor/disk/reload unchanged");
        assert_eq!(checkpoint.source.responses.len(), 2);
        assert_eq!(checkpoint.ledger.usage.len(), 2, "K2 retains usage per response occurrence, never sums it");
        assert!(matches!(checkpoint.messages_metadata, NativeMessagesMetadataState::Revision3 { .. }));
        let (request, context) = k2_shared_request(&session, &server, &checkpoint, history).await;
        let core = k2_core(&checkpoint, &request, &session.domain(&server), K2Receipt::Matching).unwrap();
        assert_eq!(core.decision.groups.iter().filter(|group| group.action == k2_replay::NativeHistoryAction::Keep).count(), 2);
        let request = k2_projected_request(&checkpoint, request, &context, &core);
        let lowered = xai_grok_sampling_types::conversation::build_messages_request(&request);
        let encoded = xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(&lowered).unwrap();
        let body: Value = serde_json::from_slice(encoded.as_bytes()).unwrap();
        let blocks = k2_body_blocks(&body);
        let native: Vec<_> = blocks.iter().filter(|block| matches!(block["type"].as_str(), Some("server_tool_use" | "tool_search_tool_result"))).cloned().collect();
        assert_eq!(native, vec![s1, s2, k2_result("srvtoolu_k2_s2", "zeta__fetch").0, k2_result("srvtoolu_k2_s1", "alpha__lookup").0], "K2 shared-boundary native emission preserves order and caller once");
        let carrier = NativeFinalCarrier::capture(encoded.as_bytes().to_vec().into()).unwrap();
        let plan = k2_shared_plan(&checkpoint, &request, context.clone(), core.clone(), carrier.clone()).unwrap();
        assert_eq!(xai_grok_sampling_types::request_validation::validate_native_messages_send(&context, Some(&plan), &carrier, Some(&carrier)), Ok(NativeHistoryReady::SendOnly));
        let citation_blocks: Vec<_> = blocks.iter().filter(|block| block["text"].as_str().is_some_and(|text| text.contains("citations"))).cloned().collect();
        let ordered_message = body["messages"].as_array().unwrap().iter().find(|message| {
            message["content"].as_array().unwrap().iter().any(|block| block["type"] == "server_tool_use" && block["id"] == "srvtoolu_k2_s1")
        }).unwrap();
        let ordered_content = ordered_message["content"].as_array().unwrap();
        let ordinary_blocks: Vec<_> = ordered_content.iter().filter(|block| !matches!(block["type"].as_str(), Some("server_tool_use" | "tool_search_tool_result" | "thinking"))).cloned().collect();
        let ordinary_text = expected.iter().filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join("\n");
        let block_types: Vec<_> = ordered_content.iter().map(|block| block["type"].as_str().unwrap()).collect();
        println!("{}", json!({"K2_ordered_checkpoint":true,"source_response_count":checkpoint.source.responses.len(),"raw_text_blocks":&expected[..3],"lowered_citation_blocks":citation_blocks,"block_types":block_types,"ordinary_replay":"PARENT_PINNED_STAGED_NOT_CREDITED","ready":"SendOnly","owning_primary_KEEP":"STAGED_NOT_CREDITED"}));
        assert_eq!(ordinary_blocks, vec![json!({"type":"text","text":ordinary_text})], "PARENT BEHAVIOUR, not K2 fidelity: one flattened text block, citations missing; ordinary ordered-content replay fidelity is STAGED (K2-U4 erratum section 4)");
        let text_position = ordered_content.iter().position(|block| block["type"] == "text").unwrap();
        let last_native_position = ordered_content.iter().rposition(|block| matches!(block["type"].as_str(), Some("server_tool_use" | "tool_search_tool_result"))).unwrap();
        assert!(text_position > last_native_position, "PARENT BEHAVIOUR, not K2 fidelity: flattened text follows the native blocks; ordinary ordered-content replay fidelity is STAGED");
        for receipt in [K2Receipt::None, K2Receipt::WrongShape, K2Receipt::Foreign] {
            let twin = k2_core(&checkpoint, &request, &session.domain(&server), receipt).unwrap();
            assert!(twin.decision.groups.iter().all(|group| group.action != k2_replay::NativeHistoryAction::Keep), "K2 missing/mismatched receipt twin demotes for qualification, not checkpoint refusal");
        }
        server.enqueue_response("/v1/messages", k2_script(k2_frames("msg_k2_no_input_twin", vec![(json!({"type":"text","text":"no-input twin complete"}), Vec::new())], "end_turn")));
        session.turn(NativeHistoryRequirement::Selected, &server, "K2 no-input owning-primary twin").await.unwrap();
        assert_eq!(tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv()).await.unwrap(), Some(NativeResponseStatus::Accepted));
        let (after, _) = session.reload().await;
        for group in &checkpoint.ledger.groups {
            assert_eq!(after.active.dispositions.get(&group.key).map(String::as_str), Some("F"), "K2 parent no-input owning-primary demotes completed groups");
        }
        assert!(session.executions.lock().unwrap().is_empty());

        for fault in ["completed-signature-missing", "unknown-start", "wrong-index", "singular-citation"] {
            let server = MockInferenceServer::start().await.unwrap();
            let mut session = K2ReplaySession::new(&server).await;
            k2_seed(&mut session, &server).await;
            let (seed_checkpoint, _) = session.reload().await;
            let prior = seed_checkpoint.source.responses;
            let mut frames = k2_ordered_frames();
            match fault {
                "completed-signature-missing" => frames.retain(|frame| frame.pointer("/delta/type").and_then(Value::as_str) != Some("signature_delta")),
                "unknown-start" => frames[1]["content_block"]["future_default"] = json!(""),
                "wrong-index" => {
                    let frame = frames.iter_mut().find(|frame| frame.pointer("/delta/type").and_then(Value::as_str) == Some("citations_delta")).unwrap();
                    frame["index"] = json!(99);
                }
                "singular-citation" => {
                    let frame = frames.iter_mut().find(|frame| frame.pointer("/delta/type").and_then(Value::as_str) == Some("citations_delta")).unwrap();
                    frame["delta"]["type"] = json!("citation_delta");
                }
                _ => unreachable!(),
            }
            server.enqueue_response("/v1/messages", k2_script(frames));
            let outcome = session.turn(NativeHistoryRequirement::Selected, &server, fault).await;
            let status = tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv()).await.unwrap().unwrap();
            assert!(outcome.is_err() || status != NativeResponseStatus::Accepted, "K2 completed-only mismatch and malformed events cannot become faithful KEEP: {fault}");
            let (checkpoint, _) = session.reload().await;
            assert!(checkpoint.source.responses.len() >= prior.len(), "K2 pre-turn native responses cannot be removed: {fault}");
            assert_eq!(&checkpoint.source.responses[..prior.len()], prior.as_slice(), "K2 pre-turn native responses remain unchanged after malformed attempt: {fault}");
            let appended = &checkpoint.source.responses[prior.len()..];
            let branch = if appended.is_empty() { "ABSENT" } else { "PRESENT" };
            let statuses: Vec<_> = appended.iter().map(|response| response.status).collect();
            println!("{}", json!({"K2_malformed_checkpoint":true,"fault":fault,"branch":branch,"statuses":statuses,"prior_responses":prior.len(),"pre_turn_responses_unchanged":true}));
            assert!(appended.iter().all(|response| response.status != NativeResponseStatus::Accepted), "K2 malformed completed capture remains incomplete after disk reload: {fault}");
        }
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn messages_split_server_pair_joins_across_responses() {
    use k2_history::*;
    tokio::task::LocalSet::new().run_until(async {
        let server = MockInferenceServer::start().await.unwrap();
        let mut session = K2ReplaySession::new(&server).await;
        k2_seed(&mut session, &server).await;
        let client = json!({"type":"tool_use","id":"client_k2_split","name":"alpha__lookup","input":{"value":"split actual"}});
        server.enqueue_response("/v1/messages", k2_script(k2_frames("msg_k2_split_1", vec![
            k2_call("srvtoolu_k2_split_s2", Some(json!({"type":"direct","retained":"split caller"}))),
            k2_call("srvtoolu_k2_split_s1", None), (client.clone(), Vec::new()),
        ], "tool_use")));
        server.enqueue_response("/v1/messages", k2_script(k2_frames("msg_k2_split_2", vec![
            k2_result("srvtoolu_k2_split_s1", "alpha__lookup"),
            k2_result("srvtoolu_k2_split_s2", "zeta__fetch"),
            (json!({"type":"text","text":"split suffix"}), Vec::new()),
        ], "end_turn")));
        session.turn(NativeHistoryRequirement::Selected, &server, "K2 two live split calls with an actual client result").await.unwrap();
        for _ in 0..2 {
            assert_eq!(tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv()).await.unwrap(), Some(NativeResponseStatus::Accepted));
        }
        assert!(session.source_acks.try_recv().is_err());
        assert_eq!(server.messages_request_count(), 3, "K2 split has one seed and exactly two real Selected requests");
        assert_eq!(*session.executions.lock().unwrap(), vec!["alpha__lookup"]);
        let (checkpoint, history) = session.reload().await;
        let mut s2 = k2_call("srvtoolu_k2_split_s2", Some(json!({"type":"direct","retained":"split caller"}))).0;
        s2["input"] = json!({"query":"exact K2 search"});
        let mut s1 = k2_call("srvtoolu_k2_split_s1", None).0;
        s1["input"] = json!({"query":"exact K2 search"});
        let r1 = k2_result("srvtoolu_k2_split_s1", "alpha__lookup").0;
        let r2 = k2_result("srvtoolu_k2_split_s2", "zeta__fetch").0;
        assert_eq!(k2_source_blocks(&checkpoint, "msg_k2_split_1"), vec![s2.clone(), s1.clone(), client]);
        assert_eq!(k2_source_blocks(&checkpoint, "msg_k2_split_2"), vec![r1.clone(), r2.clone(), json!({"type":"text","text":"split suffix"})]);
        assert_eq!(checkpoint.source.responses.len(), 3);
        assert_eq!(checkpoint.ledger.usage.len(), 3, "K2 split retains per-response usage occurrence linkage");
        assert_eq!(checkpoint.source.responses.iter().map(|response| response.source_stop_reason.as_deref()).collect::<Vec<_>>(), vec![Some("end_turn"), Some("tool_use"), Some("end_turn")]);
        assert!(matches!(checkpoint.messages_metadata, NativeMessagesMetadataState::Revision3 { .. }));
        assert_eq!(checkpoint.ledger.groups.len(), 3, "K2 split has the seed and two joined groups, no result-only group");
        let mut joined_keys = Vec::new();
        for id in ["srvtoolu_k2_split_s1", "srvtoolu_k2_split_s2"] {
            let groups: Vec<_> = checkpoint.ledger.groups.iter().filter(|group| {
                group.members.iter().any(|member| {
                    let raw: Value = serde_json::from_slice(&checkpoint.blobs[&checkpoint.source.ordered_blocks[member.occurrence as usize].block.digest]).unwrap();
                    raw["type"] == "server_tool_use" && raw["id"] == id
                })
            }).collect();
            assert_eq!(groups.len(), 1, "K2 split joins each real server id into one group");
            let group = groups[0];
            assert_eq!(group.state, "Completed");
            assert_eq!(checkpoint.ledger.lifecycle.groups.get(&group.key).map(String::as_str), Some("Completed"));
            assert_eq!(group.members.len(), 2);
            assert_ne!(group.members[0].response, group.members[1].response, "K2 split joined members span both responses");
            assert_eq!(group.validated_join.as_deref(), Some(id));
            assert!(!checkpoint.active.dispositions.contains_key(&group.key), "K2 both live groups survive the S3F continuation without premature F");
            joined_keys.push(group.key.clone());
        }
        let requests: Vec<_> = server.requests().into_iter().filter(|request| request.path == "/v1/messages").collect();
        let continuation = requests[2].body.as_ref().unwrap();
        let actual_result = json!({"type":"tool_result","tool_use_id":"client_k2_split","content":"alpha__lookup actual result: \"split actual\"","cache_control":{"type":"ephemeral"}});
        assert_eq!(continuation["messages"].as_array().unwrap().last().unwrap(), &json!({"role":"user","content":[actual_result.clone()]}), "K2 split continuation is the actual results-only client message");
        let (request, context) = k2_shared_request(&session, &server, &checkpoint, history).await;
        let core = k2_core(&checkpoint, &request, &session.domain(&server), K2Receipt::Matching).unwrap();
        let kept: Vec<_> = core.decision.groups.iter().filter(|group| group.action == k2_replay::NativeHistoryAction::Keep).collect();
        assert_eq!(kept.len(), 2, "K2 matching split core keeps both joined groups");
        let request = k2_projected_request(&checkpoint, request, &context, &core);
        let lowered = xai_grok_sampling_types::conversation::build_messages_request(&request);
        let encoded = xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(&lowered).unwrap();
        let body: Value = serde_json::from_slice(encoded.as_bytes()).unwrap();
        let blocks = k2_body_blocks(&body);
        let native: Vec<_> = blocks.iter().filter(|block| matches!(block["type"].as_str(), Some("server_tool_use" | "tool_search_tool_result"))).cloned().collect();
        assert_eq!(native, vec![s2, s1, r1, r2], "K2 split item-3 native equality preserves both id joins, raw fields and native-relative order once");
        let replay_results: Vec<_> = blocks.iter().filter(|block| block["type"] == "tool_result" && block["tool_use_id"] == "client_k2_split").cloned().collect();
        assert_eq!(replay_results, vec![actual_result], "K2 split leaves the intervening actual client result unmoved and unchanged");
        let call_message = body["messages"].as_array().unwrap().iter().position(|message| message["content"].as_array().unwrap().iter().any(|block| block["id"] == "srvtoolu_k2_split_s1")).unwrap();
        let client_message = body["messages"].as_array().unwrap().iter().position(|message| message["content"].as_array().unwrap().iter().any(|block| block["type"] == "tool_result" && block["tool_use_id"] == "client_k2_split")).unwrap();
        let result_message = body["messages"].as_array().unwrap().iter().position(|message| message["content"].as_array().unwrap().iter().any(|block| block["type"] == "tool_search_tool_result" && block["tool_use_id"] == "srvtoolu_k2_split_s1")).unwrap();
        assert!(call_message < client_message && client_message < result_message, "K2 split preserves the intervening user result between source responses");
        let result_content = body["messages"][result_message]["content"].as_array().unwrap();
        let ordinary: Vec<_> = result_content.iter().filter(|block| !matches!(block["type"].as_str(), Some("server_tool_use" | "tool_search_tool_result" | "thinking"))).cloned().collect();
        assert_eq!(ordinary, vec![json!({"type":"text","text":"split suffix"})], "PARENT BEHAVIOUR, not K2 fidelity: split ordinary replay follows the existing Assistant arm");
        let text_position = result_content.iter().position(|block| block["type"] == "text").unwrap();
        let last_native = result_content.iter().rposition(|block| block["type"] == "tool_search_tool_result").unwrap();
        assert!(text_position > last_native, "PARENT BEHAVIOUR, not K2 fidelity: split flattened text follows native blocks");
        let carrier = NativeFinalCarrier::capture(encoded.as_bytes().to_vec().into()).unwrap();
        let plan = k2_shared_plan(&checkpoint, &request, context.clone(), core.clone(), carrier.clone()).unwrap();
        assert_eq!(xai_grok_sampling_types::request_validation::validate_native_messages_send(&context, Some(&plan), &carrier, Some(&carrier)), Ok(NativeHistoryReady::SendOnly));
        for receipt in [K2Receipt::None, K2Receipt::WrongShape, K2Receipt::Foreign] {
            let twin = k2_core(&checkpoint, &request, &session.domain(&server), receipt).unwrap();
            assert!(twin.decision.groups.iter().all(|group| group.action != k2_replay::NativeHistoryAction::Keep), "K2 split receipt twin demotes for qualification on the same valid joined checkpoint");
        }
        println!("{}", json!({"K2_split_checkpoint":true,"joined_keys":joined_keys,"responses":checkpoint.source.responses,"groups":checkpoint.ledger.groups,"lifecycle":checkpoint.ledger.lifecycle,"native_replay":native,"ready":"SendOnly","ordinary_replay":"PARENT_PINNED_STAGED_NOT_CREDITED","owning_primary_KEEP":"STAGED_NOT_CREDITED"}));
        server.enqueue_response("/v1/messages", k2_script(k2_frames("msg_k2_split_no_input", vec![(json!({"type":"text","text":"split no-input twin"}), Vec::new())], "end_turn")));
        session.turn(NativeHistoryRequirement::Selected, &server, "K2 split no-input owning-primary twin").await.unwrap();
        assert_eq!(tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv()).await.unwrap(), Some(NativeResponseStatus::Accepted));
        let (after, _) = session.reload().await;
        for group in &checkpoint.ledger.groups {
            assert_eq!(after.active.dispositions.get(&group.key).map(String::as_str), Some("F"), "K2 parent split no-input owning-primary demotes completed groups");
        }
        assert_eq!(*session.executions.lock().unwrap(), vec!["alpha__lookup"]);

        let invalid_server = MockInferenceServer::start().await.unwrap();
        let mut invalid = K2ReplaySession::new(&invalid_server).await;
        k2_seed(&mut invalid, &invalid_server).await;
        invalid_server.enqueue_response("/v1/messages", k2_script(k2_frames("msg_k2_result_before_call", vec![
            k2_result("srvtoolu_k2_result_before_call", "alpha__lookup"),
            k2_call("srvtoolu_k2_result_before_call", None),
        ], "end_turn")));
        let invalid_outcome = invalid.turn(NativeHistoryRequirement::Selected, &invalid_server, "K2 result before its own server call").await;
        let invalid_status = tokio::time::timeout(K0_TIMEOUT, invalid.source_acks.recv()).await.unwrap().unwrap();
        let (invalid_checkpoint, _) = invalid.reload().await;
        let request_count = invalid_server.messages_request_count();
        if invalid_outcome.is_ok() && invalid_status == NativeResponseStatus::Accepted {
            let next = invalid.turn(NativeHistoryRequirement::Selected, &invalid_server, "K2 refuses invalid retained result-before-call").await;
            assert!(next.is_err(), "K2 result-before-own-call is invalid at the real subsequent core");
            assert_eq!(invalid_server.messages_request_count(), request_count, "K2 invalid result-before-own-call cannot reach continuation HTTP");
        }
        println!("{}", json!({"K2_result_before_call":true,"turn_refused":invalid_outcome.is_err(),"capture_status":invalid_status,"persisted_responses":invalid_checkpoint.source.responses.len(),"subsequent_HTTP":invalid_server.messages_request_count()-request_count}));
    }).await;
}

#[derive(Default)]
struct K2U5Trace {
    check_waiting: bool,
    check_tools: bool,
    builds: Vec<(ConversationRequest, xai_chat_state::RequestTransportBinding)>,
    submitted: Vec<(
        ConversationRequest,
        xai_chat_state::RequestTransportBinding,
        k2_history::NativeFinalCarrier,
    )>,
}

tokio::task_local! {
    static K2_U5_TRACE: std::rc::Rc<std::cell::RefCell<K2U5Trace>>;
}

pub(super) fn k2_u5_observe_bound(
    request: &ConversationRequest,
    binding: &xai_chat_state::RequestTransportBinding,
) {
    let _ = K2_U5_TRACE.try_with(|trace| {
        let mut trace = trace.borrow_mut();
        assert!(trace.builds.len() < 2, "K2 mixed handoff has exactly two bound builds");
        if let Some((original, original_binding)) = trace.builds.first()
            && trace.check_tools
        {
            assert_eq!(serde_json::to_value(&request.tools).unwrap(), serde_json::to_value(&original.tools).unwrap(), "K2 pending retains the whole original bound tool array, including schemas, order and deferred entries");
            assert_eq!(request.model, original.model, "K2 pending never rebuilds its model from the selected row");
            assert_eq!(request.search_admission, original.search_admission, "K2 pending retains original admission");
            assert_eq!(serde_json::to_value(&binding.config).unwrap(), serde_json::to_value(&original_binding.config).unwrap(), "K2 pending retains the original transport binding");
            assert!(binding.credentials == original_binding.credentials, "K2 pending retains original credential authority without printing it");
        }
        let pending = request.items.iter().any(|item| matches!(item, ConversationItem::Discovery { item }
            if item.kind() == xai_grok_sampling_types::conversation::tool_search::ToolSearchKind::ServerToolUse
                && item.raw()["id"] == "srvtoolu_k2_pending"));
        if pending {
            if trace.check_waiting {
                for missing in ["client_k2_c1", "client_k2_c2"] {
                    let mut partial = request.clone();
                    partial.items.retain(|item| !matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == missing));
                    let lowered = xai_grok_sampling_types::conversation::build_messages_request(&partial);
                    let result = xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(&lowered);
                    assert!(matches!(result, Err(xai_grok_sampling_types::request_validation::RequestValidationError::UnansweredToolUse { ref id }) if id == missing), "K2 waiting set is checked before cleanup: a missing actual result cannot erase its obligated client id: {missing}");
                }
                let mut duplicate = request.clone();
                let result = duplicate.items.iter().find(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")).unwrap().clone();
                duplicate.items.push(result);
                assert!(xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(&xai_grok_sampling_types::conversation::build_messages_request(&duplicate)).is_err(), "K2 duplicate actual client result cannot be a continuation");
                let mut foreign = request.clone();
                let result = foreign.items.iter_mut().find_map(|item| match item { ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c2" => Some(result), _ => None }).unwrap();
                result.tool_call_id = "client_k2_foreign".into();
                assert!(xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(&xai_grok_sampling_types::conversation::build_messages_request(&foreign)).is_err(), "K2 foreign result cannot fill a waiting hole");
            }
            let body = serde_json::to_value(xai_grok_sampling_types::conversation::build_messages_request(request)).unwrap();
            let message = body["messages"].as_array().unwrap().iter().find(|message| message["content"].as_array().unwrap().iter().any(|block| block["id"] == "srvtoolu_k2_pending")).expect("K2 pending native server call survives the real bound lowerer");
            assert_eq!(message["content"], json!(k2_u5_expected_blocks()), "K2 pending native work and both real client calls replay unchanged, not cleaned or summarized");
        }
        trace.builds.push((request.clone(), binding.clone()));
    });
}

fn k2_u5_expected_blocks() -> Vec<Value> {
    vec![
        json!({"type":"server_tool_use","id":"srvtoolu_k2_pending","name":"tool_search_tool_regex","input":{"query":"exact K2 search"},"caller":{"type":"direct","retained":"pending caller"}}),
        json!({"type":"tool_use","id":"client_k2_c1","name":"alpha__lookup","input":{"value":"first actual"}}),
        json!({"type":"tool_use","id":"client_k2_c2","name":"zeta__fetch","input":{"value":"second actual"}}),
    ]
}

fn k2_u5_frames() -> Vec<Value> {
    let blocks = k2_u5_expected_blocks();
    k2_frames(
        "msg_k2_pending",
        vec![
            k2_call(
                "srvtoolu_k2_pending",
                Some(json!({"type":"direct","retained":"pending caller"})),
            ),
            (blocks[1].clone(), Vec::new()),
            (blocks[2].clone(), Vec::new()),
        ],
        "tool_use",
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum K2U5Change {
    None,
    SelectedRow,
    InstalledRow,
    RevokedDefinition,
}

struct K2U5Handoff {
    session: K2ReplaySession,
    server: MockInferenceServer,
    other: MockInferenceServer,
    checkpoint: K2Checkpoint,
    before_result: Vec<ConversationItem>,
    trace: std::rc::Rc<std::cell::RefCell<K2U5Trace>>,
    outcome: Result<TurnOutcome, acp::Error>,
}

async fn k2_u5_handoff(check_waiting: bool, check_tools: bool, change: K2U5Change) -> K2U5Handoff {
    use k2_history::*;
    let server = MockInferenceServer::start().await.unwrap();
    let other = MockInferenceServer::start().await.unwrap();
    let mut session = K2ReplaySession::new(&server).await;
    k2_seed(&mut session, &server).await;
    session.releases[1]
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap()
        .forget();
    server.enqueue_response("/v1/messages", k2_script(k2_u5_frames()));
    server.enqueue_response(
        "/v1/messages",
        k2_script(k2_frames(
            "msg_k2_pending_done",
            vec![(
                json!({"type":"text","text":"K2 continuation complete"}),
                Vec::new(),
            )],
            "end_turn",
        )),
    );
    session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("K2 real mixed pending handoff"))
        .await;
    session
        .actor
        .session_turn_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let trace = std::rc::Rc::new(std::cell::RefCell::new(K2U5Trace {
        check_waiting,
        check_tools,
        ..Default::default()
    }));
    let run_trace = trace.clone();
    let actor = session.actor.clone();
    let domain = session.domain(&server);
    let mut running = tokio::task::spawn_local(async move {
        let capture = run_trace.clone();
        let mut prepare =
            move |request: &mut ConversationRequest,
                  binding: &xai_chat_state::RequestTransportBinding| {
                let encoded = request
                    .encoded
                    .as_ref()
                    .expect("K2 Selected submit has the real finalized carrier");
                let carrier =
                    NativeFinalCarrier::capture(encoded.as_bytes().to_vec().into()).unwrap();
                let mut trace = capture.borrow_mut();
                assert!(
                    trace.submitted.len() < 2,
                    "K2 real mixed turn submits at most two requests"
                );
                trace
                    .submitted
                    .push((request.clone(), binding.clone(), carrier));
                Ok(())
            };
        super::switch_projection::HISTORY_S3_TEST_DOMAIN
            .scope(
                domain,
                super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
                    NativeHistoryRequirement::Selected,
                    K2_U5_TRACE.scope(
                        run_trace,
                        actor.process_conversation_turn_for_native_history_test(
                            "K2-live-pending",
                            None,
                            None,
                            None,
                            &mut super::length_salvage::LengthSalvage::new(None),
                            &mut Default::default(),
                            &mut prepare,
                        ),
                    ),
                ),
            )
            .await
    });
    assert_eq!(
        tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv())
            .await
            .unwrap(),
        Some(NativeResponseStatus::Accepted),
        "K2 mixed response is actually decoded and committed before client execution"
    );
    let mut entered = Vec::new();
    for _ in 0..2 {
        entered.push(
            tokio::time::timeout(K0_TIMEOUT, session.entered.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    entered.sort();
    assert_eq!(
        entered,
        vec!["alpha__lookup", "zeta__fetch"],
        "K2 executes every real waiting client exactly once"
    );
    let before_result = tokio::time::timeout(K0_TIMEOUT, async {
        loop {
            let items = session.actor.chat_state_handle.get_conversation().await;
            if items.iter().any(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")) {
                break items;
            }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert!(!before_result.iter().any(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c2")), "K2 second actual result is held at its deterministic barrier");
    assert_eq!(
        server.messages_request_count(),
        2,
        "K2 has zero continuation HTTP while one real waiting result is missing"
    );
    assert!(
        !running.is_finished(),
        "K2 cannot settle or continue before every waiting result"
    );
    let (checkpoint, _) = session.reload().await;
    assert_eq!(
        k2_source_blocks(&checkpoint, "msg_k2_pending"),
        k2_u5_expected_blocks(),
        "K2 mixed source is retained before cleanup"
    );
    let live = checkpoint
        .ledger
        .groups
        .iter()
        .find(|group| {
            group.members.iter().any(|member| {
                let raw: Value = serde_json::from_slice(
                    &checkpoint.blobs[&checkpoint.source.ordered_blocks
                        [member.occurrence as usize]
                        .block
                        .digest],
                )
                .unwrap();
                raw["id"] == "srvtoolu_k2_pending"
            })
        })
        .unwrap();
    assert_eq!(live.state, "LivePending");
    assert_eq!(
        checkpoint
            .ledger
            .lifecycle
            .groups
            .get(&live.key)
            .map(String::as_str),
        Some("LivePending")
    );
    let origin = checkpoint.source.producing_bindings.last().unwrap();
    let mut waiting: Vec<_> = checkpoint
        .ledger
        .lifecycle
        .waiting_clients
        .iter()
        .filter(|client| client.source.producing_binding == origin.request)
        .map(|client| (client.client_id.as_str(), client.delivery.as_str()))
        .collect();
    waiting.sort();
    assert_eq!(
        waiting,
        vec![("client_k2_c1", "Waiting"), ("client_k2_c2", "Waiting")],
        "K2 captures the entire persisted waiting map before item or block cleanup"
    );
    assert!(
        !native_results_continuation(
            &checkpoint.source,
            &checkpoint.ledger,
            &before_result,
            origin
        ),
        "K2 owner refuses the actual partial result snapshot"
    );
    if change != K2U5Change::None {
        let mut row = entry_with_search_flag("k2-selected-b", false);
        row.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        row.api_base_url = Some(other.url());
        row.api_key = Some("nonsecret-k2-b-key".into());
        session
            .actor
            .models_manager
            .insert_test_entry("k2-selected-b", row);
        session
            .actor
            .models_manager
            .set_current_model_id(acp::ModelId::new("k2-selected-b"));
        assert_eq!(
            session.actor.models_manager.current_model_id(),
            acp::ModelId::new("k2-selected-b")
        );
        if change == K2U5Change::InstalledRow {
            let mut binding = session
                .actor
                .chat_state_handle
                .capture_transport_binding()
                .await
                .unwrap();
            binding.config.model = "k2-selected-b".into();
            binding.config.base_url = other.url();
            binding.config.supports_search_tool = false;
            binding.credentials.api_key = Some("nonsecret-k2-b-key".into());
            session
                .actor
                .chat_state_handle
                .install_transport_binding(binding)
                .await
                .unwrap();
        }
        if change == K2U5Change::RevokedDefinition {
            assert!(
                session
                    .actor
                    .agent
                    .borrow()
                    .tool_bridge()
                    .unregister_tool_by_name("zeta__fetch")
            );
        }
    }
    session.releases[1].add_permits(1);
    let outcome = match tokio::time::timeout(K0_TIMEOUT, &mut running).await {
        Ok(result) => result.unwrap(),
        Err(error) => {
            running.abort();
            panic!("K2 released real result must finish the bounded handoff: {error}");
        }
    };
    session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        other.messages_request_count(),
        0,
        "K2 never reconstructs or submits the pending exchange on B"
    );
    K2U5Handoff {
        session,
        server,
        other,
        checkpoint,
        before_result,
        trace,
        outcome,
    }
}

fn k2_u5_assert_continuation(handoff: &K2U5Handoff) {
    assert!(
        handoff.outcome.is_ok(),
        "K2 all real waiting results permit exactly one continuation"
    );
    assert_eq!(
        handoff.server.messages_request_count(),
        3,
        "K2 final actual result permits exactly one continuation HTTP"
    );
    assert_eq!(handoff.other.messages_request_count(), 0);
    let executions = handoff.session.executions.lock().unwrap().clone();
    let mut names = executions.clone();
    names.sort();
    assert_eq!(
        names,
        vec!["alpha__lookup", "zeta__fetch"],
        "K2 client execution is once-only, never re-executed or fabricated"
    );
    let requests: Vec<_> = handoff
        .server
        .requests()
        .into_iter()
        .filter(|request| request.path == "/v1/messages")
        .collect();
    let continuation = requests[2].body.as_ref().unwrap();
    let expected = json!({"role":"user","content":[
        {"type":"tool_result","tool_use_id":"client_k2_c1","content":"alpha__lookup actual result: \"first actual\""},
        {"type":"tool_result","tool_use_id":"client_k2_c2","content":"zeta__fetch actual result: \"second actual\"","cache_control":{"type":"ephemeral"}}
    ]});
    assert_eq!(
        continuation["messages"].as_array().unwrap().last().unwrap(),
        &expected,
        "K2 exactly one nonempty results-only user message contains one actual result per waiting id and no synthetic tail"
    );
    let pending = continuation["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| {
            message["content"]
                .as_array()
                .unwrap()
                .iter()
                .any(|block| block["id"] == "srvtoolu_k2_pending")
        })
        .expect("K2 actual HTTP retains the pending native assistant work");
    assert_eq!(
        pending["content"],
        json!(k2_u5_expected_blocks()),
        "K2 actual continuation replays the entire mixed native/client assistant unchanged"
    );
    let trace = handoff.trace.borrow();
    assert_eq!(trace.builds.len(), 2);
    assert_eq!(trace.submitted.len(), 2);
    let (_, _, carrier) = &trace.submitted[1];
    assert_eq!(
        serde_json::from_slice::<Value>(carrier.bytes()).unwrap(),
        *continuation,
        "K2 captured finalized bytes are the actual HTTP body"
    );
    let typed: xai_grok_sampling_types::messages::Message =
        serde_json::from_value(expected).unwrap();
    let bytes = serde_json::to_vec(&typed).unwrap();
    let ranges: Vec<_> = carrier
        .bytes()
        .windows(bytes.len())
        .enumerate()
        .filter_map(|(offset, window)| (window == bytes).then_some(offset))
        .collect();
    assert_eq!(
        ranges.len(),
        1,
        "K2 actual results-only body range is unique"
    );
    println!(
        "{}",
        json!({"K2_U5_submitted":true,"waiting_before_cleanup":["client_k2_c1","client_k2_c2"],"zero_early_continuation_http":true,"continuation_http":1,"executions":executions,"body_digest":carrier.digest(),"body_bytes":carrier.bytes().len(),"tools_range":carrier.tools(),"results_only_range":{"offset":ranges[0],"bytes":bytes.len(),"digest":native_content_digest(&bytes)},"pre_client_hook":"SessionActor::execute_tool_calls_with_native_recovery","owning_primary_KEEP":"STAGED_NOT_CREDITED"})
    );
}

#[tokio::test(flavor = "current_thread")]
async fn messages_pending_search_survives_client_results() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let handoff = k2_u5_handoff(false, false, K2U5Change::None).await;
            k2_u5_assert_continuation(&handoff);
            let server = MockInferenceServer::start().await.unwrap();
            let mut session = K2ReplaySession::new(&server).await;
            k2_seed(&mut session, &server).await;
            let mut frames = k2_u5_frames();
            frames.insert(
                2,
                json!({"type":"k2_unknown_semantic_event","payload":"unclassified"}),
            );
            server.enqueue_response("/v1/messages", k2_script(frames));
            let outcome = session
                .turn(
                    k2_history::NativeHistoryRequirement::Selected,
                    &server,
                    "K2 unknown semantic event must fail explicitly",
                )
                .await;
            assert!(
                outcome.is_err(),
                "K2 unknown semantic events end incomplete, never an implicit successful handoff"
            );
            assert_eq!(
                server.messages_request_count(),
                2,
                "K2 unknown semantic event cannot authorize a continuation"
            );
            assert!(
                session.executions.lock().unwrap().is_empty(),
                "K2 unknown semantic event cannot authorize client execution"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn messages_live_handoff_requires_all_waiting_results() {
    tokio::task::LocalSet::new().run_until(async {
        let handoff = k2_u5_handoff(true, false, K2U5Change::None).await;
        k2_u5_assert_continuation(&handoff);
        let trace = handoff.trace.borrow();
        let actual = &trace.builds[1].0.items;
        let origin = handoff.checkpoint.source.producing_bindings.last().unwrap();
        assert!(k2_history::native_results_continuation(&handoff.checkpoint.source, &handoff.checkpoint.ledger, actual, origin), "K2 committed owner accepts the exact actual result snapshot");
        let mut text_tail = actual.clone();
        text_tail.push(ConversationItem::user("extra non-result tail"));
        assert!(!k2_history::native_results_continuation(&handoff.checkpoint.source, &handoff.checkpoint.ledger, &text_tail, origin), "K2 results-only grammar excludes text, reminders and synthetic tails");
        assert!(!k2_history::native_results_continuation(&handoff.checkpoint.source, &handoff.checkpoint.ledger, &handoff.before_result, origin), "K2 cannot forget the withheld actual result");
        let mut duplicate = actual.clone();
        let first = duplicate.iter().find(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")).unwrap().clone();
        duplicate.push(first);
        assert!(!k2_history::native_results_continuation(&handoff.checkpoint.source, &handoff.checkpoint.ledger, &duplicate, origin), "K2 actual result duplication never satisfies a waiting obligation twice");
        println!("{}", json!({"K2_U5_waiting":true,"waiting_ids":["client_k2_c1","client_k2_c2"],"source_delivery":"Waiting","actual_result_before_release":"client_k2_c1","withheld_actual_result":"client_k2_c2","raw_snapshot_checked_before_cleanup":true,"exact_results_only_messages":1}));
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn messages_pending_retains_bound_tool_array() {
    tokio::task::LocalSet::new().run_until(async {
        let handoff = k2_u5_handoff(false, true, K2U5Change::SelectedRow).await;
        k2_u5_assert_continuation(&handoff);
        {
            let trace = handoff.trace.borrow();
            let original = &trace.submitted[0];
            let continuation = &trace.submitted[1];
            let first: Value = serde_json::from_slice(original.2.bytes()).unwrap();
            let second: Value = serde_json::from_slice(continuation.2.bytes()).unwrap();
            assert_eq!(first["tools"], second["tools"], "K2 complete original wire tools array survives a selected-row change");
            assert_eq!(original.2.declarations(), continuation.2.declarations(), "K2 original declaration bytes, not merely names, remain identical");
            for name in ["alpha__lookup", "zeta__fetch"] {
                let tool = first["tools"].as_array().unwrap().iter().find(|tool| tool["name"] == name).unwrap();
                assert_eq!(tool["defer_loading"], json!(true), "K2 full-array proof includes the original deferred definitions");
                assert!(tool["input_schema"].is_object());
            }
            assert!(original.1.credentials == continuation.1.credentials, "K2 bound credentials do not change or print their values");
            assert_eq!(original.1.config.model, "k2-replay");
            assert_eq!(continuation.1.config.base_url, handoff.server.url());
            assert_eq!(handoff.session.actor.models_manager.current_model_id(), acp::ModelId::new("k2-selected-b"));
            println!("{}", json!({"K2_U5_binding":true,"selected_row":"k2-selected-b","submitted_bound_row":"k2-replay","selection_changed_without_installing_B":true,"original_authority_still_installed":true,"original_tools_digest":original.2.tools().unwrap().digest,"continuation_tools_digest":continuation.2.tools().unwrap().digest,"whole_array_equal":true,"deferred_entries_retained":true,"body_and_transport_binding":"A"}));
        }
        for change in [K2U5Change::InstalledRow, K2U5Change::RevokedDefinition] {
            let refused = k2_u5_handoff(false, false, change).await;
            let outcome_kind = match &refused.outcome {
                Err(_) => "ERROR",
                Ok(TurnOutcome::Completed { .. }) => "COMPLETED",
                Ok(TurnOutcome::Cancelled { .. }) => "CANCELLED",
                Ok(TurnOutcome::MaxTurnsReached { .. }) => "MAX_TURNS",
                Ok(TurnOutcome::StationarityEnded) => "STATIONARITY",
            };
            println!("{}", json!({"K2_U5_refusal_outcome":true,"outcome":outcome_kind,
                "installed_B":change == K2U5Change::InstalledRow,"revoked_definition":change == K2U5Change::RevokedDefinition,
                "messages_HTTP_A":refused.server.messages_request_count(),"messages_HTTP_B":refused.other.messages_request_count()}));
            assert!(refused.outcome.is_err(), "K2 unavailable or revoked original authority ends incomplete, never rebuilds the pending exchange on B");
            assert_eq!(refused.server.messages_request_count(), 2, "K2 revoked authority has zero continuation HTTP on A");
            assert_eq!(refused.other.messages_request_count(), 0, "K2 revoked authority has zero continuation HTTP on B");
            println!("{}", json!({"K2_U5_unavailable":true,"installed_B":change == K2U5Change::InstalledRow,"revoked_definition":change == K2U5Change::RevokedDefinition,"incomplete":true,"continuation_http_A":0,"continuation_http_B":0}));
        }
    }).await;
}

fn k2lg_compiled_sources() -> Value {
    json!({
        "sampler": native_content_digest(include_bytes!("../../../../xai-grok-sampler/src/client.rs")),
        "validation": native_content_digest(include_bytes!("../../../../xai-grok-sampling-types/src/request_validation.rs")),
        "fixture": native_content_digest(include_bytes!("native_messages_replay_tests.rs")),
    })
}

async fn k2lg_selector_prelude() -> bool {
    i7yx_report_child_names();
    if std::env::var_os("K2LG_SOURCE_PROBE").is_some() {
        println!("K2LG_BUILD {}", k2lg_compiled_sources());
        return true;
    }
    if std::env::var_os("K2LG_PENDING_CHILD").is_some() {
        tokio::task::LocalSet::new().run_until(k2lg_v_cases()).await;
        return true;
    }
    false
}

async fn k2lg_pending_isolated() {
    let home = tempfile::tempdir().unwrap();
    let (mut command, parent_names) =
        i7yx_shell_child_command(I7yxChildRoute::PendingV, home.path());
    command
        .args(["--exact", "session::acp_session::native_messages_replay_tests::messages_replay_plan_checked_on_all_sends", "--nocapture"])
        .stdin(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut command);
    let output = tokio::task::spawn_blocking(move || command.output())
        .await
        .unwrap()
        .unwrap();
    i7yx_assert_child_hygiene(I7yxChildRoute::PendingV, &output.stdout, &parent_names);
    let stdout = String::from_utf8_lossy(&output.stdout);
    i7yx_print_proof_records(&output.stdout, I7yxChildRoute::PendingV);
    assert!(
        output.status.success(),
        "K2LG boundary child failed: {}",
        i7yx_output_diagnostic(I7yxChildRoute::PendingV, &output)
    );
    assert!(stdout.contains("running 1 test"));
}

fn k2lg_start_command_loop(
    session: &mut K2ReplaySession,
) -> tokio::sync::mpsc::UnboundedSender<SessionCommand> {
    let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let (chat_tx, chat_rx) = tokio::sync::mpsc::unbounded_channel();
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = session.actor.clone();
    session.tasks.push(tokio::task::spawn_local(async move {
        let _keep_channels = (chat_tx, event_tx);
        super::run_session(
            actor,
            command_rx,
            chat_rx,
            event_rx,
            None,
            Arc::new(parking_lot::Mutex::new(
                xai_grok_workspace::file_system::CodebaseIndexManager::new(),
            )),
            std::env::temp_dir(),
            crate::session::fs_watch::FsWatchCapabilities::none(),
        )
        .await;
    }));
    commands
}

fn k2lg_v_corrupt(
    request: &mut ConversationRequest,
    binding: &xai_chat_state::RequestTransportBinding,
    case: &str,
) {
    use k2_history::*;
    match case {
        "valid" | "live-defensive" | "W1" | "W3" => {}
        "missing-native" => {
            assert_eq!(request.native_bound_preparation.as_ref().unwrap().requirement(), NativeHistoryRequirement::Selected);
            request.native_history = None;
        }
        "missing-plan" => request.native_history.as_mut().unwrap().invalidate_plan(),
        "stale-context" => {
            let native = request.native_history.as_ref().unwrap();
            let mut capture = native.context().capture().clone();
            capture.history_revision += 1;
            request.native_history = Some(NativeHistoryRequest::new(NativeHistoryRequestContext::captured(
                NativeHistoryRequirement::Selected, capture, native.context().authority().clone()), native.plan().cloned()));
        }
        "stale-carrier" => {
            let mut changed = request.clone();
            changed.max_output_tokens = Some(4097);
            let client = SamplingClient::new(xai_grok_sampler::SamplerConfig {
                base_url: binding.config.base_url.clone(), model: binding.config.model.clone(),
                api_backend: xai_grok_sampler::ApiBackend::Messages, max_completion_tokens: Some(4096),
                ..Default::default()
            }).unwrap();
            request.encoded = Some(client.encode_conversation_messages(&changed).unwrap());
        }
        "tools" => request.tools.reverse(),
        "schema" => request.tools[0].parameters = json!({"type":"object","properties":{"changed":{"type":"number"}}}),
        "text" => request.items.push(ConversationItem::user("K2LG extra non-result text")),
        "missing-result" => request.items.retain(|item| !matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c2")),
        "duplicate-result" => {
            let item = request.items.iter().find(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")).unwrap().clone();
            request.items.push(item);
        }
        "foreign-result" => {
            let result = request.items.iter_mut().find_map(|item| match item { ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c2" => Some(result), _ => None }).unwrap();
            result.tool_call_id = "client_k2_foreign".into();
        }
        _ => unreachable!(),
    }
}

async fn k2lg_v_drive(case: &'static str) -> K2U5Handoff {
    use k2_history::*;
    let server = MockInferenceServer::start().await.unwrap();
    let other = MockInferenceServer::start().await.unwrap();
    let mut session = K2ReplaySession::new(&server).await;
    k2_seed(&mut session, &server).await;
    session.releases[1]
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap()
        .forget();
    server.enqueue_response("/v1/messages", k2_script(k2_u5_frames()));
    if !matches!(case, "live-defensive" | "W3" | "W1") {
        server.enqueue_response(
            "/v1/messages",
            k2_script(k2_frames(
                "msg_k2lg_done",
                vec![(json!({"type":"text","text":"K2LG done"}), Vec::new())],
                "end_turn",
            )),
        );
    }
    session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("K2LG real pending producer"))
        .await;
    session
        .actor
        .session_turn_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let trace = std::rc::Rc::new(std::cell::RefCell::new(K2U5Trace {
        check_tools: true,
        ..Default::default()
    }));
    let run_trace = trace.clone();
    let actor = session.actor.clone();
    let domain = session.domain(&server);
    let mut running = tokio::task::spawn_local(async move {
        let capture = run_trace.clone();
        let mut prepare =
            move |request: &mut ConversationRequest,
                  binding: &xai_chat_state::RequestTransportBinding| {
                let carrier = NativeFinalCarrier::capture(
                    request.encoded.as_ref().unwrap().as_bytes().to_vec().into(),
                )
                .unwrap();
                let pending = capture.borrow().submitted.len() == 1;
                capture
                    .borrow_mut()
                    .submitted
                    .push((request.clone(), binding.clone(), carrier));
                if pending {
                    k2lg_v_corrupt(request, binding, case);
                }
                Ok(())
            };
        super::switch_projection::HISTORY_S3_TEST_DOMAIN
            .scope(
                domain,
                super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
                    NativeHistoryRequirement::Selected,
                    K2_U5_TRACE.scope(
                        run_trace,
                        actor.process_conversation_turn_for_native_history_test(
                            "K2LG-live-pending",
                            None,
                            None,
                            None,
                            &mut super::length_salvage::LengthSalvage::new(None),
                            &mut Default::default(),
                            &mut prepare,
                        ),
                    ),
                ),
            )
            .await
    });
    let epoch = session.actor.turn_report.start_next_turn();
    {
        let mut state = session.actor.state.lock().await;
        state.running_task = Some(AgentTask::new_at_epoch(
            "K2LG-live-pending",
            epoch,
            running.abort_handle(),
        ));
        state
            .pending_inputs
            .push_front(user_item("K2LG-live-pending", "K2LG"));
    }
    *session.actor.current_prompt_id.lock().unwrap() = Some("K2LG-live-pending".into());
    assert_eq!(
        tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv())
            .await
            .unwrap(),
        Some(NativeResponseStatus::Accepted)
    );
    let mut entered = Vec::new();
    for _ in 0..2 {
        entered.push(
            tokio::time::timeout(K0_TIMEOUT, session.entered.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    entered.sort();
    assert_eq!(entered, vec!["alpha__lookup", "zeta__fetch"]);
    let before_result = tokio::time::timeout(K0_TIMEOUT, async {
        loop {
            let items = session.actor.chat_state_handle.get_conversation().await;
            if items.iter().any(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")) { break items; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let (checkpoint, _) = session.reload().await;
    let origin = checkpoint.source.producing_bindings.last().unwrap();
    assert_eq!(
        checkpoint
            .ledger
            .lifecycle
            .exchanges
            .get(&origin.request)
            .map(String::as_str),
        Some("LiveBound")
    );
    assert!(
        checkpoint
            .ledger
            .groups
            .iter()
            .any(|group| group.state == "LivePending" && group.origin_ref == origin.origin_ref)
    );
    assert_eq!(server.messages_request_count(), 2);
    let w1_commands = if case == "W1" {
        let commands = k2lg_start_command_loop(&mut session);
        commands
            .send(SessionCommand::Interject {
                text: "K2LG W1 real late user".into(),
                id: None,
                images: Vec::new(),
            })
            .unwrap();
        tokio::time::timeout(K0_TIMEOUT, async {
            while session.actor.pending_interjections.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        Some(commands)
    } else {
        None
    };
    let mut defensive_failures = Vec::new();
    let mut defensive_delta = 0;
    let mut held = matches!(case, "live-defensive" | "W3" | "W1").then(|| {
        server.expect_response_blocked(
            "K2LG live pending continuation",
            xai_grok_test_support::InferenceRequestMatcher::foreground(
                xai_grok_test_support::InferenceEndpoint::Messages,
            ),
            k2_script(k2_frames(
                "msg_k2lg_done",
                vec![(json!({"type":"text","text":"K2LG done"}), Vec::new())],
                "end_turn",
            )),
        )
    });
    session.releases[1].add_permits(1);
    if let Some(expectation) = held.as_mut() {
        if case == "W1" {
            let continued = tokio::time::timeout(K0_TIMEOUT, async {
                tokio::select! {
                    result = &mut running => { let _ = result.unwrap(); false },
                    _ = expectation.wait_blocked() => true,
                }
            }).await.expect("W1_REQUALIFIED_BOUNDED_OUTCOME_RACE");
            let history = session.actor.chat_state_handle.get_conversation().await;
            assert!(!history.iter().any(|item| item.text_content().contains("K2LG W1 real late user")),
                "W1_REQUALIFIED_HOLD_ASSERTION");
            assert!(continued, "W1_REQUALIFIED_CONTINUATION_SENT");
            assert!(session.actor.pending_interjections.is_empty(), "W1_REQUALIFIED_NOT_RESIDENT");
            {
                let state = session.actor.state.lock().await;
                assert_eq!(state.message_delivery.held_input.entries.iter().filter(|entry| {
                    matches!(&entry.body, super::interjection::HeldBody::Human(raw)
                        if raw.text == "K2LG W1 real late user")
                }).count(), 1, "W1_REQUALIFIED_IN_U6_CUSTODY");
            }
            let (saved, _, carrier) = trace.borrow().submitted[1].clone();
            let native = saved.native_history.as_ref().unwrap();
            assert!(validate_native_history_plan(native.context(), native.plan(), &carrier).is_ok(),
                "W1_REQUALIFIED_LIVE_BOUND_AUTHORITY");
            let bodies: Vec<_> = server.requests().into_iter().filter(|request| request.path == "/v1/messages")
                .map(|request| request.body.unwrap()).collect();
            assert_eq!(bodies.len(), 3, "W1_REQUALIFIED_ONE_CONTINUATION");
            u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
            assert!(bodies.iter().all(|body| !u6_body_text(body).contains("K2LG W1 real late user")),
                "W1_REQUALIFIED_INPUT_NOT_TRANSMITTED");
            assert_eq!(trace.borrow().builds.len(), 2, "W1_REQUALIFIED_ORIGINAL_BUILDS");
            assert_eq!(trace.borrow().submitted.len(), 2, "W1_REQUALIFIED_ORIGINAL_SUBMITS");
            assert_eq!(session.executions.lock().unwrap().len(), 2, "W1_REQUALIFIED_CLIENTS_ONCE");
            assert_eq!(other.messages_request_count(), 0, "W1_REQUALIFIED_NO_REBUILD_B");
            let (after, _) = session.reload().await;
            assert_eq!(k2_source_blocks(&after, "msg_k2_pending"), k2_source_blocks(&checkpoint, "msg_k2_pending"),
                "W1_REQUALIFIED_NATIVE_SOURCE_UNCHANGED");
            running.abort();
            assert!(matches!(tokio::time::timeout(K0_TIMEOUT, &mut running).await.unwrap(),
                Err(error) if error.is_cancelled()), "W1_REQUALIFIED_CONTROLLED_STOP");
            expectation.release();
            drop(w1_commands);
            session.actor.session_turn_active.store(false, std::sync::atomic::Ordering::SeqCst);
            println!("K2LG_V {}", json!({"route":"V2","case":"W1","window":"W1",
                "status":"OBSERVED_REQUALIFIED","time_bound":"accepted capture through continuation HTTP while exchange open",
                "continuation_http":1,"input_in_history":false,"input_in_custody":true,
                "input_resident":false,"input_transmitted":false,"source_unchanged":true,
                "client_executions":2,"controlled_test_stop":true,"post_closure_credit":false,
                "K2LG_kill":false,"L16_credit":false,"body_digest":carrier.digest()}));
            return K2U5Handoff { session, server, other, checkpoint, before_result, trace,
                outcome: Err(crate::sampling::error::local_error("k2lg_w1_controlled_stop",
                    "test stopped at the requalified open-exchange continuation")) };
        }
        tokio::time::timeout(K0_TIMEOUT, expectation.wait_blocked())
            .await
            .unwrap();
        assert!(
            !running.is_finished(),
            "K2LG setup producer remains active at the terminal barrier"
        );
        let (saved, _, carrier) = trace.borrow().submitted[1].clone();
        let native = saved.native_history.as_ref().unwrap();
        assert!(
            validate_native_history_plan(native.context(), native.plan(), &carrier).is_ok(),
            "K2LG setup direct routes require the original still-live bound authority"
        );
        if case == "W3" {
            let commands = k2lg_start_command_loop(&mut session);
            commands
                .send(SessionCommand::Interject {
                    text: "K2LG W3 real late user".into(),
                    id: None,
                    images: Vec::new(),
                })
                .unwrap();
            tokio::time::timeout(K0_TIMEOUT, async {
                while !session
                    .actor
                    .pending_interjections
                    .snapshot()
                    .iter()
                    .any(|entry| entry.text == "K2LG W3 real late user")
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let requests = server.requests();
            let submitted = requests
                .iter()
                .filter(|request| request.path == "/v1/messages")
                .nth(2)
                .unwrap()
                .body
                .as_ref()
                .unwrap();
            assert_eq!(
                server.messages_request_count(),
                3,
                "K2LG boundary W3 continuation sends exactly once"
            );
            assert_eq!(
                *submitted,
                serde_json::from_slice::<Value>(carrier.bytes()).unwrap(),
                "K2LG boundary W3 actual carrier is the original bound body"
            );
            let origin = checkpoint.source.producing_bindings.last().unwrap();
            let mut waiting: Vec<_> = checkpoint
                .ledger
                .lifecycle
                .waiting_clients
                .iter()
                .filter(|client| {
                    client.source.producing_binding == origin.request
                        && client.delivery == "Waiting"
                })
                .map(|client| client.client_id.as_str())
                .collect();
            waiting.sort();
            let blocks = submitted["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_array()
                .unwrap();
            let mut delivered: Vec<_> = blocks
                .iter()
                .map(|block| {
                    assert_eq!(
                        block["type"], "tool_result",
                        "K2LG boundary W3 results-only user tail"
                    );
                    block["tool_use_id"].as_str().unwrap()
                })
                .collect();
            delivered.sort();
            assert!(
                !waiting.is_empty(),
                "K2LG boundary W3 waiting set is real and nonempty"
            );
            assert_eq!(
                delivered, waiting,
                "K2LG boundary W3 contains all and only real waiting IDs"
            );
            assert_eq!(
                trace.borrow().builds.len(),
                2,
                "K2LG boundary W3 uses only the original and continuation builds"
            );
            assert_eq!(
                trace.borrow().submitted.len(),
                2,
                "K2LG boundary W3 captured exactly two bound requests"
            );
            assert_eq!(
                session.executions.lock().unwrap().len(),
                2,
                "K2LG boundary W3 clients execute once-only"
            );
            assert!(
                !saved
                    .items
                    .iter()
                    .any(|item| item.text_content().contains("K2LG W3 real late user")),
                "K2LG boundary W3 input is queued, not in the bound send"
            );
            assert_eq!(
                other.messages_request_count(),
                0,
                "K2LG boundary W3 never rebuilds on B"
            );
            running.abort();
            assert!(
                matches!(running.await, Err(error) if error.is_cancelled()),
                "K2LG setup W3 bounded test stop aborts its own turn"
            );
            expectation.release();
            session
                .actor
                .session_turn_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            assert!(
                session
                    .actor
                    .pending_interjections
                    .snapshot()
                    .iter()
                    .any(|entry| entry.text == "K2LG W3 real late user"),
                "K2LG boundary W3 undelivered ordinary input remains queued after the bounded stop"
            );
            let (after, _) = session.reload().await;
            assert_eq!(
                k2_source_blocks(&after, "msg_k2_pending"),
                k2_source_blocks(&checkpoint, "msg_k2_pending"),
                "K2LG boundary W3 immutable native source survives"
            );
            println!(
                "K2LG_V {}",
                json!({"route":"V2","case":"W3","window":"W3",
                "producer":"SessionCommand::Interject through real run loop after continuation HTTP",
                "continuation_http":1,"input_queued":true,"input_transmitted":false,
                "source_unchanged":true,"client_executions":2,"controlled_test_stop":true,
                "turn_completion_credit":false,"body_digest":carrier.digest()})
            );
            let outcome = Err(crate::sampling::error::local_error(
                "k2lg_w3_controlled_stop",
                "test stopped after observing the live W3 boundary",
            ));
            return K2U5Handoff {
                session,
                server,
                other,
                checkpoint,
                before_result,
                trace,
                outcome,
            };
        }
        let before = server.messages_request_count();
        for route in ["V3", "V4"] {
            for corruption in [
                "valid",
                "missing-plan",
                "missing-native",
                "stale-context",
                "tools",
                "schema",
                "text",
                "missing-result",
                "duplicate-result",
                "foreign-result",
            ] {
                let label = format!("{route}/{corruption}");
                if !k2lg_v_arm(
                    &label,
                    k2lg_v_transport(&session, &server, &trace, route, corruption),
                )
                .await
                {
                    defensive_failures.push(label);
                }
            }
        }
        defensive_delta = server.messages_request_count() - before;
        expectation.release();
    }
    let outcome = match tokio::time::timeout(K0_TIMEOUT, &mut running).await {
        Ok(result) => result.unwrap(),
        Err(error) => {
            running.abort();
            panic!("K2LG boundary bounded handoff: {error}");
        }
    };
    if let Some(expectation) = held.as_mut() {
        tokio::time::timeout(K0_TIMEOUT, expectation.wait_satisfied())
            .await
            .unwrap();
        expectation.assert_satisfied();
    }
    session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let requests = server.requests();
    let continuation = requests
        .iter()
        .filter(|request| request.path == "/v1/messages")
        .nth(2);
    println!(
        "K2LG_V {}",
        json!({"route":"V1","case":case,"producer":"K2 decoded mixed handoff","builds":trace.borrow().builds.len(),"continuation_http":usize::from(continuation.is_some()),"executions":session.executions.lock().unwrap().clone(),"body_digest":continuation.and_then(|request| request.body.as_ref()).map(|body| native_content_digest(&serde_json::to_vec(body).unwrap()))})
    );
    let (after, _) = session.reload().await;
    assert_eq!(
        k2_source_blocks(&after, "msg_k2_pending"),
        k2_source_blocks(&checkpoint, "msg_k2_pending"),
        "K2LG boundary native source stays immutable after every valid or refused send"
    );
    assert_eq!(
        other.messages_request_count(),
        0,
        "K2LG boundary never reconstructs on B"
    );
    let usage: Vec<_> = after
        .ledger
        .usage
        .iter()
        .map(|item| {
            json!({
                "request":item.request,"response_occurrence":item.response_occurrence,
                "outcome":item.outcome,"http_dispatches":item.http_dispatches,
            })
        })
        .collect();
    println!(
        "K2LG_V_SOURCE {}",
        json!({"case":case,"source_unchanged":true,"HTTP_B":0,
        "native_usage":usage,"defensive_http_delta":defensive_delta})
    );
    let success = matches!(case, "valid" | "live-defensive" | "W3");
    assert_eq!(
        server.messages_request_count(),
        if success { 3 + defensive_delta } else { 2 },
        "K2LG boundary V1 invalid pending continuation must send zero HTTP: {case}"
    );
    assert_eq!(outcome.is_ok(), success, "K2LG boundary V1 outcome: {case}");
    assert_eq!(
        session.executions.lock().unwrap().len(),
        2,
        "K2LG boundary client execution stays once-only"
    );
    assert!(
        defensive_failures.is_empty(),
        "K2LG boundary failed live defensive arms: {defensive_failures:?}"
    );
    K2U5Handoff {
        session,
        server,
        other,
        checkpoint,
        before_result,
        trace,
        outcome,
    }
}

async fn k2lg_v_transport(
    session: &K2ReplaySession,
    server: &MockInferenceServer,
    trace: &std::rc::Rc<std::cell::RefCell<K2U5Trace>>,
    route: &'static str,
    case: &'static str,
) {
    let (saved, mut binding, original) = trace.borrow().submitted[1].clone();
    let mut request = saved;
    request.x_grok_req_id = Some(format!("K2LG-{route}-{case}"));
    request.encoded = None;
    k2lg_v_corrupt(&mut request, &binding, case);
    let client = session
        .actor
        .prepare_chat_completion_from_binding(&mut binding, false)
        .await
        .unwrap();
    let before = server.messages_request_count();
    let result: Result<(), String> = tokio::time::timeout(K0_TIMEOUT, async {
        match route {
            "V3" => {
                let (mut stream, _) = client
                    .conversation_stream_messages(request)
                    .await
                    .map_err(|error| error.to_string())?;
                while let Some(event) = stream.next().await {
                    event.map_err(|error| error.to_string())?;
                }
                Ok(())
            }
            "V4" => client
                .conversation_collect(request)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string()),
            _ => unreachable!(),
        }
    })
    .await
    .unwrap();
    let delta = server.messages_request_count() - before;
    println!(
        "K2LG_V {}",
        json!({"route":route,"case":case,"producer":"saved real K2 continuation","credit":"DEFENSIVE_TRANSPORT","http_delta":delta,"expected_digest":original.digest(),"client_executions":session.executions.lock().unwrap().len()})
    );
    if case == "valid" {
        assert!(
            result.is_ok(),
            "K2LG boundary direct positive {route}: {result:?}"
        );
        assert_eq!(delta, 1, "K2LG boundary direct positive sends once {route}");
        let requests = server.requests();
        let sent = requests
            .iter()
            .filter(|request| request.path == "/v1/messages")
            .last()
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        assert_eq!(
            *sent,
            serde_json::from_slice::<Value>(original.bytes()).unwrap(),
            "K2LG boundary direct actual body equals the original bound carrier"
        );
        let primary = requests
            .iter()
            .filter(|request| request.path == "/v1/messages")
            .nth(2)
            .unwrap();
        let direct = requests
            .iter()
            .filter(|request| request.path == "/v1/messages")
            .last()
            .unwrap();
        for header in [
            "authorization",
            "x-api-key",
            "anthropic-version",
            "anthropic-beta",
        ] {
            assert!(
                primary.header(header) == direct.header(header),
                "K2LG boundary defensive transport preserves captured headers without printing values: {header}"
            );
        }
        println!(
            "K2LG_V_TRANSPORT {}",
            json!({"route":route,"case":case,
            "bound_client_preparation":true,"captured_headers_equal":true,"credential_values_printed":false})
        );
    } else {
        assert_eq!(
            delta, 0,
            "K2LG boundary direct invalid proof sends zero {route}/{case}"
        );
        assert!(
            result.is_err(),
            "K2LG boundary direct invalid proof refuses {route}/{case}"
        );
    }
    assert_eq!(
        session.executions.lock().unwrap().len(),
        2,
        "K2LG boundary defensive transport never re-executes clients"
    );
}

async fn k2lg_v_arm(label: &str, arm: impl std::future::Future<Output = ()>) -> bool {
    use futures_util::FutureExt;
    match std::panic::AssertUnwindSafe(arm).catch_unwind().await {
        Ok(()) => {
            println!("\nK2LG_ARM {}", json!({"label":label,"passed":true}));
            true
        }
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("");
            if !message.contains("K2LG boundary") {
                std::panic::resume_unwind(payload);
            }
            println!(
                "\nK2LG_ARM {}",
                json!({"label":label,"passed":false,"assertion":message})
            );
            false
        }
    }
}

async fn k2lg_v_cases() {
    println!("K2LG_BUILD {}", k2lg_compiled_sources());
    let valid = k2lg_v_drive("valid").await;
    k2_u5_assert_continuation(&valid);
    let mut failures = Vec::new();
    if !k2lg_v_arm("V3-V4/live-binding", async {
        k2lg_v_drive("live-defensive").await;
    })
    .await
    {
        failures.push("V3-V4/live-binding".into());
    }
    for case in [
        "missing-plan",
        "missing-native",
        "stale-context",
        "stale-carrier",
        "tools",
        "schema",
        "text",
        "missing-result",
        "duplicate-result",
        "foreign-result",
        "W1",
        "W3",
    ] {
        let label = format!("V1/{case}");
        if !k2lg_v_arm(&label, async {
            let handoff = k2lg_v_drive(case).await;
            if case == "W3" {
                assert!(
                    handoff.outcome.is_err(),
                    "K2LG setup W3 is an explicitly bounded live snapshot, not a completed turn"
                );
            }
        })
        .await
        {
            failures.push(label);
        }
    }
    println!(
        "K2LG_V {}",
        json!({"route":"V2","window":"W2","status":"UNSCHEDULABLE_GAP","reason":"no committed rendezvous between tool_calls.rs:539 and turn.rs:2844; no new product hook","K2LG_kill":false,"L16_credit":false})
    );
    assert!(
        failures.is_empty(),
        "K2LG boundary failed pending arms: {failures:?}"
    );
}

const K2LG_N_SELECTOR: &str = "session::acp_session::native_messages_replay_tests::messages_native_agent_grammar_uses_real_mailbox_producer";
const K2LG_N_ROOT: &str = "k2lg-root";
const K2LG_N_WORKER: &str = "k2lg-worker";
const K2LG_N_PROMPT: &str = "K2LG-N-live";
const K2LG_N_TARGET: &str = "/root/worker";

type K2lgNLog = Arc<std::sync::Mutex<Vec<(String, Value)>>>;

#[derive(Clone, Copy, Default)]
struct K2lgNOptions {
    retries: u32,
    max_turns: Option<usize>,
}

/// Forwards every chat-state persistence call to the real channel writer and records the
/// item-level appends and replacements the native arms observe.
struct K2lgNPersistence {
    inner: crate::session::chat_persistence::ChannelChatPersistence,
    log: K2lgNLog,
    n12_drop_next_native_ack: Arc<std::sync::atomic::AtomicBool>,
}

impl xai_chat_state::ChatPersistence for K2lgNPersistence {
    fn persist_message(&mut self, item: &ConversationItem) {
        self.log
            .lock()
            .unwrap()
            .push(("persist".into(), serde_json::to_value(item).unwrap()));
        xai_chat_state::ChatPersistence::persist_message(&mut self.inner, item);
    }
    fn persist_working_directory_switch_and_ack(
        &mut self,
        item: &ConversationItem,
    ) -> tokio::sync::oneshot::Receiver<
        Result<xai_chat_state::StrictAppendAck, xai_chat_state::StrictAppendError>,
    > {
        xai_chat_state::ChatPersistence::persist_working_directory_switch_and_ack(
            &mut self.inner,
            item,
        )
    }
    fn replace_history(&mut self, items: &[ConversationItem]) {
        self.log
            .lock()
            .unwrap()
            .push(("replace".into(), serde_json::to_value(items).unwrap()));
        xai_chat_state::ChatPersistence::replace_history(&mut self.inner, items);
    }
    fn replace_history_for_strip_and_ack(
        &mut self,
        items: &[ConversationItem],
    ) -> tokio::sync::oneshot::Receiver<std::io::Result<()>> {
        self.log
            .lock()
            .unwrap()
            .push(("strip".into(), serde_json::to_value(items).unwrap()));
        xai_chat_state::ChatPersistence::replace_history_for_strip_and_ack(&mut self.inner, items)
    }
    fn flush(&mut self) {
        xai_chat_state::ChatPersistence::flush(&mut self.inner);
    }
    fn read_native_history(
        &mut self,
    ) -> tokio::sync::oneshot::Receiver<
        Result<xai_chat_state::persistence::native::NativeRead, k2_history::NativeUnavailable>,
    > {
        u6b_r7_watch_native_ack(
            xai_chat_state::ChatPersistence::read_native_history(&mut self.inner),
            "read",
            self.log.clone(),
        )
    }
    fn persist_native_generation_and_ack(
        &mut self,
        write: xai_chat_state::persistence::native::NativeWrite,
    ) -> tokio::sync::oneshot::Receiver<
        Result<xai_chat_state::persistence::native::NativeAppendAck, k2_history::NativeUnavailable>,
    > {
        self.log
            .lock()
            .unwrap()
            .push(("native".into(), json!("generation")));
        u6b_n12_maybe_drop_ack(
            xai_chat_state::ChatPersistence::persist_native_generation_and_ack(&mut self.inner, write),
            &self.n12_drop_next_native_ack, &self.log, "generation",
        )
    }
    fn persist_native_projection_and_ack(
        &mut self,
        write: xai_chat_state::persistence::native::NativeProjectionWrite,
    ) -> tokio::sync::oneshot::Receiver<
        Result<k2_history::NativeCompoundAck, k2_history::NativeUnavailable>,
    > {
        self.log
            .lock()
            .unwrap()
            .push(("native".into(), json!("projection")));
        u6b_r7_watch_native_ack(
            u6b_n12_maybe_drop_ack(
                xai_chat_state::ChatPersistence::persist_native_projection_and_ack(&mut self.inner, write),
                &self.n12_drop_next_native_ack, &self.log, "projection",
            ),
            "projection",
            self.log.clone(),
        )
    }
    fn persist_native_checkpoint_and_ack(
        &mut self,
        write: xai_chat_state::persistence::native::NativeWrite,
        pending: xai_chat_state::persistence::native::NativePendingProtection,
    ) -> tokio::sync::oneshot::Receiver<
        Result<xai_chat_state::persistence::native::NativeAppendAck, k2_history::NativeUnavailable>,
    > {
        self.log
            .lock()
            .unwrap()
            .push(("native".into(), json!("checkpoint")));
        u6b_n12_maybe_drop_ack(
            xai_chat_state::ChatPersistence::persist_native_checkpoint_and_ack(
                &mut self.inner, write, pending,
            ),
            &self.n12_drop_next_native_ack, &self.log, "checkpoint",
        )
    }
}

/// Registers the receiver session as an active same-team child whose control is the
/// production `ShellChildRuntime`, so coordinator delivery uses the real follow-up seam.
struct K2lgNRunner {
    commands: tokio::sync::mpsc::UnboundedSender<SessionCommand>,
    started: std::rc::Rc<tokio::sync::Notify>,
}

impl k0_coordinator::ChildRunner for K2lgNRunner {
    type Control = crate::agent::subagent::ShellChildRuntime;
    type CompletionData = ();
    type RunFuture = k0_coordinator::LocalBoxFuture<k0_coordinator::ChildRunOutput<()>>;
    type ValidateFuture = std::future::Pending<k0_task::SubagentValidateTypeOutcome>;
    type DescribeFuture = std::future::Pending<k0_task::SubagentDescribeOutcome>;

    fn run(&self, request: k0_coordinator::ChildRunRequest<Self::Control>) -> Self::RunFuture {
        let commands = self.commands.clone();
        let started = self.started.clone();
        Box::pin(async move {
            let id = request.request.id.clone();
            let (receipt_sink, _receipts) = tokio::sync::mpsc::channel(1);
            assert!(
                request
                    .reporter
                    .started(k0_coordinator::StartedChild {
                        child_session_id: id.clone(),
                        persona: None,
                        resumed_from: None,
                        child_cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                        worktree_path: None,
                        effective_model_id: "k2-replay".into(),
                        definition_background: false,
                        control: crate::agent::subagent::ShellChildRuntime {
                            message_delivery:
                                crate::session::message_delivery::MessageDeliveryHandle::new(
                                    commands.clone(),
                                    id.clone(),
                                ),
                            child_cmd_tx: commands,
                            active_message_target_session_id: id,
                            child_signals: SessionSignalsHandle::new(),
                            _child_thread: None,
                            receipt_sink,
                            force_queue_envelope: false,
                            active_message_parent_session_id: K2LG_N_ROOT.into(),
                            active_message_parent_prompt_index: Arc::new(
                                std::sync::atomic::AtomicUsize::new(0),
                            ),
                        },
                    })
                    .await,
                "K2LG setup worker registers as the active same-team child"
            );
            started.notify_one();
            std::future::pending().await
        })
    }
    fn validate_type(&self, _: String, _: String) -> Self::ValidateFuture {
        std::future::pending()
    }
    fn describe_type(&self, _: String, _: Option<String>, _: String) -> Self::DescribeFuture {
        std::future::pending()
    }
    fn supports_wake(&self) -> bool {
        false
    }
    fn on_completed(
        &self,
        _: k0_coordinator::ChildCompletion<()>,
        published: Box<dyn FnOnce() + Send>,
    ) {
        published();
    }
}

struct K2lgN {
    session: K2ReplaySession,
    server: MockInferenceServer,
    root: xai_grok_agent::Agent,
    loop_tx: tokio::sync::mpsc::UnboundedSender<SessionCommand>,
    loop_rx: Option<tokio::sync::mpsc::UnboundedReceiver<SessionCommand>>,
    log: K2lgNLog,
    chat_token: tokio_util::sync::CancellationToken,
    agent_wide: Arc<std::sync::atomic::AtomicBool>,
    calls: usize,
}

struct K2lgNTurn {
    running: tokio::task::JoinHandle<Result<TurnOutcome, acp::Error>>,
    epoch: TurnEpoch,
    identity: std::rc::Rc<()>,
    submits: std::rc::Rc<std::cell::RefCell<Vec<Value>>>,
    turn_start_ops: usize,
}

struct K2lgNObserved {
    fx: K2lgN,
    barrier: Vec<ConversationItem>,
    barrier_ops: usize,
    turn_start_ops: usize,
    submits: std::rc::Rc<std::cell::RefCell<Vec<Value>>>,
}

/// K2's replay session with three declared differences: the agent-wide turn flag is shared
/// with the sender, the chat-state actor runs on a retained cancellation token with the
/// recording persistence, and retries/max-turns come from `options`.
async fn k2lg_n_session(
    server: &MockInferenceServer,
    options: K2lgNOptions,
    agent_wide: &Arc<std::sync::atomic::AtomicBool>,
    chat_token: &tokio_util::sync::CancellationToken,
    log: &K2lgNLog,
) -> K2ReplaySession {
    let directory = tempfile::tempdir().unwrap();
    let config = xai_grok_sampler::SamplerConfig {
        base_url: server.url(),
        model: "k2-replay".into(),
        api_key: Some("nonsecret-k2-key".into()),
        api_backend: xai_grok_sampler::ApiBackend::Messages,
        max_completion_tokens: Some(4096),
        max_retries: Some(options.retries),
        ..Default::default()
    };
    let info = crate::session::info::Info {
        id: acp::SessionId::new(format!(
            "k2lg-n-{}",
            directory.path().file_name().unwrap().to_string_lossy()
        )),
        cwd: directory.path().to_string_lossy().into_owned(),
    };
    let persistence = crate::session::persistence::new_with_explicit_dir(
        &info,
        directory.path().to_path_buf(),
        acp::ModelId::new("k2-replay"),
        xai_grok_sampler::SamplingClient::new(config.clone()).unwrap(),
        "k2-replay".into(),
        crate::session::persistence::ExplicitSessionOpen::New {
            identity: None,
            next_trace_turn: None,
        },
    )
    .await
    .unwrap();
    let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = tokio::task::spawn_local(async move {
        while let Some(message) = gateway_rx.recv().await {
            if let xai_acp_lib::AcpClientMessage::SessionNotification(arguments) = message {
                let _ = arguments.response_tx.send(Ok(()));
            }
        }
    });
    let (mut actor, _) = create_test_actor_with_chat_persistence(
        0,
        256_000,
        85,
        gateway_tx,
        persistence.tx.clone(),
        Box::new(xai_chat_state::NullChatPersistence),
    )
    .await;
    let sampling = actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .expect("K2LG setup chat-state sampling config");
    let (chat_event_tx, _chat_event_rx) = tokio::sync::mpsc::unbounded_channel();
    actor.chat_state_handle = xai_chat_state::ChatStateActor::spawn(
        vec![],
        sampling,
        Box::new(K2lgNPersistence {
            inner: crate::session::chat_persistence::ChannelChatPersistence::new(
                persistence.tx.clone(),
            ),
            log: log.clone(),
            n12_drop_next_native_ack: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }),
        chat_event_tx,
        chat_token.clone(),
    );
    actor.tool_context.is_turn_active = Some(agent_wide.clone());
    actor.max_turns = options.max_turns;
    actor.session_info = info;
    actor.attach_non_interactive.set(true);
    actor.max_retries = 0;
    actor.compaction.compaction_mode = xai_chat_state::CompactionMode::Summary;
    {
        let mut slot = actor.agent.borrow_mut();
        let mut policy = slot.compaction_policy().clone();
        policy.two_pass_enabled = false;
        policy.remote_compaction_v2 = false;
        policy.memory_flush_enabled = false;
        *slot = xai_grok_agent::Agent::new(
            slot.definition().clone(),
            slot.prompt_context().clone(),
            slot.system_prompt().to_string(),
            slot.tool_bridge().clone(),
            slot.reminder_policy().clone(),
            policy,
            Vec::new(),
            true,
        );
    }
    actor.memory.flush_config.enabled = false;
    actor.memory.dream_config.enabled = false;
    actor.memory.save_on_end = false;
    let (manager, models) = manager_with_entries();
    let mut entry = entry_with_search_flag("k2-replay", true);
    entry.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
    entry.api_base_url = Some(server.url());
    entry.api_key = Some("nonsecret-k2-key".into());
    manager.insert_test_entry("k2-replay", entry);
    manager.set_current_model_id(acp::ModelId::new("k2-replay"));
    actor.models_manager = manager;
    let mut binding = actor
        .chat_state_handle
        .capture_transport_binding()
        .await
        .unwrap();
    binding.config.base_url = server.url();
    binding.config.model = "k2-replay".into();
    binding.config.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
    binding.config.supports_search_tool = true;
    binding.config.max_completion_tokens = Some(4096);
    binding.config.max_retries = Some(options.retries);
    binding.credentials.api_key = Some("nonsecret-k2-key".into());
    binding.credentials.auth_type = xai_chat_state::AuthType::ApiKey;
    actor
        .chat_state_handle
        .install_transport_binding(binding)
        .await
        .unwrap();
    let executions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (entered_tx, entered) = tokio::sync::mpsc::unbounded_channel();
    let releases = [
        Arc::new(tokio::sync::Semaphore::new(16)),
        Arc::new(tokio::sync::Semaphore::new(16)),
    ];
    for (index, name) in ["alpha__lookup", "zeta__fetch"].into_iter().enumerate() {
        actor
            .agent
            .borrow()
            .tool_bridge()
            .register_mcp_tools(
                name.into(),
                K2ClientTool {
                    name,
                    executions: executions.clone(),
                    entered: entered_tx.clone(),
                    release: releases[index].clone(),
                },
                Some(json!({"type":"object","description":"Exact K2 original schema","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false})),
            )
            .unwrap();
    }
    {
        let mut mcp = actor.mcp_state.lock().await;
        mcp.finish_init();
        mcp.complete_init();
    }
    actor
        .workspace_ops
        .bind_local_session(
            &actor.session_id_string(),
            actor.tool_context.cwd.as_path().to_path_buf(),
            actor.tool_context.hunk_tracker_handle.clone(),
            actor.agent.borrow().tool_bridge().toolset(),
            None,
        )
        .unwrap();
    let (sampler_tx, mut sampler_rx) = tokio::sync::mpsc::unbounded_channel();
    actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
        config,
        xai_grok_sampler::RetryPolicy {
            max_retries: options.retries,
            ..Default::default()
        },
        sampler_tx,
    );
    let actor = Arc::new(actor);
    let drainer = actor.clone();
    let (source_tx, source_acks) = tokio::sync::mpsc::unbounded_channel();
    let sampler = tokio::task::spawn_local(async move {
        while let Some(event) = sampler_rx.recv().await {
            let status = match &event {
                xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                    evidence: k2_history::NativeEvidenceEvent::Attempt(attempt),
                    ..
                } => Some(attempt.status),
                _ => None,
            };
            drainer.handle_sampling_event(event).await;
            if let Some(status) = status {
                let _ = source_tx.send(status);
            }
        }
    });
    K2ReplaySession {
        actor,
        persistence,
        directory,
        _models: models,
        tasks: vec![gateway, sampler],
        executions,
        entered,
        releases,
        source_acks,
    }
}

/// Builds the sending root through the production `AgentRebuildSpec`, so the normal
/// `native_agents_enabled_for` gate decides `NativeAgentsEnabled` from declared offline
/// inputs (subagents on, v2 feature tier on, own Messages row `multi_agent_v2 = true`).
async fn k2lg_n_root(
    server: &MockInferenceServer,
    sender: xai_grok_tools::implementations::grok_build::task::backend::SubagentCoordinatorSender,
) -> xai_grok_agent::Agent {
    let mut spec = crate::session::agent_rebuild::test_rebuild_spec_default();
    {
        let spec = Arc::get_mut(&mut spec).expect("K2LG setup root rebuild spec uniquely owned");
        spec.subagents_enabled = true;
        spec.multi_agent_v2_feature = true;
        spec.subagent_event_tx = Some(sender.event_sender().0);
        spec.subagent_coordinator_sender = Some(sender);
        spec.session_id_str = K2LG_N_ROOT.into();
        spec.team_scope_id = K2LG_N_ROOT.into();
    }
    let mut row = entry_with_search_flag("k2lg-root-row", true);
    row.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
    row.info.multi_agent_v2 = Some(true);
    row.api_base_url = Some(server.url());
    row.api_key = Some("nonsecret-k2lg-root-key".into());
    spec.models_manager.insert_test_entry("k2lg-root-row", row);
    *spec
        .session_model_id
        .write()
        .expect("K2LG setup session model lock") = acp::ModelId::new("k2lg-root-row");
    let root = spec
        .build_agent(AgentDefinition::default_grok_build())
        .await
        .expect("K2LG setup root agent builds through the normal host gate");
    let gate = root
        .tool_bridge()
        .toolset()
        .resources
        .lock()
        .await
        .get::<k0_task::NativeAgentsEnabled>()
        .map(|gate| gate.0);
    assert_eq!(
        gate,
        Some(true),
        "K2LG setup native_agents_enabled_for arms the root on its declared Messages v2 row"
    );
    assert!(
        root.tool_bridge()
            .toolset()
            .get_tool_metadata("send_message")
            .is_some(),
        "K2LG setup the armed host gate adds the real send_message tool"
    );
    println!(
        "\nK2LG_N {}",
        json!({"producer":"host_gate","native_agents_enabled_for":{"subagents_enabled":true,"subagent_depth":0,"multi_agent_v2_feature":true,"model_row":true},"NativeAgentsEnabled":true,"send_message_tool":true,"row":"k2lg-root-row","api_backend":"messages"})
    );
    root
}

async fn k2lg_n_register_worker(root: &xai_grok_agent::Agent, started: &tokio::sync::Notify) {
    use xai_grok_tools::implementations::grok_build::task::backend::{
        SubagentBackend, SubagentBackendResource,
    };
    let backend = root
        .tool_bridge()
        .read_resource::<SubagentBackendResource>()
        .await
        .expect("K2LG setup root carries its real coordinator backend");
    let spawned = tokio::time::timeout(
        K0_TIMEOUT,
        backend.backend().spawn(
            k0_task::SubagentRequest {
                id: K2LG_N_WORKER.into(),
                prompt: "K2LG receiver worker".into(),
                description: "worker".into(),
                subagent_type: "general-purpose".into(),
                parent_session_id: K2LG_N_ROOT.into(),
                parent_prompt_id: None,
                resume_from: None,
                cwd: None,
                runtime_overrides: k0_task::SubagentRuntimeOverrides {
                    native_agent: Some(k0_task::NativeAgentSpawn {
                        task_name: "worker".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                run_in_background: true,
                surface_completion: false,
                await_to_completion: false,
                fork_context: false,
                context: Default::default(),
                owner: Default::default(),
                cancel_token: Default::default(),
                spawn_root: Default::default(),
            },
            None,
        ),
    )
    .await
    .expect("K2LG setup worker spawn returns")
    .expect("K2LG setup worker spawn accepted");
    assert!(
        spawned.success,
        "K2LG setup named worker spawn succeeds: {:?}",
        spawned.error
    );
    tokio::time::timeout(K0_TIMEOUT, started.notified())
        .await
        .expect("K2LG setup worker becomes active");
}

async fn k2lg_n_fixture(options: K2lgNOptions) -> K2lgN {
    let server = MockInferenceServer::start().await.unwrap();
    let agent_wide = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let chat_token = tokio_util::sync::CancellationToken::new();
    let log = K2lgNLog::default();
    let mut session = k2lg_n_session(&server, options, &agent_wide, &chat_token, &log).await;
    let (loop_tx, loop_rx) = tokio::sync::mpsc::unbounded_channel();
    let started = std::rc::Rc::new(tokio::sync::Notify::new());
    let (sender, receiver) = k0_coordinator::SubagentCoordinator::<K2lgNRunner>::channel();
    session.tasks.push(tokio::task::spawn_local(
        k0_coordinator::SubagentCoordinator::from_channel(
            receiver,
            K2lgNRunner {
                commands: loop_tx.clone(),
                started: started.clone(),
            },
            Default::default(),
        )
        .run(),
    ));
    let root = k2lg_n_root(&server, sender).await;
    k2lg_n_register_worker(&root, &started).await;
    K2lgN {
        session,
        server,
        root,
        loop_tx,
        loop_rx: Some(loop_rx),
        log,
        chat_token,
        agent_wide,
        calls: 0,
    }
}

/// R5-1 harness setup: the turn under test heads the queue with `current_prompt_id` pinned
/// and `session_turn_active` stored; the receiver run loop starts on the first install.
async fn k2lg_n_running(
    fx: &mut K2lgN,
    handle: tokio::task::AbortHandle,
) -> (TurnEpoch, std::rc::Rc<()>) {
    let epoch = fx.session.actor.turn_report.start_next_turn();
    let identity = {
        let mut state = fx.session.actor.state.lock().await;
        state.running_task = Some(AgentTask::new_at_epoch(K2LG_N_PROMPT, epoch, handle));
        state
            .pending_inputs
            .push_front(user_item(K2LG_N_PROMPT, "K2LGN"));
        state.running_task.as_ref().unwrap().identity.clone()
    };
    *fx.session.actor.current_prompt_id.lock().unwrap() = Some(K2LG_N_PROMPT.into());
    fx.session
        .actor
        .session_turn_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    if let Some(command_rx) = fx.loop_rx.take() {
        let (chat_tx, chat_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let actor = fx.session.actor.clone();
        fx.session.tasks.push(tokio::task::spawn_local(async move {
            let _keep_channels = (chat_tx, event_tx);
            super::run_session(
                actor,
                command_rx,
                chat_rx,
                event_rx,
                None,
                Arc::new(parking_lot::Mutex::new(
                    xai_grok_workspace::file_system::CodebaseIndexManager::new(),
                )),
                std::env::temp_dir(),
                crate::session::fs_watch::FsWatchCapabilities::none(),
            )
            .await;
        }));
    }
    (epoch, identity)
}

async fn k2lg_n_stub_running(fx: &mut K2lgN) -> (TurnEpoch, std::rc::Rc<()>) {
    let stub = tokio::task::spawn_local(std::future::pending::<()>());
    let handle = stub.abort_handle();
    fx.session.tasks.push(stub);
    k2lg_n_running(fx, handle).await
}

fn k2lg_n_native_text(body: &str) -> String {
    format!("Untrusted message from agent /root to {K2LG_N_TARGET} (not user consent):\n{body}")
}

fn k2lg_n_is_native(item: &ConversationItem) -> bool {
    matches!(item, ConversationItem::User(user)
        if user.synthetic_reason == Some(xai_grok_sampling_types::SyntheticReason::SystemReminder))
        && item
            .text_content()
            .starts_with("Untrusted message from agent /root to /root/worker (not user consent):")
}

fn k2lg_n_natives(items: &[ConversationItem]) -> Vec<String> {
    items
        .iter()
        .filter(|item| k2lg_n_is_native(item))
        .map(ConversationItem::text_content)
        .collect()
}

fn k2lg_n_values(items: &[ConversationItem]) -> Vec<Value> {
    items
        .iter()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect()
}

fn k2lg_n_resident(session: &K2ReplaySession) -> Vec<ConversationItem> {
    session
        .actor
        .pending_native_agent_messages
        .lock()
        .unwrap()
        .clone()
}

fn k2lg_n_ops(fx: &K2lgN) -> Vec<(String, Value)> {
    fx.log.lock().unwrap().clone()
}

fn k2lg_n_native_persists(ops: &[(String, Value)]) -> usize {
    ops.iter()
        .filter(|(op, value)| {
            op == "persist"
                && serde_json::from_value::<ConversationItem>(value.clone())
                    .is_ok_and(|item| k2lg_n_is_native(&item))
        })
        .count()
}

fn k2lg_n_bodies(server: &MockInferenceServer) -> Vec<Value> {
    let bodies: Vec<Value> = server
        .requests()
        .into_iter()
        .filter(|request| request.path == "/v1/messages")
        .filter_map(|request| request.body)
        .collect();
    let _ = U6_ARM_OBSERVATIONS.try_with(|observations| {
        observations.borrow_mut().push(json!({
            "stage": "request traffic summary",
            "requests": bodies.iter().map(|body| {
                let encoded = serde_json::to_string(body).unwrap();
                json!({
                    "roles": body["messages"].as_array().map(|messages| messages.iter().map(|message| message["role"].clone()).collect::<Vec<_>>()),
                    "tools": body["tools"].as_array().map(Vec::len),
                    "tool_names": body["tools"].as_array().map(|tools| tools.iter().map(|tool| tool.get("name").cloned()).collect::<Vec<_>>()),
                    "bytes": encoded.len(),
                    "has_native_pending": encoded.contains("srvtoolu_k2_pending"),
                    "has_notification": encoded.contains("U6b independent completion notification"),
                    "has_monitor": encoded.contains("U6b lifecycle equal monitor payload"),
                })
            }).collect::<Vec<_>>()
        }));
    });
    bodies
}

fn k2lg_n_text_count(body: &Value, native_body: &str) -> usize {
    let needle = serde_json::to_string(&k2lg_n_native_text(native_body)).unwrap();
    serde_json::to_string(body)
        .unwrap()
        .matches(&needle[1..needle.len() - 1])
        .count()
}

fn k2lg_n_pending_group(checkpoint: &K2Checkpoint) -> Option<String> {
    checkpoint
        .ledger
        .groups
        .iter()
        .find(|group| {
            group.members.iter().any(|member| {
                let raw: Value = serde_json::from_slice(
                    &checkpoint.blobs[&checkpoint.source.ordered_blocks
                        [member.occurrence as usize]
                        .block
                        .digest],
                )
                .unwrap();
                raw["id"] == "srvtoolu_k2_pending"
            })
        })
        .map(|group| group.state.clone())
}

fn k2lg_n_live(checkpoint: &K2Checkpoint) -> bool {
    checkpoint
        .source
        .producing_bindings
        .last()
        .is_some_and(|origin| {
            checkpoint
                .ledger
                .lifecycle
                .exchanges
                .get(&origin.request)
                .map(String::as_str)
                == Some("LiveBound")
                && checkpoint.ledger.groups.iter().any(|group| {
                    group.state == "LivePending" && group.origin_ref == origin.origin_ref
                })
        })
}

async fn k2lg_n_disk(session: &K2ReplaySession) -> Vec<ConversationItem> {
    use crate::session::storage::StorageAdapter;
    let (respond_to, flushed) = tokio::sync::oneshot::channel();
    session
        .persistence
        .tx
        .send(PersistenceMsg::FlushAndAck { respond_to })
        .unwrap();
    flushed.await.unwrap().unwrap();
    crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    )
    .load_session(&session.actor.session_info)
    .await
    .unwrap()
    .chat_history
}

async fn k2lg_n_selected<F: std::future::Future>(fx: &K2lgN, future: F) -> F::Output {
    super::switch_projection::HISTORY_S3_TEST_DOMAIN
        .scope(
            fx.session.domain(&fx.server),
            super::switch_projection::HISTORY_S3_TEST_REQUIREMENT
                .scope(k2_history::NativeHistoryRequirement::Selected, future),
        )
        .await
}

async fn k2lg_n_close_mailbox(fx: &K2lgN) {
    fx.chat_token.cancel();
    tokio::time::timeout(K0_TIMEOUT, async {
        while fx
            .session
            .actor
            .chat_state_handle
            .get_sampling_config()
            .await
            .is_some()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("K2LG setup the real chat-state receiver closes");
}

/// One real `send_message` from the gated root: tool -> coordinator -> `ShellChildRuntime`
/// -> `SessionCommand::AgentMessage` -> `message_item` -> resident buffer.
async fn k2lg_n_send(fx: &mut K2lgN, body: &str) {
    fx.calls += 1;
    let before = k2lg_n_resident(&fx.session).len();
    let result = fx
        .root
        .tool_bridge()
        .call(
            "send_message",
            json!({"target": K2LG_N_TARGET, "message": body}),
            &format!("k2lg_n_send_{}", fx.calls),
        )
        .await
        .unwrap_or_else(|error| {
            panic!("K2LG boundary real send_message tool call failed: {error}")
        });
    let xai_grok_tools::types::output::ToolOutput::Dynamic(output) = &result.output else {
        panic!("K2LG boundary send_message returns its coordinator receipt");
    };
    assert_eq!(
        output.value["status"]
            .as_str()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("delivered"),
        "K2LG boundary the real coordinator delivers live to the worker: {}",
        output.value
    );
    tokio::time::timeout(K0_TIMEOUT, async {
        while k2lg_n_resident(&fx.session).len() <= before {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("K2LG boundary the receiver run loop buffers the real native message");
    println!(
        "\nK2LG_N {}",
        json!({"producer":"send_message->coordinator->ShellChildRuntime->AgentMessage->message_item->resident","call":fx.calls,"status":output.value["status"],"resident":k2lg_n_resident(&fx.session).len()})
    );
}

fn k2lg_n_closure_frames() -> Vec<Value> {
    k2_frames(
        "msg_k2lg_n_closed",
        vec![
            k2_result("srvtoolu_k2_pending", "alpha__lookup"),
            (
                json!({"type":"text","text":"K2LG N continuation closes the pending search"}),
                Vec::new(),
            ),
        ],
        "end_turn",
    )
}

fn k2lg_n_after_frames(id: &str) -> Vec<Value> {
    k2_frames(
        id,
        vec![(
            json!({"type":"text","text":"K2LG N after the native release"}),
            Vec::new(),
        )],
        "end_turn",
    )
}

fn k2lg_n_results_tail() -> Value {
    json!({"role":"user","content":[
        {"type":"tool_result","tool_use_id":"client_k2_c1","content":"alpha__lookup actual result: \"first actual\""},
        {"type":"tool_result","tool_use_id":"client_k2_c2","content":"zeta__fetch actual result: \"second actual\"","cache_control":{"type":"ephemeral"}}
    ]})
}

/// Seeds K2's completed group, then drives K2's real mixed pending handoff under Selected
/// with the second client held, the turn installed per R5-1 and the receiver loop running.
async fn k2lg_n_handoff(fx: &mut K2lgN, continuation: Option<ScriptedResponse>) -> K2lgNTurn {
    use k2_history::*;
    k2_seed(&mut fx.session, &fx.server).await;
    fx.session.releases[1]
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap()
        .forget();
    fx.server
        .enqueue_response("/v1/messages", k2_script(k2_u5_frames()));
    if let Some(continuation) = continuation {
        fx.server.enqueue_response("/v1/messages", continuation);
    }
    fx.session.actor.chat_state_handle.begin_turn_capture();
    let _ = fx.session.actor.chat_state_handle.get_conversation().await;
    let turn_start_ops = fx.log.lock().unwrap().len();
    fx.session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user(
            "K2LG N real native producer during the live handoff",
        ))
        .await;
    fx.session
        .actor
        .session_turn_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let submits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let record = submits.clone();
    let log = fx.log.clone();
    let actor = fx.session.actor.clone();
    let domain = fx.session.domain(&fx.server);
    let running = tokio::task::spawn_local(async move {
        let mut prepare =
            move |request: &mut ConversationRequest,
                  _: &xai_chat_state::RequestTransportBinding| {
                record.borrow_mut().push(json!({
                    "items": serde_json::to_value(&request.items).unwrap(),
                    "history_revision": request
                        .native_bound_preparation
                        .as_ref()
                        .and_then(|preparation| preparation.history_revision()),
                    "persistence_ops": log.lock().unwrap().len(),
                }));
                Ok(())
            };
        super::switch_projection::HISTORY_S3_TEST_DOMAIN
            .scope(
                domain,
                super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
                    NativeHistoryRequirement::Selected,
                    actor.process_conversation_turn_for_native_history_test(
                        K2LG_N_PROMPT,
                        None,
                        None,
                        None,
                        &mut super::length_salvage::LengthSalvage::new(None),
                        &mut Default::default(),
                        &mut prepare,
                    ),
                ),
            )
            .await
    });
    let (epoch, identity) = k2lg_n_running(fx, running.abort_handle()).await;
    assert_eq!(
        tokio::time::timeout(K0_TIMEOUT, fx.session.source_acks.recv())
            .await
            .unwrap(),
        Some(NativeResponseStatus::Accepted),
        "K2LG setup K2's mixed response is decoded and committed before client execution"
    );
    let mut entered = Vec::new();
    for _ in 0..2 {
        entered.push(
            tokio::time::timeout(K0_TIMEOUT, fx.session.entered.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    entered.sort();
    assert_eq!(entered, vec!["alpha__lookup", "zeta__fetch"]);
    tokio::time::timeout(K0_TIMEOUT, async {
        loop {
            let items = fx.session.actor.chat_state_handle.get_conversation().await;
            if items.iter().any(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fx.server.messages_request_count(), 2);
    let (checkpoint, _) = fx.session.reload().await;
    assert!(
        k2lg_n_live(&checkpoint),
        "K2LG setup K2's exchange is LiveBound with a LivePending group while clients execute"
    );
    K2lgNTurn {
        running,
        epoch,
        identity,
        submits,
        turn_start_ops,
    }
}

async fn k2lg_n_n1() -> K2lgNObserved {
    const BODY: &str = "K2LG N native body during the live handoff";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    let mut turn = k2lg_n_handoff(&mut fx, None).await;
    let sender_turn = super::turn_task::TurnActiveGuard::activate(Some(&fx.agent_wide));
    assert!(
        fx.agent_wide.load(std::sync::atomic::Ordering::SeqCst),
        "K2LG setup the sender session's turn is in flight on the agent-wide flag"
    );
    k2lg_n_send(&mut fx, BODY).await;
    assert_eq!(
        k2lg_n_natives(&k2lg_n_resident(&fx.session)),
        vec![k2lg_n_native_text(BODY)],
        "K2LG boundary N1 the real plaintext reminder is resident while K2's continuation is owed"
    );
    assert!(
        k2lg_n_natives(&fx.session.actor.chat_state_handle.get_conversation().await).is_empty(),
        "K2LG boundary N1 no native item reaches history while pending"
    );
    drop(sender_turn);
    assert!(
        !fx.agent_wide.load(std::sync::atomic::Ordering::SeqCst),
        "K2LG setup the sender session's turn ended and cleared the agent-wide flag"
    );
    let mut held = fx.server.expect_response_blocked(
        "K2LG N continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    tokio::time::timeout(K0_TIMEOUT, held.wait_blocked())
        .await
        .expect("K2LG boundary N1 the legitimate results-only continuation reaches HTTP while the native prefix stays resident");
    let barrier = fx.session.actor.chat_state_handle.get_conversation().await;
    let barrier_ops = fx.log.lock().unwrap().len();
    assert_eq!(
        k2lg_n_natives(&k2lg_n_resident(&fx.session)),
        vec![k2lg_n_native_text(BODY)],
        "K2LG boundary N1 per-session rule keeps the receiver's prefix resident after the sender's turn ended"
    );
    assert!(
        k2lg_n_natives(&barrier).is_empty(),
        "K2LG boundary N1 history holds no native item before closure"
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 3);
    assert_eq!(
        bodies[2]["messages"].as_array().unwrap().last().unwrap(),
        &k2lg_n_results_tail(),
        "K2LG boundary N1 the continuation is the exact results-only message"
    );
    assert_eq!(
        k2lg_n_text_count(&bodies[2], BODY),
        0,
        "K2LG boundary N1 the native reminder is not transmitted before closure"
    );
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_k2lg_n_after")),
    );
    held.release();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("K2LG boundary N1 the turn finishes after closure")
        .unwrap();
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        outcome.is_ok(),
        "K2LG boundary N1 closure release completes the same turn: {:?}",
        outcome.err()
    );
    let after = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        k2lg_n_natives(&after),
        vec![k2lg_n_native_text(BODY)],
        "K2LG boundary N1 the unchanged native prefix is released exactly once after closure"
    );
    assert!(
        k2lg_n_resident(&fx.session).is_empty(),
        "K2LG boundary N1 the released prefix leaves the resident buffer"
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(
        bodies.len(),
        4,
        "K2LG boundary N1 exactly one same-turn model round follows the release"
    );
    assert_eq!(
        k2lg_n_text_count(&bodies[3], BODY),
        1,
        "K2LG boundary N1 the model sees the native reminder once, only after closure"
    );
    let (checkpoint, _) = fx.session.reload().await;
    assert_eq!(
        k2lg_n_pending_group(&checkpoint).as_deref(),
        Some("Completed"),
        "K2LG boundary N1 the matching server closure was decoded before the release"
    );
    let prompts: Vec<String> = fx
        .session
        .actor
        .state
        .lock()
        .await
        .pending_inputs
        .iter()
        .map(|input| input.prompt_id.clone())
        .collect();
    assert_eq!(
        prompts,
        vec![K2LG_N_PROMPT.to_string()],
        "K2LG boundary N1 the release opens no new prompt"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N1","closure":"Completed","continuation_http":1,"post_release_rounds":1,"native_occurrences":1,"per_session":{"agent_wide_flag_after_sender_turn":false,"receiver_prefix_resident":true},"W_TE":"U6-owned, not asserted"})
    );
    K2lgNObserved {
        fx,
        barrier,
        barrier_ops,
        turn_start_ops: turn.turn_start_ops,
        submits: turn.submits,
    }
}

async fn k2lg_n_n6_i(observed: &K2lgNObserved) {
    let ops = k2lg_n_ops(&observed.fx);
    let natives: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (op, value))| {
            op == "persist"
                && serde_json::from_value::<ConversationItem>(value.clone())
                    .is_ok_and(|item| k2lg_n_is_native(&item))
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        natives.len(),
        1,
        "K2LG boundary N6/i one persistence append for the one occurrence"
    );
    let release = natives[0];
    assert!(
        release > observed.barrier_ops,
        "K2LG boundary N6/i the native append follows closure"
    );
    // The release is one chat-state command: a repair inside it would replace history just
    // before the append, a prune just after it.
    assert_ne!(
        ops[release - 1].0,
        "replace",
        "K2LG boundary N6/i no integrity repair precedes the native append"
    );
    assert!(
        ops.get(release + 1).is_none_or(|(op, _)| op != "replace"),
        "K2LG boundary N6/i no prune replacement follows the native append"
    );
    let appended: ConversationItem = serde_json::from_value(ops[release].1.clone()).unwrap();
    assert_eq!(
        k2lg_n_natives(std::slice::from_ref(&appended)),
        vec![k2lg_n_native_text(
            "K2LG N native body during the live handoff"
        )],
        "K2LG boundary N6/i the native prefix is appended unchanged"
    );
    let between = &ops[observed.barrier_ops..release];
    assert!(
        between
            .iter()
            .all(|(op, _)| op == "persist" || op == "native"),
        "K2LG boundary N6/i no history replacement between closure and the release"
    );
    let closure_appends = between.iter().filter(|(op, _)| op == "persist").count();
    let live = observed
        .fx
        .session
        .actor
        .chat_state_handle
        .get_conversation()
        .await;
    assert_eq!(
        live.iter().position(k2lg_n_is_native),
        Some(observed.barrier.len() + closure_appends),
        "K2LG boundary N6/i the native item lands exactly where a plain append lands"
    );
    let names: Vec<&str> = ops[observed.barrier_ops..]
        .iter()
        .map(|(op, _)| op.as_str())
        .collect();
    let revisions: Vec<Value> = observed
        .submits
        .borrow()
        .iter()
        .map(|submit| json!({"history_revision":submit["history_revision"],"persistence_ops":submit["persistence_ops"]}))
        .collect();
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/i","release_op_index":release,"closure_appends":closure_appends,"ops_after_closure":names,"plain_append_equivalent":true,"native_persists":1,"repair_before":false,"prune_after":false,"submit_revisions":revisions})
    );
}

async fn k2lg_n_n6_iv(observed: &K2lgNObserved) {
    let appended: Vec<Value> = k2lg_n_ops(&observed.fx)[observed.turn_start_ops..]
        .iter()
        .filter(|(op, _)| op == "persist")
        .map(|(_, value)| value.clone())
        .collect();
    let capture = observed
        .fx
        .session
        .actor
        .chat_state_handle
        .take_turn_messages()
        .await
        .expect("K2LG boundary N6/iv the active turn capture is present");
    assert!(
        !capture.compaction_occurred,
        "K2LG boundary N6/iv no compaction occurred in the turn"
    );
    assert_eq!(
        k2lg_n_values(&capture.messages),
        appended,
        "K2LG boundary N6/iv pre_replacement_messages plus tail equal the turn's ordered content"
    );
    assert_eq!(
        k2lg_n_natives(&capture.messages).len(),
        1,
        "K2LG boundary N6/iv the capture holds the native occurrence once"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/iv","observer":"TakeTurnMessages (consuming, last read)","captured_items":capture.messages.len(),"turn_appends":appended.len()})
    );
}

async fn k2lg_n_turn_end() {
    const FIRST: &str = "K2LG N turn-end first";
    const SECOND: &str = "K2LG N turn-end second";
    let mut fx = k2lg_n_fixture(K2lgNOptions {
        max_turns: Some(1),
        ..Default::default()
    })
    .await;
    let mut turn = k2lg_n_handoff(&mut fx, None).await;
    k2lg_n_send(&mut fx, FIRST).await;
    k2lg_n_send(&mut fx, SECOND).await;
    fx.session.releases[1].add_permits(1);
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("K2LG boundary N1 turn-end leg the turn returns at its max-turns limit")
        .unwrap();
    assert!(
        matches!(outcome, Ok(TurnOutcome::MaxTurnsReached { .. })),
        "K2LG boundary N1 turn-end leg the turn ends with K2's continuation still owed"
    );
    assert_eq!(
        k2lg_n_natives(&k2lg_n_resident(&fx.session)),
        vec![k2lg_n_native_text(FIRST), k2lg_n_native_text(SECOND)],
        "K2LG boundary N1 turn-end leg the prefix is resident when the turn returns"
    );
    assert!(
        k2lg_n_natives(&fx.session.actor.chat_state_handle.get_conversation().await).is_empty(),
        "K2LG boundary N1 turn-end leg no native item reached history during the turn"
    );
    assert_eq!(fx.server.messages_request_count(), 2);
    let (checkpoint, _) = fx.session.reload().await;
    assert!(
        k2lg_n_live(&checkpoint),
        "K2LG boundary N1 turn-end leg the exchange is still LiveBound with a LivePending group"
    );
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let ops_before = k2lg_n_ops(&fx).len();
    let owned = k2lg_n_selected(
        &fx,
        fx.session.actor.handle_completion(
            K2LG_N_PROMPT.into(),
            turn.epoch,
            &turn.identity,
            Ok(PromptTurnOk {
                stop_reason: acp::StopReason::MaxTurnRequests,
                total_tokens: 0,
                turn_snapshot: None,
                completion_kind: PromptCompletionKind::MaxTurnsReached { limit: 1 },
                structured_output: None,
                usage: None,
                tool_overrides: None,
            }),
            Some(0),
        ),
    )
    .await;
    assert!(owned, "K2LG setup the turn-end completion is owned");
    let after = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        k2lg_n_natives(&after),
        vec![k2lg_n_native_text(FIRST), k2lg_n_native_text(SECOND)],
        "K2LG boundary N1 turn-end leg the turn-end flush releases the prefix in resident order"
    );
    assert!(
        k2lg_n_resident(&fx.session).is_empty(),
        "K2LG boundary N1 turn-end leg the released prefix leaves the resident buffer"
    );
    assert_eq!(
        k2lg_n_native_persists(&k2lg_n_ops(&fx)[ops_before..]),
        2,
        "K2LG boundary N1 turn-end leg one persistence append per occurrence"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N1/turn-end","site":"turn_end.rs:220 handle_completion","session_turn_active":false,"exchange_live_at_release":true,"released":2})
    );
}

async fn k2lg_n_n2() {
    const BODY: &str = "K2LG N equal body";
    const USER: &str = "K2LG N ordinary user after native";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    let mut turn = k2lg_n_handoff(&mut fx, None).await;
    k2lg_n_send(&mut fx, BODY).await;
    k2lg_n_send(&mut fx, BODY).await;
    assert_eq!(
        k2lg_n_natives(&k2lg_n_resident(&fx.session)),
        vec![k2lg_n_native_text(BODY), k2lg_n_native_text(BODY)],
        "K2LG boundary N2 two equal-body occurrences stay two resident items"
    );
    let mut held = fx.server.expect_response_blocked(
        "K2LG N2 continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    tokio::time::timeout(K0_TIMEOUT, held.wait_blocked())
        .await
        .expect("K2LG boundary N2 the continuation reaches HTTP with both occurrences resident");
    assert_eq!(k2lg_n_resident(&fx.session).len(), 2);
    fx.loop_tx
        .send(SessionCommand::Interject {
            text: USER.into(),
            id: None,
            images: Vec::new(),
        })
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, async {
        while !fx
            .session
            .actor
            .pending_interjections
            .snapshot()
            .iter()
            .any(|entry| entry.text == USER)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("K2LG setup the ordinary user input is queued through the real run loop");
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_k2lg_n2_after")),
    );
    held.release();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("K2LG boundary N2 the turn finishes after closure")
        .unwrap();
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        outcome.is_ok(),
        "K2LG boundary N2 the release completes the same turn: {:?}",
        outcome.err()
    );
    let after = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        k2lg_n_natives(&after),
        vec![k2lg_n_native_text(BODY), k2lg_n_native_text(BODY)],
        "K2LG boundary N2 two logical occurrences, no text dedup and no repeat"
    );
    let first = after.iter().position(k2lg_n_is_native).unwrap();
    let user = after
        .iter()
        .position(|item| {
            matches!(item, ConversationItem::User(_)) && item.text_content().contains(USER)
        })
        .expect("K2LG boundary N2 the ordinary user input is delivered");
    assert!(
        k2lg_n_is_native(&after[first + 1]) && first + 2 == user,
        "K2LG boundary N2 the native prefix is released in resident order before the ordinary user batch"
    );
    assert_eq!(
        k2lg_n_native_persists(&k2lg_n_ops(&fx)),
        2,
        "K2LG boundary N2 one persistence append per logical occurrence"
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4);
    assert_eq!(
        k2lg_n_text_count(&bodies[3], BODY),
        2,
        "K2LG boundary N2 the next model round carries both occurrences once each"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N2","native_occurrences":2,"order":"native,native,user","persist_appends":2})
    );
}

async fn k2lg_n_n3() {
    const BODY: &str = "K2LG N closed mailbox";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    k2lg_n_stub_running(&mut fx).await;
    k2lg_n_send(&mut fx, BODY).await;
    let _ = fx.session.actor.chat_state_handle.get_conversation().await;
    let disk_before = k2lg_n_values(&k2lg_n_disk(&fx.session).await);
    let ops_before = k2lg_n_ops(&fx).len();
    k2lg_n_close_mailbox(&fx).await;
    let read = fx
        .session
        .actor
        .chat_state_handle
        .read_native_history()
        .await;
    assert!(
        matches!(read, Err(k2_history::NativeUnavailable::Persistence)),
        "K2LG setup the closed receiver is first met at read_native_history"
    );
    for attempt in 0..2 {
        let drained =
            k2lg_n_selected(&fx, fx.session.actor.drain_interjections_at_safe_point()).await;
        assert!(
            !drained,
            "K2LG boundary N3 a failed fallible enqueue reports no delivery (attempt {attempt})"
        );
        assert_eq!(
            k2lg_n_natives(&k2lg_n_resident(&fx.session)),
            vec![k2lg_n_native_text(BODY)],
            "K2LG boundary N3 the original prefix stays resident after a closed-mailbox failure (attempt {attempt})"
        );
    }
    k2lg_n_selected(&fx, fx.session.actor.flush_pending_skill_reminders()).await;
    assert_eq!(
        k2lg_n_natives(&k2lg_n_resident(&fx.session)),
        vec![k2lg_n_native_text(BODY)],
        "K2LG boundary N3 the second flush site is lossless too"
    );
    assert_eq!(
        k2lg_n_ops(&fx).len(),
        ops_before,
        "K2LG boundary N3 no persistence append on a closed mailbox"
    );
    assert_eq!(
        k2lg_n_values(&k2lg_n_disk(&fx.session).await),
        disk_before,
        "K2LG boundary N3 no native history reaches disk"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N3","first_met":"read_native_history Err(Persistence)","enqueue":"Err(ChatStateMailboxClosed)","resident_after_failures":1,"delivery_reported":false})
    );
}

async fn k2lg_n_n4() {
    const FIRST: &str = "K2LG N first prefix";
    const LATER: &str = "K2LG N later suffix";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    k2lg_n_stub_running(&mut fx).await;
    k2lg_n_send(&mut fx, FIRST).await;
    let drained = k2lg_n_selected(&fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    assert!(
        drained,
        "K2LG boundary N4 the first prefix is enqueued successfully"
    );
    assert!(
        k2lg_n_resident(&fx.session).is_empty(),
        "K2LG boundary N4 the successfully enqueued prefix is removed once"
    );
    assert_eq!(
        k2lg_n_natives(&fx.session.actor.chat_state_handle.get_conversation().await),
        vec![k2lg_n_native_text(FIRST)]
    );
    k2lg_n_send(&mut fx, LATER).await;
    k2lg_n_close_mailbox(&fx).await;
    let drained = k2lg_n_selected(&fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    assert!(
        !drained,
        "K2LG boundary N4 the later release failure reports no delivery"
    );
    assert_eq!(
        k2lg_n_natives(&k2lg_n_resident(&fx.session)),
        vec![k2lg_n_native_text(LATER)],
        "K2LG boundary N4 only the unsent suffix remains eligible; the accepted prefix never reappears"
    );
    assert_eq!(
        k2lg_n_natives(&k2lg_n_disk(&fx.session).await),
        vec![k2lg_n_native_text(FIRST)],
        "K2LG boundary N4 the accepted prefix is persisted exactly once"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N4","accepted_prefix":1,"later_failure":"closed mailbox","resident_after":"unsent suffix only","constructed_defensive":false})
    );
}

async fn k2lg_n_n5() {
    const BODY: &str = "K2LG N retried transport";
    let mut fx = k2lg_n_fixture(K2lgNOptions {
        retries: 2,
        ..Default::default()
    })
    .await;
    let mut turn = k2lg_n_handoff(&mut fx, None).await;
    k2lg_n_send(&mut fx, BODY).await;
    let mut held = fx.server.expect_response_blocked(
        "K2LG N5 continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    tokio::time::timeout(K0_TIMEOUT, held.wait_blocked())
        .await
        .expect("K2LG boundary N5 the continuation reaches HTTP with the native prefix resident");
    let mut throttled = ScriptedResponse::json(
        429,
        json!({"type":"error","error":{"type":"rate_limit_error","message":"K2LG N transient rate limit"}}),
    );
    throttled.headers.push(("retry-after".into(), "1".into()));
    fx.server.enqueue_response("/v1/messages", throttled);
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_k2lg_n5_after")),
    );
    held.release();
    let outcome = tokio::time::timeout(Duration::from_secs(90), &mut turn.running)
        .await
        .expect("K2LG boundary N5 the turn finishes after the transport retry")
        .unwrap();
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        outcome.is_ok(),
        "K2LG boundary N5 the retried round completes: {:?}",
        outcome.err()
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(
        bodies.len(),
        5,
        "K2LG boundary N5 the post-release request is retried once at the transport"
    );
    assert_eq!(
        bodies[3], bodies[4],
        "K2LG boundary N5 the retry resends identical request bytes"
    );
    assert_eq!(k2lg_n_text_count(&bodies[4], BODY), 1);
    assert_eq!(
        k2lg_n_natives(&fx.session.actor.chat_state_handle.get_conversation().await),
        vec![k2lg_n_native_text(BODY)],
        "K2LG boundary N5 the retry never readmits or appends the native message again"
    );
    assert_eq!(
        k2lg_n_native_persists(&k2lg_n_ops(&fx)),
        1,
        "K2LG boundary N5 one persistence append despite the transport retry"
    );
    assert!(k2lg_n_resident(&fx.session).is_empty());
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N5","http_attempts_for_round":2,"identical_bytes":true,"native_appends":1,"native_persists":1})
    );
}

/// Shared tail of the N6/ii routes: the first subsequent ordinary push, then the live
/// history and the disk read-back.
async fn k2lg_n_ordinary_push(
    fx: &K2lgN,
    text: &str,
) -> (Vec<ConversationItem>, Vec<ConversationItem>) {
    fx.session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user(text))
        .await;
    let live = fx.session.actor.chat_state_handle.get_conversation().await;
    (live, k2lg_n_disk(&fx.session).await)
}

type K2lgNRoute = (
    Vec<ConversationItem>,
    Vec<ConversationItem>,
    Vec<(String, Value)>,
);

fn k2lg_n_compare(native: &[ConversationItem], control: &[ConversationItem], route: &str) -> usize {
    let positions: Vec<usize> = native
        .iter()
        .enumerate()
        .filter(|(_, item)| k2lg_n_is_native(item))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        positions.len(),
        1,
        "K2LG boundary N6/ii {route}: one native occurrence"
    );
    let mut stripped = native.to_vec();
    stripped.remove(positions[0]);
    assert_eq!(
        k2lg_n_values(&stripped),
        k2lg_n_values(control),
        "K2LG boundary N6/ii {route}: the native release adds no repair, prune, rewrite or duplicate relative to the matched control"
    );
    assert_eq!(
        positions[0],
        control.len() - 1,
        "K2LG boundary N6/ii {route}: the native prefix sits immediately before the first subsequent ordinary push"
    );
    positions[0]
}

async fn k2lg_n_cancel_client(native: bool) -> K2lgNRoute {
    const BODY: &str = "K2LG N cancel during client execution";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    let mut turn = k2lg_n_handoff(&mut fx, None).await;
    if native {
        k2lg_n_send(&mut fx, BODY).await;
    }
    let ops_before = k2lg_n_ops(&fx).len();
    let _cancel = k2lg_n_selected(
        &fx,
        fx.session
            .actor
            .cancel_running_task(crate::session::CancelOptions::default()),
    )
    .await;
    let joined = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("K2LG boundary N6/ii command-Cancel ends the turn under test");
    assert!(
        matches!(joined, Err(ref error) if error.is_cancelled()),
        "K2LG boundary N6/ii command-Cancel aborts the turn under test"
    );
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    if native {
        assert!(
            k2lg_n_resident(&fx.session).is_empty(),
            "K2LG boundary N6/ii the command-Cancel flush releases the resident prefix"
        );
    }
    let (live, disk) = k2lg_n_ordinary_push(&fx, "K2LG N first ordinary push after cancel").await;
    (live, disk, k2lg_n_ops(&fx)[ops_before..].to_vec())
}

async fn k2lg_n_cancel_results(native: bool) -> K2lgNRoute {
    const BODY: &str = "K2LG N cancel after genuine results";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    let mut turn = k2lg_n_handoff(&mut fx, None).await;
    if native {
        k2lg_n_send(&mut fx, BODY).await;
    }
    let mut held = fx.server.expect_response_blocked(
        "K2LG N6 cancel-after-results continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    tokio::time::timeout(K0_TIMEOUT, held.wait_blocked())
        .await
        .expect("K2LG boundary N6/ii cancel-after-results the continuation reaches HTTP with the genuine results in history");
    if native {
        assert_eq!(
            k2lg_n_resident(&fx.session).len(),
            1,
            "K2LG boundary N6/ii cancel-after-results the prefix is still resident"
        );
    }
    let ops_before = k2lg_n_ops(&fx).len();
    let _cancel = k2lg_n_selected(
        &fx,
        fx.session
            .actor
            .cancel_running_task(crate::session::CancelOptions::default()),
    )
    .await;
    held.release();
    let joined = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("K2LG boundary N6/ii command-Cancel ends the turn under test");
    assert!(
        matches!(joined, Err(ref error) if error.is_cancelled()),
        "K2LG boundary N6/ii command-Cancel aborts the turn under test"
    );
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    if native {
        assert!(
            k2lg_n_resident(&fx.session).is_empty(),
            "K2LG boundary N6/ii the command-Cancel flush releases the resident prefix"
        );
    }
    let (live, disk) = k2lg_n_ordinary_push(&fx, "K2LG N first ordinary push after cancel").await;
    (live, disk, k2lg_n_ops(&fx)[ops_before..].to_vec())
}

async fn k2lg_n_error(native: bool) -> K2lgNRoute {
    const BODY: &str = "K2LG N error route";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    let mut turn = k2lg_n_handoff(
        &mut fx,
        Some(ScriptedResponse::json(
            400,
            json!({"type":"error","error":{"type":"invalid_request_error","message":"K2LG N offline continuation failure"}}),
        )),
    )
    .await;
    if native {
        k2lg_n_send(&mut fx, BODY).await;
    }
    fx.session.releases[1].add_permits(1);
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("K2LG boundary N6/ii error route the turn returns")
        .unwrap();
    let error = outcome.err().expect(
        "K2LG boundary N6/ii error route the continuation failure ends the turn with an error",
    );
    assert_eq!(
        fx.server.messages_request_count(),
        3,
        "K2LG boundary N6/ii error route the ordinary continuation attempt is unchanged by the native message"
    );
    if native {
        assert_eq!(
            k2lg_n_resident(&fx.session).len(),
            1,
            "K2LG boundary N6/ii error route the prefix is still resident when the turn ends"
        );
    }
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let ops_before = k2lg_n_ops(&fx).len();
    let owned = k2lg_n_selected(
        &fx,
        fx.session.actor.handle_completion(
            K2LG_N_PROMPT.into(),
            turn.epoch,
            &turn.identity,
            Err(error),
            Some(0),
        ),
    )
    .await;
    assert!(owned, "K2LG setup the error completion is owned");
    if native {
        assert!(
            k2lg_n_resident(&fx.session).is_empty(),
            "K2LG boundary N6/ii error route the turn-end flush releases the prefix"
        );
    }
    let (live, disk) =
        k2lg_n_ordinary_push(&fx, "K2LG N first ordinary push after the error").await;
    (live, disk, k2lg_n_ops(&fx)[ops_before..].to_vec())
}

async fn k2lg_n_n6_cancel_client() {
    let (native, native_disk, ops) = k2lg_n_cancel_client(true).await;
    let (control, control_disk, _) = k2lg_n_cancel_client(false).await;
    let position = k2lg_n_compare(&native, &control, "command-Cancel during client execution");
    k2lg_n_compare(
        &native_disk,
        &control_disk,
        "command-Cancel during client execution (disk)",
    );
    assert!(
        matches!(&native[position - 1], ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c2"),
        "K2LG boundary N6/ii the cancel's eager repair of the dangling waiting call precedes the native release"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/ii-cancel-during-client","site":"cancel.rs:799 after eager repair cancel.rs:718-726","matched_control_equal_after_first_ordinary_push":true,"ops":ops.iter().map(|(op, _)| op.as_str()).collect::<Vec<_>>()})
    );
}

async fn k2lg_n_n6_cancel_results() {
    let (native, native_disk, ops) = k2lg_n_cancel_results(true).await;
    let (control, control_disk, _) = k2lg_n_cancel_results(false).await;
    let position = k2lg_n_compare(&native, &control, "command-Cancel after genuine results");
    k2lg_n_compare(
        &native_disk,
        &control_disk,
        "command-Cancel after genuine results (disk)",
    );
    assert!(
        matches!(&native[position - 1], ConversationItem::ToolResult(result)
            if result.tool_call_id == "client_k2_c2" && result.content.contains("actual result")),
        "K2LG boundary N6/ii a cancel after the genuine results leaves nothing to repair"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/ii-cancel-after-results","site":"cancel.rs:799","repair":"none needed","matched_control_equal_after_first_ordinary_push":true,"ops":ops.iter().map(|(op, _)| op.as_str()).collect::<Vec<_>>()})
    );
}

async fn k2lg_n_n6_error() {
    let (native, native_disk, ops) = k2lg_n_error(true).await;
    let (control, control_disk, control_ops) = k2lg_n_error(false).await;
    k2lg_n_compare(&native, &control, "error route");
    k2lg_n_compare(&native_disk, &control_disk, "error route (disk)");
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/ii-error","site":"turn_end.rs:220 after a continuation error","matched_control_equal_after_first_ordinary_push":true,"native_ops":ops.iter().map(|(op, _)| op.as_str()).collect::<Vec<_>>(),"control_ops":control_ops.iter().map(|(op, _)| op.as_str()).collect::<Vec<_>>()})
    );
}

async fn k2lg_n_n6_prune() {
    const BODY: &str = "K2LG N prune eligibility";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    for index in 0..13 {
        fx.server.enqueue_response(
            "/v1/messages",
            k2_script(k2_frames(
                &format!("msg_k2lg_n_ordinary_{index}"),
                vec![(
                    json!({"type":"tool_use","id":format!("k2lg_n_ordinary_{index}"),"name":"alpha__lookup","input":{"value":format!("ordinary {index}")}}),
                    Vec::new(),
                )],
                "tool_use",
            )),
        );
        fx.server.enqueue_response(
            "/v1/messages",
            k2_script(k2_frames(
                &format!("msg_k2lg_n_ordinary_{index}_done"),
                vec![(
                    json!({"type":"text","text":"ordinary turn done"}),
                    Vec::new(),
                )],
                "end_turn",
            )),
        );
        fx.session.actor.chat_state_handle.increment_prompt_index();
        fx.session
            .turn(
                k2_history::NativeHistoryRequirement::Inactive,
                &fx.server,
                &format!("K2LG N ordinary prompt {index}"),
            )
            .await
            .expect("K2LG setup ordinary seeded turn");
    }
    let cleared = |items: &[ConversationItem]| -> Vec<String> {
        items
            .iter()
            .filter_map(|item| match item {
                ConversationItem::ToolResult(result)
                    if result.tool_call_id.starts_with("k2lg_n_ordinary_")
                        && !result.content.contains("actual result") =>
                {
                    Some(result.tool_call_id.clone())
                }
                _ => None,
            })
            .collect()
    };
    let seeded = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        !cleared(&seeded).is_empty(),
        "K2LG setup the seeded history reached real prune eligibility"
    );
    k2lg_n_stub_running(&mut fx).await;
    k2lg_n_send(&mut fx, BODY).await;
    let before = fx.session.actor.chat_state_handle.get_conversation().await;
    let ops_before = k2lg_n_ops(&fx).len();
    let drained = k2lg_n_selected(&fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    assert!(
        drained,
        "K2LG boundary N6/iii the native prefix is released"
    );
    let after = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        k2lg_n_values(&after[..before.len()]),
        k2lg_n_values(&before),
        "K2LG boundary N6/iii the native release prunes or rewrites nothing"
    );
    assert_eq!(after.len(), before.len() + 1);
    assert!(k2lg_n_is_native(&after[before.len()]));
    let ops: Vec<(String, Value)> = k2lg_n_ops(&fx)[ops_before..]
        .iter()
        .filter(|(op, _)| op != "native")
        .cloned()
        .collect();
    assert_eq!(
        (ops.len(), k2lg_n_native_persists(&ops)),
        (1, 1),
        "K2LG boundary N6/iii the release is exactly one append with no prune replacement"
    );
    fx.session.actor.chat_state_handle.increment_prompt_index();
    fx.session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user(
            "K2LG N first ordinary push after native",
        ))
        .await;
    let pushed = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        cleared(&pushed).len() > cleared(&after).len(),
        "K2LG setup the boundary result was eligible for the next real prompt"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/iii","seeded_turns":13,"cleared_before":cleared(&seeded).len(),"cleared_after_native":cleared(&after).len(),"cleared_after_next_prompt":cleared(&pushed).len()})
    );
}

async fn k2lg_n_inactive() {
    const BODY: &str = "K2LG N inactive plain flush";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    k2lg_n_stub_running(&mut fx).await;
    k2lg_n_send(&mut fx, BODY).await;
    let ops_before = k2lg_n_ops(&fx).len();
    let drained = fx.session.actor.drain_interjections_at_safe_point().await;
    assert!(
        drained,
        "K2LG boundary N6 Inactive keeps the plain flush and reports native activity"
    );
    assert!(k2lg_n_resident(&fx.session).is_empty());
    assert_eq!(
        k2lg_n_natives(&fx.session.actor.chat_state_handle.get_conversation().await),
        vec![k2lg_n_native_text(BODY)]
    );
    assert_eq!(k2lg_n_native_persists(&k2lg_n_ops(&fx)[ops_before..]), 1);
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/inactive-no-plan","path":"take + push_tool_result","native_appends":1})
    );
}

async fn k2lg_n_arm(label: &str, arm: impl std::future::Future<Output = ()>) -> bool {
    use futures_util::FutureExt;
    match std::panic::AssertUnwindSafe(arm).catch_unwind().await {
        Ok(()) => {
            println!("\nK2LG_N_ARM {}", json!({"label":label,"passed":true}));
            true
        }
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("");
            if !message.contains("K2LG boundary") {
                std::panic::resume_unwind(payload);
            }
            println!(
                "\nK2LG_N_ARM {}",
                json!({"label":label,"passed":false,"assertion":message})
            );
            false
        }
    }
}

fn k2lg_n_compiled_sources() -> Value {
    json!({
        "session_setup": native_content_digest(include_bytes!("../acp_session_impl/session_setup.rs")),
        "interjection": native_content_digest(include_bytes!("../acp_session_impl/interjection.rs")),
        "fixture": native_content_digest(include_bytes!("native_messages_replay_tests.rs")),
    })
}

async fn k2lg_n_cases() {
    println!("\nK2LG_N_BUILD {}", k2lg_n_compiled_sources());
    let mut failures = Vec::new();
    let mut n1 = None;
    if !k2lg_n_arm("N1/live-retain-per-session-closure-release", async {
        n1 = Some(k2lg_n_n1().await);
    })
    .await
    {
        failures.push("N1/live-retain-per-session-closure-release");
    }
    if !k2lg_n_arm("N6/i-post-closure-mid-turn", async {
        k2lg_n_n6_i(
            n1.as_ref()
                .expect("K2LG boundary N6/i needs the completed N1 run"),
        )
        .await;
    })
    .await
    {
        failures.push("N6/i-post-closure-mid-turn");
    }
    if !k2lg_n_arm("N6/iv-active-turn-capture", async {
        k2lg_n_n6_iv(
            n1.as_ref()
                .expect("K2LG boundary N6/iv needs the completed N1 run"),
        )
        .await;
    })
    .await
    {
        failures.push("N6/iv-active-turn-capture");
    }
    drop(n1);
    if !k2lg_n_arm("N1/turn-end-leg", k2lg_n_turn_end()).await {
        failures.push("N1/turn-end-leg");
    }
    if !k2lg_n_arm("N2/equal-body-order", k2lg_n_n2()).await {
        failures.push("N2/equal-body-order");
    }
    if !k2lg_n_arm("N3/closed-mailbox", k2lg_n_n3()).await {
        failures.push("N3/closed-mailbox");
    }
    if !k2lg_n_arm("N4/success-prefix-then-failure", k2lg_n_n4()).await {
        failures.push("N4/success-prefix-then-failure");
    }
    if !k2lg_n_arm("N5/transport-retry", k2lg_n_n5()).await {
        failures.push("N5/transport-retry");
    }
    if !k2lg_n_arm(
        "N6/ii-command-cancel-during-client",
        k2lg_n_n6_cancel_client(),
    )
    .await
    {
        failures.push("N6/ii-command-cancel-during-client");
    }
    if !k2lg_n_arm(
        "N6/ii-command-cancel-after-results",
        k2lg_n_n6_cancel_results(),
    )
    .await
    {
        failures.push("N6/ii-command-cancel-after-results");
    }
    if !k2lg_n_arm("N6/ii-error-route", k2lg_n_n6_error()).await {
        failures.push("N6/ii-error-route");
    }
    if !k2lg_n_arm("N6/iii-prune-eligibility", k2lg_n_n6_prune()).await {
        failures.push("N6/iii-prune-eligibility");
    }
    if !k2lg_n_arm("N6/inactive-no-plan", k2lg_n_inactive()).await {
        failures.push("N6/inactive-no-plan");
    }
    assert!(
        failures.is_empty(),
        "K2LG boundary failed native arms: {failures:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn messages_native_agent_grammar_uses_real_mailbox_producer() {
    i7yx_report_child_names();
    if std::env::var_os("K2LG_N_CHILD").is_some() {
        tokio::task::LocalSet::new().run_until(k2lg_n_cases()).await;
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (mut command, parent_names) =
        i7yx_shell_child_command(I7yxChildRoute::NativeN, home.path());
    command
        .args(["--exact", K2LG_N_SELECTOR, "--nocapture"])
        .stdin(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut command);
    let output = tokio::task::spawn_blocking(move || command.output())
        .await
        .unwrap()
        .unwrap();
    i7yx_assert_child_hygiene(I7yxChildRoute::NativeN, &output.stdout, &parent_names);
    let stdout = String::from_utf8_lossy(&output.stdout);
    i7yx_print_proof_records(&output.stdout, I7yxChildRoute::NativeN);
    assert!(
        output.status.success(),
        "K2LG boundary native child failed: {}",
        i7yx_output_diagnostic(I7yxChildRoute::NativeN, &output)
    );
    assert!(stdout.contains("running 1 test"));
}


const KOXD_U6_SELECTOR: &str = "session::acp_session::native_messages_replay_tests::messages_pending_holds_harness_user_content";
const KOXD_U6_CHILD: &str = "KOXD_U6A_CHILD";
const KOXD_U6_PROMPT: &str = "KOXD-U6-live";

fn u6_png(side: u32) -> String {
    use base64::Engine;
    let image = image::RgbImage::from_fn(side, side, |x, y| {
        let value = x
            .wrapping_mul(0x45d9f3b)
            .wrapping_add(y.wrapping_mul(0x119de1f3));
        image::Rgb([value as u8, (value >> 8) as u8, (value >> 16) as u8])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
    )
}

#[derive(Clone, Debug)]
struct U6ClientTool {
    name: &'static str,
    executions: Arc<std::sync::Mutex<Vec<String>>>,
    entered: tokio::sync::mpsc::UnboundedSender<String>,
    release: Arc<tokio::sync::Semaphore>,
    image: Option<Arc<str>>,
}

impl xai_grok_tools::types::tool_metadata::ToolMetadata for U6ClientTool {
    fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
        xai_grok_tools::types::tool::ToolKind::Other
    }
    fn tool_namespace(&self) -> xai_grok_tools::types::tool::ToolNamespace {
        xai_grok_tools::types::tool::ToolNamespace::MCP
    }
    fn description_template(&self) -> &str {
        "Bounded offline U6 client tool"
    }
}

impl xai_tool_runtime::Tool for U6ClientTool {
    type Args = Value;
    type Output = xai_grok_tools::types::output::ToolOutput;
    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(self.name).unwrap()
    }
    fn description(
        &self,
        _: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(self.name, "Bounded offline U6 client tool")
    }
    async fn run(
        &self,
        _: xai_tool_runtime::ToolCallContext,
        arguments: Value,
    ) -> Result<Self::Output, xai_tool_runtime::ToolError> {
        self.executions.lock().unwrap().push(self.name.into());
        self.entered.send(self.name.into()).unwrap();
        self.release.acquire().await.unwrap().forget();
        let text = format!("{} actual result: {}", self.name, arguments["value"]);
        if let Some(image) = &self.image {
            let mut output = xai_grok_tools::types::output::MCPOutput::okay_output(
                self.name.into(),
                "u6".into(),
                text,
            );
            output
                .extracted_images
                .push(xai_grok_tools::util::base64_images::ExtractedImage {
                    data: image
                        .strip_prefix("data:image/png;base64,")
                        .unwrap()
                        .to_owned(),
                    mime_type: "image/png".into(),
                });
            Ok(xai_grok_tools::types::output::ToolOutput::MCP(output))
        } else {
            Ok(xai_grok_tools::types::output::ToolOutput::Text(text.into()))
        }
    }
}

#[derive(Clone, Default)]
struct U6PersistenceControl {
    updates: Arc<std::sync::Mutex<Vec<Value>>>,
    cleanup: Arc<tokio::sync::Notify>,
    cleanup_count: Arc<std::sync::atomic::AtomicUsize>,
    bookkeeping_feedback: bool,
    media_resample: bool,
    barriers: Arc<std::sync::atomic::AtomicUsize>,
    hold_barriers: Arc<std::sync::atomic::AtomicBool>,
    held_barriers: Arc<std::sync::Mutex<Vec<PersistenceMsg>>>,
    barrier_held: Arc<tokio::sync::Notify>,
    user_chunks: Arc<std::sync::Mutex<Vec<Value>>>,
    announcement_updates: Arc<std::sync::atomic::AtomicUsize>,
    plan_updates: Arc<std::sync::atomic::AtomicUsize>,
    plan_tools: bool,
    idle_observer: Option<std::rc::Rc<K0IdleObserver>>,
    n12_drop_next_native_ack: Arc<std::sync::atomic::AtomicBool>,
}

struct U6Fixture {
    fx: K2lgN,
    persistence_control: U6PersistenceControl,
}

async fn u6_install_running(
    fx: &mut K2lgN,
    handle: tokio::task::AbortHandle,
) -> (TurnEpoch, std::rc::Rc<()>) {
    let epoch = fx.session.actor.turn_report.start_next_turn();
    let identity = {
        let mut state = fx.session.actor.state.lock().await;
        state.running_task = Some(AgentTask::new_at_epoch(KOXD_U6_PROMPT, epoch, handle));
        state
            .pending_inputs
            .push_front(user_item(KOXD_U6_PROMPT, "U6"));
        state.running_task.as_ref().unwrap().identity.clone()
    };
    *fx.session.actor.current_prompt_id.lock().unwrap() = Some(KOXD_U6_PROMPT.into());
    if !fx.session.actor.session_turn_active.load(std::sync::atomic::Ordering::SeqCst) {
        let guard = super::turn_task::TurnActiveGuard::activate(Some(&fx.session.actor.session_turn_active));
        fx.session.tasks.push(tokio::task::spawn_local(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        }));
        u6_observe(json!({"stage":"R7 fixture-owned manual turn guard","active":true}));
    }
    if let Some(command_rx) = fx.loop_rx.take() {
        let (chat_tx, chat_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let actor = fx.session.actor.clone();
        fx.session.tasks.push(tokio::task::spawn_local(async move {
            let _keep = (chat_tx, event_tx);
            super::run_session(
                actor,
                command_rx,
                chat_rx,
                event_rx,
                None,
                Arc::new(parking_lot::Mutex::new(
                    xai_grok_workspace::file_system::CodebaseIndexManager::new(),
                )),
                std::env::temp_dir(),
                crate::session::fs_watch::FsWatchCapabilities::none(),
            )
            .await;
        }));
    }
    (epoch, identity)
}

async fn u6_monitor(fx: &K2lgN, task: &str, text: &str) {
    let buffer = fx
        .session
        .actor
        .tool_context
        .monitor_event_buffer
        .as_ref()
        .unwrap();
    let before = buffer.len();
    fx.loop_tx
        .send(SessionCommand::InjectNotification {
            prompt_id: format!("u6-monitor-{task}"),
            prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(format!(
                "<monitor-event description=\"u6\" task_id=\"{task}\">\n{text}\n</monitor-event>"
            )))],
            priority: NotificationPriority::Next,
            source: NotificationSource::MonitorEvent {
                task_id: task.into(),
            },
        })
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, async {
        while buffer.len() <= before {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("H6_MONITOR_REAL_INGRESS");
}

fn u6_body_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| {
            message["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|part| part["text"].as_str())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn u6_assert_results_only(checkpoint: &K2Checkpoint, initial: &Value, continuation: &Value) {
    let origin = checkpoint.source.producing_bindings.last().unwrap();
    let mut waiting: Vec<_> = checkpoint
        .ledger
        .lifecycle
        .waiting_clients
        .iter()
        .filter(|client| {
            client.source.producing_binding == origin.request && client.delivery == "Waiting"
        })
        .map(|client| client.client_id.as_str())
        .collect();
    waiting.sort();
    let messages = continuation["messages"].as_array().unwrap();
    let tail = messages.last().unwrap();
    let blocks = tail["content"].as_array().unwrap();
    assert!(
        tail["role"] == "user" && !blocks.is_empty(),
        "H6_RESULTS_ONLY_NONEMPTY_USER"
    );
    assert!(
        blocks.iter().all(|block| block["type"] == "tool_result"),
        "H6_RESULTS_ONLY_NO_USER_CONTENT"
    );
    let mut actual: Vec<_> = blocks
        .iter()
        .map(|block| block["tool_use_id"].as_str().unwrap())
        .collect();
    actual.sort();
    assert!(
        actual == waiting && !waiting.is_empty(),
        "H6_RESULTS_ONLY_REAL_WAITING_IDS"
    );
    assert!(
        initial["tools"] == continuation["tools"],
        "H6_ORIGINAL_FULL_TOOLS"
    );
    assert!(
        initial["model"] == continuation["model"],
        "H6_ORIGINAL_BOUND_MODEL"
    );
}

async fn u6_session(
    server: &MockInferenceServer,
    options: K2lgNOptions,
    agent_wide: &Arc<std::sync::atomic::AtomicBool>,
    chat_token: &tokio_util::sync::CancellationToken,
    log: &K2lgNLog,
    control: &U6PersistenceControl,
    image: Option<Arc<str>>,
) -> K2ReplaySession {
    let idle_observer = control.idle_observer.clone();
    let title_home = crate::util::grok_home::grok_home();
    assert!(title_home.starts_with(std::env::temp_dir()), "H6_R7_PRIVATE_TITLE_HOME");
    let title_cwd = title_home.to_string_lossy().into_owned();
    let session_parent = crate::util::grok_home::ensure_sessions_cwd_dir_in(&title_home, &title_cwd).unwrap();
    let directory = tempfile::Builder::new().prefix("k2lg-n-").tempdir_in(session_parent).unwrap();
    let config = xai_grok_sampler::SamplerConfig {
        base_url: server.url(),
        model: "k2-replay".into(),
        api_key: Some("nonsecret-k2-key".into()),
        api_backend: xai_grok_sampler::ApiBackend::Messages,
        max_completion_tokens: Some(4096),
        max_retries: Some(options.retries),
        ..Default::default()
    };
    let info = crate::session::info::Info {
        id: acp::SessionId::new(directory.path().file_name().unwrap().to_string_lossy().into_owned()),
        cwd: title_cwd,
    };
    let persistence = {
        let initial = crate::session::persistence::new_with_explicit_dir(
            &info, directory.path().to_path_buf(), acp::ModelId::new("k2-replay"),
            xai_grok_sampler::SamplingClient::new(config.clone()).unwrap(), "k2-replay".into(),
            crate::session::persistence::ExplicitSessionOpen::New { identity: None, next_trace_turn: None },
        ).await.unwrap();
        const TITLE: &str = "U6b already generated fixture session title";
        initial.tx.send(PersistenceMsg::GeneratedTitle(TITLE.into())).unwrap();
        let (respond_to, persisted) = tokio::sync::oneshot::channel();
        initial.tx.send(PersistenceMsg::FlushAndAck { respond_to }).unwrap();
        tokio::time::timeout(K0_TIMEOUT, persisted).await.unwrap().unwrap().unwrap();
        drop(initial);
        let (loaded, handle) = crate::session::persistence::load_light(
            &info, None, false, crate::session::persistence::SessionDeps {
                sampling_client: xai_grok_sampler::SamplingClient::new(config.clone()).unwrap(),
                storage_mode: crate::config::StorageMode::Local, auth_manager: None, relay_sync: None, gateway: None,
                session_summary_model: "k2-replay".into(), registry_title_sync: None,
                search_index: crate::session::storage::search::SharedSearchIndex::never_indexed(), session_kind: None,
            },
        ).await.unwrap();
        assert_eq!(loaded.summary.generated_title.as_deref(), Some(TITLE), "H6_R7_REAL_TITLED_RESUME");
        u6_observe(json!({"stage":"R7 title fixture state","loaded_title":true,"real_load_surface":"persistence::load_light"}));
        handle
    };
    let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let cleanup_control = control.clone();
    let gateway = tokio::task::spawn_local(async move {
        while let Some(message) = gateway_rx.recv().await {
            if let xai_acp_lib::AcpClientMessage::SessionNotification(arguments) = message {
                if matches!(&arguments.request.update, acp::SessionUpdate::Plan(plan)
                    if plan.entries.iter().any(|entry| entry.content == "U6 bookkeeping observer")
                        && plan.entries.iter().all(|entry| entry.status == acp::PlanEntryStatus::Completed))
                {
                    cleanup_control
                        .cleanup_count
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    cleanup_control.cleanup.notify_one();
                }
                let _ = arguments.response_tx.send(Ok(()));
            }
        }
    });
    let (persistence_tx, mut persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let real_persistence = persistence.tx.clone();
    let updates = control.updates.clone();
    let barriers = control.barriers.clone();
    let observation_control = control.clone();
    let persistence_proxy = tokio::task::spawn_local(async move {
        while let Some(message) = persistence_rx.recv().await {
            if matches!(&message, PersistenceMsg::FlushAndAck { .. }) {
                barriers.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if observation_control
                    .hold_barriers
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    observation_control
                        .held_barriers
                        .lock()
                        .unwrap()
                        .push(message);
                    observation_control.barrier_held.notify_one();
                    continue;
                }
            }
            if matches!(&message, PersistenceMsg::AnnouncementState(_)) {
                observation_control
                    .announcement_updates
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if matches!(&message, PersistenceMsg::PlanModeState(_)) {
                observation_control
                    .plan_updates
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if let PersistenceMsg::Update(SessionUpdate::Acp(notification)) = &message {
                if matches!(
                    &notification.update,
                    acp::SessionUpdate::UserMessageChunk(_)
                ) {
                    observation_control
                        .user_chunks
                        .lock()
                        .unwrap()
                        .push(serde_json::to_value(notification).unwrap());
                }
                updates
                    .lock()
                    .unwrap()
                    .push(serde_json::to_value(notification).unwrap());
            }
            if real_persistence.send(message).is_err() {
                break;
            }
        }
    });
    let (mut actor, _) = create_test_actor_with_chat_persistence(
        0,
        256_000,
        85,
        gateway_tx,
        persistence_tx.clone(),
        Box::new(xai_chat_state::NullChatPersistence),
    )
    .await;
    let sampling = actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .expect("K2LG setup chat-state sampling config");
    let (chat_event_tx, _chat_event_rx) = tokio::sync::mpsc::unbounded_channel();
    actor.chat_state_handle = xai_chat_state::ChatStateActor::spawn(
        vec![],
        sampling,
        Box::new(K2lgNPersistence {
            inner: crate::session::chat_persistence::ChannelChatPersistence::new(
                persistence_tx.clone(),
            ),
            log: log.clone(),
            n12_drop_next_native_ack: control.n12_drop_next_native_ack.clone(),
        }),
        chat_event_tx,
        chat_token.clone(),
    );
    actor.tool_context.is_turn_active = Some(agent_wide.clone());
    actor.tool_context.monitor_event_buffer = Some(MonitorEventBuffer::new());
    actor.max_turns = options.max_turns;
    actor.session_info = info;
    actor.attach_non_interactive.set(true);
    if control.media_resample {
        *actor.agent.borrow_mut() = super::support::test_agent_with_tools(vec![
            xai_grok_tools::registry::types::ToolConfig::for_tool::<
                xai_grok_tools::implementations::grok_build::image_gen::ImageGenTool,
            >(),
        ])
        .await;
    }
    if control.plan_tools {
        *actor.agent.borrow_mut() = super::support::test_agent_with_plan_tools().await;
        *actor.plan_mode.lock() =
            crate::session::plan_mode::PlanModeTracker::new(directory.path().to_path_buf());
    }
    if control.bookkeeping_feedback {
        actor.feedback_manager = Arc::new(crate::session::feedback_manager::FeedbackManager::new(
            "u6-bookkeeping",
            None,
            crate::session::feedback_manager::FeedbackManagerConfig {
                feedback_enabled: true,
                ..Default::default()
            },
        ));
        actor
            .notifications
            .gateway_enabled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    actor.max_retries = 0;
    actor.compaction.compaction_mode = xai_chat_state::CompactionMode::Summary;
    {
        let mut slot = actor.agent.borrow_mut();
        let mut policy = slot.compaction_policy().clone();
        policy.two_pass_enabled = false;
        policy.remote_compaction_v2 = false;
        policy.memory_flush_enabled = false;
        *slot = xai_grok_agent::Agent::new(
            slot.definition().clone(),
            slot.prompt_context().clone(),
            slot.system_prompt().to_string(),
            slot.tool_bridge().clone(),
            slot.reminder_policy().clone(),
            policy,
            Vec::new(),
            true,
        );
    }
    actor.memory.flush_config.enabled = false;
    actor.memory.dream_config.enabled = false;
    actor.memory.save_on_end = false;
    let (manager, models) = manager_with_entries();
    let mut entry = entry_with_search_flag("k2-replay", true);
    entry.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
    entry.api_base_url = Some(server.url());
    entry.api_key = Some("nonsecret-k2-key".into());
    manager.insert_test_entry("k2-replay", entry);
    manager.set_current_model_id(acp::ModelId::new("k2-replay"));
    actor.models_manager = manager;
    let mut binding = actor
        .chat_state_handle
        .capture_transport_binding()
        .await
        .unwrap();
    binding.config.base_url = server.url();
    binding.config.model = "k2-replay".into();
    binding.config.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
    binding.config.supports_search_tool = true;
    binding.config.max_completion_tokens = Some(4096);
    binding.config.max_retries = Some(options.retries);
    binding.credentials.api_key = Some("nonsecret-k2-key".into());
    binding.credentials.auth_type = xai_chat_state::AuthType::ApiKey;
    actor
        .chat_state_handle
        .install_transport_binding(binding)
        .await
        .unwrap();
    let executions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (entered_tx, entered) = tokio::sync::mpsc::unbounded_channel();
    let releases = [
        Arc::new(tokio::sync::Semaphore::new(16)),
        Arc::new(tokio::sync::Semaphore::new(16)),
    ];
    for (index, name) in ["alpha__lookup", "zeta__fetch"].into_iter().enumerate() {
        actor
        .agent
        .borrow()
        .tool_bridge()
        .register_mcp_tools(
            name.into(),
            U6ClientTool {
                name,
                executions: executions.clone(),
                entered: entered_tx.clone(),
                release: releases[index].clone(),
                image: if index == 1 { image.clone() } else { None },
            },
            Some(json!({"type":"object","description":"Exact K2 original schema","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false})),
        )
        .unwrap();
    }
    {
        let mut mcp = actor.mcp_state.lock().await;
        mcp.finish_init();
        mcp.complete_init();
    }
    actor
        .workspace_ops
        .bind_local_session(
            &actor.session_id_string(),
            actor.tool_context.cwd.as_path().to_path_buf(),
            actor.tool_context.hunk_tracker_handle.clone(),
            actor.agent.borrow().tool_bridge().toolset(),
            None,
        )
        .unwrap();
    let (sampler_tx, mut sampler_rx) = tokio::sync::mpsc::unbounded_channel();
    actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
        config,
        xai_grok_sampler::RetryPolicy {
            max_retries: options.retries,
            ..Default::default()
        },
        sampler_tx,
    );
    actor.turn_summary_enabled = false;
    actor.title_refresh_enabled = false;
    if let Some(idle) = idle_observer.as_ref() {
        let mut extensions = xai_agent_lifecycle::LocalExtensionRegistryBuilder::default();
        extensions.session_lifecycle_contributor(idle.clone());
        actor.extension_registry = extensions.build();
    }
    let actor = Arc::new(actor);
    let drainer = actor.clone();
    let (source_tx, source_acks) = tokio::sync::mpsc::unbounded_channel();
    let sampler = tokio::task::spawn_local(async move {
        while let Some(event) = sampler_rx.recv().await {
            let status = match &event {
                xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                    evidence: k2_history::NativeEvidenceEvent::Attempt(attempt),
                    ..
                } => Some(attempt.status),
                _ => None,
            };
            drainer.handle_sampling_event(event).await;
            if let Some(status) = status {
                let _ = source_tx.send(status);
            }
        }
    });
    K2ReplaySession {
        actor,
        persistence,
        directory,
        _models: models,
        tasks: vec![gateway, persistence_proxy, sampler],
        executions,
        entered,
        releases,
        source_acks,
    }
}

async fn u6_fixture_with_bookkeeping(
    image: Option<Arc<str>>,
    options: K2lgNOptions,
    bookkeeping_feedback: bool,
    media_resample: bool,
) -> U6Fixture {
    u6_fixture_controlled(
        image,
        options,
        U6PersistenceControl {
            bookkeeping_feedback,
            media_resample,
            ..Default::default()
        },
    )
    .await
}

async fn u6_fixture_controlled(
    image: Option<Arc<str>>,
    options: K2lgNOptions,
    persistence_control: U6PersistenceControl,
) -> U6Fixture {
    let server = MockInferenceServer::start().await.unwrap();
    let agent_wide = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let chat_token = tokio_util::sync::CancellationToken::new();
    let log = K2lgNLog::default();
    let mut session = u6_session(
        &server,
        options,
        &agent_wide,
        &chat_token,
        &log,
        &persistence_control,
        image,
    )
    .await;
    let (loop_tx, loop_rx) = tokio::sync::mpsc::unbounded_channel();
    let started = std::rc::Rc::new(tokio::sync::Notify::new());
    let (sender, receiver) = k0_coordinator::SubagentCoordinator::<K2lgNRunner>::channel();
    session.tasks.push(tokio::task::spawn_local(
        k0_coordinator::SubagentCoordinator::from_channel(
            receiver,
            K2lgNRunner {
                commands: loop_tx.clone(),
                started: started.clone(),
            },
            Default::default(),
        )
        .run(),
    ));
    let root = k2lg_n_root(&server, sender).await;
    k2lg_n_register_worker(&root, &started).await;
    U6Fixture {
        fx: K2lgN {
            session,
            server,
            root,
            loop_tx,
            loop_rx: Some(loop_rx),
            log,
            chat_token,
            agent_wide,
            calls: 0,
        },
        persistence_control,
    }
}

async fn u6_handoff_with_salvage(
    fx: &mut K2lgN,
    budget: Option<u32>,
    schema: Option<Value>,
    wait_timeout: Option<u64>,
    stepper: Option<U6TurnStepper>,
) -> K2lgNTurn {
    let stepper = Some(stepper.unwrap_or_default());
    U6_ARM_STEPPERS.with(|steppers| {
        steppers
            .borrow_mut()
            .push(stepper.as_ref().unwrap().clone())
    });
    k2_seed(&mut fx.session, &fx.server).await;
    fx.session.releases[1]
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap()
        .forget();
    let frames = wait_timeout.map_or_else(k2_u5_frames, |timeout| {
        u6_wait_frames(k2_u5_frames(), "client_u6_wait", timeout)
    });
    fx.server
        .enqueue_response("/v1/messages", k2_script(frames));
    fx.session.actor.chat_state_handle.begin_turn_capture();
    let _ = fx.session.actor.chat_state_handle.get_conversation().await;
    let turn_start_ops = fx.log.lock().unwrap().len();
    fx.session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("U6 real mixed handoff"))
        .await;
    let submits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let record = submits.clone();
    let actor = fx.session.actor.clone();
    let domain = fx.session.domain(&fx.server);
    let observations = U6_ARM_OBSERVATIONS.with(Clone::clone);
    let session_turn_active_guard =
        super::turn_task::TurnActiveGuard::activate(Some(&actor.session_turn_active));
    let mut running = tokio::task::spawn_local(async move {
        let _session_turn_active_guard = session_turn_active_guard;
        let mut prepare =
            move |request: &mut ConversationRequest,
                  binding: &xai_chat_state::RequestTransportBinding| {
                assert!(record.borrow().len() < 16, "H6_REQUEST_BUDGET");
                record.borrow_mut().push(json!({
            "items": serde_json::to_value(&request.items).unwrap(),
            "tools": serde_json::to_value(&request.tools).unwrap(),
            "model": request.model,
            "bound_model": binding.config.model,
            "body_digest": request.encoded.as_ref().map(|body| native_content_digest(body.as_bytes())),
        }));
                Ok(())
            };
        let mut salvage = super::length_salvage::LengthSalvage::new(budget);
        let mut sampling = Default::default();
        let future = super::switch_projection::HISTORY_S3_TEST_DOMAIN.scope(
            domain,
            super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
                k2_history::NativeHistoryRequirement::Selected,
                actor.process_conversation_turn_for_native_history_test(
                    KOXD_U6_PROMPT,
                    None,
                    None,
                    schema,
                    &mut salvage,
                    &mut sampling,
                    &mut prepare,
                ),
            ),
        );
        U6_ARM_OBSERVATIONS.scope(observations, async {
            let outcome = U6SteppedTurn {
                future: Box::pin(future),
                stepper,
            }.await;
            if let Err(error) = &outcome {
                u6_observe(json!({
                    "stage": "direct turn error outcome",
                    "error_code": error.data.as_ref().and_then(|data| data.get("code")).and_then(Value::as_str),
                    "data_keys": error.data.as_ref().and_then(Value::as_object).map(|data| data.keys().cloned().collect::<Vec<_>>()),
                    "message_digest": native_content_digest(error.message.as_bytes()),
                    "local_message_digest": error.data.as_ref().and_then(|data| data.get("message")).and_then(Value::as_str).map(|message| native_content_digest(message.as_bytes())),
                }));
            }
            outcome
        }).await
    });
    let (epoch, identity) = u6_install_running(fx, running.abort_handle()).await;
    let captured = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut running => {
                let outcome = result.unwrap();
                u6_observe(json!({"stage":"real handoff setup rejected before native capture",
                    "error_code":outcome.as_ref().err().and_then(|error| error.data.as_ref())
                        .and_then(|data| data.get("code")).and_then(Value::as_str),
                    "local_message":outcome.as_ref().err().and_then(|error| error.data.as_ref())
                        .and_then(|data| data.get("message")).and_then(Value::as_str)
                        .map(|message| message.chars().take(256).collect::<String>()),
                    "error_message_digest":outcome.as_ref().err().map(|error| native_content_digest(error.message.as_bytes())),
                    "http_count":fx.server.messages_request_count()}));
                false
            },
            status = fx.session.source_acks.recv() => status == Some(k2_history::NativeResponseStatus::Accepted),
        }
    }).await.expect("H6_HANDOFF_CAPTURE_OUTCOME_RACE");
    assert!(captured, "H6_DECODED_NATIVE_HANDOFF");
    let mut entered = u6_client_dispatch(
        &mut fx.session.entered,
        &mut running,
        json!("Accepted"),
        "real handoff client dispatch",
        "H6_HANDOFF_DISPATCH_OUTCOME_RACE",
        "H6_HANDOFF_TURN_ENDED_BEFORE_DISPATCH",
    )
    .await;
    entered.sort();
    assert!(
        entered == vec!["alpha__lookup", "zeta__fetch"],
        "H6_REAL_CLIENT_EXECUTION"
    );
    let conversation = &fx.session.actor.chat_state_handle;
    u6_await_turn_progress(
        &mut running,
        "real handoff first actual client result",
        "H6_FIRST_ACTUAL_RESULT",
        "H6_FIRST_ACTUAL_RESULT_TURN_ENDED",
        || async move {
            conversation.get_conversation().await.iter().any(|item| {
                matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")
            })
        },
    )
    .await;
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_HANDOFF_RELOAD").await;
    assert!(k2lg_n_live(&checkpoint), "H6_REAL_LIVE_PENDING");
    assert_eq!(
        fx.server.messages_request_count(),
        2,
        "H6_ZERO_EARLY_CONTINUATION"
    );
    K2lgNTurn {
        running,
        epoch,
        identity,
        submits,
        turn_start_ops,
    }
}

async fn u6_l1() -> Value {
    let mut fixture = u6_fixture(Some(Arc::<str>::from(u6_png(32))), K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff(fx).await;
    let _turn_guard = K0Abort(turn.running.abort_handle());
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_L1_RELOAD").await;
    k0_admit_steer(&fx.session.actor).await;
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L1 results-only continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_u6_l1_after")),
    );
    fx.session.releases[1].add_permits(1);
    let (continued, finished_outcome) = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => (false, Some(result.expect("H6_L1_TURN_TASK"))),
            _ = continuation.wait_blocked() => (true, None),
        }
    })
    .await
    .expect("H6_L1_BOUNDED_OUTCOME_RACE");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    let held = !history.iter().any(|item| {
        item.text_content().contains("parent steer k0")
            || item
                .text_content()
                .contains("Image extracted from tool result")
    });
    u6_observe(
        json!({"stage":"L1 hold before closure","continuation_observed":continued,"input_in_history":!held,
    "turn_finished":finished_outcome.is_some(),"turn_refused":finished_outcome.as_ref().is_some_and(Result::is_err),
    "error_type":finished_outcome.as_ref().and_then(|result| result.as_ref().err()).and_then(|error| error.data.as_ref()).and_then(|data| data.get("error_type")).and_then(Value::as_str),
    "http_count":fx.server.messages_request_count()}),
    );
    if !continued {
        continuation.release();
    }
    assert!(held, "H6_L1_HOLD_ASSERTION");
    assert!(continued, "H6_L1_RESULTS_ONLY_CONTINUATION");
    let bodies = k2lg_n_bodies(&fx.server);
    u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
    u6_monitor(fx, "u6-a", "U6 first monitor").await;
    u6_monitor(fx, "u6-b", "U6 second monitor").await;
    k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
    assert!(
        !fx.session
            .actor
            .chat_state_handle
            .get_conversation()
            .await
            .iter()
            .any(|item| item.text_content().contains("U6 first monitor")),
        "H6_L1_MONITOR_HOLD"
    );
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L1_CLOSED_CHECKPOINT",
    )
    .await;
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("H6_L1_CLOSED_EXCHANGE_PROGRESS")
        .unwrap();
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    for needle in [
        "parent steer k0",
        "Image extracted from tool result",
        "U6 first monitor",
        "U6 second monitor",
    ] {
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(needle))
                .count(),
            1,
            "H6_L1_ONCE_ONLY_RELEASE"
        );
    }
    assert!(outcome.is_ok(), "H6_L1_CLOSED_EXCHANGE_OUTCOME");
    continuation.assert_satisfied();
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4, "H6_L1_FIRST_RELEASE_WITHOUT_PROMPT");
    let tail = bodies[3]["messages"].as_array().unwrap().last().unwrap();
    let blocks = tail["content"].as_array().unwrap();
    let text = blocks
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let steer = text.find("parent steer k0").expect("H6_L1_PARENT_RELEASE");
    let followup = text
        .find("Image extracted from tool result")
        .expect("H6_L1_REAL_FOLLOWUP_RELEASE");
    let monitor = text
        .find("U6 first monitor")
        .expect("H6_L1_MONITOR_RELEASE");
    assert!(
        steer < followup && followup < monitor,
        "H6_L1_GLOBAL_OCCURRENCE_ORDER"
    );
    assert_eq!(
        blocks
            .iter()
            .filter(|block| block["type"] == "image")
            .count(),
        1,
        "H6_L1_FOLLOWUP_IMAGE"
    );
    assert_eq!(
        blocks
            .iter()
            .filter(|block| block["text"].as_str().is_some_and(|text| text
                .contains("U6 first monitor")
                && text.contains("U6 second monitor")))
            .count(),
        1,
        "H6_L1_MAXIMAL_MONITOR_RUN"
    );
    assert_eq!(
        fx.session.executions.lock().unwrap().len(),
        2,
        "H6_L1_ONCE_ONLY_CLIENTS"
    );
    let (after, _) = u6_quiescent_reload(fx, "H6_R20_L1_AFTER_RELOAD").await;
    assert!(
        k2_source_blocks(&after, "msg_k2_pending")
            == k2_source_blocks(&checkpoint, "msg_k2_pending"),
        "H6_L1_NATIVE_SOURCE_UNCHANGED"
    );
    json!({"continuation_http":1,"release_http":1,"client_executions":2,"first_closed_safe_point":"turn.rs:3913","continuation_body_digest":native_content_digest(&serde_json::to_vec(&bodies[2]).unwrap()),"release_body_digest":native_content_digest(&serde_json::to_vec(&bodies[3]).unwrap()),"monitor_run":2})
}

tokio::task_local! {
    static U6_ARM_OBSERVATIONS: std::rc::Rc<std::cell::RefCell<Vec<Value>>>;
}

fn u6_observe(value: Value) {
    U6_ARM_OBSERVATIONS.with(|observations| observations.borrow_mut().push(value));
}

fn u6_assertion(payload: &(dyn std::any::Any + Send)) -> String {
    let text = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("");
    text.split_whitespace()
        .find_map(|word| {
            let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
            word.starts_with("H6_").then(|| word.to_owned())
        })
        .unwrap_or_else(|| "H6_UNEXPECTED_PANIC".into())
}

fn u6_label_in_group(label: &str, group: &str) -> bool {
    if group == "L17" {
        return label.starts_with("L19-native-");
    }
    if group == "L16" {
        return false;
    }
    if group == "L15" {
        return label == "L15-memory" || label == "L15-mcp" || label.starts_with("L15m");
    }
    for longer in ["L10b", "L14b", "L14c", "L13b"] {
        if group == longer {
            return label == longer || label.starts_with(&format!("{longer}-"));
        }
        if label == longer || label.starts_with(&format!("{longer}-")) {
            return false;
        }
    }
    label == group || label.starts_with(&format!("{group}-"))
}

fn u6_group_keeps(label: &str) -> bool {
    std::env::var("KOXD_U6_GROUP")
        .ok()
        .map(|group| u6_label_in_group(label, &group))
        .unwrap_or(true)
}

async fn u6_arm<F>(records: &mut Vec<Value>, label: &str, future: F)
where
    F: std::future::Future<Output = Value>,
{
    use futures_util::FutureExt;
    if !u6_group_keeps(label) {
        records.push(json!({
            "label": label,
            "passed": true,
            "status": "SKIPPED_OTHER_GROUP",
            "assertion": null,
            "observations": [],
            "panics": [],
            "result": null
        }));
        return;
    }
    let observations = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let panics = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_panics = panics.clone();
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let text = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("");
        captured_panics.lock().unwrap().push(json!({
            "file":info.location().map(|location| location.file()),
            "line":info.location().map(|location| location.line()),
            "message_digest":native_content_digest(text.as_bytes()),
            "left_u64":text.lines().find_map(|line| line.trim().strip_prefix("left: ").and_then(|value| value.parse::<u64>().ok())),
            "right_u64":text.lines().find_map(|line| line.trim().strip_prefix("right: ").and_then(|value| value.parse::<u64>().ok()))}));
    }));
    let steppers = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let result = U6_ARM_OBSERVATIONS
        .scope(
            observations.clone(),
            U6_ARM_STEPPERS.scope(
                steppers,
                std::panic::AssertUnwindSafe(future).catch_unwind(),
            ),
        )
        .await;
    std::panic::set_hook(previous_hook);
    let (passed, assertion, result) = match result {
        Ok(value) => (true, None, value),
        Err(error) => (false, Some(u6_assertion(error.as_ref())), Value::Null),
    };
    records.push(json!({"label":label,"passed":passed,"assertion":assertion,"observations":observations.borrow().clone(),"panics":panics.lock().unwrap().clone(),"result":result}));
}

fn u6_write_proof(records: &[Value]) {
    use std::io::Write;
    let mut file = tempfile::Builder::new()
        .prefix("k3u6a-h6-proof-")
        .suffix(".json")
        .tempfile_in(std::env::temp_dir())
        .unwrap();
    file.write_all(
        &serde_json::to_vec(&json!({"selector":KOXD_U6_SELECTOR,"arms":records})).unwrap(),
    )
    .unwrap();
    file.flush().unwrap();
    file.keep().unwrap();
}

async fn u6_isolated() {
    let home = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.env_clear();
    for name in [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "DEVELOPER_DIR",
        "SDKROOT",
        "DYLD_LIBRARY_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
        "__CF_USER_TEXT_ENCODING",
        "CARGO_TARGET_DIR",
        "TMPDIR",
        "CARGO_BUILD_JOBS",
        "CARGO_INCREMENTAL",
        "RUST_MIN_STACK",
        "GROK_SHELL_BUNDLE_RG_PATH",
        "GROK_TOOLS_BUNDLE_RG_PATH",
        "CARGO_NET_OFFLINE",
        "KOXD_U6_GROUP",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env(KOXD_U6_CHILD, "1")
        .env("GROK_HOME", home.path())
        .args(["--exact", KOXD_U6_SELECTOR, "--nocapture"])
        .stdin(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut command);
    let output = tokio::task::spawn_blocking(move || command.output())
        .await
        .unwrap()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines().filter(|line| line.starts_with("K2LG_V ")) {
        println!("{line}");
    }
    for line in stdout
        .lines()
        .filter_map(|line| line.strip_prefix("KOXD_U6_ARM "))
    {
        let value: Value = serde_json::from_str(line).expect("H6_CHILD_ARM_RECORD");
        println!(
            "KOXD_U6_ARM {}",
            json!({"label":value["label"],"passed":value["passed"],"assertion":value["assertion"],"status":value["status"]})
        );
    }
    assert!(
        output.status.success(),
        "H6_CHILD_FAILED status={:?} stdout_digest={} stderr_digest={}",
        output.status.code(),
        native_content_digest(&output.stdout),
        native_content_digest(&output.stderr)
    );
    assert!(
        stdout.contains("running 1 test"),
        "H6_CHILD_EXACT_SELECTION"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn messages_pending_holds_harness_user_content() {
    if std::env::var_os(KOXD_U6_CHILD).is_none() {
        u6_isolated().await;
        return;
    }
    if std::env::var("KOXD_U6_GROUP").ok().as_deref() == Some("L16") {
        println!(
            "K2LG_V {{\"route\":\"V2\",\"window\":\"W2\",\"status\":\"UNSCHEDULABLE_GAP\",\"reason\":\"no committed rendezvous; no new product hook\",\"K2LG_kill\":false,\"L16_credit\":false}}"
        );
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut records = Vec::new();
            u6_arm(&mut records, "L5", u6b_monitor_protocol("completion")).await;
            u6_arm(&mut records, "L5-no-next-turn", u6b_monitor_protocol("no-next-turn")).await;
            u6_arm(&mut records, "L6-before-admission", u6b_monitor_protocol("drop-before-admission")).await;
            u6_arm(&mut records, "L6-pinned", u6b_monitor_protocol("drop-held")).await;
            u6_arm(&mut records, "L6-retained-cancel", u6b_monitor_protocol("drop-after-cancel")).await;
            u6_arm(&mut records, "L7-user-start", u6b_monitor_protocol("user-start")).await;
            u6_arm(&mut records, "L7-consumed-completion", u6b_consumed_completion_dedupe()).await;
            u6_arm(&mut records, "L7-idle", u6b_monitor_protocol("idle")).await;
            u6_arm(&mut records, "L7-cancel", u6b_monitor_protocol("cancel")).await;
            u6_arm(&mut records, "L7-rewind", u6b_monitor_protocol("rewind")).await;
            u6_arm(&mut records, "L8", u6b_monitor_protocol("kill")).await;
            u6_arm(&mut records, "L11", u6b_monitor_protocol("equal")).await;
            u6_arm(&mut records, "L11-pinned-producer-cap", u6b_pinned_producer_cap()).await;
            u6_arm(&mut records, "N-10", u6b_n10()).await;
            u6_arm(&mut records, "N-1", u6b_human_band(300, 128, "H6_N1_HUMAN_UNCAPPED")).await;
            u6_arm(&mut records, "N-2", u6b_n2()).await;
            u6_arm(&mut records, "N-3", u6b_human_band(1, 256 * 1024, "H6_N3_HUMAN_UNCAPPED")).await;
            u6_arm(&mut records, "N-4", u6b_monitor_band()).await;
            u6_arm(&mut records, "N-7", u6b_deferred_image()).await;
            u6_arm(&mut records, "N-8", u6b_deferred_band()).await;
            u6_arm(&mut records, "L1", u6_l1()).await;
            u6_arm(&mut records, "L1-N", u6_l1_native()).await;
            u6_arm(&mut records, "L1-unavailable-view", u6_unavailable_view_hold()).await;
            u6_arm(&mut records, "L2-pre-completion", u6_l2("pre-completion")).await;
            u6_arm(&mut records, "L2-tool-batch", u6_l2("tool-batch")).await;
            u6_arm(&mut records, "L2-empty-continuation", u6_l2("empty-continuation")).await;
            u6_arm(&mut records, "L2-loop-top", u6_l2_loop_top_structured_output_retry()).await;
            u6_arm(&mut records, "L2-new-late-bookkeeping", u6_l2_late_bookkeeping()).await;
            u6_arm(&mut records, "L3-another-pending", u6_l3(false)).await;
            u6_arm(&mut records, "L3-rearm", u6_l3(true)).await;
            u6_arm(&mut records, "L4", u6_l4()).await;
            u6_arm(&mut records, "L12", u6_l12()).await;
            u6_arm(&mut records, "TW-1-resident-arrival", u6_tw1(true)).await;
            u6_arm(&mut records, "TW-1-already-custody", u6_tw1(false)).await;
            u6_arm(&mut records, "L10-parent-stamps", u6_parent_stamp_preservation()).await;
            u6_arm(&mut records, "L10b", u6_l10b()).await;
            u6_arm(&mut records, "L14", u6_l14()).await;
            u6_arm(&mut records, "L14c-base", u6_l14c(false)).await;
            u6_arm(&mut records, "L14c-straddling", u6_l14c(true)).await;
            u6_arm(&mut records, "L14b-closed", u6_l14b_closed("matching")).await;
            u6_arm(&mut records, "L14b-latch-absent", u6_l14b_closed("absent")).await;
            u6_arm(&mut records, "L14b-latch-inert", u6_l14b_closed("inert")).await;
            u6_arm(&mut records, "L14b-persistence-recovery", u6_l14b_persistence_recovery()).await;
            u6_arm(&mut records, "L14b-command-Cancel-new-owner", u6_l14b_command_cancel()).await;
            u6_arm(&mut records, "L14b-constructed-partial-loop", u6_l14b_constructed_partial_loop()).await;
            u6_arm(&mut records, "N-12-u66-scoped", u6b_n12_u66_scoped_turn_end()).await;
            u6_arm(&mut records, "N-12c-chat-close", u6b_n12c(U6bCloseCause::ChatClose)).await;
            u6_arm(&mut records, "N-12c-ActorDrop", u6b_n12c(U6bCloseCause::ActorDrop)).await;
            u6_arm(&mut records, "N-12c-HardTeardown", u6b_n12c(U6bCloseCause::HardTeardown)).await;
            u6_arm(&mut records, "N-14-u66-scoped", u6b_n14_u66_scoped()).await;
            u6_arm(&mut records, "L13b-preparation-ActorDrop", u6b_l13b(U6bTerminalAwait::Preparation, xai_message_delivery_core::TerminalCause::ActorDrop)).await;
            u6_arm(&mut records, "L13b-preparation-HardTeardown", u6b_l13b(U6bTerminalAwait::Preparation, xai_message_delivery_core::TerminalCause::HardTeardown)).await;
            u6_arm(&mut records, "L13b-persistence-ActorDrop", u6b_l13b(U6bTerminalAwait::PersistenceBarrier, xai_message_delivery_core::TerminalCause::ActorDrop)).await;
            u6_arm(&mut records, "L13b-persistence-HardTeardown", u6b_l13b(U6bTerminalAwait::PersistenceBarrier, xai_message_delivery_core::TerminalCause::HardTeardown)).await;
            u6_arm(&mut records, "N-6-long-id-steer", u6b_n6_long_id_steer()).await;
            u6_arm(&mut records, "N-11-steer-receipt", u6b_n11_steer_receipt()).await;
            u6_arm(&mut records, "N-5-33rd-steer", u6b_n5_33rd_steer()).await;
            u6_arm(&mut records, "N-9-projecting-straddle", u6b_n9_projecting_straddle()).await;
            u6_arm(&mut records, "R19-constructed-orphan-release", u6b_r19_constructed_orphan_release()).await;
            u6_arm(&mut records, "U66-constructed-missing-stamp", u6b_u66_constructed_missing_stamp()).await;
            u6_arm(&mut records, "L9-cancel-rebuild", u6b_l9_cancel_rebuild()).await;
            u6_arm(&mut records, "L13-shared-take", u6b_l13_shared_leaders(U6bL13Operation::Release)).await;
            u6_arm(&mut records, "L13-shared-unpin", u6b_l13_shared_leaders(U6bL13Operation::LogicalEnd)).await;
            u6_arm(&mut records, "L13-shared-discard", u6b_l13_shared_leaders(U6bL13Operation::KillBackground)).await;
            u6_arm(&mut records, "L18-wait-Cancel-rebuild-next", u6b_l18_wait_cancel(U6bL18Resume::NextAdmittedTurn)).await;
            u6_arm(&mut records, "L18-wait-send-now", u6b_l18_wait_cancel(U6bL18Resume::SendNow)).await;
            u6_arm(&mut records, "L18-ready-result-priority", u6b_l18_ready_result_priority()).await;
            u6_arm(&mut records, "DB11-native-handoff-after-terminal", u6b_db11_native_handoff_after_terminal()).await;
            u6_arm(&mut records, "DB11-constructed-terminal-stamp", u6b_db11_constructed_terminal_stamp()).await;
            u6_arm(&mut records, "N-11-peer-text-crossing", u6b_n11_peer(false)).await;
            u6_arm(&mut records, "N-11-peer-native-crossing", u6b_n11_peer(true)).await;
            u6_arm(&mut records, "L15-memory", u6b_l15(U6bL15Source::Memory)).await;
            u6_arm(&mut records, "L15-mcp", u6b_l15(U6bL15Source::Mcp)).await;
            u6_arm(&mut records, "L19-Cancel-late", u6b_l19_hold(U6bL19Trigger::Cancel, U6bL19Skills::Late, U6bL19Native::Absent)).await;
            u6_arm(&mut records, "L19-owned-error-late", u6b_l19_hold(U6bL19Trigger::OwnedError, U6bL19Skills::Late, U6bL19Native::Absent)).await;
            u6_arm(&mut records, "L19-Cancel-refused-skill", u6b_l19_hold(U6bL19Trigger::Cancel, U6bL19Skills::RefusedResident, U6bL19Native::Absent)).await;
            u6_arm(&mut records, "L19-owned-error-refused-skill", u6b_l19_hold(U6bL19Trigger::OwnedError, U6bL19Skills::RefusedResident, U6bL19Native::Absent)).await;
            u6_arm(&mut records, "L19-native-Cancel-retention", u6b_l19_hold(U6bL19Trigger::Cancel, U6bL19Skills::Late, U6bL19Native::RetainedPrefix)).await;
            u6_arm(&mut records, "L19-native-owned-error-retention", u6b_l19_hold(U6bL19Trigger::OwnedError, U6bL19Skills::Late, U6bL19Native::RetainedPrefix)).await;
            u6_arm(&mut records, "L19-no-hold-Cancel", u6b_l19_unheld(U6bL19Trigger::Cancel)).await;
            u6_arm(&mut records, "L19-no-hold-owned-error", u6b_l19_unheld(U6bL19Trigger::OwnedError)).await;
            u6_arm(&mut records, "L20-revoked", u6b_l20(U6bL20Activation::Withdraw, U6bL20Completion::Closure)).await;
            u6_arm(&mut records, "L20-on-off-on", u6b_l20(U6bL20Activation::Reenable, U6bL20Completion::Closure)).await;
            u6_arm(&mut records, "L20-approval-revoked", u6b_l20(U6bL20Activation::ApprovalWithdraw, U6bL20Completion::Closure)).await;
            u6_arm(&mut records, "L20-Cancel-revoked", u6b_l20(U6bL20Activation::Withdraw, U6bL20Completion::Cancel)).await;
            u6_arm(&mut records, "L20-owned-error-revoked", u6b_l20(U6bL20Activation::Withdraw, U6bL20Completion::OwnedError)).await;
            u6_arm(&mut records, "L21-media-held", u6b_l21_held(U6bL21Writer::MediaResample)).await;
            u6_arm(&mut records, "L21-media-unheld", u6b_l21_unheld(U6bL21Writer::MediaResample)).await;
            u6_arm(&mut records, "L21-tool-call-Length-held", u6b_l21_tool_call_length_tripwire()).await;
            u6_arm(&mut records, "L21-tool-call-Length-unheld", u6b_l21_unheld(U6bL21Writer::ToolCallLength)).await;
            u6_arm(&mut records, "L21-tool-free-Length-held", u6b_l21_held(U6bL21Writer::ToolFreeLength)).await;
            u6_arm(&mut records, "L21-tool-free-Length-unheld", u6b_l21_unheld(U6bL21Writer::ToolFreeLength)).await;
            u6_arm(&mut records, "N-11-stationarity-crossing-constructed", u6b_n11_u67_band_custody(super::interjection::HeldSource::StationarityReminder, "StationarityBandFull", "stationarity_band_full")).await;
            u6_arm(&mut records, "N-11-S10-crossing-constructed", u6b_n11_u67_band_custody(super::interjection::HeldSource::LengthReminder, "LengthReminderBandFull", "length_reminder_band_full")).await;
            u6_arm(&mut records, "N-11-media-band-crossing-constructed", u6b_n11_u67_band_custody(super::interjection::HeldSource::MediaResample, "MediaResampleBandFull", "media_resample_band_full")).await;
            u6_arm(&mut records, "N-11-tool-free-Length-band-crossing-constructed", u6b_n11_u67_band_custody(super::interjection::HeldSource::ToolFreeLength, "ToolFreeLengthBandFull", "tool_free_length_band_full")).await;
            u6_arm(&mut records, "N-11-skill-crossing", u6b_n11_u67_skill_crossing()).await;
            u6_arm(&mut records, "N-11-peer-at-flush", u6b_n11_u67_peer_at_flush()).await;
            u6_arm(&mut records, "N-11-peer-flush-refusal-constructed", u6b_n11_u67_peer_flush_refusal()).await;
            u6_arm(&mut records, "N-12-U67-peer-S10-writer-band-extension", u6b_n12_u67_band_extension()).await;
            u6_arm(&mut records, "N-13-full-steer-isolated-constructed", u6b_n13_band_isolation(super::interjection::HeldSource::ParentSteer)).await;
            u6_arm(&mut records, "N-13-full-monitor-isolated-constructed", u6b_n13_band_isolation(super::interjection::HeldSource::MonitorEvent)).await;
            u6_arm(&mut records, "N-13-full-deferred-isolated-constructed", u6b_n13_band_isolation(super::interjection::HeldSource::DeferredFollowUp)).await;
            u6_arm(&mut records, "N-13-full-stationarity-isolated-constructed", u6b_n13_band_isolation(super::interjection::HeldSource::StationarityReminder)).await;
            u6_arm(&mut records, "N-13-full-skill-isolated-constructed", u6b_n13_band_isolation(super::interjection::HeldSource::SkillReminder)).await;
            u6_arm(&mut records, "N-13-full-plan-isolated-constructed", u6b_n13_band_isolation(super::interjection::HeldSource::PlanActivation)).await;
            u6_arm(&mut records, "N-13-full-peer-isolated-constructed", u6b_n13_band_isolation(super::interjection::HeldSource::PeerAgent)).await;
            u6_arm(&mut records, "N-13-full-media-resample-isolated-constructed", u6b_n13_band_isolation(super::interjection::HeldSource::MediaResample)).await;
            u6_arm(&mut records, "N-13-full-tool-free-Length-isolated-constructed", u6b_n13_band_isolation(super::interjection::HeldSource::ToolFreeLength)).await;
            u6_arm(&mut records, "N-13-full-S10-isolated-constructed", u6b_n13_band_isolation(super::interjection::HeldSource::LengthReminder)).await;
            u6_arm(&mut records, "N-13-real-mailbox-peer-human-isolation", u6b_n13_real_peer_isolation()).await;
            u6_arm(&mut records, "L15m-native-memory-durable", u6b_l15m_native_memory(U6bL15mArm::Durable)).await;
            u6_arm(&mut records, "L15m-native-memory-rearm-once", u6b_l15m_native_memory(U6bL15mArm::Rearm)).await;
            u6_arm(&mut records, "L15m-selected-no-checkpoint-system-head", u6b_l15m_selected_no_checkpoint()).await;
            u6_arm(&mut records, "L15m-inactive-guard-system-head", u6b_l15m_inactive_guard()).await;
            u6_write_proof(&records);
            for record in &records {
                println!("KOXD_U6_ARM {}", record);
            }
            assert!(
                records.iter().all(|record| record["passed"] == true ||
                    (record["label"] == "L2-loop-top" && record["status"] == "NOT_RUN"
                        && record["authority"] == "H-K3U6a-R8 R8-3")),
                "H6_ALLOCATED_ARM_FAILURE"
            );
        })
        .await;
}

async fn u6_interject(fx: &K2lgN, text: &str, images: Vec<acp::ImageContent>) {
    fx.loop_tx
        .send(SessionCommand::Interject {
            text: text.to_owned(),
            id: None,
            images,
        })
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, async {
        while !fx
            .session
            .actor
            .pending_interjections
            .snapshot()
            .iter()
            .any(|item| item.text == text)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("H6_REAL_HUMAN_INGRESS");
}

async fn u6_l1_native() -> Value {
    const NATIVE: &str = "U6 L1-N native prefix";
    const HUMAN: &str = "U6 L1-N ordinary human";
    let mut fixture = u6_fixture(Some(Arc::<str>::from(u6_png(32))), K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_L1N_RELOAD").await;
    k0_admit_steer(&fx.session.actor).await;
    u6_interject(fx, HUMAN, Vec::new()).await;
    k2lg_n_send(fx, NATIVE).await;
    k2lg_n_send(fx, NATIVE).await;
    let resident = k2lg_n_values(&k2lg_n_resident(&fx.session));
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L1-N continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_u6_l1n_after")),
    );
    fx.session.releases[1].add_permits(1);
    let continued = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => { let _ = result.unwrap(); false },
            _ = continuation.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L1N_OUTCOME_RACE");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        !history
            .iter()
            .any(|item| item.text_content().contains(HUMAN)
                || item.text_content().contains(NATIVE)
                || item.text_content().contains("parent steer k0")
                || item
                    .text_content()
                    .contains("Image extracted from tool result")),
        "H6_L1N_HOLD_ASSERTION"
    );
    assert!(continued, "H6_L1N_CONTINUATION");
    let bodies = k2lg_n_bodies(&fx.server);
    u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
    assert!(
        k2lg_n_values(&k2lg_n_resident(&fx.session)) == resident,
        "H6_L1N_RESIDENT_PREFIX_UNCHANGED"
    );
    u6_monitor(fx, "u6-l1n", "U6 L1-N monitor").await;
    k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L1N_CLOSED_CHECKPOINT",
    )
    .await;
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("H6_L1N_FIRST_RELEASE_PROGRESS")
        .unwrap();
    assert!(outcome.is_ok(), "H6_L1N_OUTCOME");
    continuation.assert_satisfied();
    let after = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        k2lg_n_natives(&after) == vec![k2lg_n_native_text(NATIVE), k2lg_n_native_text(NATIVE)],
        "H6_L1N_NATIVE_PREFIX_ONCE"
    );
    let first = after
        .iter()
        .position(k2lg_n_is_native)
        .expect("H6_L1N_NATIVE_FIRST");
    let parent = after
        .iter()
        .position(|item| item.text_content().contains("parent steer k0"))
        .expect("H6_L1N_PARENT_RELEASE");
    assert!(
        k2lg_n_is_native(&after[first + 1]) && first + 2 == parent,
        "H6_L1N_NATIVE_BEFORE_MERGED_BATCH"
    );
    for text in [
        HUMAN,
        "parent steer k0",
        "Image extracted from tool result",
        "U6 L1-N monitor",
    ] {
        assert_eq!(
            after
                .iter()
                .filter(|item| item.text_content().contains(text))
                .count(),
            1,
            "H6_L1N_ORDINARY_ONCE"
        );
    }
    assert!(
        k2lg_n_resident(&fx.session).is_empty(),
        "H6_L1N_NO_DOUBLE_HOLD"
    );
    assert_eq!(
        k2lg_n_native_persists(&k2lg_n_ops(fx)),
        2,
        "H6_L1N_NATIVE_PERSIST_ONCE"
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4, "H6_L1N_SAME_TURN_RELEASE");
    assert_eq!(
        k2lg_n_text_count(&bodies[3], NATIVE),
        2,
        "H6_L1N_TRANSMITTED_PREFIX_ONCE"
    );
    json!({"producer":"send_message->coordinator->AgentMessage->message_item->resident",
        "native_occurrences":2,"order":"native,native,merged ordinary batch",
        "first_closed_safe_point":"turn.rs:3913","native_persists":2,"waiting_client_content":false})
}

async fn u6_handoff(fx: &mut K2lgN) -> K2lgNTurn {
    u6_handoff_with_salvage(fx, None, None, None, None).await
}

async fn u6_l2(site: &str) -> Value {
    const HUMAN: &str = "U6 L2 held human";
    let mut fixture =
        u6_fixture_with_bookkeeping(None, K2lgNOptions::default(), false, site == "loop-top").await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff_with_salvage(
        fx,
        (site == "empty-continuation").then_some(1),
        None,
        None,
        None,
    )
    .await;
    let _stop = K0Abort(turn.running.abort_handle());
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_L2_RELOAD").await;
    u6_interject(fx, HUMAN, Vec::new()).await;
    let mut blocks = vec![
        k2_result("srvtoolu_k2_pending", "alpha__lookup"),
        (
            json!({"type":"text","text":"U6 L2 closes the real server exchange"}),
            Vec::new(),
        ),
    ];
    if site == "tool-batch" {
        blocks.push((json!({"type":"tool_use","id":"client_u6_l2","name":"alpha__lookup","input":{"value":"L2"}}), Vec::new()));
    }
    if site == "loop-top" {
        for index in 0..(2 * xai_grok_tools::media_gen_limits::DEFAULT_MAX_PARALLEL_IMAGE_GEN) {
            blocks.push((
                json!({"type":"tool_use","id":format!("client_u6_l2_media_{index}"),
                "name":"image_gen","input":{"prompt":"U6 loop-top resample; never executed"}}),
                Vec::new(),
            ));
        }
    }
    let stop = if site == "empty-continuation" {
        "max_tokens"
    } else {
        "end_turn"
    };
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L2 pending continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2_frames("msg_u6_l2_closed", blocks, stop)),
    );
    let mut empty = (site == "empty-continuation").then(|| {
        fx.server.expect_response_blocked(
            "U6 L2 real empty continuation",
            xai_grok_test_support::InferenceRequestMatcher::foreground(
                xai_grok_test_support::InferenceEndpoint::Messages,
            ),
            k2_script(k2_frames("msg_u6_l2_empty", Vec::new(), "max_tokens")),
        )
    });
    let mut released = fx.server.expect_response_blocked(
        "U6 L2 first release request",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_after_frames("msg_u6_l2_after")),
    );
    fx.session.releases[1].add_permits(1);
    let continued = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => { let _ = result.unwrap(); false },
            _ = continuation.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L2_CONTINUATION_RACE");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        !history
            .iter()
            .any(|item| item.text_content().contains(HUMAN)),
        "H6_L2_HOLD_ASSERTION"
    );
    assert!(continued, "H6_L2_CONTINUATION");
    let bodies = k2lg_n_bodies(&fx.server);
    u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L2_CLOSED_CHECKPOINT",
    )
    .await;
    if let Some(empty) = empty.as_mut() {
        let observed = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                result = &mut turn.running => { let _ = result.unwrap(); false },
                _ = empty.wait_blocked() => true,
            }
        })
        .await
        .expect("H6_L2_EMPTY_CONTINUATION_RACE");
        let read = fx
            .session
            .actor
            .chat_state_handle
            .read_native_history()
            .await;
        u6_observe(json!({"stage":"L2 empty continuation before its terminal",
            "empty_http_observed":observed,"read_ok":read.is_ok(),
            "groups":read.as_ref().ok().and_then(|read| read.checkpoint.as_ref()).map(|checkpoint|
                checkpoint.ledger.groups.iter().map(|group| group.state.clone()).collect::<Vec<_>>())}));
        assert!(observed, "H6_L2_EMPTY_CONTINUATION_REACHED");
        u6_release_after_checkpoint(
            fx,
            &mut turn.running,
            &empty,
            false,
            "H6_R15_L2_EMPTY_CHECKPOINT",
        )
        .await;
    }
    let release_observed = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => {
                let outcome = result.unwrap();
                u6_observe(json!({"stage":"L2 first eligible caller", "site":site,
                    "turn_finished":true,"turn_ok":outcome.is_ok(),
                    "error_type":outcome.as_ref().err().and_then(|error| error.data.as_ref())
                        .and_then(|data| data.get("error_type")).and_then(Value::as_str)}));
                false
            },
            _ = released.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L2_FIRST_RELEASE_RACE");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    let count = history
        .iter()
        .filter(|item| item.text_content().contains(HUMAN))
        .count();
    if site == "loop-top" {
        let read = fx
            .session
            .actor
            .chat_state_handle
            .read_native_history()
            .await;
        u6_observe(
            json!({"stage":"R8 media resample route", "release_http_observed":release_observed,
            "live_pending_after_resample":read.as_ref().ok().and_then(|read| read.checkpoint.as_ref()).is_some_and(|checkpoint| k2lg_n_live(checkpoint)),
            "read_ok":read.is_ok(),"declarations_unchanged":bodies[1]["tools"] == bodies[2]["tools"],
            "first_candidate_caller":"turn.rs:2844", "media_calls_executed":0}),
        );
    }
    u6_observe(
        json!({"stage":"L2 first release", "site":site,"release_observed":release_observed,
        "human_in_history":count,"http_count":fx.server.messages_request_count()}),
    );
    assert!(
        release_observed && count == 1,
        "H6_L2_FIRST_CLOSED_SAFE_POINT_RELEASE"
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert!(
        u6_body_text(bodies.last().unwrap()).contains(HUMAN),
        "H6_L2_ACTUAL_RELEASE_BODY"
    );
    assert_eq!(
        bodies.len(),
        if site == "empty-continuation" { 5 } else { 4 },
        "H6_L2_NO_EXTRA_PROMPT"
    );
    released.release();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("H6_L2_FINISH")
        .unwrap();
    assert!(outcome.is_ok(), "H6_L2_OUTCOME");
    continuation.assert_satisfied();
    released.assert_satisfied();
    if let Some(empty) = empty {
        empty.assert_satisfied();
    }
    json!({"site":site,"first_release":1,"human_occurrences":count,"http":bodies.len(),
        "new_prompt":false,"caller":match site { "tool-batch" => "tool_calls.rs:539",
            "empty-continuation" => "turn.rs:3139", "loop-top" => "turn.rs:2844", _ => "turn.rs:3913" }})
}

async fn u6_l4() -> Value {
    const HUMAN: &str = "U6 L4 retained across round boundary";
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    u6_interject(fx, HUMAN, Vec::new()).await;
    let mut salvage = super::length_salvage::LengthSalvage::new(Some(1));
    let _ = salvage.on_length_stop();
    salvage.response_arrived();
    salvage.round_boundary();
    assert!(
        !salvage.awaiting_continuation(),
        "H6_L4_REAL_ROUND_BOUNDARY"
    );
    let delivered = k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_L4_RELOAD").await;
    assert!(k2lg_n_live(&checkpoint), "H6_L4_NATIVE_EXCHANGE_STILL_LIVE");
    assert!(
        !delivered
            && !history
                .iter()
                .any(|item| item.text_content().contains(HUMAN)),
        "H6_L4_NO_BOOKKEEPING_CLOSURE"
    );
    assert_eq!(fx.server.messages_request_count(), 2, "H6_L4_NO_EARLY_HTTP");
    json!({"round_boundary":true,"native_pending":true,"delivery":false,
        "bookkeeping_observation":"round-boundary policy exercised, not server closure"})
}

fn u6_next_pending(close_original: bool) -> Vec<Value> {
    let mut blocks = Vec::new();
    if close_original {
        blocks.push(k2_result("srvtoolu_k2_pending", "alpha__lookup"));
    }
    blocks.extend([k2_call("srvtoolu_u6_next", None),
        (json!({"type":"tool_use","id":"client_u6_next_a","name":"alpha__lookup","input":{"value":"next A"}}), Vec::new()),
        (json!({"type":"tool_use","id":"client_u6_next_b","name":"zeta__fetch","input":{"value":"next B"}}), Vec::new())]);
    k2_frames("msg_u6_next_pending", blocks, "tool_use")
}

async fn u6_l3(rearm: bool) -> Value {
    const FIRST: &str = "U6 L3 first held human";
    const SECOND: &str = "U6 L3 second held human";
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    u6_interject(fx, FIRST, Vec::new()).await;
    let frames = if rearm {
        k2lg_n_closure_frames()
    } else {
        u6_next_pending(true)
    };
    let mut first_continuation = fx.server.expect_response_blocked(
        "U6 L3 first continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(frames),
    );
    fx.session.releases[1].add_permits(1);
    let continued = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => { let _ = result.unwrap(); false },
            _ = first_continuation.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L3_FIRST_OUTCOME_RACE");
    assert!(
        !fx.session
            .actor
            .chat_state_handle
            .get_conversation()
            .await
            .iter()
            .any(|item| item.text_content().contains(FIRST)),
        "H6_L3_INITIAL_HOLD"
    );
    assert!(continued, "H6_L3_FIRST_CONTINUATION");
    let mut first_release = rearm.then(|| {
        fx.server.expect_response_blocked(
            "U6 L3 first release before re-arm",
            xai_grok_test_support::InferenceRequestMatcher::foreground(
                xai_grok_test_support::InferenceEndpoint::Messages,
            ),
            k2_script(u6_next_pending(false)),
        )
    });
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &first_continuation,
        !rearm,
        "H6_R15_L3_FIRST_RELEASE_CHECKPOINT",
    )
    .await;
    if let Some(released) = first_release.as_mut() {
        let (released_http, finished) = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                _ = released.wait_blocked() => (true, None),
                result = &mut turn.running => (false, Some(result.unwrap())),
            }
        })
        .await
        .expect("H6_L3_FIRST_RELEASE_OUTCOME_RACE");
        let custody = u6_mixed_keys(fx).await;
        let history = fx.session.actor.chat_state_handle.get_conversation().await;
        let history_has_first = history
            .iter()
            .any(|item| item.text_content().contains(FIRST));
        u6_observe(
            json!({"stage":"L3 first release race", "release_http":released_http,
            "turn_finished":finished.is_some(), "turn_refused":finished.as_ref().is_some_and(Result::is_err),
            "custody_present":!custody.is_empty(), "history_has_first":history_has_first}),
        );
        assert!(
            released_http && finished.is_none() && history_has_first,
            "H6_L3_FIRST_RELEASE"
        );
        assert!(custody.is_empty(), "H6_L3_FIRST_RELEASE_CUSTODY_COMMITTED");
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(FIRST))
                .count(),
            1,
            "H6_L3_RELEASE_COMPLETED_BEFORE_REARM"
        );
        u6_release_after_checkpoint(
            fx,
            &mut turn.running,
            &released,
            true,
            "H6_R15_L3_NEXT_HANDOFF_CHECKPOINT",
        )
        .await;
    }
    let next_capture = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            biased;
            status = fx.session.source_acks.recv() => Ok(status),
            result = &mut turn.running => Err(result),
        }
    })
    .await;
    let next_status = match next_capture {
        Ok(Ok(status)) => status,
        Ok(Err(result)) => u6_turn_ended(
            result,
            "L3 next handoff native capture",
            "H6_L3_NEXT_TURN_ENDED_BEFORE_CAPTURE",
            json!({"capture":null}),
        ),
        Err(_) => u6_bound_expired(
            &turn.running,
            "L3 next handoff native capture",
            "H6_L3_NEXT_CAPTURE_OUTCOME_RACE",
            json!({"capture":null}),
        ),
    };
    assert!(next_status.is_some(), "H6_L3_REAL_NEXT_NATIVE_CAPTURE");
    let mut entered = u6_client_dispatch(
        &mut fx.session.entered,
        &mut turn.running,
        json!(next_status),
        "L3 next handoff client dispatch",
        "H6_L3_NEXT_DISPATCH_OUTCOME_RACE",
        "H6_L3_NEXT_TURN_ENDED_BEFORE_DISPATCH",
    )
    .await;
    entered.sort();
    assert_eq!(
        entered,
        vec!["alpha__lookup", "zeta__fetch"],
        "H6_L3_NEXT_REAL_CLIENTS"
    );
    let (next, _) = u6_quiescent_reload(fx, "H6_R20_L3_NEXT_RELOAD").await;
    assert!(k2lg_n_live(&next), "H6_L3_ANOTHER_PENDING_CALL");
    if !rearm {
        assert!(
            !fx.session
                .actor
                .chat_state_handle
                .get_conversation()
                .await
                .iter()
                .any(|item| item.text_content().contains(FIRST)),
            "H6_L3_RETAIN_WHILE_ANOTHER_PENDING"
        );
    }
    u6_interject(fx, SECOND, Vec::new()).await;
    k0_admit_steer(&fx.session.actor).await;
    let mut next_continuation = fx.server.expect_response_blocked(
        "U6 L3 next results-only continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2_frames(
            "msg_u6_next_closed",
            vec![
                k2_result("srvtoolu_u6_next", "alpha__lookup"),
                (
                    json!({"type":"text","text":"U6 L3 closes remaining call"}),
                    Vec::new(),
                ),
            ],
            "end_turn",
        )),
    );
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_u6_l3_after")),
    );
    fx.session.releases[1].add_permits(1);
    let continued = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => {
                let outcome = result.unwrap();
                u6_observe(json!({"stage":"L3 next handoff","turn_finished":true,
                    "turn_refused":outcome.is_err(),"http_count":fx.server.messages_request_count()}));
                false
            },
            _ = next_continuation.wait_blocked() => true,
        }
    }).await.expect("H6_L3_NEXT_OUTCOME_RACE");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        !history
            .iter()
            .any(|item| item.text_content().contains(SECOND)
                || item.text_content().contains("parent steer k0")),
        "H6_L3_REARM_HOLD_ASSERTION"
    );
    assert!(continued, "H6_L3_NEXT_RESULTS_ONLY_CONTINUATION");
    let bodies = k2lg_n_bodies(&fx.server);
    let initial = if rearm { 3 } else { 2 };
    u6_assert_results_only(&next, &bodies[initial], &bodies[initial + 1]);
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &next_continuation,
        false,
        "H6_R15_L3_FINAL_CLOSED_CHECKPOINT",
    )
    .await;
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("H6_L3_FINAL_RELEASE")
        .unwrap();
    assert!(outcome.is_ok(), "H6_L3_OUTCOME");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    for text in [FIRST, SECOND, "parent steer k0"] {
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(text))
                .count(),
            1,
            "H6_L3_ONCE_ONLY_RELEASE"
        );
    }
    assert_eq!(
        fx.session.executions.lock().unwrap().len(),
        4,
        "H6_L3_CLIENTS_ONCE"
    );
    first_continuation.assert_satisfied();
    next_continuation.assert_satisfied();
    if let Some(released) = first_release {
        released.assert_satisfied();
    }
    json!({"rearm_after_completed_release":rearm,"another_pending_retained":!rearm,
        "client_executions":4,"new_prompt":false,"second_hold":true})
}

async fn u6_l12() -> Value {
    const HUMAN: &str = "U6 L12 once-only human";
    let mut fixture = u6_fixture(
        Some(Arc::<str>::from(u6_png(32))),
        K2lgNOptions {
            retries: 2,
            max_turns: None,
        },
    )
    .await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    k0_admit_steer(&fx.session.actor).await;
    u6_interject(fx, HUMAN, Vec::new()).await;
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L12 continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    let continued = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => { let _ = result.unwrap(); false },
            _ = continuation.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L12_OUTCOME_RACE");
    assert!(
        !fx.session
            .actor
            .chat_state_handle
            .get_conversation()
            .await
            .iter()
            .any(|item| item.text_content().contains(HUMAN)),
        "H6_L12_HOLD_ASSERTION"
    );
    assert!(continued, "H6_L12_CONTINUATION");
    u6_monitor(fx, "u6-l12", "U6 L12 monitor").await;
    k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    let mut throttled = ScriptedResponse::json(
        429,
        json!({"type":"error","error":{"type":"rate_limit_error","message":"bounded U6 retry"}}),
    );
    throttled.headers.push(("retry-after".into(), "1".into()));
    let mut throttled = fx.server.expect_response_blocked(
        "U6 L12 first post-release transport attempt",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        throttled,
    );
    let mut retry = fx.server.expect_response_blocked(
        "U6 L12 actual post-release transport retry",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_after_frames("msg_u6_l12_after")),
    );
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L12_CLOSED_CHECKPOINT",
    )
    .await;
    let first_observed = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => { let _ = result.unwrap(); false },
            _ = throttled.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L12_FIRST_RELEASE_OUTCOME_RACE");
    let before_retry = fx.session.actor.chat_state_handle.get_conversation().await;
    u6_observe(
        json!({"stage":"L12 first post-release HTTP cardinality", "http_observed":first_observed,
        "human":before_retry.iter().filter(|item| item.text_content().contains(HUMAN)).count(),
        "parent":before_retry.iter().filter(|item| item.text_content().contains("parent steer k0")).count(),
        "followup":before_retry.iter().filter(|item| item.text_content().contains("Image extracted from tool result")).count(),
        "monitor":before_retry.iter().filter(|item| item.text_content().contains("U6 L12 monitor")).count()}),
    );
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    for text in [
        HUMAN,
        "parent steer k0",
        "Image extracted from tool result",
        "U6 L12 monitor",
    ] {
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(text))
                .count(),
            1,
            "H6_L12_LOGICAL_DELIVERY_ONCE"
        );
    }
    assert!(first_observed, "H6_L12_FIRST_POST_RELEASE_HTTP");
    throttled.release();
    let retry_observed = tokio::time::timeout(Duration::from_secs(90), async {
        tokio::select! {
            result = &mut turn.running => { let _ = result.unwrap(); false },
            _ = retry.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L12_ACTUAL_RETRY_OUTCOME_RACE");
    assert!(retry_observed, "H6_L12_ACTUAL_RETRY_HTTP");
    retry.release();
    let outcome = tokio::time::timeout(Duration::from_secs(90), &mut turn.running)
        .await
        .expect("H6_L12_RETRY_FINISH")
        .unwrap();
    u6_observe(json!({"stage":"L12 transport outcome","ok":outcome.is_ok(),
        "error_type":outcome.as_ref().err().and_then(|error| error.data.as_ref())
            .and_then(|data| data.get("error_type")).and_then(Value::as_str),
        "http_count":fx.server.messages_request_count()}));
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    for text in [
        HUMAN,
        "parent steer k0",
        "Image extracted from tool result",
        "U6 L12 monitor",
    ] {
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(text))
                .count(),
            1,
            "H6_L12_LOGICAL_DELIVERY_ONCE"
        );
    }
    assert!(outcome.is_ok(), "H6_L12_OUTCOME");
    let requests: Vec<_> = fx
        .server
        .requests()
        .into_iter()
        .filter(|request| request.path == "/v1/messages")
        .collect();
    assert_eq!(requests.len(), 5, "H6_L12_TWO_POST_RELEASE_ATTEMPTS");
    assert!(
        requests[3].body == requests[4].body,
        "H6_L12_ACTUAL_RETRY_BODY_EQUAL"
    );
    let updates = fixture.persistence_control.updates.lock().unwrap();
    assert_eq!(
        updates
            .iter()
            .filter(|item| item.to_string().contains(HUMAN))
            .count(),
        1,
        "H6_L12_HUMAN_PERSIST_ONCE"
    );
    assert_eq!(
        fx.session.executions.lock().unwrap().len(),
        2,
        "H6_L12_CLIENT_EXECUTION_ONCE"
    );
    continuation.assert_satisfied();
    throttled.assert_satisfied();
    retry.assert_satisfied();
    json!({"post_release_transport_attempts":2,"equal_actual_submitted_bodies":true,
        "raw_request_bytes":"not recorded by this server", "logical_occurrences_per_source":1,
        "human_persistence":1,"client_executions":2})
}

async fn u6_fixture(image: Option<Arc<str>>, options: K2lgNOptions) -> U6Fixture {
    u6_fixture_with_bookkeeping(image, options, false, false).await
}

async fn u6_l2_late_bookkeeping() -> Value {
    const HUMAN: &str = "U6 L2 new late-bookkeeping admission";
    use crate::tools::todo::{TodoItem, TodoPriority, TodoState, TodoStatus};
    use xai_grok_tools::types::resources::State as ResourceState;
    let mut fixture = u6_fixture_with_bookkeeping(None, K2lgNOptions::default(), true, false).await;
    let fx = &mut fixture.fx;
    k2_seed(&mut fx.session, &fx.server).await;
    let bridge = fx.session.actor.agent.borrow().tool_bridge().clone();
    let resources = bridge.shared_resources().await;
    let mut todos = TodoState::default();
    todos.push(
        "u6-bookkeeping".into(),
        TodoItem {
            content: "U6 bookkeeping observer".into(),
            priority: TodoPriority::Medium,
            status: TodoStatus::InProgress,
            meta: None,
        },
    );
    resources.lock().await.insert(ResourceState(todos));
    let dummy = tokio::task::spawn_local(std::future::pending::<()>());
    let _stop = K0Abort(dummy.abort_handle());
    u6_install_running(fx, dummy.abort_handle()).await;
    fx.session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("U6 real late-bookkeeping turn"))
        .await;
    let mut first = fx.server.expect_response_blocked(
        "U6 L2 before real bookkeeping",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_after_frames("msg_u6_late_first")),
    );
    let mut after = fx.server.expect_response_blocked(
        "U6 L2 after new late admission",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_after_frames("msg_u6_late_after")),
    );
    let actor = fx.session.actor.clone();
    let mut prepare =
        |_: &mut ConversationRequest, _: &xai_chat_state::RequestTransportBinding| Ok(());
    let mut salvage = super::length_salvage::LengthSalvage::new(None);
    let mut sampling = Default::default();
    let turn = super::switch_projection::HISTORY_S3_TEST_DOMAIN.scope(
        fx.session.domain(&fx.server),
        super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
            k2_history::NativeHistoryRequirement::Selected,
            actor.process_conversation_turn_for_native_history_test(
                KOXD_U6_PROMPT,
                None,
                None,
                None,
                &mut salvage,
                &mut sampling,
                &mut prepare,
            ),
        ),
    );
    tokio::pin!(turn);
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            biased;
            _ = first.wait_blocked() => (),
            _ = &mut turn => panic!("H6_L2_LATE_EARLY_END"),
        }
    })
    .await
    .expect("H6_L2_LATE_FIRST_HTTP");
    let before = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        !before
            .iter()
            .any(|item| item.text_content().contains(HUMAN)),
        "H6_L2_LATE_NO_EARLY_ADMISSION"
    );
    first.release();
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            biased;
            _ = fixture.persistence_control.cleanup.notified() => (),
            _ = &mut turn => panic!("H6_L2_LATE_BOOKKEEPING_AWAIT_NOT_REACHED"),
        }
    })
    .await
    .expect("H6_L2_LATE_REAL_BOOKKEEPING_NOTIFICATION");
    assert_eq!(
        fixture
            .persistence_control
            .cleanup_count
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "H6_L2_LATE_CLEANUP_OBSERVED"
    );
    u6_interject(fx, HUMAN, Vec::new()).await;
    assert_eq!(
        fx.server.messages_request_count(),
        2,
        "H6_L2_LATE_NOT_NEXT_PROMPT"
    );
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            biased;
            _ = after.wait_blocked() => (),
            _ = &mut turn => panic!("H6_L2_LATE_NOT_RELEASED"),
        }
    })
    .await
    .expect("H6_L2_LATE_POST_BOOKKEEPING_RELEASE");
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 3, "H6_L2_LATE_ONCE_ONLY_HTTP");
    assert!(
        u6_body_text(&bodies[2]).contains(HUMAN),
        "H6_L2_LATE_ACTUAL_POST_BOOKKEEPING_BODY"
    );
    after.release();
    assert!(
        tokio::time::timeout(K0_TIMEOUT, &mut turn)
            .await
            .unwrap()
            .is_ok(),
        "H6_L2_LATE_OUTCOME"
    );
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        history
            .iter()
            .filter(|item| item.text_content().contains(HUMAN))
            .count(),
        1,
        "H6_L2_LATE_ADMISSION_ONCE"
    );
    first.assert_satisfied();
    after.assert_satisfied();
    json!({"caller":"turn.rs:3924","new_late_admission":true,"older_held_first_release_credit":false,
        "barrier":"real transient Plan cleanup, then feedback-manager snapshot await",
        "producer":"SessionCommand::Interject through real run loop","new_prompt":false,"delivery":1})
}

async fn u6_wait_fixture() -> U6Fixture {
    use xai_grok_tools::implementations::grok_build::task::backend::SubagentBackendResource;
    let fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let bridge = fixture
        .fx
        .session
        .actor
        .agent
        .borrow()
        .tool_bridge()
        .clone();
    bridge
        .register_mcp_tools(
            "get_task_output".into(),
            xai_grok_tools::implementations::grok_build::task_output::TaskOutputTool,
            None,
        )
        .unwrap();
    let backend = fixture
        .fx
        .root
        .tool_bridge()
        .read_resource::<SubagentBackendResource>()
        .await
        .expect("H6_TW1_REAL_COORDINATOR_BACKEND");
    bridge.shared_resources().await.lock().await.insert(backend);
    fixture
}

fn u6_wait_frames(mut frames: Vec<Value>, id: &str, timeout_ms: u64) -> Vec<Value> {
    let terminal = frames
        .iter()
        .position(|frame| frame["type"] == "message_delta")
        .expect("H6_TW1_REAL_FRAME_TERMINAL");
    let index = frames
        .iter()
        .filter(|frame| frame["type"] == "content_block_start")
        .count();
    frames.splice(
        terminal..terminal,
        [
            json!({"type":"content_block_start","index":index,"content_block":{
            "type":"tool_use","id":id,"name":"get_task_output",
            "input":{"task_ids":[K2LG_N_WORKER],"timeout_ms":timeout_ms}}}),
            json!({"type":"content_block_stop","index":index}),
        ],
    );
    frames
}

async fn u6_wait_started(fx: &K2lgN, running: &mut U6Running) {
    u6_await_turn_progress(
        running,
        "TW-1 real wait in flight",
        "H6_TW1_REAL_WAIT_IN_FLIGHT",
        "H6_TW1_REAL_WAIT_IN_FLIGHT_TURN_ENDED",
        || async move { fx.session.actor.tool_context.blocking_wait_depth.depth() != 0 },
    )
    .await;
}

async fn u6_wait_result(fx: &K2lgN, running: &mut U6Running, id: &str) {
    u6_await_turn_progress(
        running,
        "TW-1 real wait outcome",
        "H6_TW1_REAL_WAIT_OUTCOME",
        "H6_TW1_REAL_WAIT_OUTCOME_TURN_ENDED",
        || async move {
            fx.session.actor.chat_state_handle.get_conversation().await.iter().any(|item| {
                matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == id)
            })
        },
    )
    .await;
}

async fn u6_tw1(resident_arrival: bool) -> Value {
    const HUMAN: &str = "U6 TW-1 resident versus custody";
    let mut fixture = u6_wait_fixture().await;
    let fx = &mut fixture.fx;
    let mut turn =
        u6_handoff_with_salvage(fx, None, None, resident_arrival.then_some(60_000), None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    if resident_arrival {
        u6_wait_started(fx, &mut turn.running).await;
        assert!(
            fx.session
                .actor
                .state
                .lock()
                .await
                .message_delivery
                .held_input
                .latch
                .is_some(),
            "H6_TW1_REAL_HOLD_ENGAGED"
        );
        u6_interject(fx, HUMAN, Vec::new()).await;
        u6_wait_result(fx, &mut turn.running, "client_u6_wait").await;
        assert_eq!(
            fx.session
                .actor
                .tool_context
                .blocking_wait_depth
                .interrupted_wait_ids(),
            Some(vec![K2LG_N_WORKER.to_owned()]),
            "H6_TW1_RESIDENT_INTERRUPTS_REAL_WAIT"
        );
        let mut continuation = fx.server.expect_response_blocked(
            "U6 TW-1 interrupted wait continuation",
            xai_grok_test_support::InferenceRequestMatcher::foreground(
                xai_grok_test_support::InferenceEndpoint::Messages,
            ),
            k2_script(k2lg_n_closure_frames()),
        );
        fx.server.enqueue_response(
            "/v1/messages",
            k2_script(k2lg_n_after_frames("msg_u6_tw1_arrival_after")),
        );
        u6_readable_checkpoint(
            fx,
            &mut turn.running,
            true,
            "H6_R15_TW1_READABLE_PENDING_CHECKPOINT",
        )
        .await;
        let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_TW1_ARRIVAL_RELOAD").await;
        fx.session.releases[1].add_permits(1);
        let continued = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                result = &mut turn.running => { let _ = result.unwrap(); false },
                _ = continuation.wait_blocked() => true,
            }
        })
        .await
        .expect("H6_TW1_ARRIVAL_CONTINUATION_RACE");
        assert!(
            !fx.session
                .actor
                .chat_state_handle
                .get_conversation()
                .await
                .iter()
                .any(|item| item.text_content().contains(HUMAN)),
            "H6_TW1_ARRIVAL_ADMITTED_TO_CUSTODY"
        );
        assert!(continued, "H6_TW1_ARRIVAL_RESULTS_ONLY_CONTINUATION");
        let bodies = k2lg_n_bodies(&fx.server);
        u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
        u6_release_after_checkpoint(
            fx,
            &mut turn.running,
            &continuation,
            false,
            "H6_R15_TW1_RESIDENT_CLOSED_CHECKPOINT",
        )
        .await;
        assert!(
            tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
                .await
                .unwrap()
                .unwrap()
                .is_ok(),
            "H6_TW1_ARRIVAL_FIRST_CLOSED_RELEASE"
        );
        continuation.assert_satisfied();
    } else {
        u6_interject(fx, HUMAN, Vec::new()).await;
        assert!(
            !k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
            "H6_TW1_CUSTODY_ADMISSION_WHILE_PENDING"
        );
        assert!(
            fx.session.actor.pending_interjections.is_empty(),
            "H6_TW1_CUSTODY_NOT_RESIDENT"
        );
        let mut continuation = fx.server.expect_response_blocked(
            "U6 TW-1 next pending handoff",
            xai_grok_test_support::InferenceRequestMatcher::foreground(
                xai_grok_test_support::InferenceEndpoint::Messages,
            ),
            k2_script(u6_wait_frames(
                u6_next_pending(true),
                "client_u6_wait_next",
                1_000,
            )),
        );
        fx.session.releases[1].add_permits(1);
        let continued = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                result = &mut turn.running => { let _ = result.unwrap(); false },
                _ = continuation.wait_blocked() => true,
            }
        })
        .await
        .expect("H6_TW1_CUSTODY_FIRST_CONTINUATION_RACE");
        assert!(continued, "H6_TW1_CUSTODY_FIRST_CONTINUATION");
        u6_release_after_checkpoint(
            fx,
            &mut turn.running,
            &continuation,
            true,
            "H6_R15_TW1_CUSTODY_NEXT_CHECKPOINT",
        )
        .await;
        u6_wait_started(fx, &mut turn.running).await;
        assert!(
            fx.session.actor.pending_interjections.is_empty(),
            "H6_TW1_NO_CUSTODY_WAKE"
        );
        u6_wait_result(fx, &mut turn.running, "client_u6_wait_next").await;
        assert!(
            fx.session
                .actor
                .tool_context
                .blocking_wait_depth
                .interrupted_wait_ids()
                .is_none(),
            "H6_TW1_CUSTODY_DOES_NOT_RETRIGGER"
        );
        u6_readable_checkpoint(
            fx,
            &mut turn.running,
            true,
            "H6_R15_TW1_READABLE_PENDING_CHECKPOINT",
        )
        .await;
        let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_TW1_CUSTODY_RELOAD").await;
        assert!(k2lg_n_live(&checkpoint), "H6_TW1_REAL_NEXT_PENDING_CALL");
        let mut closed = fx.server.expect_response_blocked(
            "U6 TW-1 custody wait continuation",
            xai_grok_test_support::InferenceRequestMatcher::foreground(
                xai_grok_test_support::InferenceEndpoint::Messages,
            ),
            k2_script(k2_frames(
                "msg_u6_tw1_closed",
                vec![
                    k2_result("srvtoolu_u6_next", "alpha__lookup"),
                    (
                        json!({"type":"text","text":"U6 TW-1 closes the exchange"}),
                        Vec::new(),
                    ),
                ],
                "end_turn",
            )),
        );
        fx.server.enqueue_response(
            "/v1/messages",
            k2_script(k2lg_n_after_frames("msg_u6_tw1_custody_after")),
        );
        fx.session.releases[1].add_permits(1);
        let continued = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                result = &mut turn.running => { let _ = result.unwrap(); false },
                _ = closed.wait_blocked() => true,
            }
        })
        .await
        .expect("H6_TW1_CUSTODY_NEXT_CONTINUATION_RACE");
        assert!(
            !fx.session
                .actor
                .chat_state_handle
                .get_conversation()
                .await
                .iter()
                .any(|item| item.text_content().contains(HUMAN)),
            "H6_TW1_CUSTODY_HOLD_ASSERTION"
        );
        assert!(continued, "H6_TW1_CUSTODY_RESULTS_ONLY_CONTINUATION");
        let bodies = k2lg_n_bodies(&fx.server);
        u6_assert_results_only(&checkpoint, &bodies[2], &bodies[3]);
        u6_release_after_checkpoint(
            fx,
            &mut turn.running,
            &closed,
            false,
            "H6_R15_TW1_CUSTODY_CLOSED_CHECKPOINT",
        )
        .await;
        assert!(
            tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
                .await
                .unwrap()
                .unwrap()
                .is_ok(),
            "H6_TW1_CUSTODY_FIRST_CLOSED_RELEASE"
        );
        closed.assert_satisfied();
        continuation.assert_satisfied();
    }
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        history
            .iter()
            .filter(|item| item.text_content().contains(HUMAN))
            .count(),
        1,
        "H6_TW1_ONCE_ONLY_RELEASE"
    );
    json!({"resident_arrival":resident_arrival,
        "tool":"get_task_output", "backend":"existing real SubagentCoordinator with scripted worker",
        "outcome":if resident_arrival { "interrupted by resident arrival" } else { "own bounded query timeout" },
        "custody_retriggers":false,"first_closed_release":1,"provider_or_model_calls":0})
}

#[derive(Clone, Default)]
struct U6TurnStepper(std::rc::Rc<U6StepState>);

#[derive(Default)]
struct U6StepState {
    paused: std::cell::Cell<bool>,
    permits: std::cell::Cell<u32>,
    finished: std::cell::Cell<bool>,
    wake: Arc<U6StepWake>,
    advanced: tokio::sync::Notify,
}

struct U6StepWake {
    ready: std::sync::atomic::AtomicBool,
    target: std::sync::Mutex<Option<std::task::Waker>>,
}

impl Default for U6StepWake {
    fn default() -> Self {
        Self {
            ready: std::sync::atomic::AtomicBool::new(true),
            target: Default::default(),
        }
    }
}

impl std::task::Wake for U6StepWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.ready.store(true, std::sync::atomic::Ordering::SeqCst);
        let target = self.target.lock().unwrap().clone();
        if let Some(target) = target {
            target.wake();
        }
    }
}

impl U6TurnStepper {
    fn wake_task(&self) {
        let target = self.0.wake.target.lock().unwrap().clone();
        if let Some(target) = target {
            target.wake();
        }
    }
    fn pause(&self) {
        self.0.paused.set(true);
    }
    fn resume(&self) {
        self.0.paused.set(false);
        self.wake_task();
    }
    async fn step(&self) -> bool {
        if self.0.finished.get() {
            return false;
        }
        self.0.permits.set(self.0.permits.get() + 1);
        self.wake_task();
        tokio::time::timeout(K0_TIMEOUT, self.0.advanced.notified())
            .await
            .expect("H6_U63_REAL_TURN_POLL_ADVANCES");
        !self.0.finished.get()
    }
}

struct U6SteppedTurn<F> {
    future: std::pin::Pin<Box<F>>,
    stepper: Option<U6TurnStepper>,
}
impl<F: std::future::Future> std::future::Future for U6SteppedTurn<F> {
    type Output = F::Output;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let stepper = self.stepper.clone();
        let result = if let Some(stepper) = &stepper {
            *stepper.0.wake.target.lock().unwrap() = Some(cx.waker().clone());
            if stepper.0.paused.get() {
                if stepper.0.permits.get() == 0
                    || !stepper
                        .0
                        .wake
                        .ready
                        .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return std::task::Poll::Pending;
                }
                stepper.0.permits.set(stepper.0.permits.get() - 1);
            }
            stepper
                .0
                .wake
                .ready
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let waker = std::task::Waker::from(stepper.0.wake.clone());
            let mut inner = std::task::Context::from_waker(&waker);
            self.future.as_mut().poll(&mut inner)
        } else {
            self.future.as_mut().poll(cx)
        };
        if let Some(stepper) = &stepper {
            if result.is_ready() {
                stepper.0.finished.set(true);
            }
            if stepper.0.paused.get() {
                stepper.0.advanced.notify_one();
            }
        }
        result
    }
}

async fn u6_step_to_http(
    stepper: &U6TurnStepper,
    expectation: &mut xai_grok_test_support::InferenceExpectation,
) {
    use futures_util::FutureExt;
    for _ in 0..256 {
        let reached = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                biased;
                _ = expectation.wait_blocked() => true,
                advanced = stepper.step() => {
                    assert!(advanced, "H6_U63_REAL_CONTINUATION_NOT_STRANDED");
                    false
                }
            }
        })
        .await
        .expect("H6_U63_HTTP_PHASE_OUTCOME_RACE");
        if reached {
            stepper.0.permits.set(0);
            let _ = stepper.0.advanced.notified().now_or_never();
            return;
        }
    }
    panic!("H6_U63_HTTP_STEP_BOUND");
}

fn u6_resident_keys(actor: &SessionActor) -> Vec<Value> {
    let owner = xai_interjection_core::events::next_occurrence();
    let view = actor.pending_interjections.release_view(owner, |_| true);
    view.iter().map(|(key, entry)| json!({"key":key.as_u64(),"text_digest":native_content_digest(entry.text.as_bytes()),
        "images":entry.attachments.len(),"image_digest":native_content_digest(&serde_json::to_vec(&entry.attachments).unwrap())})).collect()
}

async fn u6_queue_resident(fx: &K2lgN, text: &str, images: Vec<acp::ImageContent>) {
    let mut blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(
        text.to_owned(),
    ))];
    blocks.extend(images.into_iter().map(acp::ContentBlock::Image));
    let (respond_to, _response) = tokio::sync::oneshot::channel();
    let mut request = super::prompt_queue::QueueInputRequest::from_input_origin(
        blocks,
        format!("u6-queued-{}", uuid::Uuid::now_v7()),
        InputOrigin::new(PromptOrigin::User),
        PromptMode::Agent,
        respond_to,
    );
    request.client_identifier = fx
        .session
        .actor
        .state
        .lock()
        .await
        .pending_inputs
        .front()
        .and_then(|input| input.queue_meta.as_ref())
        .and_then(|meta| meta.owner.clone());
    assert!(
        !fx.session.actor.queue_input(request).await,
        "H6_U63_REAL_QUEUE_NO_CANCEL"
    );
    fx.session.actor.promote_queued_as_interjections().await;
    assert!(
        fx.session
            .actor
            .pending_interjections
            .snapshot()
            .iter()
            .any(|entry| entry.text == text),
        "H6_U63_REAL_PROMPT_QUEUE_TO_RESIDENT"
    );
}

#[derive(Clone)]
struct U6ParentInput {
    message_id: String,
    prompt_id: String,
    sender_session_id: String,
    text: Arc<str>,
}

struct U6ParentReceipt {
    input: U6ParentInput,
    result: tokio::sync::oneshot::Receiver<crate::session::commands::PromptTurnResult>,
}

async fn u6_admit_steer(actor: &Arc<SessionActor>, text: &str, sender: &str) -> U6ParentReceipt {
    use xai_grok_tools::implementations::grok_build::task::backend::{
        ChannelBackend, SubagentBackend,
    };
    let (deliveries, mut delivery_rx) = tokio::sync::mpsc::unbounded_channel();
    let started = std::rc::Rc::new(tokio::sync::Notify::new());
    let runner = K0SteerRunner {
        control: K0SteerControl(deliveries),
        started: started.clone(),
    };
    let (commands, receiver) = k0_coordinator::SubagentCoordinator::<K0SteerRunner>::channel();
    let task = tokio::task::spawn_local(
        k0_coordinator::SubagentCoordinator::from_channel(receiver, runner, Default::default())
            .run(),
    );
    let _coordinator = K0Abort(task.abort_handle());
    let (result_tx, _result_rx) = tokio::sync::oneshot::channel();
    commands
        .send(k0_task::SubagentEvent::Spawn(
            k0_task::SubagentSpawnRequest {
                request: Box::new(k0_task::SubagentRequest {
                    id: "k0-child".into(),
                    prompt: "in-process U6 Steer receiver".into(),
                    description: "U6 Steer fixture".into(),
                    subagent_type: "fixture".into(),
                    parent_session_id: sender.into(),
                    parent_prompt_id: None,
                    resume_from: None,
                    cwd: None,
                    runtime_overrides: Default::default(),
                    run_in_background: true,
                    surface_completion: false,
                    await_to_completion: false,
                    fork_context: false,
                    context: Default::default(),
                    owner: Default::default(),
                    cancel_token: Default::default(),
                    spawn_root: Default::default(),
                }),
                result_tx,
                registered_tx: None,
            },
        ))
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, started.notified())
        .await
        .unwrap();
    let actor = actor.clone();
    let owner = sender.to_owned();
    let delivery_task = tokio::task::spawn_local(async move {
        let (delivery, respond_to) = delivery_rx.recv().await.unwrap();
        let message = delivery.message().clone();
        let (receipts, mut receipt_rx) = tokio::sync::mpsc::channel(1);
        let (completion, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
        actor
            .admit_parent_agent_message(
                delivery,
                receipts,
                xai_grok_telemetry::TelemetryCtx::new(owner, Arc::new(tokio::sync::Mutex::new(0))),
                respond_to,
                completion,
            )
            .await;
        let receipt = receipt_rx.recv().await.expect("H6_U63_PARENT_REAL_RECEIPT");
        U6ParentReceipt {
            input: U6ParentInput {
                message_id: message.message_id,
                prompt_id: receipt.prompt_id,
                sender_session_id: message.sender_session_id,
                text: message.text,
            },
            result: receipt.result,
        }
    });
    let _delivery = K0Abort(delivery_task.abort_handle());
    let backend = ChannelBackend::for_coordinator_session(commands, sender);
    let outcome = tokio::time::timeout(
        K0_TIMEOUT,
        backend.send_active_message(
            k0_task::ActiveAgentMessageRequest::try_new_with_operation(
                "k0-child",
                text.to_owned(),
                k0_task::ActiveAgentMessageOperation::Steer,
            )
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, k0_task::ActiveAgentMessageOutcome::Accepted { .. }),
        "H6_U63_PARENT_REAL_ADMISSION"
    );
    delivery_task.await.unwrap()
}

async fn u6_u63_prepare_fixture(
    custody: Option<&str>,
) -> (
    U6Fixture,
    K2lgNTurn,
    U6TurnStepper,
    xai_grok_test_support::InferenceExpectation,
) {
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let stepper = U6TurnStepper::default();
    let mut turn =
        u6_handoff_with_salvage(&mut fixture.fx, None, None, None, Some(stepper.clone())).await;
    if let Some(text) = custody {
        u6_interject(&fixture.fx, text, Vec::new()).await;
        assert!(
            !k2lg_n_selected(
                &fixture.fx,
                fixture.fx.session.actor.drain_interjections_at_safe_point()
            )
            .await,
            "H6_U63_RAW_CUSTODY_ADMITTED"
        );
    }
    stepper.pause();
    let mut continuation = fixture.fx.server.expect_response_blocked(
        "U6 U63 real pending continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fixture.fx.session.releases[1].add_permits(1);
    u6_step_to_http(&stepper, &mut continuation).await;
    assert!(
        !turn.running.is_finished(),
        "H6_U63_REAL_CONTINUATION_IN_FLIGHT"
    );
    (fixture, turn, stepper, continuation)
}

async fn u6_u63_step_to_preparation(
    fixture: &U6Fixture,
    stepper: &U6TurnStepper,
    barrier_before: usize,
) {
    for _ in 0..256 {
        assert!(stepper.step().await, "H6_U63_PREPARATION_REACHABLE");
        if fixture.fx.session.actor.pending_interjections.is_empty() {
            assert_eq!(
                fixture
                    .persistence_control
                    .barriers
                    .load(std::sync::atomic::Ordering::SeqCst),
                barrier_before,
                "H6_U63_PREPARATION_NOT_PERSISTENCE_BARRIER"
            );
            return;
        }
    }
    panic!("H6_U63_PREPARATION_STEP_BOUND");
}

fn u6_image(side: u32) -> acp::ImageContent {
    let encoded = u6_png(side);
    acp::ImageContent::new(
        encoded
            .strip_prefix("data:image/png;base64,")
            .unwrap()
            .to_owned(),
        "image/png",
    )
}

async fn u6_l14() -> Value {
    const CUSTODY: &str = "U6 L14 raw custody";
    let (fixture, mut turn, stepper, continuation) = u6_u63_prepare_fixture(Some(CUSTODY)).await;
    let _stop = K0Abort(turn.running.abort_handle());
    u6_queue_resident(&fixture.fx, "U6 L14 resident image", vec![u6_image(47)]).await;
    let original = u6_resident_keys(&fixture.fx.session.actor);
    let before = u6_check_bands(&fixture.fx, &[]).await;
    let barriers = fixture.persistence_control.barriers.load(std::sync::atomic::Ordering::SeqCst);
    u6_release_after_checkpoint(&fixture.fx, &mut turn.running, &continuation, false,
        "H6_R15_L14_CLOSED_CHECKPOINT").await;
    u6_u63_step_to_preparation(&fixture, &stepper, barriers).await;
    let during = u6_check_bands(&fixture.fx, &[]).await;
    let raw_shared = {
        let state = fixture.fx.session.actor.state.lock().await;
        state.message_delivery.held_input.entries.iter().any(|entry|
            matches!(&entry.body, super::interjection::HeldBody::Human(raw)
                if raw.text == CUSTODY && Arc::strong_count(raw) > 1))
    };
    assert_eq!(during, before, "H6_L14_BANDS_UNCHANGED_DURING_PREPARATION");
    u6_interject(&fixture.fx, "U6 L14 later resident", Vec::new()).await;
    let later = u6_resident_keys(&fixture.fx.session.actor);
    turn.running.abort();
    assert!(matches!(turn.running.await, Err(error) if error.is_cancelled()), "H6_L14_REAL_PREPARATION_ABORT");
    let restored = u6_resident_keys(&fixture.fx.session.actor);
    let expected: Vec<_> = original.into_iter().chain(later).collect();
    assert_eq!(restored, expected, "H6_L14_ORIGINAL_KEYS_FRONT_ORDER_ATTACHMENTS");
    let state = fixture.fx.session.actor.state.lock().await;
    let ledger = &state.message_delivery.held_input;
    assert!(ledger.entries.iter().any(|entry|
        matches!(&entry.body, super::interjection::HeldBody::Human(raw) if raw.text == CUSTODY)),
        "H6_L14_RAW_CUSTODY_NEVER_LEFT");
    assert_eq!(ledger.bands(fixture.fx.session.actor.live_parent_steer_slots(&state)), before,
        "H6_L14_ABORT_CLEARS_PREPARED_CHARGE");
    assert!(ledger.entries.iter().filter_map(|entry| entry.preparing.as_ref()).all(|preparing|
        !preparing.load(std::sync::atomic::Ordering::Acquire)), "H6_L14_PREPARATION_OWNER_RELEASED");
    assert!(raw_shared, "H6_L14_ACTUAL_RAW_PREPARATION_REFERENCE");
    drop(state);
    u6_observe(json!({"stage":"L14 real preparation abort","prepared_raw_reference":raw_shared,
        "bands_unchanged":during == before,
        "barriers_sent":fixture.persistence_control.barriers.load(std::sync::atomic::Ordering::SeqCst)-barriers}));
    assert!(!fixture.persistence_control.updates.lock().unwrap().iter().any(|update|
        update.to_string().contains(CUSTODY) || update.to_string().contains("U6 L14 resident image")),
        "H6_L14_NO_HUMAN_PERSISTENCE");
    json!({"await":"merged release prepare_interjection_images; fresh resident PNG, guard held and no FlushAndAck",
        "raw_custody_retained":true,"resident_order_restored":true,"bands_unchanged":true,
        "prepared_raw_reference":raw_shared})
}

async fn u6_l14c(straddling: bool) -> Value {
    let early = if straddling {
        "U6 L14c H0"
    } else {
        "U6 L14c H1"
    };
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let stepper = U6TurnStepper::default();
    let mut turn = u6_handoff_with_salvage(fx, None, None, None, Some(stepper.clone())).await;
    let _stop = K0Abort(turn.running.abort_handle());
    if straddling {
        u6_interject(fx, early, Vec::new()).await;
    }
    let mut parents = Vec::new();
    if straddling {
        parents.push(u6_admit_steer(&fx.session.actor, "U6 L14c S_a", "u6-parent").await);
    }
    assert!(
        !k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_L14C_REAL_HELD_ADMISSION"
    );
    stepper.pause();
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L14c real closure",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    u6_step_to_http(&stepper, &mut continuation).await;
    u6_queue_resident(
        fx,
        if straddling { "U6 L14c I" } else { early },
        if straddling {
            vec![u6_image(49)]
        } else {
            Vec::new()
        },
    )
    .await;
    let image_key = straddling.then(|| u6_resident_keys(&fx.session.actor)[0].clone());
    let barriers = fixture
        .persistence_control
        .barriers
        .load(std::sync::atomic::Ordering::SeqCst);
    u6_release_after_checkpoint(
        &fixture.fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L14C_CLOSED_CHECKPOINT",
    )
    .await;
    u6_u63_step_to_preparation(&fixture, &stepper, barriers).await;
    u6_interject(&fixture.fx, "U6 L14c H2", Vec::new()).await;
    u6_monitor(&fixture.fx, "u6-l14c", "U6 L14c M").await;
    parents.push(
        u6_admit_steer(
            &fixture.fx.session.actor,
            if straddling {
                "U6 L14c S_b"
            } else {
                "U6 L14c late Steer"
            },
            "u6-parent",
        )
        .await,
    );
    let mut first = Vec::new();
    for _ in 0..256 {
        assert!(stepper.step().await, "H6_L14C_FIRST_PREFIX_PROGRESS");
        first = fixture
            .fx
            .session
            .actor
            .chat_state_handle
            .get_conversation()
            .await;
        if first.iter().any(|item| item.text_content().contains(early)) {
            break;
        }
    }
    let labels = if straddling {
        vec![
            early,
            "U6 L14c S_a",
            "U6 L14c I",
            "U6 L14c H2",
            "U6 L14c M",
            "U6 L14c S_b",
        ]
    } else {
        vec![early, "U6 L14c H2", "U6 L14c M", "U6 L14c late Steer"]
    };
    let first_labels: Vec<_> = first
        .iter()
        .flat_map(|item| {
            labels
                .iter()
                .copied()
                .filter(move |label| item.text_content().contains(label))
        })
        .collect();
    u6_observe(
        json!({"stage":"L14c first logical prefix","straddling":straddling,"labels":first_labels}),
    );
    assert_eq!(first_labels, vec![early], "H6_L14C_FIRST_PREFIX_CUTOFF");
    if let Some(key) = &image_key {
        assert!(
            u6_resident_keys(&fixture.fx.session.actor).contains(key),
            "H6_L14C_IMAGE_SUFFIX_RESTORED_KEYED"
        );
    }
    fixture.fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_u6_l14c_after")),
    );
    stepper.resume();
    assert!(
        tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
            .await
            .unwrap()
            .unwrap()
            .is_ok(),
        "H6_L14C_SECOND_RELEASE_PROGRESS"
    );
    let after = fixture
        .fx
        .session
        .actor
        .chat_state_handle
        .get_conversation()
        .await;
    let observed: Vec<_> = after
        .iter()
        .flat_map(|item| {
            labels
                .iter()
                .copied()
                .filter(move |label| item.text_content().contains(label))
        })
        .collect();
    assert_eq!(observed, labels, "H6_L14C_GLOBAL_ORDER_ACROSS_PREFIXES");
    if straddling {
        let bodies = k2lg_n_bodies(&fixture.fx.server);
        assert_eq!(
            bodies.last().unwrap()["messages"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|part| part["type"] == "image")
                .count(),
            1,
            "H6_L14C_IMAGE_REACHES_SECOND_RELEASE"
        );
    }
    continuation.assert_satisfied();
    json!({"straddling":straddling,"cutoff":"earliest unprepared human, lowered to whole parent binding",
        "preparation_await":if straddling { "fresh image pipeline" } else { "current_model_id oneshot" },
        "first_prefix":first_labels,"final_order":observed,"new_prompt":false})
}

fn u6_source_name(source: super::interjection::HeldSource) -> &'static str {
    use super::interjection::HeldSource;
    match source {
        HeldSource::HumanInterjection => "human_interjection",
        HeldSource::ParentSteer => "parent_steer",
        HeldSource::MonitorEvent => "monitor_event",
        HeldSource::DeferredFollowUp => "deferred_followup",
        HeldSource::StationarityReminder => "stationarity_reminder",
        HeldSource::SkillReminder => "skill_reminder",
        HeldSource::PlanActivation => "plan_activation",
        HeldSource::PeerAgent => "peer_agent",
        HeldSource::MediaResample => "media_resample",
        HeldSource::ToolFreeLength => "tool_free_length",
        HeldSource::LengthReminder => "length_reminder",
    }
}

async fn u6_check_bands(
    fx: &K2lgN,
    _parents: &[U6ParentReceipt],
) -> super::interjection::HeldBands {
    use super::interjection::{HeldBand, HeldSource};
    let state = fx.session.actor.state.lock().await;
    let ledger = &state.message_delivery.held_input;
    assert!(ledger.refusal.is_none(), "H6_N10_UNREFUSED_MIXED_FIXTURE");
    let count = |source| ledger.entries.iter().filter(|entry| entry.source == source).count();
    let band = |source, limit| HeldBand { held: count(source), limit, refused_total: 0 };
    let monitor_pins = match (ledger.pin_owner, fx.session.actor.tool_context.monitor_event_buffer.as_ref()) {
        (Some(owner), Some(buffer)) => buffer.release_view(owner, |_| false).iter().count(),
        _ => 0,
    };
    let live = fx.session.actor.live_parent_steer_slots(&state);
    let actual = ledger.bands(live);
    let expected = [
        band(HeldSource::HumanInterjection, None),
        HeldBand { held: live, limit: Some(32), refused_total: 0 },
        HeldBand { held: monitor_pins, limit: Some(128), refused_total: 0 },
        band(HeldSource::DeferredFollowUp, Some(128)),
        band(HeldSource::StationarityReminder, Some(128)),
        band(HeldSource::SkillReminder, Some(128)),
        band(HeldSource::PlanActivation, Some(1)),
    ];
    assert_eq!([actual.human, actual.steer, actual.monitor, actual.deferred, actual.stationarity, actual.skill, actual.plan],
        expected, "H6_N10_BAND_STORE_RECOUNT");
    assert_eq!(actual.parent_stamps, ledger.parent_stamps.len(), "H6_N10_STAMP_RECOUNT");
    actual
}

async fn u6_parent_stamp_preservation() -> Value {
    let (fixture, mut turn, _, continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    let mut parents = Vec::new();
    for index in 0..32 {
        let before = u6_check_bands(fx, &parents).await;
        let mut parent = u6_admit_steer(&fx.session.actor, "U6 L10 parent payload fixed",
            &format!("u6-parent-{index:04}-{}", "p".repeat(index * 20))).await;
        assert!(parent.result.try_recv().is_err(), "H6_L10_NO_EARLY_PARENT_DELIVERY_ACK");
        parents.push(parent);
        assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
            "H6_L10_PARENT_HOLD");
        let after = u6_check_bands(fx, &parents).await;
        assert_eq!(after.steer.held, before.steer.held + 1, "H6_L10_REAL_PARENT_STAMP_ADMISSION");
    }
    assert_eq!(fx.session.actor.state.lock().await.message_delivery.held_input.parent_stamps.len(),
        32, "H6_L10_ALL_REAL_PARENT_STAMPS");
    continuation.release();
    turn.running.abort();
    let _ = turn.running.await;
    json!({"mode":"parent-stamps","kind":"existing all-mode stamp-bound preservation before U6.6 requalification",
        "real_admissions":32,"unsubmitted_parent_receipts":32})
}

async fn u6_l10b() -> Value {
    let (mut fixture, mut turn, stepper, continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &mut fixture.fx;
    let image = u6_image(220);
    assert!(
        serde_json::to_vec(&image).unwrap().len() > 32_768,
        "H6_L10B_REAL_OVERSIZED_RESIDENT_IMAGE"
    );
    u6_queue_resident(fx, "U6 L10b resident image", vec![image]).await;
    let original = u6_resident_keys(&fx.session.actor);
    u6_monitor(fx, "u6-l10b", "U6 L10b pinned monitor").await;
    k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
    assert!(fx.session.actor.pending_interjections.is_empty(), "H6_L10B_IMAGE_MOVED_TO_CUSTODY");
    let image_key = original.first().and_then(|entry| entry["key"].as_u64()).expect("H6_L10B_ORIGINAL_IMAGE_KEY");
    let state = fx.session.actor.state.lock().await;
    let humans = state.message_delivery.held_input.entries.iter()
        .filter(|entry| entry.source == super::interjection::HeldSource::HumanInterjection).collect::<Vec<_>>();
    assert_eq!(humans.len(), 1, "H6_L10B_ONE_HELD_HUMAN_IMAGE");
    assert_eq!(humans[0].key.as_u64(), image_key, "H6_L10B_IMAGE_ORIGINAL_OCCURRENCE");
    let super::interjection::HeldBody::Human(raw) = &humans[0].body else { panic!("H6_L10B_ORIGINAL_RAW_IMAGE"); };
    assert_eq!(native_content_digest(&serde_json::to_vec(&raw.attachments).unwrap()),
        original[0]["image_digest"].as_str().unwrap(), "H6_L10B_ORIGINAL_IMAGE_BYTES");
    assert!(
        state
            .message_delivery
            .held_input
            .entries
            .iter()
            .any(|entry| entry.source == super::interjection::HeldSource::MonitorEvent),
        "H6_L10B_LATER_MONITOR_PINNED"
    );
    assert!(state.message_delivery.held_input.entries.iter().any(|entry|
        entry.source == super::interjection::HeldSource::MonitorEvent && entry.key.as_u64() > image_key),
        "H6_L10B_IMAGE_KEY_BEFORE_MONITOR");
    drop(state);
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_u6_l10b_after")),
    );
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L10B_CLOSED_CHECKPOINT",
    )
    .await;
    stepper.resume();
    assert!(
        tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
            .await
            .unwrap()
            .unwrap()
            .is_ok(),
        "H6_L10B_SAME_TURN_RELEASE_PROGRESS"
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(
        bodies.len(),
        4,
        "H6_L10B_FIRST_RELEASE_NO_CANCEL_PROMPT_FALLBACK"
    );
    let blocks = bodies.last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"]
        .as_array()
        .unwrap();
    let image_at = blocks
        .iter()
        .position(|part| part["type"] == "image")
        .expect("H6_L10B_IMAGE_DELIVERED");
    let monitor_at = blocks
        .iter()
        .position(|part| {
            part["text"]
                .as_str()
                .is_some_and(|text| text.contains("U6 L10b pinned monitor"))
        })
        .expect("H6_L10B_MONITOR_DELIVERED");
    assert!(
        image_at < monitor_at,
        "H6_L10B_IMAGE_BEFORE_LATER_PINNED_MONITOR"
    );
    continuation.assert_satisfied();
    json!({"producer":"QueueInputRequest->queue_input->promote_queued_as_interjections (prompt_queue 638-655)",
        "oversized_resident_preparation_not_held_admission":true,"order":"image,monitor", "new_prompt_or_cancel":false})
}

async fn u6_step_to_persistence_barrier(stepper: &U6TurnStepper, control: &U6PersistenceControl) {
    use futures_util::FutureExt;
    for _ in 0..256 {
        if !control.held_barriers.lock().unwrap().is_empty() {
            return;
        }
        let reached = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                biased;
                _ = control.barrier_held.notified() => true,
                advanced = stepper.step() => {
                    assert!(advanced, "H6_L14B_REAL_BARRIER_TURN_ADVANCE");
                    false
                }
            }
        })
        .await
        .expect("H6_L14B_REAL_PERSISTENCE_BARRIER_RACE");
        if reached {
            stepper.0.permits.set(0);
            let _ = stepper.0.advanced.notified().now_or_never();
            assert!(
                !control.held_barriers.lock().unwrap().is_empty(),
                "H6_L14B_TEST_OWNS_REAL_FLUSH_REPLY"
            );
            return;
        }
    }
    panic!("H6_L14B_REAL_BARRIER_STEP_BOUND");
}

fn u6_forward_held_barrier(fixture: &U6Fixture) {
    let message = fixture
        .persistence_control
        .held_barriers
        .lock()
        .unwrap()
        .remove(0);
    fixture
        .fx
        .session
        .persistence
        .tx
        .send(message)
        .expect("H6_L14B_REAL_PERSISTENCE_RESTORED");
}

fn u6_fail_held_barrier(fixture: &U6Fixture) {
    let message = fixture
        .persistence_control
        .held_barriers
        .lock()
        .unwrap()
        .remove(0);
    let PersistenceMsg::FlushAndAck { respond_to } = message else {
        panic!("H6_L14B_EXPECTED_REAL_FLUSH");
    };
    let _ = respond_to.send(Err(std::io::Error::other(
        "U6 test-owned persistence unavailable",
    )));
}

fn u6_user_persists(control: &U6PersistenceControl, needle: &str) -> usize {
    control
        .user_chunks
        .lock()
        .unwrap()
        .iter()
        .filter(|update| update.to_string().contains(needle))
        .count()
}

async fn u6_mixed_keys(fx: &K2lgN) -> Vec<Value> {
    let state = fx.session.actor.state.lock().await;
    state
        .message_delivery
        .held_input
        .entries
        .iter()
        .map(|entry| json!({"key":entry.key.as_u64(),"source":u6_source_name(entry.source)}))
        .collect()
}

struct U6MixedFixture {
    fixture: U6Fixture,
    turn: K2lgNTurn,
    stepper: U6TurnStepper,
    continuation: xai_grok_test_support::InferenceExpectation,
    parent: U6ParentReceipt,
    keys: Vec<Value>,
    resident_keys: Vec<Value>,
    plan_text: String,
    announcements_before: usize,
    plan_writes_before: usize,
    monitor_keys: Vec<Value>,
}

async fn u6_l14b_fixture() -> U6MixedFixture {
    use crate::tools::todo::{TodoItem, TodoPriority, TodoState, TodoStatus};
    use xai_grok_tools::types::resources::State as ResourceState;
    let control = U6PersistenceControl {
        bookkeeping_feedback: true,
        plan_tools: true,
        ..Default::default()
    };
    let mut fixture = u6_fixture_controlled(
        Some(Arc::<str>::from(u6_png(32))),
        K2lgNOptions::default(),
        control,
    )
    .await;
    let fx = &mut fixture.fx;
    let bridge = fx.session.actor.agent.borrow().tool_bridge().clone();
    let mut todos = TodoState::default();
    todos.push(
        "u6-bookkeeping".into(),
        TodoItem {
            content: "U6 bookkeeping observer".into(),
            priority: TodoPriority::Medium,
            status: TodoStatus::InProgress,
            meta: None,
        },
    );
    bridge
        .shared_resources()
        .await
        .lock()
        .await
        .insert(ResourceState(todos));
    let stepper = U6TurnStepper::default();
    let mut turn = u6_handoff_with_salvage(fx, None, None, None, Some(stepper.clone())).await;
    u6_interject(fx, "U6 L14b custody human", Vec::new()).await;
    let parent = u6_admit_steer(&fx.session.actor, "U6 L14b parent zero", "u6-parent").await;
    let (responds_to, replied) = tokio::sync::oneshot::channel();
    fx.loop_tx
        .send(SessionCommand::SessionMode {
            session_mode: acp::SessionModeId::new("plan"),
            responds_to,
        })
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, replied)
        .await
        .expect("H6_L14B_REAL_PLAN_COMMAND")
        .unwrap();
    fx.session
        .actor
        .apply_skill_update_effects(
            xai_grok_tools::types::skill_discovery_tracker::SkillUpdateEffects {
                system_reminder: Some("U6 L14b skill reminder".into()),
                send_available_commands: false,
                kind: xai_grok_tools::types::skill_discovery_tracker::SkillUpdateKind::Discovery,
            },
        )
        .await;
    u6_monitor(fx, "u6-l14b", "U6 L14b pinned monitor").await;
    assert!(
        !k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_L14B_REAL_MIXED_HELD_ADMISSION"
    );
    k2lg_n_selected(fx, fx.session.actor.flush_skills_at_native_safe_point()).await;
    stepper.pause();
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L14b results-only closure",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    u6_step_to_http(&stepper, &mut continuation).await;
    let keys = u6_mixed_keys(fx).await;
    for source in [
        "human_interjection",
        "parent_steer",
        "deferred_followup",
        "monitor_event",
        "skill_reminder",
        "plan_activation",
    ] {
        assert!(
            keys.iter().any(|entry| entry["source"] == source),
            "H6_L14B_EACH_REAL_READY_SOURCE"
        );
    }
    let plan_text = fx
        .session
        .actor
        .plan_mode
        .lock()
        .pending_activation_text()
        .expect("H6_L14B_REAL_PENDING_ACTIVATION")
        .to_owned();
    let announcements_before = fixture
        .persistence_control
        .announcement_updates
        .load(std::sync::atomic::Ordering::SeqCst);
    let plan_writes_before = fixture
        .persistence_control
        .plan_updates
        .load(std::sync::atomic::Ordering::SeqCst);
    let monitor_keys = u6_pinned_monitor_keys(fx).await;
    assert_eq!(
        monitor_keys.len(),
        1,
        "H6_L14B_REAL_PINNED_MONITOR_AUTHORITY"
    );
    u6_queue_resident(fx, "U6 L14b guarded resident", Vec::new()).await;
    let resident_keys = u6_resident_keys(&fx.session.actor);
    let barriers = fixture
        .persistence_control
        .barriers
        .load(std::sync::atomic::Ordering::SeqCst);
    u6_release_after_checkpoint(
        &fixture.fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L14B_CLOSED_CHECKPOINT",
    )
    .await;
    u6_u63_step_to_preparation(&fixture, &stepper, barriers).await;
    fixture
        .persistence_control
        .hold_barriers
        .store(true, std::sync::atomic::Ordering::SeqCst);
    u6_step_to_persistence_barrier(&stepper, &fixture.persistence_control).await;
    U6MixedFixture {
        fixture,
        turn,
        stepper,
        continuation,
        parent,
        keys,
        resident_keys,
        plan_text,
        announcements_before,
        plan_writes_before,
        monitor_keys,
    }
}

async fn u6_l14b_closed(latch_state: &str) -> Value {
    let mut mixed = u6_l14b_fixture().await;
    let _stop = K0Abort(mixed.turn.running.abort_handle());
    let resident_before = mixed.resident_keys.clone();
    let barrier_before = mixed
        .fixture
        .persistence_control
        .barriers
        .load(std::sync::atomic::Ordering::SeqCst);
    u6_readable_checkpoint(
        &mixed.fixture.fx,
        &mut mixed.turn.running,
        false,
        "H6_R15_L14B_PRE_CLOSE_CHECKPOINT",
    )
    .await;
    k2lg_n_close_mailbox(&mixed.fixture.fx).await;
    u6_forward_held_barrier(&mixed.fixture);
    u6_step_to_persistence_barrier(&mixed.stepper, &mixed.fixture.persistence_control).await;
    assert!(
        mixed
            .fixture
            .persistence_control
            .cleanup_count
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0,
        "H6_L14B_REAL_BOOKKEEPING_BETWEEN_ATTEMPTS"
    );
    assert_eq!(
        mixed
            .fixture
            .persistence_control
            .barriers
            .load(std::sync::atomic::Ordering::SeqCst),
        barrier_before + 1,
        "H6_L14B_REAL_PRECOMP_THEN_POST_BOOKKEEPING"
    );
    let progress_before_second = {
        let mut state = mixed.fixture.fx.session.actor.state.lock().await;
        let ledger = &mut state.message_delivery.held_input;
        let progress = ledger.parent_persisted;
        if latch_state == "absent" {
            ledger.latch = None;
            ledger.latch_meta = None;
        }
        if latch_state == "inert" {
            if let Some(latch) = &mut ledger.latch {
                latch.binding = xai_message_delivery_core::TurnBinding::new(
                    "U6 constructed inert latch".into(),
                    mixed.turn.epoch,
                );
            }
            if let Some(meta) = &mut ledger.latch_meta {
                meta.prompt_id = "U6 constructed inert latch".into();
            }
        }
        ledger.refresh_header();
        progress
    };
    let mut second = u6_admit_steer(
        &mixed.fixture.fx.session.actor,
        "U6 L14b parent one",
        "u6-parent",
    )
    .await;
    mixed
        .fixture
        .persistence_control
        .hold_barriers
        .store(false, std::sync::atomic::Ordering::SeqCst);
    u6_forward_held_barrier(&mixed.fixture);
    mixed.stepper.resume();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut mixed.turn.running)
        .await
        .expect("H6_L14B_REAL_SECOND_ATTEMPT_FINISH")
        .unwrap();
    assert!(
        outcome.is_ok(),
        "H6_L14B_BOOKKEEPING_RETURNS_WITH_CLOSED_CHAT"
    );
    let (respond_to, observed) = tokio::sync::oneshot::channel();
    mixed
        .fixture
        .fx
        .session
        .actor
        .notifications
        .persistence_tx
        .send(PersistenceMsg::FlushAndAck { respond_to })
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, observed)
        .await
        .expect("H6_L14B_FAILED_ATTEMPTS_RECEIVER_FENCE")
        .unwrap()
        .unwrap();
    assert_eq!(
        u6_pinned_monitor_keys(&mixed.fixture.fx).await,
        mixed.monitor_keys,
        "H6_L14B_PINNED_MONITOR_AUTHORITY_RETAINED"
    );
    assert_eq!(
        mixed
            .fixture
            .persistence_control
            .plan_updates
            .load(std::sync::atomic::Ordering::SeqCst),
        mixed.plan_writes_before,
        "H6_L14B_NO_PLAN_DELIVERY_EFFECTS_ON_FAILURE"
    );
    assert_eq!(
        u6_user_persists(&mixed.fixture.persistence_control, "U6 L14b custody human"),
        0,
        "H6_L14B_NO_HUMAN_PERSIST_BEFORE_FAILED_SUBMIT"
    );
    assert_eq!(
        u6_user_persists(
            &mixed.fixture.persistence_control,
            "U6 L14b guarded resident"
        ),
        0,
        "H6_L14B_NO_RESIDENT_PERSIST_ON_FAILED_SUBMIT"
    );
    assert_eq!(
        u6_mixed_keys(&mixed.fixture.fx).await,
        mixed.keys,
        "H6_L14B_EACH_CUSTODY_KEY_RETAINED"
    );
    assert_eq!(
        u6_resident_keys(&mixed.fixture.fx.session.actor),
        resident_before,
        "H6_L14B_GUARDED_RESIDENT_RESTORED"
    );
    assert_eq!(
        mixed
            .fixture
            .fx
            .session
            .actor
            .plan_mode
            .lock()
            .pending_activation_text(),
        Some(mixed.plan_text.as_str()),
        "H6_L14B_REVOCABLE_PLAN_RETAINED_ON_FAILURE"
    );
    assert_eq!(
        mixed
            .fixture
            .persistence_control
            .announcement_updates
            .load(std::sync::atomic::Ordering::SeqCst),
        mixed.announcements_before,
        "H6_L14B_NO_SKILL_DELIVERY_EFFECTS_ON_FAILURE"
    );
    assert!(
        mixed.parent.result.try_recv().is_err() && second.result.try_recv().is_err(),
        "H6_L14B_NO_PARENT_DELIVERY_ACK_ON_FAILURE"
    );
    let first_count = u6_user_persists(&mixed.fixture.persistence_control, "U6 L14b parent zero");
    let second_count = u6_user_persists(&mixed.fixture.persistence_control, "U6 L14b parent one");
    u6_observe(
        json!({"stage":"L14b two real failed attempts","latch_state":latch_state,
        "first_parent_persists":first_count,"second_parent_persists":second_count,
        "progress_before_second":progress_before_second,"bookkeeping_observed":true,
        "callers":["turn.rs:3913","turn.rs:3924"]}),
    );
    assert_eq!(first_count, 1, "H6_L14B_PARENT_PREFIX_PERSISTED_ONCE");
    assert_eq!(second_count, 1, "H6_L14B_SECOND_STEER_UNSENT_SUFFIX_ONCE");
    assert_eq!(
        progress_before_second, 1,
        "H6_L14B_PROGRESS_OWNED_BY_ACTUAL_DELIVERY"
    );
    assert_eq!(
        mixed.fixture.fx.server.messages_request_count(),
        3,
        "H6_L14B_NO_FAILED_RELEASE_HTTP"
    );
    let parents = [mixed.parent, second];
    let _bands = u6_check_bands(&mixed.fixture.fx, &parents).await;
    mixed.continuation.assert_satisfied();
    json!({"latch_state":latch_state,"real_precomp_and_post_bookkeeping":true,"second_steer_between_attempts":true,
        "closed_mailbox_reopened":false,"human_persistence":0,"parent_persistence_per_identity":1})
}

async fn u6_l14b_persistence_recovery() -> Value {
    let mut mixed = u6_l14b_fixture().await;
    let _stop = K0Abort(mixed.turn.running.abort_handle());
    u6_fail_held_barrier(&mixed.fixture);
    u6_step_to_persistence_barrier(&mixed.stepper, &mixed.fixture.persistence_control).await;
    assert!(
        mixed
            .fixture
            .persistence_control
            .cleanup_count
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0,
        "H6_L14B_PERSISTENCE_FAILURE_REAL_BOOKKEEPING"
    );
    for text in [
        "U6 L14b parent zero",
        "U6 L14b custody human",
        "U6 L14b guarded resident",
    ] {
        assert_eq!(
            u6_user_persists(&mixed.fixture.persistence_control, text),
            0,
            "H6_L14B_UNAVAILABLE_PERSISTENCE_ATTEMPT_HAS_NO_CHUNK"
        );
    }
    assert_eq!(
        u6_mixed_keys(&mixed.fixture.fx).await,
        mixed.keys,
        "H6_L14B_PERSISTENCE_FAILURE_PREFIX_RETAINED"
    );
    let mut released_request = mixed.fixture.fx.server.expect_response_blocked(
        "U6 L14b first successful release request",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_after_frames("msg_u6_l14b_recovered")),
    );
    mixed
        .fixture
        .persistence_control
        .hold_barriers
        .store(false, std::sync::atomic::Ordering::SeqCst);
    u6_forward_held_barrier(&mixed.fixture);
    mixed.stepper.resume();
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            biased;
            _ = released_request.wait_blocked() => (),
            _ = &mut mixed.turn.running => panic!("H6_L14B_RECOVERY_FIRST_RELEASE_HTTP"),
        }
    })
    .await
    .expect("H6_L14B_RECOVERY_FIRST_RELEASE_HTTP_BOUND");
    let bodies = k2lg_n_bodies(&mixed.fixture.fx.server);
    let released_text = u6_body_text(bodies.last().unwrap());
    for text in [
        "U6 L14b parent zero",
        "U6 L14b custody human",
        "U6 L14b guarded resident",
        "Image extracted from tool result",
        "U6 L14b pinned monitor",
        "U6 L14b skill reminder",
        mixed.plan_text.as_str(),
    ] {
        assert_eq!(
            released_text.matches(text).count(),
            1,
            "H6_L14B_FIRST_RELEASE_EACH_READY_SOURCE_ONCE"
        );
    }
    released_request.release();
    assert!(
        tokio::time::timeout(K0_TIMEOUT, &mut mixed.turn.running)
            .await
            .unwrap()
            .unwrap()
            .is_ok(),
        "H6_L14B_RESTORED_TEST_PERSISTENCE_SUCCESS"
    );
    released_request.assert_satisfied();
    let history = mixed
        .fixture
        .fx
        .session
        .actor
        .chat_state_handle
        .get_conversation()
        .await;
    for text in [
        "U6 L14b parent zero",
        "U6 L14b custody human",
        "U6 L14b guarded resident",
        "Image extracted from tool result",
        "U6 L14b pinned monitor",
        "U6 L14b skill reminder",
        mixed.plan_text.as_str(),
    ] {
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(text))
                .count(),
            1,
            "H6_L14B_RECOVERED_EACH_SOURCE_ONCE"
        );
    }
    assert!(
        mixed
            .fixture
            .fx
            .session
            .actor
            .plan_mode
            .lock()
            .pending_activation_text()
            .is_none(),
        "H6_L14B_PLAN_CONSUMED_ONLY_AFTER_SUCCESS"
    );
    assert_eq!(
        u6_user_persists(&mixed.fixture.persistence_control, "U6 L14b custody human"),
        1,
        "H6_L14B_HUMAN_PERSIST_ONLY_AFTER_SUCCESS"
    );
    mixed.continuation.assert_satisfied();
    json!({"first_persistence_barrier":"real receiver error","second":"restored test-owned persistence",
        "real_callers":["turn.rs:3913","turn.rs:3924"],"each_ready_source_once":true})
}

async fn u6_l14b_command_cancel() -> Value {
    let mut mixed = u6_l14b_fixture().await;
    let _stop = K0Abort(mixed.turn.running.abort_handle());
    k2lg_n_close_mailbox(&mixed.fixture.fx).await;
    u6_forward_held_barrier(&mixed.fixture);
    u6_step_to_persistence_barrier(&mixed.stepper, &mixed.fixture.persistence_control).await;
    let old_progress = mixed
        .fixture
        .fx
        .session
        .actor
        .state
        .lock()
        .await
        .message_delivery
        .held_input
        .parent_persisted;
    let old_epoch = mixed.turn.epoch;
    let (parent_entries_before_cancel, old_parent_ids) = {
        let state = mixed.fixture.fx.session.actor.state.lock().await;
        let ids: Vec<_> = state.message_delivery.held_input.entries.iter()
            .filter_map(|entry| match &entry.body {
                super::interjection::HeldBody::Parent(identity) => Some(identity.clone()),
                _ => None,
            }).collect();
        (ids.len(), ids)
    };
    mixed
        .fixture
        .fx
        .loop_tx
        .send(SessionCommand::Cancel(crate::session::CancelOptions {
            user_initiated: true,
            trigger: Some(crate::session::CancelTrigger::CtrlC),
            ..Default::default()
        }))
        .unwrap();
    let joined = tokio::time::timeout(K0_TIMEOUT, &mut mixed.turn.running)
        .await
        .expect("H6_L14B_REAL_COMMAND_CANCEL_FINISH");
    assert!(
        matches!(joined, Err(error) if error.is_cancelled()),
        "H6_L14B_COMMAND_CANCEL_ABORTS_ACTUAL_TURN"
    );
    tokio::time::timeout(K0_TIMEOUT, async {
        while mixed
            .fixture
            .fx
            .session
            .actor
            .state
            .lock()
            .await
            .running_task
            .is_some()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("H6_L14B_COMMAND_CANCEL_SETTLES_OLD_OWNER");
    let old_parent_result = tokio::time::timeout(K0_TIMEOUT, &mut mixed.parent.result)
        .await
        .expect("H6_L14B_OLD_PARENT_TERMINAL_RECEIPT")
        .expect("H6_L14B_OLD_PARENT_RECEIPT_SENDER");
    assert!(
        matches!(old_parent_result, Ok(ref done) if done.stop_reason == acp::StopReason::Cancelled
        && matches!(&done.completion_kind, crate::session::commands::PromptCompletionKind::Cancelled { .. })),
        "H6_L14B_OLD_PARENT_OWNER_SETTLED"
    );
    {
        let actor = &mixed.fixture.fx.session.actor;
        let state = actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        let parent_entries_after_cancel = ledger.entries.iter()
            .filter(|entry| matches!(&entry.body, super::interjection::HeldBody::Parent(_))).count();
        let stamps_live = ledger.parent_stamps.iter()
            .all(|(identity, _)| actor.parent_identity_is_live(&state, identity));
        let references_live = ledger.entries.iter().all(|entry| match &entry.body {
            super::interjection::HeldBody::Parent(identity) => actor.parent_identity_is_live(&state, identity),
            _ => true,
        });
        let old_stamps_absent = ledger.parent_stamps.iter()
            .all(|(identity, _)| !old_parent_ids.contains(identity));
        let old_references_absent = ledger.entries.iter().all(|entry| match &entry.body {
            super::interjection::HeldBody::Parent(identity) => !old_parent_ids.contains(identity),
            _ => true,
        });
        u6_observe(json!({"stage":"R19 real command Cancel pruning", "constructed":false,
            "parent_entries_before_cancel":parent_entries_before_cancel,
            "parent_entries_after_cancel":parent_entries_after_cancel,
            "live_parent_slots":actor.live_parent_steer_slots(&state),
            "stamp_count":ledger.parent_stamps.len(), "stamps_live":stamps_live,
            "references_live":references_live, "old_stamps_absent":old_stamps_absent,
            "old_references_absent":old_references_absent}));
        assert!(stamps_live && old_stamps_absent, "H6_R19_CANCEL_STAMP_PRUNE");
        assert!(references_live && old_references_absent, "H6_R19_CANCEL_ORPHAN_PRUNE");
    }
    let retained = u6_mixed_keys(&mixed.fixture.fx).await;
    for source in ["human_interjection", "deferred_followup", "skill_reminder"] {
        let before: Vec<_> = mixed
            .keys
            .iter()
            .filter(|entry| entry["source"] == source)
            .collect();
        let after: Vec<_> = retained
            .iter()
            .filter(|entry| entry["source"] == source)
            .collect();
        assert_eq!(
            after, before,
            "H6_L14B_COMMAND_CANCEL_RETAINS_NONPARENT_CUSTODY"
        );
    }
    assert_eq!(
        mixed.fixture.fx.server.messages_request_count(),
        3,
        "H6_L14B_CUSTODY_DOES_NOT_RESTART"
    );
    mixed
        .fixture
        .persistence_control
        .hold_barriers
        .store(false, std::sync::atomic::Ordering::SeqCst);
    while !mixed
        .fixture
        .persistence_control
        .held_barriers
        .lock()
        .unwrap()
        .is_empty()
    {
        u6_forward_held_barrier(&mixed.fixture);
    }
    let (respond_to, _response) = tokio::sync::oneshot::channel();
    let (ready, ready_rx) = tokio::sync::oneshot::channel();
    let mut request = super::prompt_queue::QueueInputRequest::from_input_origin(
        vec![acp::ContentBlock::Text(acp::TextContent::new(
            "U6 L14b new admitted turn",
        ))],
        "u6-l14b-new-owner".into(),
        InputOrigin::new(PromptOrigin::User),
        PromptMode::Agent,
        respond_to,
    );
    request.client_identifier = Some("U6".into());
    request.initial_child_prompt_ready = Some(ready);
    assert!(
        !mixed.fixture.fx.session.actor.queue_input(request).await,
        "H6_L14B_NEW_TURN_REAL_QUEUE_ADMISSION"
    );
    let (completion, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
    mixed
        .fixture
        .fx
        .session
        .actor
        .clone()
        .maybe_start_running_task(completion)
        .await;
    let _publication_release = tokio::time::timeout(K0_TIMEOUT, ready_rx)
        .await
        .expect("H6_L14B_NEW_TURN_REAL_READINESS_BARRIER")
        .unwrap();
    let new_handle = {
        let state = mixed.fixture.fx.session.actor.state.lock().await;
        let task = state
            .running_task
            .as_ref()
            .expect("H6_L14B_NEW_ADMITTED_OWNER_RUNNING");
        assert!(
            task.epoch != old_epoch && task.prompt_id == "u6-l14b-new-owner",
            "H6_L14B_NEW_DELIVERY_BINDING"
        );
        task.handle.clone()
    };
    let _new_turn = K0Abort(new_handle);
    let _new_parent = u6_admit_steer(
        &mixed.fixture.fx.session.actor,
        "U6 L14b new-owner parent",
        "u6-parent",
    )
    .await;
    let delivered = k2lg_n_selected(
        &mixed.fixture.fx,
        mixed
            .fixture
            .fx
            .session
            .actor
            .drain_admitted_messages_at_safe_point(),
    )
    .await;
    assert!(!delivered, "H6_L14B_CLOSED_MAILBOX_NEVER_REOPENS");
    let new_progress = mixed
        .fixture
        .fx
        .session
        .actor
        .state
        .lock()
        .await
        .message_delivery
        .held_input
        .parent_persisted;
    let (respond_to, observed) = tokio::sync::oneshot::channel();
    mixed
        .fixture
        .fx
        .session
        .actor
        .notifications
        .persistence_tx
        .send(PersistenceMsg::FlushAndAck { respond_to })
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, observed)
        .await
        .expect("H6_L14B_NEW_OWNER_RECEIVER_FENCE")
        .unwrap()
        .unwrap();
    let new_persists = u6_user_persists(
        &mixed.fixture.persistence_control,
        "U6 L14b new-owner parent",
    );
    u6_observe(
        json!({"stage":"L14b real command Cancel then new admitted owner", "old_progress":old_progress,
        "new_parent_persists":new_persists,"new_progress":new_progress,"new_queue_readiness":true,"closed_mailbox_reopened":false,
        "monitor_sweep":"U6.5 pre-state, NOT CREDITED","plan_teardown":"U6.7 pre-state, NOT CREDITED"}),
    );
    assert_eq!(
        old_progress, 1,
        "H6_L14B_OLD_OWNER_RETAINED_PREFIX_PROGRESS"
    );
    assert_eq!(
        new_persists, 1,
        "H6_L14B_NEW_OWNER_FIRST_PARENT_NOT_SKIPPED"
    );
    assert_eq!(
        u6_user_persists(&mixed.fixture.persistence_control, "U6 L14b custody human"),
        0,
        "H6_L14B_CANCEL_PATH_NO_FAILED_HUMAN_PERSISTENCE"
    );
    mixed.continuation.assert_satisfied();
    json!({"command":"SessionCommand::Cancel CtrlC", "new_turn":"real queue admission and publication readiness",
        "new_attempt":"new owner's first explicit real safe point, not old post-bookkeeping", "closed_mailbox_reopened":false,
        "full_cancel_rebuild_end_U6_6_credit":false,"pin_sweep_credit":false})
}

async fn u6_l14b_constructed_partial_loop() -> Value {
    use super::interjection::{
        HeldBindingMeta, HeldCommitError, HeldParentSelection, HeldParentView,
    };
    let mixed = u6_l14b_fixture().await;
    let _stop = K0Abort(mixed.turn.running.abort_handle());
    let _second = u6_admit_steer(
        &mixed.fixture.fx.session.actor,
        "U6 L14b partial-loop parent one",
        "u6-parent",
    )
    .await;
    let _third = u6_admit_steer(
        &mixed.fixture.fx.session.actor,
        "U6 L14b partial-loop parent two",
        "u6-parent",
    )
    .await;
    let actor = &mixed.fixture.fx.session.actor;
    let send = |parent: &HeldParentView| {
        let update = acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(
            acp::ContentBlock::Text(acp::TextContent::new(parent.text.to_string())),
        ));
        actor
            .notifications
            .persistence_tx
            .send(PersistenceMsg::Update(SessionUpdate::Acp(Box::new(
                acp::SessionNotification::new(actor.session_info.id.clone(), update),
            ))))
            .map_err(|_| HeldCommitError::PersistenceUnavailable)
    };
    let mut state = actor.state.lock().await;
    let task = state.running_task.as_ref().unwrap();
    let binding = xai_message_delivery_core::TurnBinding::new(task.prompt_id.clone(), task.epoch);
    let meta = HeldBindingMeta {
        prompt_id: task.prompt_id.clone(),
        epoch: format!("{:?}", task.epoch),
    };
    let mut sends = 0;
    let first = actor.commit_held_parent_batch(
        &mut state,
        &binding,
        &meta,
        HeldParentSelection::All,
        |parents, ledger| {
            SessionActor::persist_held_parent_prefix(parents, ledger, |parent| {
                sends += 1;
                if sends == 2 {
                    return Err(HeldCommitError::PersistenceUnavailable);
                }
                send(parent)
            })
        },
    );
    assert!(
        matches!(first, Err(HeldCommitError::PersistenceUnavailable)),
        "H6_L14B_CONSTRUCTED_SECOND_SEND_FAILURE"
    );
    assert_eq!(
        state.message_delivery.held_input.parent_persisted, 1,
        "H6_L14B_CONSTRUCTED_PREFIX_PROGRESS_RETAINED"
    );
    let mut retried = Vec::new();
    let second = actor.commit_held_parent_batch(
        &mut state,
        &binding,
        &meta,
        HeldParentSelection::All,
        |parents, ledger| {
            SessionActor::persist_held_parent_prefix(parents, ledger, |parent| {
                send(parent)?;
                retried.push(parent.text.to_string());
                Ok(())
            })?;
            actor
                .chat_state_handle
                .try_push_user_messages_batch(
                    parents
                        .iter()
                        .map(|parent| ConversationItem::agent_message(parent.text.to_string()))
                        .collect(),
                )
                .map_err(|_| HeldCommitError::ChatStateUnavailable)
        },
    );
    assert!(second.is_ok(), "H6_L14B_CONSTRUCTED_SUFFIX_COMMIT_SUCCEEDS");
    assert_eq!(
        retried,
        vec![
            "U6 L14b partial-loop parent one",
            "U6 L14b partial-loop parent two"
        ],
        "H6_L14B_PARTIAL_LOOP_RETRIES_ONLY_UNSENT_SUFFIX"
    );
    assert_eq!(
        state.message_delivery.held_input.parent_persisted, 0,
        "H6_L14B_SETTLED_PARENT_PREFIX_CLEARED"
    );
    assert!(
        state.message_delivery.held_input.parent_owner.is_none(),
        "H6_L14B_SETTLED_PARENT_OWNER_CLEARED"
    );
    drop(state);
    tokio::time::timeout(K0_TIMEOUT, async {
        while [
            "U6 L14b parent zero",
            "U6 L14b partial-loop parent one",
            "U6 L14b partial-loop parent two",
        ]
        .into_iter()
        .map(|text| u6_user_persists(&mixed.fixture.persistence_control, text))
        .sum::<usize>()
            < 3
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("H6_L14B_PARTIAL_LOOP_REAL_PERSISTENCE_RECEIVER");
    let history = actor.chat_state_handle.get_conversation().await;
    for text in [
        "U6 L14b parent zero",
        "U6 L14b partial-loop parent one",
        "U6 L14b partial-loop parent two",
    ] {
        assert_eq!(
            u6_user_persists(&mixed.fixture.persistence_control, text),
            1,
            "H6_L14B_PARTIAL_LOOP_PARENT_PERSISTED_ONCE"
        );
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(text))
                .count(),
            1,
            "H6_L14B_PARTIAL_LOOP_PARENT_SUBMITTED_ONCE"
        );
    }
    json!({"kind":"constructed partial-loop second-send failure", "helper":"actual persist_held_parent_prefix",
        "retry":"only unsent suffix", "production_closed_channel_reopened":false})
}

async fn u6_pinned_monitor_keys(fx: &K2lgN) -> Vec<Value> {
    let state = fx.session.actor.state.lock().await;
    let Some(owner) = state.message_delivery.held_input.pin_owner else {
        return Vec::new();
    };
    let buffer = fx
        .session
        .actor
        .tool_context
        .monitor_event_buffer
        .as_ref()
        .unwrap();
    let view = buffer.release_view(owner, |_| false);
    view.iter().map(|(key, event)| json!({"key":key.as_u64(),"task_id":event.task_id,
        "owner":event.owner_session_id,"text_digest":native_content_digest(event.event_text.as_bytes())})).collect()
}

tokio::task_local! {
    static U6_ARM_STEPPERS: std::rc::Rc<std::cell::RefCell<Vec<U6TurnStepper>>>;
}

pub(super) fn u6_record_native_safe_point(
    point: &str,
    observed: Option<(bool, bool, bool)>,
    holding: bool,
    retaining: bool,
) {
    let _ = U6_ARM_OBSERVATIONS.try_with(|observations| {
        let mut records = observations.borrow_mut();
        assert!(records.len() < 4096, "H6_R15_OBSERVATION_BOUND");
        let (live, unavailable, chat_closed) = observed.unwrap_or_default();
        records.push(json!({"stage":"native safe point", "point":point,
            "selected":observed.is_some(), "live":live, "unavailable":unavailable,
            "chat_closed":chat_closed, "holding":holding,
            "release_path":if holding { "hold" } else if retaining { "merged_release" } else { "unheld" }}));
    });
}

pub(super) fn u6_record_release_outcome(committed: bool) {
    let _ = U6_ARM_OBSERVATIONS.try_with(|observations| {
        observations
            .borrow_mut()
            .push(json!({"stage":"merged release outcome",
            "release_path":if committed { "submitted" } else { "submit_failed" }}));
    });
}

fn u6_current_stepper() -> U6TurnStepper {
    U6_ARM_STEPPERS.with(|steppers| steppers.borrow().last().unwrap().clone())
}

/// How the R15-4 precondition may step the turn (H-K3U6a-R19 R19-3).
#[derive(Clone, Copy, PartialEq, Eq)]
enum U6PreconditionStepping {
    /// A response was just released: no step while an owned response is still being drained,
    /// because the drainer persists its native evidence before `Completed` drops the ownership.
    HoldUntilDrained,
    Free,
}

async fn u6_readable_checkpoint(
    fx: &K2lgN,
    running: &mut tokio::task::JoinHandle<Result<TurnOutcome, acp::Error>>,
    expected_live: bool,
    tag: &str,
) -> K2Checkpoint {
    u6_persisted_readable_checkpoint(
        fx,
        running,
        expected_live,
        tag,
        U6PreconditionStepping::Free,
    )
    .await
}

/// Persisted native read served in the session persistence actor's own queue, so the
/// precondition never holds the native disk locks a concurrent persist takes under a deadline.
async fn u6_persisted_native_read(
    fx: &K2lgN,
) -> Result<xai_chat_state::persistence::native::NativeRead, k2_history::NativeUnavailable> {
    let (respond_to, read) = tokio::sync::oneshot::channel();
    if fx
        .session
        .persistence
        .tx
        .send(PersistenceMsg::ReadNativeHistory { respond_to })
        .is_err()
    {
        return Err(k2_history::NativeUnavailable::Persistence);
    }
    read.await
        .unwrap_or(Err(k2_history::NativeUnavailable::Persistence))
}

/// Holds the stepped turn until the checkpoint is readable in the actor and equal on disk:
/// the turn is stepped only while no owned response is draining and the view is unreadable.
async fn u6_persisted_readable_checkpoint(
    fx: &K2lgN,
    running: &mut tokio::task::JoinHandle<Result<TurnOutcome, acp::Error>>,
    expected_live: bool,
    tag: &str,
    stepping: U6PreconditionStepping,
) -> K2Checkpoint {
    let stepper = u6_current_stepper();
    let was_paused = stepper.0.paused.get();
    stepper.pause();
    let steps = std::cell::Cell::new(0u32);
    let drain_holds = std::cell::Cell::new(0u32);
    let persisted_reads = std::cell::Cell::new(0u32);
    let wait = async {
        loop {
            if stepping == U6PreconditionStepping::HoldUntilDrained
                && !fx.session.actor.turn_stream_drained.lock().is_empty()
            {
                drain_holds.set(drain_holds.get() + 1);
                tokio::time::sleep(Duration::from_millis(2)).await;
                continue;
            }
            let actor = fx
                .session
                .actor
                .chat_state_handle
                .read_native_history()
                .await;
            if let Some(checkpoint) = actor
                .as_ref()
                .ok()
                .and_then(|read| read.checkpoint.as_ref())
                && k2lg_n_live(checkpoint) == expected_live
            {
                persisted_reads.set(persisted_reads.get() + 1);
                let disk = u6_persisted_native_read(fx).await;
                if disk
                    .as_ref()
                    .ok()
                    .and_then(|read| read.checkpoint.as_ref())
                    .is_some_and(|disk| {
                        disk.source == checkpoint.source && disk.ledger == checkpoint.ledger
                    })
                {
                    return Some((**checkpoint).clone());
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
                continue;
            }
            if stepper.0.finished.get() {
                return None;
            }
            if stepper
                .0
                .wake
                .ready
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                steps.set(steps.get() + 1);
                assert!(steps.get() <= 512, "H6_R15_NATIVE_PRECONDITION_STEP_BOUND");
                if !stepper.step().await {
                    return None;
                }
            } else {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    };
    let result = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            biased;
            checkpoint = wait => (checkpoint, false),
            _ = running => (None, true),
        }
    })
    .await;
    if !was_paused {
        stepper.resume();
    }
    let bounded_wait_completed = result.is_ok();
    let (checkpoint, turn_finished) = result.unwrap_or((None, false));
    u6_observe(json!({"stage":"native checkpoint precondition", "tag":tag,
        "expected_live":expected_live, "readable":checkpoint.is_some(),
        "turn_finished":turn_finished, "bounded_wait_completed":bounded_wait_completed,
        "hold_until_drained":stepping == U6PreconditionStepping::HoldUntilDrained,
        "drain_holds":drain_holds.get(), "steps":steps.get(),
        "persisted_reads":persisted_reads.get()}));
    assert!(checkpoint.is_some(), "{tag}");
    checkpoint.unwrap()
}

async fn u6_release_after_checkpoint(
    fx: &K2lgN,
    running: &mut tokio::task::JoinHandle<Result<TurnOutcome, acp::Error>>,
    response: &xai_grok_test_support::InferenceExpectation,
    expected_live: bool,
    tag: &str,
) {
    let stepper = u6_current_stepper();
    let was_paused = stepper.0.paused.get();
    stepper.pause();
    response.release();
    u6_persisted_readable_checkpoint(
        fx,
        running,
        expected_live,
        tag,
        U6PreconditionStepping::HoldUntilDrained,
    )
    .await;
    if !was_paused {
        stepper.resume();
    }
}

/// Runs the committed K2 `reload()` at a quiescent point (H-K3U6a-R20 R20-3). The stepped turn is
/// held and nothing is draining, and a native read served in the persistence actor's own queue on
/// the product route has completed every earlier persist, so no chat or native persist can run
/// between reload()'s flush and its disk reads.
async fn u6_quiescent_reload(fx: &K2lgN, tag: &str) -> (K2Checkpoint, Vec<ConversationItem>) {
    let stepper = u6_current_stepper();
    let was_paused = stepper.0.paused.get();
    stepper.pause();
    let drain_holds = std::cell::Cell::new(0u32);
    let fence_rounds = std::cell::Cell::new(0u32);
    let settled = tokio::time::timeout(K0_TIMEOUT, async {
        loop {
            if !fx.session.actor.turn_stream_drained.lock().is_empty() {
                drain_holds.set(drain_holds.get() + 1);
                tokio::time::sleep(Duration::from_millis(2)).await;
                continue;
            }
            fence_rounds.set(fence_rounds.get() + 1);
            assert!(fence_rounds.get() <= 512, "H6_R20_QUIESCENCE_ROUND_BOUND");
            let persisted = fx.log.lock().unwrap().len();
            let _ = fx.session.actor.chat_state_handle.get_conversation().await;
            let (respond_to, read) = tokio::sync::oneshot::channel();
            let sent = fx
                .session
                .actor
                .notifications
                .persistence_tx
                .send(PersistenceMsg::ReadNativeHistory { respond_to })
                .is_ok();
            if !sent || read.await.is_err() {
                return false;
            }
            if fx.session.actor.turn_stream_drained.lock().is_empty()
                && fx.log.lock().unwrap().len() == persisted
            {
                return true;
            }
        }
    })
    .await;
    let quiet = matches!(settled, Ok(true));
    u6_observe(json!({"stage":"R20 quiescent reload", "tag":tag,
        "turn_finished":stepper.0.finished.get(), "settled":quiet,
        "drain_holds":drain_holds.get(), "fence_rounds":fence_rounds.get()}));
    assert!(quiet, "{tag}");
    let reloaded = fx.session.reload().await;
    if !was_paused {
        stepper.resume();
    }
    reloaded
}

async fn u6_unavailable_view_hold() -> Value {
    const HUMAN: &str = "U6 R15 unavailable native view human";
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let holder = tokio::task::spawn_local(std::future::pending::<()>());
    let _stop = K0Abort(holder.abort_handle());
    u6_install_running(&mut fixture.fx, holder.abort_handle()).await;
    let fx = &fixture.fx;
    let read = fx
        .session
        .actor
        .chat_state_handle
        .read_native_history()
        .await;
    assert!(
        read.as_ref().is_ok_and(|read| read.checkpoint.is_none()),
        "H6_R15_UNAVAILABLE_VIEW_PRECONDITION"
    );
    assert!(
        fx.session
            .actor
            .chat_state_handle
            .get_sampling_config()
            .await
            .is_some(),
        "H6_R15_OPEN_CHAT_PRECONDITION"
    );
    k2lg_n_selected(fx, fx.session.actor.arm_native_hold_latch()).await;
    u6_interject(fx, HUMAN, Vec::new()).await;
    let resident = u6_resident_keys(&fx.session.actor);
    let delivered = k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        !delivered
            && !history
                .iter()
                .any(|item| item.text_content().contains(HUMAN)),
        "H6_R15_UNAVAILABLE_STAYS_HELD"
    );
    let key = {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert!(ledger.latch.is_some(), "H6_R15_LATCH_RETAINED");
        let entry = ledger.entries.iter().find(|entry| {
            matches!(&entry.body, super::interjection::HeldBody::Human(raw) if raw.text == HUMAN)
        }).expect("H6_R15_RAW_CUSTODY_RETAINED");
        assert_eq!(
            entry.key.as_u64(),
            resident[0]["key"].as_u64().unwrap(),
            "H6_R15_ARRIVAL_KEY_PRESERVED"
        );
        entry.key
    };
    assert!(
        fx.session.actor.pending_interjections.is_empty(),
        "H6_R15_OPTION_K_CUSTODY"
    );
    assert!(
        !k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_R15_REPEATED_SAFE_POINT_STILL_HELD"
    );
    let state = fx.session.actor.state.lock().await;
    assert!(
        state
            .message_delivery
            .held_input
            .entries
            .iter()
            .any(|entry| entry.key == key),
        "H6_R15_NO_UNAVAILABLE_VIEW_RELEASE"
    );
    assert_eq!(
        u6_user_persists(&fixture.persistence_control, HUMAN),
        0,
        "H6_R15_NO_UNAVAILABLE_VIEW_PERSISTENCE"
    );
    assert_eq!(
        fx.server.messages_request_count(),
        0,
        "H6_R15_NO_PROVIDER_REQUEST"
    );
    json!({"native_view":"unavailable", "chat_open":true,
        "safe_point_only":true, "latch_setup":"constructed running binding via W05 setter",
        "human_producer":"SessionCommand::Interject", "original_key_preserved":true,
        "terminal_settlement_credited":false, "terminal_settlement_owner":"koxd-U6.6",
        "authority":"H-K3U6a-R15 R15-2/R15-3"})
}

pub(super) fn u6_record_commit_observation(
    observed: Option<(bool, bool, bool)>,
    native_hold: bool,
    excluded: bool,
) {
    let _ = U6_ARM_OBSERVATIONS.try_with(|observations| {
        let (live, unavailable, chat_closed) = observed.unwrap_or_default();
        observations.borrow_mut().push(json!({"stage":"merged commit recheck",
            "live":live, "unavailable":unavailable, "chat_closed":chat_closed,
            "native_hold":native_hold, "excluded":excluded,
            "release_path":if excluded { "owner_excluded" } else if native_hold { "hold" } else { "commit" }}));
    });
}

/// Join handle of an arm's U6 stepped turn (H-K3U6a-R24 R24-2).
type U6Running = tokio::task::JoinHandle<Result<TurnOutcome, acp::Error>>;

/// Names-only outcome of a stepped turn that ended before awaited product progress
/// (H-K3U6a-R24 R24-2): the local error code, the typed kind fields and every
/// `NativeUnavailable` variant the refusal names. Message text and values are never kept.
fn u6_turn_end_names(
    outcome: &Result<Result<TurnOutcome, acp::Error>, tokio::task::JoinError>,
) -> Value {
    let name = |data: Option<&Value>, key: &str| {
        data.and_then(|data| data.get(key))
            .and_then(Value::as_str)
            .filter(|name| {
                !name.is_empty()
                    && name.len() <= 64
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
            .map(str::to_owned)
    };
    match outcome {
        Err(join) => json!({"turn_finished":true, "result":"join_error",
            "join":if join.is_cancelled() { "cancelled" } else { "panicked" }}),
        Ok(Ok(_)) => json!({"turn_finished":true, "result":"ok"}),
        Ok(Err(error)) => {
            let data = error.data.as_ref();
            let message = data
                .and_then(|data| data.get("message"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut unavailable: Vec<&str> = Vec::new();
            for word in message.split(|c: char| !c.is_ascii_alphanumeric()) {
                if !unavailable.contains(&word)
                    && serde_json::from_value::<k2_history::NativeUnavailable>(json!(word)).is_ok()
                {
                    unavailable.push(word);
                }
            }
            json!({"turn_finished":true, "result":"error",
                "error_code":name(data, "code"), "error_kind":name(data, "error_kind"),
                "error_type":name(data, "error_type"), "native_unavailable":unavailable})
        }
    }
}

/// Fails `tag` because the stepped turn ended before the awaited product progress
/// (H-K3U6a-R24 R24-2), recording the turn's outcome by name only.
fn u6_turn_ended(
    outcome: Result<Result<TurnOutcome, acp::Error>, tokio::task::JoinError>,
    stage: &str,
    tag: &str,
    progress: Value,
) -> ! {
    u6_observe(
        json!({"stage":stage, "tag":tag, "turn_ended_before_progress":true,
        "turn":u6_turn_end_names(&outcome), "progress":progress}),
    );
    panic!("{tag}");
}

/// Fails `tag` because the unchanged K0 bound expired first; records names and counts only.
fn u6_bound_expired(running: &U6Running, stage: &str, tag: &str, progress: Value) -> ! {
    u6_observe(
        json!({"stage":stage, "tag":tag, "turn_ended_before_progress":false,
        "bounded_wait_completed":false, "turn_finished":running.is_finished(),
        "progress":progress}),
    );
    panic!("{tag}");
}

/// Receives both handoff client-tool entries under the unchanged K0 bound, raced against the
/// stepped turn's outcome (H-K3U6a-R24 R24-2).
async fn u6_client_dispatch(
    entries: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    running: &mut U6Running,
    capture: Value,
    stage: &str,
    tag: &str,
    ended_tag: &str,
) -> Vec<String> {
    let mut entered = Vec::new();
    let raced = tokio::time::timeout(K0_TIMEOUT, async {
        while entered.len() < 2 {
            tokio::select! {
                biased;
                entry = entries.recv() => {
                    entered.push(entry.expect("H6_REAL_CLIENT_ENTRY_CHANNEL_OPEN"));
                }
                result = &mut *running => return Some(result),
            }
        }
        None
    })
    .await;
    let progress = json!({"capture":capture, "client_entries":entered.clone(),
        "client_entry_count":entered.len()});
    match raced {
        Ok(None) => entered,
        Ok(Some(result)) => u6_turn_ended(result, stage, ended_tag, progress),
        Err(_) => u6_bound_expired(running, stage, tag, progress),
    }
}

/// Polls `progressed` under the unchanged K0 bound, raced against the stepped turn's outcome
/// (H-K3U6a-R24 R24-2). A finished turn is consumed only after a final re-check fails, so a
/// turn that ends right after its progress never fails here.
async fn u6_await_turn_progress<F, P>(
    running: &mut U6Running,
    stage: &str,
    tag: &str,
    ended_tag: &str,
    mut progressed: F,
) where
    F: FnMut() -> P,
    P: std::future::Future<Output = bool>,
{
    let raced = tokio::time::timeout(K0_TIMEOUT, async {
        loop {
            if progressed().await {
                return false;
            }
            if running.is_finished() {
                return !progressed().await;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    match raced {
        Ok(false) => {}
        Ok(true) => u6_turn_ended(running.await, stage, ended_tag, json!({})),
        Err(_) => u6_bound_expired(running, stage, tag, json!({})),
    }
}

async fn u6b_n2() -> Value {
    let mut legs = Vec::new();
    for (count, width) in [(3, 21_500), (9, 30_917)] {
        let (mut fixture, mut turn, stepper, continuation) = u6_u63_prepare_fixture(None).await;
        let _stop = K0Abort(turn.running.abort_handle());
        let fx = &mut fixture.fx;
        let mut prefixes = Vec::new();
        for index in 0..count {
            let prefix = format!("U6b N2 {count} {index}:");
            let text = format!("{prefix}{}", "h".repeat(width - prefix.len()));
            u6_queue_resident(fx, &text, Vec::new()).await;
            prefixes.push(prefix);
        }
        let original = u6_resident_keys(&fx.session.actor);
        assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
            "H6_N2_NO_EARLY_RELEASE");
        {
            let state = fx.session.actor.state.lock().await;
            let ledger = &state.message_delivery.held_input;
            assert_eq!(ledger.entries.iter().filter(|entry| entry.source ==
                super::interjection::HeldSource::HumanInterjection).count(), count,
                "H6_N2_FIRST_RELEASE_PROGRESS");
            assert_eq!(ledger.entries.iter().map(|entry| entry.key.as_u64()).collect::<Vec<_>>(),
                original.iter().map(|entry| entry["key"].as_u64().unwrap()).collect::<Vec<_>>(),
                "H6_N2_ORIGINAL_KEYS_HELD");
        }
        fx.server.enqueue_response("/v1/messages",
            k2_script(k2lg_n_after_frames("msg_u6b_n2_after")));
        u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
            "H6_N2_READABLE_CLOSED_CHECKPOINT").await;
        stepper.resume();
        let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
            .expect("H6_N2_FIRST_RELEASE_PROGRESS").expect("H6_N2_TURN_TASK");
        assert!(outcome.is_ok(), "H6_N2_FIRST_RELEASE_PROGRESS");
        let bodies = k2lg_n_bodies(&fx.server);
        assert_eq!(bodies.len(), 4, "H6_N2_FIRST_RELEASE_PROGRESS");
        let text = u6_body_text(bodies.last().unwrap());
        let mut previous = 0;
        for (index, prefix) in prefixes.iter().enumerate() {
            assert_eq!(text.matches(prefix).count(), 1, "H6_N2_FIRST_RELEASE_PROGRESS");
            let position = text.find(prefix).expect("H6_N2_FIRST_RELEASE_PROGRESS");
            assert!(index == 0 || previous < position, "H6_N2_KEY_ORDER");
            previous = position;
        }
        assert!(fx.session.actor.state.lock().await.message_delivery.held_input.entries.is_empty(),
            "H6_N2_RELEASE_EMPTIES_CUSTODY");
        continuation.assert_satisfied();
        legs.push(json!({"count":count,"input_bytes":count*width,"same_turn":true,
            "http_count":bodies.len(),"once_only_key_order":true,"fallback_row":false,
            "producer":"QueueInputRequest->queue_input->promote_queued_as_interjections"}));
    }
    json!({"legs":legs,"release_oracle":"H6_N2_FIRST_RELEASE_PROGRESS"})
}

async fn u6b_human_band(count: usize, width: usize, tag: &'static str) -> Value {
    let (mut fixture, mut turn, stepper, continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &mut fixture.fx;
    let mut texts = Vec::new();
    let mut prefixes = Vec::new();
    for index in 0..count {
        let prefix = format!("U6b {tag} {index}:");
        let text = format!("{prefix}{}", "q".repeat(width - prefix.len()));
        u6_queue_resident(fx, &text, Vec::new()).await;
        prefixes.push(prefix);
        texts.push(text);
    }
    let original = u6_resident_keys(&fx.session.actor);
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_HUMAN_BAND_NO_EARLY_RELEASE");
    {
        let state = fx.session.actor.state.lock().await;
        let keys = state.message_delivery.held_input.entries.iter()
            .filter(|entry| entry.source == super::interjection::HeldSource::HumanInterjection)
            .map(|entry| entry.key.as_u64()).collect::<Vec<_>>();
        assert_eq!(keys.len(), count, "{tag}");
        assert_eq!(keys, original.iter().map(|entry| entry["key"].as_u64().unwrap())
            .collect::<Vec<_>>(), "H6_HUMAN_BAND_ORIGINAL_KEYS");
        assert!(fx.session.actor.pending_interjections.snapshot().is_empty(), "{tag}");
    }
    fx.server.enqueue_response("/v1/messages",
        k2_script(k2lg_n_after_frames("msg_u6b_human_band_after")));
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_HUMAN_BAND_READABLE_CLOSED_CHECKPOINT").await;
    stepper.resume();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_HUMAN_BAND_RELEASE_PROGRESS").expect("H6_HUMAN_BAND_TURN_TASK");
    assert!(outcome.is_ok(), "H6_HUMAN_BAND_RELEASE_PROGRESS");
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4, "H6_HUMAN_BAND_NO_FALLBACK_REQUEST");
    let text = u6_body_text(bodies.last().unwrap());
    let mut previous = 0;
    for (index, prefix) in prefixes.iter().enumerate() {
        assert_eq!(text.matches(prefix).count(), 1, "H6_HUMAN_BAND_ONCE_ONLY");
        let position = text.find(prefix).expect("H6_HUMAN_BAND_ONCE_ONLY");
        assert!(index == 0 || previous < position, "H6_HUMAN_BAND_KEY_ORDER");
        previous = position;
    }
    if count == 1 && width >= 256 * 1024 {
        let expected = xai_interjection_core::format_interjection(texts[0].clone());
        assert!(expected.len() < texts[0].len(), "H6_N3_EXISTING_FORMATTER_CUT");
        assert!(text.contains(&expected), "H6_N3_FORMATTED_MODEL_TEXT");
    }
    let state = fx.session.actor.state.lock().await;
    assert!(state.message_delivery.held_input.entries.is_empty(), "H6_HUMAN_BAND_EMPTY");
    assert!(state.pending_inputs.iter().all(|input| input.prompt_id == KOXD_U6_PROMPT),
        "H6_HUMAN_BAND_NO_FALLBACK_ROW");
    drop(state);
    continuation.assert_satisfied();
    json!({"producer":"QueueInputRequest->queue_input->promote_queued_as_interjections",
        "count":count,"input_bytes":count*width,"first_closed_safe_point_release":true,
        "same_turn":true,"once_only_key_order":true,"fallback_row":false,
        "formatter_cut_only":count==1 && width>=256*1024})
}

async fn u6b_monitor_ingress(
    fx: &K2lgN,
    running: &mut tokio::task::JoinHandle<Result<TurnOutcome, acp::Error>>,
    task: &str,
    text: &str,
) -> u64 {
    let buffer = fx.session.actor.tool_context.monitor_event_buffer.as_ref().unwrap();
    fx.loop_tx.send(SessionCommand::InjectNotification {
        prompt_id: format!("u6b-monitor-{task}"),
        prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(format!(
            "<monitor-event description=\"u6b\" task_id=\"{task}\">\n{text}\n</monitor-event>")))],
        priority: NotificationPriority::Next,
        source: NotificationSource::MonitorEvent { task_id: task.into() },
    }).unwrap();
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = running => panic!("H6_U65_MONITOR_INGRESS_TURN_ENDED"),
            key = async {
                loop {
                    let key = buffer.release_view(xai_interjection_core::events::next_occurrence(),
                        |event| event.task_id == task && event.event_text.contains(text))
                        .iter().find_map(|(key, event)|
                            (event.task_id == task && event.event_text.contains(text)).then_some(key.as_u64()));
                    if let Some(key) = key {
                        return key;
                    }
                    tokio::task::yield_now().await;
                }
            } => key,
        }
    }).await.expect("H6_U65_MONITOR_INGRESS_BOUND")
}

async fn u6b_monitor_band() -> Value {
    let (mut fixture, mut turn, stepper, continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &mut fixture.fx;
    const HUMAN: &str = "U6b N4 independent human";
    u6_queue_resident(fx, HUMAN, Vec::new()).await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N4_HUMAN_HELD");
    for index in 0..129 {
        let task = format!("u6b-n4-{index}");
        let text = format!("U6b N4 monitor {index:03}");
        u6b_monitor_ingress(fx, &mut turn.running, &task, &text).await;
        k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
    }
    {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert_eq!(ledger.entries.iter().filter(|entry| entry.source ==
            super::interjection::HeldSource::MonitorEvent).count(), 128,
            "H6_N4_MONITOR_BAND_COUNT");
        assert_eq!(ledger.entries.iter().filter(|entry| entry.source ==
            super::interjection::HeldSource::HumanInterjection).count(), 1,
            "H6_N4_HUMAN_INDEPENDENT");
        let refusal = ledger.refusal.as_ref().expect("H6_N4_REFUSAL_RECORD");
        assert_eq!(refusal.source, super::interjection::HeldSource::MonitorEvent,
            "H6_N4_REFUSAL_SOURCE");
        assert_eq!(format!("{:?}", refusal.reason), "MonitorBandFull", "H6_N4_REFUSAL_REASON");
        assert_eq!(refusal.observed_len, 128, "H6_N4_REFUSAL_COUNT");
    }
    assert_eq!(u6_pinned_monitor_keys(fx).await.len(), 128, "H6_N4_OWNER_PINS");
    let buffer = fx.session.actor.tool_context.monitor_event_buffer.as_ref().unwrap();
    let unpinned = buffer.release_view(xai_interjection_core::events::next_occurrence(),
        |event| event.task_id == "u6b-n4-128").iter().count();
    assert_eq!(unpinned, 1, "H6_N4_REFUSED_PIN_STAYS_RESIDENT");
    fx.server.enqueue_response("/v1/messages",
        k2_script(k2lg_n_after_frames("msg_u6b_n4_after")));
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_N4_READABLE_CLOSED_CHECKPOINT").await;
    stepper.resume();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N4_RELEASE_PROGRESS").expect("H6_N4_TURN_TASK");
    assert!(outcome.is_ok(), "H6_N4_RELEASE_PROGRESS");
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4, "H6_N4_NO_FALLBACK_REQUEST");
    let text = u6_body_text(bodies.last().unwrap());
    assert_eq!(text.matches(HUMAN).count(), 1, "H6_N4_HUMAN_RELEASED_ONCE");
    for index in 0..129 {
        assert_eq!(text.matches(&format!("U6b N4 monitor {index:03}")).count(), 1,
            "H6_N4_MONITOR_RELEASED_ONCE");
    }
    assert!(u6_pinned_monitor_keys(fx).await.is_empty(), "H6_N4_PINS_EMPTY");
    continuation.assert_satisfied();
    json!({"producer":"SessionCommand::InjectNotification", "pinned":128,
        "refusal":"monitor_band_full", "refused_resident_released":true,
        "human_band_independent":true, "once_only_monitor_count":129})
}

async fn u6b_deferred_image() -> Value {
    let mut fixture = u6_fixture(Some(Arc::<str>::from(u6_png(160))), K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_N7_INITIAL_CHECKPOINT").await;
    let mut continuation = fx.server.expect_response_blocked("U6b N7 results-only continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages),
        k2_script(k2lg_n_closure_frames()));
    fx.server.enqueue_response("/v1/messages", k2_script(k2lg_n_after_frames("msg_u6b_n7_after")));
    fx.session.releases[1].add_permits(1);
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = continuation.wait_blocked() => {},
            _ = &mut turn.running => panic!("H6_N7_CONTINUATION_PROGRESS"),
        }
    }).await.expect("H6_N7_CONTINUATION_PROGRESS");
    {
        let state = fx.session.actor.state.lock().await;
        let deferred = state.message_delivery.held_input.entries.iter()
            .filter(|entry| entry.source == super::interjection::HeldSource::DeferredFollowUp)
            .collect::<Vec<_>>();
        assert_eq!(deferred.len(), 1, "H6_N7_FOLLOWUP_HELD");
        assert!(deferred.iter().any(|entry| matches!(&entry.body,
            super::interjection::HeldBody::Owned(item)
            if serde_json::to_vec(item).unwrap().len() > 32_768)),
            "H6_N7_REAL_OVERSIZED_FOLLOWUP");
    }
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 3, "H6_N7_PENDING_REQUEST_COUNT");
    u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_N7_READABLE_CLOSED_CHECKPOINT").await;
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N7_RELEASE_PROGRESS").expect("H6_N7_TURN_TASK");
    assert!(outcome.is_ok(), "H6_N7_RELEASE_PROGRESS");
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4, "H6_N7_FIRST_RELEASE_REQUEST");
    let image_count = bodies[3]["messages"].as_array().unwrap().iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .filter(|part| part["type"] == "image").count();
    assert_eq!(image_count, 1, "H6_N7_IMAGE_RELEASED_ONCE");
    assert!(fx.session.actor.state.lock().await.message_delivery.held_input.entries.is_empty(),
        "H6_N7_CUSTODY_EMPTY");
    continuation.assert_satisfied();
    json!({"source":"real client MCP image follow-up", "image_side":160,
        "per_entry_byte_limit":false,"held_before_closure":true,
        "first_closed_safe_point_release":true,"image_once":true,"same_turn":true})
}

async fn u6b_deferred_band() -> Value {
    let (mut fixture, mut turn, _, continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &mut fixture.fx;
    u6_queue_resident(fx, "U6b N8 independent human", Vec::new()).await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N8_HUMAN_HELD");
    for index in 0..128 {
        k2lg_n_selected(fx, fx.session.actor.hold_or_push_user_item(
            ConversationItem::system_reminder(format!("U6b N8 constructed follow-up {index}")),
            super::interjection::HeldSource::DeferredFollowUp)).await
            .expect("H6_N8_ACCEPTED_PREFIX");
    }
    let error = k2lg_n_selected(fx, fx.session.actor.hold_or_push_user_item(
        ConversationItem::system_reminder("U6b N8 refused constructed follow-up"),
        super::interjection::HeldSource::DeferredFollowUp)).await
        .expect_err("H6_N8_BAND_LIMIT");
    assert_eq!(error.data.as_ref().and_then(|data| data.get("code")).and_then(Value::as_str),
        Some("native_held_input_incomplete"), "H6_N8_TYPED_INCOMPLETE");
    let message = error.data.as_ref().and_then(|data| data.get("message"))
        .and_then(Value::as_str).unwrap_or(&error.message);
    assert!(message.contains("deferred_band_full"), "H6_N8_NAMED_REASON");
    {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert_eq!(ledger.entries.iter().filter(|entry| entry.source ==
            super::interjection::HeldSource::DeferredFollowUp).count(), 128,
            "H6_N8_PREFIX_RETAINED");
        assert_eq!(ledger.entries.iter().filter(|entry| entry.source ==
            super::interjection::HeldSource::HumanInterjection).count(), 1,
            "H6_N8_HUMAN_INDEPENDENT");
        let refusal = ledger.refusal.as_ref().expect("H6_N8_REFUSAL_RECORD");
        assert_eq!(refusal.source, super::interjection::HeldSource::DeferredFollowUp,
            "H6_N8_REFUSAL_SOURCE");
        assert_eq!(format!("{:?}", refusal.reason), "DeferredBandFull", "H6_N8_REFUSAL_REASON");
        assert_eq!(refusal.observed_len, 128, "H6_N8_REFUSAL_COUNT");
    }
    assert_eq!(fx.server.messages_request_count(), 3, "H6_N8_NO_NEW_CONTINUATION_HTTP");
    assert!(!turn.running.is_finished(), "H6_N8_REAL_LATCH_STILL_ACTIVE");
    continuation.release();
    turn.running.abort();
    let _ = turn.running.await;
    json!({"kind":"constructed custody-call fallback on a real active native latch",
        "producer":"SessionActor::hold_or_push_user_item, not a 129-follow-up tool fixture",
        "typed_outcome":"native_held_input_incomplete", "reason":"deferred_band_full",
        "accepted_prefix":128,"human_independent":true,"new_http":0,
        "real_129_followup_exchange_end_proven":false})
}

async fn u6b_drop_monitor(fx: &K2lgN, running: &mut U6Running, task: &str) {
    fx.loop_tx.send(SessionCommand::DropMonitorNotifications { task_id: task.into() }).unwrap();
    let buffer = fx.session.actor.tool_context.monitor_event_buffer.as_ref().unwrap();
    u6_await_turn_progress(running, "real monitor drop", "H6_L6_DROP_REMOVES_OCCURRENCE",
        "H6_L6_DROP_TURN_ENDED", || async {
            buffer.release_view(xai_interjection_core::events::next_occurrence(),
                |event| event.task_id == task).iter().count() == 0
        }).await;
}

async fn u6b_monitor_protocol(mode: &str) -> Value {
    const TASK: &str = "u6b-lifecycle";
    const TEXT: &str = "U6b lifecycle equal monitor payload";
    let idle = std::rc::Rc::new(K0IdleObserver::default());
    let mut fixture = u6_fixture_controlled(None, K2lgNOptions::default(), U6PersistenceControl {
        idle_observer: Some(idle.clone()), ..Default::default()
    }).await;
    let fx = &mut fixture.fx;
    let schema = json!({"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false});
    let mut turn = u6_handoff_with_salvage(fx, None, Some(schema), None, None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let mut closing = fx.server.expect_response_blocked("U6b final server closure + valid StructuredOutput",
        xai_grok_test_support::InferenceRequestMatcher::foreground(xai_grok_test_support::InferenceEndpoint::Messages),
        k2_script(k2_frames("msg_u6b_monitor_final", vec![
            k2_result("srvtoolu_k2_pending", "alpha__lookup"),
            (json!({"type":"tool_use","id":"client_u6b_monitor_so","name":"StructuredOutput","input":{"answer":"finished"}}), Vec::new())
        ], "tool_use")));
    fx.session.releases[1].add_permits(1);
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = closing.wait_blocked() => {},
            _ = &mut turn.running => panic!("H6_L5_FINAL_SAMPLE_TURN_ENDED"),
        }
    }).await.expect("H6_L5_FINAL_SAMPLE_BOUND");
    let first = u6b_monitor_ingress(fx, &mut turn.running, TASK, TEXT).await;
    if mode == "drop-before-admission" {
        u6b_drop_monitor(fx, &mut turn.running, TASK).await;
    } else {
        k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
        assert_eq!(u6_pinned_monitor_keys(fx).await.len(), 1, "H6_L5_REAL_OWNER_PIN");
    }
    let dropped = matches!(mode, "drop-before-admission" | "drop-held" | "drop-after-cancel");
    let mut delivered_key = first;
    if matches!(mode, "drop-held" | "equal") {
        u6b_drop_monitor(fx, &mut turn.running, TASK).await;
        if mode == "equal" {
            delivered_key = u6b_monitor_ingress(fx, &mut turn.running, TASK, TEXT).await;
            assert!(delivered_key > first, "H6_L11_EQUAL_PAYLOAD_DISTINCT_OCCURRENCE");
            k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
        }
    }
    if mode == "user-start" {
        assert!(fx.session.actor.consume_deferred_completions().await.is_empty(), "H6_L7_USER_START_NO_COMPLETION");
        assert_eq!(u6_pinned_monitor_keys(fx).await.len(), 1, "H6_L7_USER_START_KEEPS_PIN");
    }
    let foreign_owner = xai_interjection_core::events::next_occurrence();
    if mode == "kill" {
        let buffer = fx.session.actor.tool_context.monitor_event_buffer.as_ref().unwrap();
        buffer.push(xai_grok_tools::implementations::grok_build::monitor::types::MonitorEventNotification {
            task_id: "u6b-foreign-pin".into(), event_text: "foreign monitor must stay resident".into(),
            owner_session_id: Some("u6b-foreign-session".into()),
        });
        buffer.pin_matching(foreign_owner, |event| event.task_id == "u6b-foreign-pin", |_, _| true);
        assert_eq!(buffer.release_view(foreign_owner, |_| false).iter().count(), 1, "H6_L8_FOREIGN_PIN_ESTABLISHED");
    }
    if matches!(mode, "cancel" | "rewind" | "kill" | "drop-after-cancel") {
        if mode == "rewind" { fx.session.actor.state.lock().await.rewindable = true; }
        let options = crate::session::CancelOptions {
            cancel_subagents: false, kill_background_tasks: mode == "kill", user_initiated: true,
            trigger: Some(crate::session::CancelTrigger::CtrlC),
            history: if mode == "rewind" { crate::session::CancelHistoryDisposition::RewindIfNoOutput {
                prompt_id: Some(KOXD_U6_PROMPT.into())
            }} else { crate::session::CancelHistoryDisposition::Keep },
        };
        let outcome = fx.session.actor.cancel_running_task(options).await;
        assert!(outcome.turn_stopped, "H6_L7_CANCEL_STOPS_REAL_TURN");
        let joined = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await.expect("H6_L7_CANCEL_OUTCOME_BOUND");
        assert!(matches!(joined, Err(error) if error.is_cancelled()), "H6_L7_CANCEL_OWNS_ABORT");
        if mode == "drop-after-cancel" {
            fx.loop_tx.send(SessionCommand::DropMonitorNotifications { task_id: TASK.into() }).unwrap();
            tokio::time::timeout(K0_TIMEOUT, async {
                while !u6_pinned_monitor_keys(fx).await.is_empty() { tokio::task::yield_now().await; }
            }).await.expect("H6_L6_RETAINED_CANCEL_DROP_REMOVES_PIN");
        }
        if mode == "kill" {
            let buffer = fx.session.actor.tool_context.monitor_event_buffer.as_ref().unwrap();
            assert_eq!(buffer.release_view(foreign_owner, |_| false).iter().count(), 1, "H6_L8_FOREIGN_PIN_SURVIVES_KILL");
            buffer.unpin_all(foreign_owner);
        }
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert!(state.pending_notifications.iter().all(|notice| !matches!(&notice.source,
            NotificationSource::MonitorEvent { task_id } if task_id == TASK)), "H6_L7_CANCEL_NO_NOTIFICATION_COPY");
        let monitor_entries = ledger.entries.iter().filter(|entry| entry.source == super::interjection::HeldSource::MonitorEvent).count();
        assert_eq!(monitor_entries, usize::from(mode != "kill"), "H6_L8_KILL_DISCARDS_OWN_CUSTODY");
        return json!({"mode":mode,"constructed":false,"cancelled":true,"monitor_entries":monitor_entries,
            "dropped":dropped,"http":fx.server.messages_request_count()});
    }
    closing.release();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await.expect("H6_L5_FINAL_OUTCOME_BOUND");
    assert!(matches!(outcome, Ok(Ok(TurnOutcome::Completed { structured_output: Some(Ok(_)), .. }))),
        "H6_L5_MATCHING_FINAL_RESPONSE_COMPLETED");
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_L5_COMMITTED_CLOSURE_READ").await;
    let response = checkpoint.source.responses.iter().find(|response|
        response.provider_response_id.as_ref().and_then(Value::as_str) == Some("msg_u6b_monitor_final"))
        .expect("H6_L5_MATCHING_SERVER_RESULT");
    assert!(checkpoint.ledger.transitions.iter().any(|entry|
        entry["request"].as_str() == Some(response.request.as_str()) &&
        entry["occurrence"].as_u64() == Some(response.occurrence) && entry["primary_resolution"] == "Committed"),
        "H6_L5_PRIMARY_COMMITTED");
    fx.session.actor.state.lock().await.notifications_suppressed = true;
    let (processed, acknowledgement) = tokio::sync::oneshot::channel();
    fx.loop_tx.send(SessionCommand::InjectTurnCompletion {
        prompt_id: KOXD_U6_PROMPT.into(), epoch: turn.epoch,
        result: Box::new(crate::session::commands::ok_end_turn(0, None)), elapsed_ms: Some(0), processed,
    }).unwrap();
    tokio::time::timeout(K0_TIMEOUT, acknowledgement).await.expect("H6_L5_COMPLETION_COMMAND_BOUND").unwrap();
    tokio::time::timeout(K0_TIMEOUT, idle.notification.notified()).await.expect("H6_L5_POST_DRAIN_IDLE_OBSERVER");
    assert!(idle.count.get() > 0, "H6_L5_REGISTERED_IDLE_CONTRIBUTOR");
    {
        let state = fx.session.actor.state.lock().await;
        assert!(state.running_task.is_none(), "H6_L5_IDLE_AFTER_REAL_COMPLETION");
        assert!(state.pending_notifications.iter().all(|notice| !matches!(&notice.source,
            NotificationSource::MonitorEvent { task_id } if task_id == TASK)), "H6_L5_NO_SECOND_PENDING_EVENT");
        let ledger = &state.message_delivery.held_input;
        let pinned = ledger.bands(fx.session.actor.live_parent_steer_slots(&state)).monitor.held;
        assert_eq!(pinned, usize::from(!dropped), "H6_L7_COMPLETION_SWEEP_KEEPS_OWNER_PIN");
        if !dropped {
            assert!(ledger.entries.iter().any(|entry| entry.key.as_u64() == delivered_key),
                "H6_L7_COMPLETION_SWEEP_KEEPS_LEDGER_REFERENCE");
        }
    }
    if mode == "idle" {
        fx.session.actor.state.lock().await.notifications_suppressed = false;
        let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
        fx.session.actor.clone().maybe_drain_notifications(completion_tx).await;
        assert_eq!(u6_pinned_monitor_keys(fx).await.len(), 1, "H6_L7_IDLE_SWEEP_KEEPS_PIN");
        fx.session.actor.state.lock().await.notifications_suppressed = true;
    }
    if mode == "no-next-turn" {
        assert_eq!(fx.server.messages_request_count(), 3, "H6_L5_NO_NEXT_TURN_NO_HTTP");
        return json!({"mode":mode,"constructed":false,"post_drain_observer":true,"http":3,
            "pinned_key":delivered_key,"idle_count":idle.count.get()});
    }
    let mut next = fx.server.expect_response_blocked("U6b actual next notification-turn request",
        xai_grok_test_support::InferenceRequestMatcher::foreground(xai_grok_test_support::InferenceEndpoint::Messages),
        k2_script(k2lg_n_after_frames("msg_u6b_monitor_next")));
    fx.session.actor.state.lock().await.notifications_suppressed = false;
    fx.loop_tx.send(SessionCommand::InjectNotification {
        prompt_id: "u6b-monitor-next-notification".into(),
        prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new("U6b independent completion notification"))],
        priority: NotificationPriority::Next,
        source: NotificationSource::MonitorCompleted { task_id: "u6b-independent-next".into() },
    }).unwrap();
    tokio::time::timeout(K0_TIMEOUT, next.wait_blocked()).await.expect("H6_L5_ACTUAL_NEXT_NOTIFICATION_HTTP");
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4, "H6_L5_NEXT_NOTIFICATION_ONLY_ONE_REQUEST");
    let monitor_count = |body: &Value| body["messages"].as_array().into_iter().flatten()
        .filter(|message| message["role"] == "user")
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str())
        .map(|text| text.matches(TEXT).count()).sum::<usize>();
    let counts = bodies.iter().map(monitor_count).collect::<Vec<_>>();
    assert_eq!(counts, vec![0, 0, 0, usize::from(!dropped)], "H6_L11_ONLY_CURRENT_EQUAL_OCCURRENCE_DELIVERS");
    assert!(u6_pinned_monitor_keys(fx).await.is_empty(), "H6_L5_SUBMITTED_PIN_REMOVED");
    {
        let state = fx.session.actor.state.lock().await;
        assert!(state.message_delivery.held_input.entries.iter().all(|entry|
            entry.source != super::interjection::HeldSource::MonitorEvent), "H6_L6_ABSENT_KEY_SUPPRESSES_AND_PRUNES_RELEASE");
    }
    next.release();
    tokio::time::timeout(K0_TIMEOUT, idle.notification.notified()).await.expect("H6_L5_NEXT_NOTIFICATION_COMPLETES");
    closing.assert_satisfied();
    next.assert_satisfied();
    json!({"mode":mode,"constructed":false,"post_drain_observer":true,"http":4,
        "monitor_counts":counts,"first_key":first,"delivered_key":delivered_key})
}

async fn u6b_n10() -> Value {
    use super::interjection::{HeldBand, HeldBands, HeldBindingMeta, HeldCommitError, HeldParentSelection, HeldSource};
    let (fixture, turn, stepper, _continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    stepper.pause();
    let actor = &fixture.fx.session.actor;
    let _parent = u6_admit_steer(actor, "U6b N10 live projecting Steer", "u6b-n10").await;
    let mut state = actor.state.lock().await;
    let task = state.running_task.as_ref().expect("H6_N10_RUNNING_OWNER");
    let binding = xai_message_delivery_core::TurnBinding::new(task.prompt_id.clone(), task.epoch);
    let meta = HeldBindingMeta { prompt_id: task.prompt_id.clone(), epoch: format!("{:?}", task.epoch) };
    assert_eq!(actor.live_parent_steer_slots(&state), 1, "H6_N10_PENDING_STEER_SLOT");
    let result = actor.commit_held_parent_batch(&mut state, &binding, &meta, HeldParentSelection::All,
        |parents, _| {
            assert_eq!(parents.len(), 1, "H6_N10_REAL_PARENT_COMMIT_MEMBERSHIP");
            Err(HeldCommitError::ChatStateUnavailable)
        });
    assert!(matches!(result, Err(HeldCommitError::ChatStateUnavailable)), "H6_N10_PROJECTING_FAILURE_INSTALLED");
    assert!(actor.held_parent_views(&state, &binding, &meta).is_empty(), "H6_N10_PENDING_ONLY_VIEW_EMPTY");
    let live = actor.live_parent_steer_slots(&state);
    assert_eq!(live, 1, "H6_N10_ALL_LIVE_STEER_SLOTS");
    let ledger = &state.message_delivery.held_input;
    let count = |source| ledger.entries.iter().filter(|entry| entry.source == source).count();
    let entry_band = |source, limit| HeldBand { held: count(source), limit, refused_total: 0 };
    let pin_count = match (ledger.pin_owner, actor.tool_context.monitor_event_buffer.as_ref()) {
        (Some(owner), Some(buffer)) => buffer.release_view(owner, |_| false).iter().count(),
        _ => 0,
    };
    let expected = [
        entry_band(HeldSource::HumanInterjection, None),
        HeldBand { held: 1, limit: Some(32), refused_total: 0 },
        HeldBand { held: pin_count, limit: Some(128), refused_total: 0 },
        entry_band(HeldSource::DeferredFollowUp, Some(128)),
        entry_band(HeldSource::StationarityReminder, Some(128)),
        entry_band(HeldSource::SkillReminder, Some(128)),
        entry_band(HeldSource::PlanActivation, Some(1)),
    ];
    let actual: HeldBands = ledger.bands(live);
    assert_eq!([actual.human, actual.steer, actual.monitor, actual.deferred, actual.stationarity, actual.skill, actual.plan],
        expected, "H6_N10_BAND_STORE_RECOUNT");
    assert_eq!(actual.parent_stamps, ledger.parent_stamps.len(), "H6_N10_STAMP_RECOUNT");
    json!({"constructed":true,"reason":"real admitted parent, deliberately failed batch creates Projecting",
        "live_steer_slots":live,"pending_view":0,"monitor_pins":pin_count,"parent_stamps":actual.parent_stamps,
        "human":actual.human.held,"deferred":actual.deferred.held,"stationarity":actual.stationarity.held,
        "skill":actual.skill.held,"plan":actual.plan.held})
}

async fn u6_l2_loop_top_structured_output_retry() -> Value {
    const HUMAN: &str = "SOQ L2 loop-top held human";
    const INVALID_CALL: &str = "client_soq_so_invalid";
    const VALID_CALL: &str = "client_soq_so_valid";
    let last_stage = std::cell::Cell::new("S1");
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        let record = |check: &str, token: &str, observed: Value, expected: Value, holds: bool, premises: &[&str]| {
            u6_observe(json!({"stage":"SOQ oracle","check":check,"observed":observed,"expected":expected,"holds":holds,"premises":premises}));
            assert!(holds, "{token}");
        };
        let digest = |value: &Value, token: &str| {
            format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(serde_json::to_vec(value).expect(token)))
        };
        let parts = |body: &Value| -> Vec<(usize, usize, Value)> {
            body.get("messages").and_then(Value::as_array).into_iter().flatten().enumerate()
                .flat_map(|(i, message)| message.get("content").and_then(Value::as_array).into_iter().flatten().enumerate()
                    .map(move |(j, part)| (i, j, part.clone())))
                .collect()
        };
        let user_parts = |body: &Value| -> Vec<(usize, usize, Value)> {
            parts(body).into_iter().filter(|(i, _, _)| body["messages"][*i]["role"] == "user").collect()
        };
        let containment = |body: &Value| user_parts(body).iter()
            .filter(|(_, _, part)| part["type"] == "text" && part["text"].as_str().is_some_and(|text| text.contains(HUMAN))).count();
        let primary = |checkpoint: &K2Checkpoint, request: &str, lease: u64, attempt: u64, occurrence: u64| -> Vec<Value> {
            checkpoint.ledger.transitions.iter().filter(|entry| entry.get("primary_resolution").is_some()
                && entry["request"].as_str() == Some(request) && entry["lease"].as_u64() == Some(lease)
                && entry["attempt"].as_u64() == Some(attempt) && entry["occurrence"].as_u64() == Some(occurrence)).cloned().collect()
        };
        let schema = json!({"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false});
        let invalid_input = json!({"verdict":7});
        let valid_input = json!({"answer":"SOQ final valid structured output"});
        u6_observe(json!({"stage":"SOQ S1 real fixture","bookkeeping_feedback":false,"media_resample":false}));
        let mut fixture = u6_fixture_with_bookkeeping(None, K2lgNOptions::default(), false, false).await;
        let updates = fixture.persistence_control.updates.clone();
        let fx = &mut fixture.fx;
        last_stage.set("S2");
        let budget: Option<u32> = None;
        u6_observe(json!({"stage":"SOQ S2 schema-bearing handoff","schema_sha256":digest(&schema,"H6_SOQ_S5_DECLARATIONS"),"salvage_budget_enabled":budget.is_some_and(|value| value > 0)}));
        let mut turn = u6_handoff_with_salvage(fx, budget, Some(schema.clone()), None, None).await;
        last_stage.set("S3");
        let _stop = K0Abort(turn.running.abort_handle());
        last_stage.set("S4");
        u6_observe(json!({"stage":"SOQ S4 LivePending checkpoint"}));
        let (checkpoint, _) = u6_quiescent_reload(fx, "H6_SOQ_R20_RELOAD").await;
        let live = k2lg_n_live(&checkpoint);
        let origin = checkpoint.source.producing_bindings.last().map(|binding| binding.origin_ref.clone());
        u6_observe(json!({"stage":"SOQ S4 live and origin","live":live,"origin_found":origin.is_some()}));
        assert!(live && origin.is_some(), "H6_SOQ_S4_LIVE_PENDING");
        let origin = origin.expect("H6_SOQ_S4_LIVE_PENDING");
        let original_groups: Vec<_> = checkpoint.ledger.groups.iter().filter(|group| {
            group.origin_ref == origin && group.members.iter().any(|member| {
                checkpoint.source.ordered_blocks.get(member.occurrence as usize)
                    .and_then(|block| checkpoint.blobs.get(&block.block.digest))
                    .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
                    .is_some_and(|raw| raw["id"] == "srvtoolu_k2_pending")
            })
        }).collect();
        u6_observe(json!({"stage":"SOQ S4 original group","matching_groups":original_groups.len()}));
        assert!(original_groups.len() == 1, "H6_SOQ_S4_LIVE_PENDING");
        let original_group = (*original_groups[0]).clone();
        last_stage.set("S5");
        let initial_bodies = k2lg_n_bodies(&fx.server);
        u6_observe(json!({"stage":"SOQ S5 request snapshot","http":initial_bodies.len()}));
        assert!(initial_bodies.len() == 2, "H6_SOQ_S5_DECLARATIONS");
        let tools = initial_bodies[1]["tools"].clone();
        let tool_entries: Vec<_> = tools.as_array().into_iter().flatten().collect();
        let mut tool_names: Vec<_> = tool_entries.iter().filter_map(|tool| tool["name"].as_str().map(str::to_owned)).collect();
        tool_names.sort();
        let tools_digest = digest(&tools, "H6_SOQ_S5_DECLARATIONS");
        u6_observe(json!({"stage":"SOQ S5 declaration names","names":tool_names,"tools_sha256":tools_digest}));
        assert!(["alpha__lookup","zeta__fetch"].iter().all(|name| tool_names.iter().any(|actual| actual == name)), "H6_SOQ_S5_DECLARATIONS");
        let so_entries: Vec<_> = tool_entries.iter().filter(|tool| tool["name"] == "StructuredOutput").collect();
        let so_schema_equal = so_entries.first().is_some_and(|entry| entry["input_schema"] == schema);
        u6_observe(json!({"stage":"SOQ S5 StructuredOutput declaration","count":so_entries.len(),"input_schema_equal":so_schema_equal}));
        assert!(so_entries.len() == 1 && so_schema_equal, "H6_SOQ_S5_STRUCTURED_OUTPUT_DECLARATION");
        let so_digest = digest(so_entries[0], "H6_SOQ_S5_STRUCTURED_OUTPUT_DECLARATION");
        let schema_reminder = user_parts(&initial_bodies[1]).iter().any(|(_, _, part)| part["type"] == "text"
            && part["text"].as_str().is_some_and(|text| text.contains("A response schema is required")));
        u6_observe(json!({"stage":"SOQ S5 declaration snapshot","tools_sha256":tools_digest,"structured_output_declaration_sha256":so_digest,"schema_reminder_present":schema_reminder}));
        last_stage.set("S6");
        u6_observe(json!({"stage":"SOQ S6 real resident ingress"}));
        u6_interject(fx, HUMAN, Vec::new()).await;
        let resident_s6 = fx.session.actor.pending_interjections.snapshot().iter().filter(|item| item.text == HUMAN).count();
        let history_s6 = fx.session.actor.chat_state_handle.get_conversation().await;
        let history_count_s6 = history_s6.iter().filter(|item| item.text_content().contains(HUMAN)).count();
        u6_observe(json!({"stage":"SOQ S6 resident precondition","resident":resident_s6,"human_in_history":history_count_s6}));
        assert!(resident_s6 == 1 && history_count_s6 == 0, "H6_SOQ_S6_RESIDENT");
        last_stage.set("S7");
        let matcher = xai_grok_test_support::InferenceRequestMatcher::foreground(xai_grok_test_support::InferenceEndpoint::Messages);
        let mut n = fx.server.expect_response_blocked("SOQ N closure + invalid StructuredOutput", matcher, k2_script(k2_frames(
            "msg_soq_n_closed", vec![k2_result("srvtoolu_k2_pending", "alpha__lookup"),
                (json!({"type":"tool_use","id":INVALID_CALL,"name":"StructuredOutput","input":invalid_input.clone()}), Vec::new())], "tool_use")));
        let mut n1 = fx.server.expect_response_blocked("SOQ N+1 valid StructuredOutput", matcher, k2_script(k2_frames(
            "msg_soq_n1_valid", vec![(json!({"type":"tool_use","id":VALID_CALL,"name":"StructuredOutput","input":valid_input.clone()}), Vec::new())], "tool_use")));
        last_stage.set("S8");
        u6_observe(json!({"stage":"SOQ S8 release second handoff client"}));
        fx.session.releases[1].add_permits(1);
        last_stage.set("S9");
        let reached = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! { biased; _ = n.wait_blocked() => None, outcome = &mut turn.running => Some(outcome) }
        }).await;
        let progress = json!({"http_count":fx.server.messages_request_count(),"human_in_history":fx.session.actor.chat_state_handle.get_conversation().await.iter().filter(|item| item.text_content().contains(HUMAN)).count()});
        match reached {
            Ok(None) => {},
            Ok(Some(outcome)) => u6_turn_ended(outcome, "SOQ S9 Barrier B1", "H6_SOQ_B1_TURN_ENDED", progress),
            Err(_) => u6_bound_expired(&turn.running, "SOQ S9 Barrier B1", "H6_SOQ_B1_BOUND", progress),
        }
        u6_observe(json!({"stage":"SOQ barrier","barrier":"B1"}));
        let b1_index = U6_ARM_OBSERVATIONS.with(|records| records.borrow().len().saturating_sub(1));
        last_stage.set("O1");
        let bodies_b1 = k2lg_n_bodies(&fx.server);
        record("O1.1", "H6_SOQ_O1_1", json!({"http":bodies_b1.len()}), json!({"http":3}), bodies_b1.len() == 3, &["PR-01","PR-03"]);
        u6_observe(json!({"stage":"SOQ O1.1 committed results-only check"}));
        u6_assert_results_only(&checkpoint, &bodies_b1[1], &bodies_b1[2]);
        let history_b1 = fx.session.actor.chat_state_handle.get_conversation().await;
        let history_count_b1 = history_b1.iter().filter(|item| item.text_content().contains(HUMAN)).count();
        let containment_b1 = containment(&bodies_b1[2]);
        record("O1.2", "H6_SOQ_O1_2", json!({"body_containment":containment_b1,"history":history_count_b1}), json!({"body_containment":0,"history":0}), containment_b1 == 0 && history_count_b1 == 0, &["PR-03"]);
        let running_b1 = !turn.running.is_finished();
        record("O1.3", "H6_SOQ_O1_3", json!(running_b1), json!(true), running_b1, &["PR-17"]);
        let held_b1 = {
            let state = fx.session.actor.state.lock().await;
            state.message_delivery.held_input.entries.iter().filter(|entry| matches!(&entry.body, super::interjection::HeldBody::Human(raw) if raw.text == HUMAN)).count()
        };
        let resident_b1 = fx.session.actor.pending_interjections.snapshot().iter().filter(|item| item.text == HUMAN).count();
        record("O1.4", "H6_SOQ_O1_4", json!({"held":held_b1,"resident":resident_b1}), json!({"held":1,"resident":0}), held_b1 == 1 && resident_b1 == 0, &["PR-03","PR-10"]);
        u6_observe(json!({"stage":"SOQ B1 snapshots","body_sha256":bodies_b1.iter().map(|body| digest(body,"H6_SOQ_O1_1")).collect::<Vec<_>>(),"human_containment_by_body":bodies_b1.iter().map(|body| containment(body)).collect::<Vec<_>>()}));
        last_stage.set("S10");
        let stepper = u6_current_stepper();
        let was_paused = stepper.0.paused.get();
        stepper.pause();
        let n_ownership_snapshot: std::collections::HashSet<_> = {
            let owned = fx.session.actor.turn_stream_drained.lock();
            owned.keys().cloned().collect()
        };
        u6_observe(json!({"stage":"SOQ C1 ownership snapshot","n_ownership_snapshot_size":n_ownership_snapshot.len()}));
        assert!(!n_ownership_snapshot.is_empty(), "H6_SOQ_N_OWNERSHIP_SNAPSHOT_EMPTY");
        n.release();
        let steps = std::cell::Cell::new(0u32);
        let drain_holds = std::cell::Cell::new(0u32);
        let persisted_reads = std::cell::Cell::new(0u32);
        let rounds = std::cell::Cell::new(0u32);
        let last_counter = std::cell::Cell::new(std::time::Instant::now());
        let n_response_found = std::cell::Cell::new(None::<usize>);
        let last_primary = std::cell::RefCell::new(Value::Null);
        let saved = std::cell::RefCell::new(None);
        let (f, st, s, d, pr, r, clock, ownership, found, resolution, saved_ref, primary_ref) =
            (&*fx, &stepper, &steps, &drain_holds, &persisted_reads, &rounds, &last_counter,
             &n_ownership_snapshot, &n_response_found, &last_primary, &saved, &primary);
        u6_await_turn_progress(&mut turn.running, "SOQ S10 N committed closure", "H6_SOQ_N_COMMITTED_CHECKPOINT", "H6_SOQ_N_COMMITTED_TURN_ENDED", || async move {
            let mut new_ownership_keys: Vec<_> = {
                let owned = f.session.actor.turn_stream_drained.lock();
                owned.keys().filter(|key| !ownership.contains(*key)).map(|key| key.as_str().to_owned()).collect()
            };
            new_ownership_keys.sort();
            let http = f.server.messages_request_count();
            r.set(r.get() + 1);
            if !new_ownership_keys.is_empty() || http > 3 {
                u6_observe(json!({"stage":"SOQ C1 negative","request_after_n_before_committed":true,"new_ownership_keys":new_ownership_keys,"messages_request_count":http,"round":r.get(),"steps":s.get(),"drain_holds":d.get(),"persisted_reads":pr.get(),"n_response_found":found.get(),"n_primary":resolution.borrow().clone()}));
                panic!("H6_SOQ_O2_6");
            }
            if r.get() == 1 || clock.get().elapsed() >= Duration::from_millis(250) {
                u6_observe(json!({"stage":"SOQ wait counters","round":r.get(),"steps":s.get(),"drain_holds":d.get(),"persisted_reads":pr.get(),"messages_request_count":http,"n_response_found":found.get(),"n_primary":resolution.borrow().clone(),"new_ownership_keys":new_ownership_keys}));
                clock.set(std::time::Instant::now());
            }
            let draining = !f.session.actor.turn_stream_drained.lock().is_empty();
            if draining {
                d.set(d.get() + 1);
                tokio::time::sleep(Duration::from_millis(2)).await;
                return false;
            }
            let actor = f.session.actor.chat_state_handle.read_native_history().await;
            if let Some(checkpoint) = actor.as_ref().ok().and_then(|read| read.checkpoint.as_ref()) {
                let responses: Vec<_> = checkpoint.source.responses.iter().filter(|response|
                    response.provider_response_id.as_ref().and_then(Value::as_str) == Some("msg_soq_n_closed")).collect();
                found.set(Some(responses.len()));
                let n_primary = if responses.len() == 1 {
                    let response = responses[0];
                    primary_ref(checkpoint, &response.request, response.lease, response.attempt, response.occurrence)
                } else { Vec::new() };
                let current = if n_primary.len() == 1 { n_primary[0]["primary_resolution"].clone() } else { Value::Null };
                resolution.replace(current.clone());
                if n_primary.len() == 1 && current == "Rejected" {
                    u6_observe(json!({"stage":"SOQ C1 negative","n_primary":"Rejected","n_response_found":responses.len(),"round":r.get(),"steps":s.get()}));
                    panic!("H6_SOQ_O2_3");
                }
                if n_primary.len() == 1 && current == "Committed" {
                    pr.set(pr.get() + 1);
                    let disk = u6_persisted_native_read(f).await;
                    if disk.as_ref().ok().and_then(|read| read.checkpoint.as_ref()).is_some_and(|disk| disk.source == checkpoint.source && disk.ledger == checkpoint.ledger) {
                        saved_ref.replace(Some((**checkpoint).clone()));
                        return true;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    return false;
                }
            }
            if st.0.finished.get() { return false; }
            if st.0.wake.ready.load(std::sync::atomic::Ordering::SeqCst) {
                s.set(s.get() + 1);
                if s.get() > 512 {
                    u6_observe(json!({"stage":"SOQ C1 step bound","steps":s.get(),"round":r.get()}));
                    panic!("H6_SOQ_N_COMMITTED_STEP_BOUND");
                }
                let _ = st.step().await;
            } else {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            false
        }).await;
        last_stage.set("O2");
        let closed = saved.into_inner();
        u6_observe(json!({"stage":"SOQ O2 saved checkpoint","found":closed.is_some(),"stepper_paused":stepper.0.paused.get()}));
        assert!(closed.is_some() && stepper.0.paused.get(), "H6_SOQ_N_COMMITTED_CHECKPOINT");
        let closed = closed.expect("H6_SOQ_N_COMMITTED_CHECKPOINT");
        let n_responses: Vec<_> = closed.source.responses.iter().filter(|response|
            response.provider_response_id.as_ref().and_then(Value::as_str) == Some("msg_soq_n_closed")).collect();
        record("O2.1", "H6_SOQ_O2_1", json!({"n_responses":n_responses.len()}), json!({"n_responses":1}), n_responses.len() == 1, &["PR-04","PR-22"]);
        let n_response = n_responses[0];
        let n_usages: Vec<_> = closed.ledger.usage.iter().filter(|usage| usage.request == n_response.request
            && usage.lease == n_response.lease && usage.attempt == n_response.attempt
            && usage.response_occurrence == Some(n_response.occurrence)).collect();
        record("O2.1", "H6_SOQ_O2_1", json!({"usage_count":n_usages.len()}), json!({"usage_count":1}), n_usages.len() == 1, &["PR-04","PR-22"]);
        let n_usage = n_usages[0];
        let accepted = matches!(n_response.status, k2_history::NativeResponseStatus::Accepted)
            && matches!(n_usage.outcome, k2_history::NativeResponseStatus::Accepted);
        let tool_use_stop = n_usage.source_stop_reason.as_deref() == Some("tool_use");
        u6_observe(json!({"stage":"SOQ N coordinates","request":n_response.request,"lease":n_response.lease,"attempt":n_response.attempt,"occurrence":n_response.occurrence,"source_stop_reason":n_usage.source_stop_reason,"status":n_response.status,"outcome":n_usage.outcome}));
        record("O2.1", "H6_SOQ_O2_1", json!({"n_responses":n_responses.len(),"usage_count":n_usages.len(),"accepted":accepted,"tool_use_stop":tool_use_stop}), json!({"n_responses":1,"usage_count":1,"accepted":true,"tool_use_stop":true}), accepted && tool_use_stop, &["PR-04","PR-22"]);
        let groups: Vec<_> = closed.ledger.groups.iter().filter(|group| group.key == original_group.key && group.origin_ref == origin).collect();
        let member_from_n = |group: &k2_history::NativeGroupRecord| group.members.iter().any(|member|
            closed.source.ordered_blocks.get(member.occurrence as usize).is_some_and(|block| block.coordinates.response == n_response.occurrence));
        let matching_result = groups.first().is_some_and(|group| group.members.iter().any(|member| {
            closed.source.ordered_blocks.get(member.occurrence as usize).filter(|block| block.coordinates.response == n_response.occurrence)
                .and_then(|block| closed.blobs.get(&block.block.digest)).and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
                .is_some_and(|raw| raw["type"] == "tool_search_tool_result" && raw["tool_use_id"] == "srvtoolu_k2_pending")
        }));
        let orphan_groups = closed.ledger.groups.iter().filter(|group| group.key != original_group.key && member_from_n(group)).count();
        let completed_join = groups.first().is_some_and(|group| group.state == "Completed" && group.validated_join.as_deref() == Some("srvtoolu_k2_pending"));
        record("O2.2", "H6_SOQ_O2_2", json!({"same_group_count":groups.len(),"completed_validated_join":completed_join,"matching_result_from_n":matching_result,"other_groups_from_n":orphan_groups}), json!({"same_group_count":1,"completed_validated_join":true,"matching_result_from_n":true,"other_groups_from_n":0}), groups.len() == 1 && completed_join && matching_result && orphan_groups == 0, &["PR-05"]);
        let n_primary = primary(&closed, &n_response.request, n_response.lease, n_response.attempt, n_response.occurrence);
        let committed = n_primary.len() == 1 && n_primary[0]["primary_resolution"] == "Committed";
        record("O2.3", "H6_SOQ_O2_3", json!({"primary_count":n_primary.len(),"committed":committed}), json!({"primary_count":1,"committed":true}), committed, &["PR-06"]);
        let history_c1 = fx.session.actor.chat_state_handle.get_conversation().await;
        let n_history = history_c1.iter().filter(|item| matches!(item, ConversationItem::Assistant(assistant) if assistant.tool_calls.iter().any(|call| call.id.as_ref() == INVALID_CALL))).count();
        let n_roles: Vec<_> = closed.active.view_links.iter().filter(|link| link.source.response == n_response.occurrence)
            .map(|link| serde_json::to_value(&link.role).expect("H6_SOQ_O2_4")).collect();
        let discovery_link = n_roles.iter().any(|role| role == "Discovery");
        record("O2.4", "H6_SOQ_O2_4", json!({"history_assistant_calls":n_history,"view_links":n_roles.len(),"roles":n_roles,"discovery_link":discovery_link}), json!({"history_assistant_calls":1,"has_view_links":true,"discovery_link":true}), n_history == 1 && !n_roles.is_empty() && discovery_link, &["PR-06"]);
        let masked_groups = closed.ledger.groups.iter().filter(|group| group.state == "LivePending" || group.state == "AbandonedPending").count();
        record("O2.5", "H6_SOQ_O2_5", json!({"masked_groups":masked_groups,"live":k2lg_n_live(&closed)}), json!({"masked_groups":0,"live":false}), masked_groups == 0 && !k2lg_n_live(&closed), &["PR-05"]);
        let held_c1 = {
            let state = fx.session.actor.state.lock().await;
            state.message_delivery.held_input.entries.iter().filter(|entry| matches!(&entry.body, super::interjection::HeldBody::Human(raw) if raw.text == HUMAN)).count()
        };
        let resident_c1 = fx.session.actor.pending_interjections.snapshot().iter().filter(|item| item.text == HUMAN).count();
        let history_count_c1 = history_c1.iter().filter(|item| item.text_content().contains(HUMAN)).count();
        let bodies_c1 = k2lg_n_bodies(&fx.server);
        let containment_c1 = bodies_c1.get(2).map(&containment);
        let observations_c1 = U6_ARM_OBSERVATIONS.with(|records| records.borrow().clone());
        let b6_b1_c1: Vec<_> = observations_c1.iter().skip(b1_index + 1).filter(|event| matches!(event["stage"].as_str(), Some("native safe point" | "merged commit recheck" | "merged release outcome"))).cloned().collect();
        let release_before_c1 = b6_b1_c1.iter().any(|event| event["stage"] == "merged release outcome" || (event["stage"] == "native safe point" && event["release_path"] == "merged_release"));
        let running_c1 = !turn.running.is_finished();
        record("O2.6", "H6_SOQ_O2_6", json!({"held":held_c1,"resident":resident_c1,"history":history_count_c1,"http":fx.server.messages_request_count(),"body_containment":containment_c1,"release_record":release_before_c1,"running":running_c1}), json!({"held":1,"resident":0,"history":0,"http":3,"body_containment":0,"release_record":false,"running":true}), held_c1 == 1 && resident_c1 == 0 && history_count_c1 == 0 && fx.server.messages_request_count() == 3 && containment_c1 == Some(0) && !release_before_c1 && running_c1, &["PR-09","PR-16"]);
        u6_observe(json!({"stage":"SOQ B6 window B1 C1","observations":b6_b1_c1}));
        let submits_c1 = turn.submits.borrow().clone();
        let bound_c1 = submits_c1.len() == 2 && submits_c1[0]["tools"] == submits_c1[1]["tools"] && submits_c1[0]["bound_model"] == submits_c1[1]["bound_model"];
        let body_authority_c1 = bodies_c1.len() == 3 && bodies_c1[2]["tools"] == tools && bodies_c1[2]["model"] == initial_bodies[1]["model"];
        record("O2.7", "H6_SOQ_O2_7", json!({"submits":submits_c1.len(),"equal_bound_tools_model":bound_c1,"equal_body_tools_model":body_authority_c1}), json!({"submits":2,"equal_bound_tools_model":true,"equal_body_tools_model":true}), bound_c1 && body_authority_c1, &["PR-18"]);
        u6_observe(json!({"stage":"SOQ closure checkpoint C1","steps":steps.get(),"drain_holds":drain_holds.get(),"persisted_reads":persisted_reads.get(),"rounds":rounds.get(),"n_response_found":n_response_found.get(),"n_primary":last_primary.borrow().clone()}));
        let c1_index = U6_ARM_OBSERVATIONS.with(|records| records.borrow().len().saturating_sub(1));
        if !was_paused { stepper.resume(); }
        last_stage.set("S11");
        let reached = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! { biased; _ = n1.wait_blocked() => None, outcome = &mut turn.running => Some(outcome) }
        }).await;
        let progress = json!({"http_count":fx.server.messages_request_count(),"human_in_history":fx.session.actor.chat_state_handle.get_conversation().await.iter().filter(|item| item.text_content().contains(HUMAN)).count()});
        match reached {
            Ok(None) => {},
            Ok(Some(outcome)) => u6_turn_ended(outcome, "SOQ S11 first release HTTP", "H6_SOQ_NO_RELEASE_HTTP", progress),
            Err(_) => u6_bound_expired(&turn.running, "SOQ S11 first release HTTP", "H6_SOQ_B2_BOUND", progress),
        }
        u6_observe(json!({"stage":"SOQ barrier","barrier":"B2"}));
        let b2_index = U6_ARM_OBSERVATIONS.with(|records| records.borrow().len().saturating_sub(1));
        let n1_owned_request_names: Vec<_> = {
            let owned = fx.session.actor.turn_stream_drained.lock();
            owned.keys().map(|key| key.as_str().to_owned()).collect()
        };
        last_stage.set("O3");
        let saved_b2 = std::cell::RefCell::new(None);
        let (f, saved_ref) = (&*fx, &saved_b2);
        u6_await_turn_progress(&mut turn.running, "SOQ O3 checkpoint read", "H6_SOQ_O3_UNREADABLE", "H6_SOQ_B2_TURN_ENDED", || async move {
            let actor = f.session.actor.chat_state_handle.read_native_history().await;
            let disk = u6_persisted_native_read(f).await;
            if let Some(checkpoint) = actor.as_ref().ok().and_then(|read| read.checkpoint.as_ref())
                && disk.as_ref().ok().and_then(|read| read.checkpoint.as_ref()).is_some_and(|disk| disk.source == checkpoint.source && disk.ledger == checkpoint.ledger)
            {
                saved_ref.replace(Some((**checkpoint).clone()));
                true
            } else {
                tokio::time::sleep(Duration::from_millis(2)).await;
                false
            }
        }).await;
        let released = saved_b2.into_inner();
        u6_observe(json!({"stage":"SOQ O3 saved checkpoint","found":released.is_some()}));
        assert!(released.is_some(), "H6_SOQ_O3_UNREADABLE");
        let released = released.expect("H6_SOQ_O3_UNREADABLE");
        let n_still_primary = primary(&released, &n_response.request, n_response.lease, n_response.attempt, n_response.occurrence);
        let same_group = released.ledger.groups.iter().any(|group| group.key == original_group.key && group.origin_ref == origin && group.state == "Completed" && group.validated_join.as_deref() == Some("srvtoolu_k2_pending"));
        let masked_b2 = released.ledger.groups.iter().filter(|group| group.state == "LivePending" || group.state == "AbandonedPending").count();
        let still_committed = n_still_primary.len() == 1 && n_still_primary[0]["primary_resolution"] == "Committed";
        let later_responses = released.source.responses.iter().filter(|response| response.occurrence > n_response.occurrence).count();
        let later_usage = released.ledger.usage.iter().filter(|usage| usage.response_occurrence.is_some_and(|occurrence| occurrence > n_response.occurrence)).count();
        let later_primary = released.ledger.transitions.iter().filter(|entry| entry.get("primary_resolution").is_some() && entry["occurrence"].as_u64().is_some_and(|occurrence| occurrence > n_response.occurrence)).count();
        u6_observe(json!({"stage":"SOQ O3.1 later records diagnostic","responses":later_responses,"usage":later_usage,"primary":later_primary,"required":false}));
        record("O3.1", "H6_SOQ_O3_1", json!({"n_committed":still_committed,"same_completed_join":same_group,"masking_groups":masked_b2}), json!({"n_committed":true,"same_completed_join":true,"masking_groups":0}), still_committed && same_group && masked_b2 == 0, &["PR-05","PR-06","PR-13"]);
        let bodies_b2 = k2lg_n_bodies(&fx.server);
        record("O3.2", "H6_SOQ_O3_2", json!({"http":bodies_b2.len()}), json!({"http":4}), bodies_b2.len() == 4, &["PR-07","PR-12"]);
        let body_parts = parts(&bodies_b2[3]);
        let invalid_calls: Vec<_> = body_parts.iter().filter(|(i, _, part)| bodies_b2[3]["messages"][*i]["role"] == "assistant"
            && part["type"] == "tool_use" && part["id"] == INVALID_CALL && part["name"] == "StructuredOutput").collect();
        let corrections: Vec<_> = user_parts(&bodies_b2[3]).into_iter().filter(|(_, _, part)| part["type"] == "tool_result" && part["tool_use_id"] == INVALID_CALL).collect();
        let argument_equal = invalid_calls.first().is_some_and(|call| call.2["input"] == invalid_input);
        let is_error = corrections.first().is_some_and(|part| part.2["is_error"] == true);
        let corrective_text = corrections.first().map(|part| {
            if let Some(text) = part.2["content"].as_str() { text.to_owned() } else {
                part.2["content"].as_array().into_iter().flatten().filter(|content| content["type"] == "text")
                    .filter_map(|content| content["text"].as_str()).collect::<Vec<_>>().concat()
            }
        }).unwrap_or_default();
        let corrective_present = corrective_text.contains("Fix the arguments and call StructuredOutput again.");
        let corrective_ordered = invalid_calls.first().zip(corrections.first()).is_some_and(|(call, result)| (result.0, result.1) > (call.0, call.1));
        let valid_call_parts = body_parts.iter().filter(|(_, _, part)| part["id"] == VALID_CALL || part["tool_use_id"] == VALID_CALL).count();
        let history_b2 = fx.session.actor.chat_state_handle.get_conversation().await;
        let invalid_results_history = history_b2.iter().filter(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == INVALID_CALL)).count();
        record("O3.2", "H6_SOQ_O3_2", json!({"http":bodies_b2.len(),"invalid_tool_use":invalid_calls.len(),"input_equal":argument_equal,"corrective_results":corrections.len(),"is_error":is_error,"corrective_text_present":corrective_present,"result_after_call":corrective_ordered,"valid_call_parts":valid_call_parts,"history_results":invalid_results_history}), json!({"http":4,"invalid_tool_use":1,"input_equal":true,"corrective_results":1,"is_error":true,"corrective_text_present":true,"result_after_call":true,"valid_call_parts":0,"history_results":1}), invalid_calls.len() == 1 && argument_equal && corrections.len() == 1 && is_error && corrective_present && corrective_ordered && valid_call_parts == 0 && invalid_results_history == 1, &["PR-07","PR-12"]);
        u6_observe(json!({"stage":"SOQ corrective part digests","tool_use_sha256":digest(&invalid_calls[0].2,"H6_SOQ_O3_2"),"tool_result_sha256":digest(&corrections[0].2,"H6_SOQ_O3_2")}));
        let (pending_notifications, executed_notifications, output_notifications) = {
            let updates = updates.lock().expect("H6_SOQ_O3_3");
            let mut counts = (0usize, 0usize, 0usize);
            for update in updates.iter() {
                let payload = update.get("update").unwrap_or(update);
                let id = payload.get("toolCallId").or_else(|| payload.get("tool_call_id")).and_then(Value::as_str);
                if id == Some(INVALID_CALL) {
                    match payload["status"].as_str() {
                        Some("pending") => counts.0 += 1,
                        Some("in_progress" | "inProgress" | "completed") => counts.1 += 1,
                        _ => {},
                    }
                    if payload.get("content").and_then(Value::as_array).is_some_and(|parts| !parts.is_empty()) { counts.2 += 1; }
                }
            }
            counts
        };
        let mut clients = fx.session.executions.lock().expect("H6_SOQ_O3_3").clone();
        clients.sort();
        let clients_unchanged = clients == vec!["alpha__lookup".to_owned(),"zeta__fetch".to_owned()];
        let structured_dispatches = executed_notifications + output_notifications + clients.iter().filter(|name| name.as_str() == "StructuredOutput").count();
        record("O3.3", "H6_SOQ_O3_3", json!({"client_execution_names":clients,"clients_unchanged":clients_unchanged,"pending_notifications":pending_notifications,"executed_notifications":executed_notifications,"output_notifications":output_notifications,"structured_output_dispatches_observed":structured_dispatches}), json!({"clients_unchanged":true,"executed_notifications":0,"output_notifications":0,"structured_output_dispatches_observed":0}), clients_unchanged && executed_notifications == 0 && output_notifications == 0 && structured_dispatches == 0, &["PR-07","PR-21"]);
        let released_human: Vec<_> = history_b2.iter().filter(|item| item.text_content().contains(HUMAN))
            .filter(|item| !history_c1.iter().any(|before| before.text_content() == item.text_content())).map(ConversationItem::text_content).collect();
        record("O3.4", "H6_SOQ_O3_4", json!({"newly_released_history_items":released_human.len()}), json!({"newly_released_history_items":1}), released_human.len() == 1, &["PR-09","PR-10","PR-15","PR-17"]);
        let reference = &released_human[0];
        let human_counts: Vec<_> = bodies_b2.iter().map(|body| user_parts(body).iter().filter(|(_, _, part)| part["type"] == "text" && part["text"].as_str() == Some(reference.as_str())).count()).collect();
        let containment_counts: Vec<_> = bodies_b2.iter().map(|body| containment(body)).collect();
        let human_positions: Vec<_> = user_parts(&bodies_b2[3]).into_iter().filter(|(_, _, part)| part["type"] == "text" && part["text"].as_str() == Some(reference.as_str())).map(|(i, j, _)| (i, j)).collect();
        let ordered = human_positions.first().is_some_and(|position| *position > (invalid_calls[0].0, invalid_calls[0].1) && *position > (corrections[0].0, corrections[0].1));
        let (held_b2, same_task_epoch) = {
            let state = fx.session.actor.state.lock().await;
            (state.message_delivery.held_input.entries.iter().filter(|entry| matches!(&entry.body, super::interjection::HeldBody::Human(raw) if raw.text == HUMAN)).count(),
             state.running_task.as_ref().is_some_and(|task| task.identity == turn.identity && task.epoch == turn.epoch))
        };
        let history_count_b2 = history_b2.iter().filter(|item| item.text_content().contains(HUMAN)).count();
        let running_b2 = !turn.running.is_finished();
        let counts_hold = human_counts == vec![0,0,0,1] && containment_counts == human_counts
            && containment_b1 == human_counts[2] && containment_c1 == Some(human_counts[2]);
        u6_observe(json!({"stage":"SOQ structural HUMAN rule","rule":"user text part byte-equals the unique actor-history item newly released after C1; per-part containment agrees at B1 and C1","human_count_by_body":human_counts,"human_containment_by_body":containment_counts,"matched_text_sha256":format!("{:x}",<sha2::Sha256 as sha2::Digest>::digest(reference.as_bytes())),"body_sha256":bodies_b2.iter().map(|body| digest(body,"H6_SOQ_O3_4")).collect::<Vec<_>>()}));
        record("O3.4", "H6_SOQ_O3_4", json!({"counts_hold":counts_hold,"human_positions":human_positions.len(),"after_assistant_and_result":ordered,"history":history_count_b2,"held":held_b2,"same_task_epoch":same_task_epoch,"running_b1":running_b1,"running_c1":running_c1,"running_b2":running_b2}), json!({"counts_hold":true,"human_positions":1,"after_assistant_and_result":true,"history":1,"held":0,"same_task_epoch":true,"running_b1":true,"running_c1":true,"running_b2":true}), counts_hold && human_positions.len() == 1 && ordered && history_count_b2 == 1 && held_b2 == 0 && same_task_epoch && running_b1 && running_c1 && running_b2, &["PR-09","PR-10","PR-15","PR-17"]);
        let observations_b2 = U6_ARM_OBSERVATIONS.with(|records| records.borrow().clone());
        let b6_c1_b2: Vec<_> = observations_b2.iter().skip(c1_index + 1).take(b2_index.saturating_sub(c1_index + 1))
            .filter(|event| matches!(event["stage"].as_str(), Some("native safe point" | "merged commit recheck" | "merged release outcome"))).cloned().collect();
        let submitted = b6_c1_b2.iter().enumerate().any(|(index, event)| event["stage"] == "merged release outcome" && event["release_path"] == "submitted"
            && b6_c1_b2.iter().take(index).any(|before| before["stage"] == "native safe point" && before["holding"] == false && before["release_path"] == "merged_release"));
        u6_observe(json!({"stage":"SOQ B6 window C1 B2","observations":b6_c1_b2,"submit_after_closed_safe_point":submitted}));
        record("O3.4", "H6_SOQ_O3_4_NO_SUBMIT_OBSERVATION", json!({"submit_after_closed_safe_point":submitted}), json!({"submit_after_closed_safe_point":true}), submitted, &["PR-15"]);
        let tool_batch_excluded = clients_unchanged && structured_dispatches == 0;
        let nonempty_tool_calls = invalid_calls.len() == 1 && corrective_present && is_error;
        let empty_continuation_excluded = budget.is_none() && tool_use_stop;
        let caller_proven = tool_batch_excluded && nonempty_tool_calls && empty_continuation_excluded && submitted && same_task_epoch;
        let inferred_caller = if caller_proven { "loop-top" } else { "unqualified" };
        let caller_premises = json!([
            {"premise":"PR-09","caller":"tool-batch","excluded":tool_batch_excluded,"observation":"O3.3"},
            {"premise":"PR-09","caller":"pre-completion","excluded":nonempty_tool_calls,"observation":"O3.2"},
            {"premise":"PR-09","caller":"post-bookkeeping","excluded":nonempty_tool_calls,"observation":"O3.2"},
            {"premise":"PR-09","caller":"empty-continuation","excluded":empty_continuation_excluded,"observation":"S2 and O2.1"}]);
        u6_observe(json!({"stage":"SOQ caller premises","caller":inferred_caller,"reading":"D-2","premises":caller_premises}));
        record("O3.5", "H6_SOQ_O3_5", json!({"tool_batch_excluded":tool_batch_excluded,"pre_completion_excluded":nonempty_tool_calls,"post_bookkeeping_excluded":nonempty_tool_calls,"empty_continuation_excluded":empty_continuation_excluded,"loop_top_inferred":caller_proven}), json!({"tool_batch_excluded":true,"pre_completion_excluded":true,"post_bookkeeping_excluded":true,"empty_continuation_excluded":true,"loop_top_inferred":true}), caller_proven, &["PR-09"]);
        let submits_b2 = turn.submits.borrow().clone();
        let bound_b2 = submits_b2.len() == 3 && submits_b2.iter().all(|submit| submit["tools"] == submits_b2[0]["tools"] && submit["bound_model"] == submits_b2[0]["bound_model"]);
        let body_authority_b2 = bodies_b2[3]["tools"] == tools && bodies_b2[3]["model"] == initial_bodies[1]["model"];
        record("O3.6", "H6_SOQ_O3_6", json!({"submits":submits_b2.len(),"equal_bound_tools_model":bound_b2,"equal_body_tools_model":body_authority_b2,"actual_http":fx.server.messages_request_count() == 4}), json!({"submits":3,"equal_bound_tools_model":true,"equal_body_tools_model":true,"actual_http":true}), bound_b2 && body_authority_b2 && fx.server.messages_request_count() == 4, &["PR-11","PR-18"]);
        let after_n_user_parts: Vec<_> = user_parts(&bodies_b2[3]).into_iter().filter(|(i, j, _)| (*i, *j) > (invalid_calls[0].0, invalid_calls[0].1)).collect();
        let reminder_parts: Vec<_> = after_n_user_parts.iter().filter(|(_, _, part)| part["type"] == "text" && part["text"].as_str().is_some_and(|text| text.starts_with("<system-reminder>"))).collect();
        let new_prompt_parts = after_n_user_parts.iter().filter(|(_, _, part)| {
            !((part["type"] == "tool_result" && part["tool_use_id"] == INVALID_CALL)
                || (part["type"] == "text" && part["text"].as_str() == Some(reference.as_str()))
                || (part["type"] == "text" && part["text"].as_str().is_some_and(|text| text.starts_with("<system-reminder>"))))
        }).count();
        u6_observe(json!({"stage":"SOQ O3.7 reminder parts","count":reminder_parts.len(),"kinds":reminder_parts.iter().map(|_| "system-reminder").collect::<Vec<_>>(),"sha256":reminder_parts.iter().map(|part| digest(&part.2,"H6_SOQ_O3_7")).collect::<Vec<_>>()}));
        record("O3.7", "H6_SOQ_O3_7", json!({"http":fx.server.messages_request_count(),"new_prompt_parts":new_prompt_parts}), json!({"http":4,"new_prompt_parts":0}), fx.server.messages_request_count() == 4 && new_prompt_parts == 0, &["PR-20"]);
        last_stage.set("S12");
        n1.release();
        let outcome = match tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await {
            Ok(outcome) => outcome,
            Err(_) => u6_bound_expired(&turn.running, "SOQ S12 finish", "H6_SOQ_FINISH_BOUND", json!({"http_count":fx.server.messages_request_count()})),
        };
        let turn_outcome = u6_turn_end_names(&outcome);
        u6_observe(json!({"stage":"SOQ final outcome","turn":turn_outcome}));
        let structured_value = match &outcome {
            Ok(Ok(TurnOutcome::Completed { structured_output: Some(Ok(value)), .. })) => Some(value),
            _ => None,
        };
        let structured_matches = structured_value.is_some_and(|value| *value == valid_input);
        let structured_digest = structured_value.map(|value| digest(value, "H6_SOQ_O4_OUTCOME"));
        record("O4.OUTCOME", "H6_SOQ_O4_OUTCOME", json!({"result_ok":turn_outcome["result"] == "ok","completed_with_valid_structured_output":structured_matches,"structured_output_sha256":structured_digest}), json!({"result_ok":true,"completed_with_valid_structured_output":true}), turn_outcome["result"] == "ok" && structured_matches, &["PR-14"]);
        let final_count = fx.server.messages_request_count();
        record("O4.COUNT", "H6_SOQ_O4_COUNT", json!(final_count), json!(4), final_count == 4, &["PR-17"]);
        let final_history = fx.session.actor.chat_state_handle.get_conversation().await;
        let final_human = final_history.iter().filter(|item| item.text_content().contains(HUMAN)).count();
        let final_valid_results = final_history.iter().filter(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == VALID_CALL && !result.is_error)).count();
        record("O4.HISTORY", "H6_SOQ_O4_HISTORY", json!({"human":final_human,"non_error_valid_tool_results":final_valid_results}), json!({"human":1,"non_error_valid_tool_results":1}), final_human == 1 && final_valid_results == 1, &["PR-14"]);
        let final_read = tokio::time::timeout(K0_TIMEOUT, u6_persisted_native_read(fx)).await;
        let final_checkpoint = final_read.as_ref().ok().and_then(|read| read.as_ref().ok()).and_then(|read| read.checkpoint.as_ref());
        let n1_records = final_checkpoint.map(|checkpoint| {
            let owned_request = |request: &str| n1_owned_request_names.iter().any(|name| name == request);
            let responses = checkpoint.source.responses.iter().filter(|response| response.occurrence > n_response.occurrence
                || response.provider_response_id.as_ref().and_then(Value::as_str) == Some("msg_soq_n1_valid") || owned_request(&response.request)).count();
            let usage = checkpoint.ledger.usage.iter().filter(|entry| entry.response_occurrence.is_some_and(|occurrence| occurrence > n_response.occurrence) || owned_request(&entry.request)).count();
            let primary = checkpoint.ledger.transitions.iter().filter(|entry| entry.get("primary_resolution").is_some()
                && (entry["occurrence"].as_u64().is_some_and(|occurrence| occurrence > n_response.occurrence) || entry["request"].as_str().is_some_and(owned_request))).count();
            let terminal = checkpoint.ledger.lifecycle.terminal.iter().filter(|(key, marker)| marker.last_accepted_response.is_some_and(|occurrence| occurrence > n_response.occurrence)
                || n1_owned_request_names.iter().any(|request| key.starts_with(&format!("{request}:")))).count();
            json!({"responses":responses,"usage":usage,"primary":primary,"terminal":terminal})
        }).unwrap_or(Value::Null);
        u6_observe(json!({"stage":"SOQ O4.F1 diagnostic","checkpoint_readable":final_checkpoint.is_some(),"n1_records_found":n1_records,"request_keys_observed":n1_owned_request_names.len(),"required":false,"premises":["PR-13"]}));
        last_stage.set("S13");
        let satisfied = n.is_satisfied() && n1.is_satisfied();
        record("O4.SATISFIED", "H6_SOQ_O4_SATISFIED", json!(satisfied), json!(true), satisfied, &["PR-14"]);
        n.assert_satisfied();
        n1.assert_satisfied();
        last_stage.set("S14");
        json!({"route":"S-A","credited":true,"qualification_only":false,"caller":inferred_caller,"caller_reading":"D-2","closure_reading":"D-10",
            "suffix_requests":final_count.saturating_sub(initial_bodies.len()),"http":final_count,"human_occurrences_in_release":human_counts[3],
            "structured_output_retries":corrections.len(),"structured_output_dispatches_observed":structured_dispatches,
            "tools_snapshot_sha256":tools_digest,"structured_output_declaration_sha256":so_digest,
            "body_sha256":bodies_b2.iter().map(|body| digest(body,"H6_SOQ_O4_COUNT")).collect::<Vec<_>>(),
            "human_count_by_body":human_counts,"human_containment_by_body":containment_counts,
            "final_structured_output_matches":structured_matches,"final_structured_output_sha256":structured_digest,"n1_records_found":n1_records})
    }).await;
    match result {
        Ok(value) => value,
        Err(_) => {
            u6_observe(json!({"stage":"SOQ candidate arm ceiling","last_stage":last_stage.get()}));
            panic!("H6_SOQ_CANDIDATE_ARM_60S_CEILING");
        }
    }
}

async fn u6b_consumed_completion_dedupe() -> Value {
    const TASK: &str = "u6b-consumed-completion";
    const MONITOR: &str = "U6b consumed pinned monitor must not leak";
    const COMPLETION: &str = "U6b consumed TaskCompleted sole signal";
    let (fixture, mut turn, stepper, _continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    stepper.pause();
    let fx = &fixture.fx;
    let key = u6b_monitor_ingress(fx, &mut turn.running, TASK, MONITOR).await;
    k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
    fx.loop_tx.send(SessionCommand::InjectNotification {
        prompt_id: "u6b-consumed-completion-notice".into(),
        prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(COMPLETION))],
        priority: NotificationPriority::Later,
        source: NotificationSource::MonitorCompleted { task_id: TASK.into() },
    }).unwrap();
    u6_await_turn_progress(&mut turn.running, "real pending completion notice", "H6_L7_COMPLETION_NOTICE_BOUND",
        "H6_L7_COMPLETION_NOTICE_TURN_ENDED", || async {
            fx.session.actor.state.lock().await.pending_notifications.iter().any(|notice|
                matches!(&notice.source, NotificationSource::MonitorCompleted { task_id } if task_id == TASK))
        }).await;
    assert_eq!(fx.session.actor.consume_deferred_completions().await, vec![TASK.to_owned()],
        "H6_L7_CONSUMED_COMPLETION_IDENTITY");
    assert!(u6_pinned_monitor_keys(fx).await.is_empty(), "H6_L7_CONSUMED_COMPLETION_DISCARDS_PIN");
    assert!(fx.session.actor.state.lock().await.message_delivery.held_input.entries.iter()
        .all(|entry| entry.key.as_u64() != key), "H6_L7_CONSUMED_COMPLETION_PRUNES_LEDGER_REFERENCE");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(history.iter().all(|item| !item.text_content().contains(MONITOR)), "H6_L7_COMPLETION_SOLE_SIGNAL_NO_MONITOR_COPY");
    assert_eq!(history.iter().filter(|item| item.text_content().contains(COMPLETION)).count(),
        1, "H6_L7_COMPLETION_SOLE_SIGNAL_ONCE");
    turn.running.abort();
    let _ = turn.running.await;
    json!({"source":"real InjectNotification producers and consume_deferred_completions boundary",
        "pinned_occurrences":0,"ledger_references":0,"completion_signals":1})
}

async fn u6b_pinned_producer_cap() -> Value {
    let (fixture, mut turn, stepper, _continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    stepper.pause();
    let fx = &fixture.fx;
    let pinned = u6b_monitor_ingress(fx, &mut turn.running, "u6b-pinned-cap", "U6b pinned producer-cap payload").await;
    k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
    for index in 0..51 {
        u6b_monitor_ingress(fx, &mut turn.running, &format!("u6b-unpinned-cap-{index}"),
            &format!("U6b unpinned producer-cap payload {index}")).await;
    }
    let keys = u6_pinned_monitor_keys(fx).await;
    assert_eq!(keys.len(), 1, "H6_L11_PRODUCER_CAP_PRESERVES_PIN");
    assert_eq!(keys[0]["key"].as_u64(), Some(pinned), "H6_L11_PRODUCER_CAP_PRESERVES_OCCURRENCE");
    let buffer = fx.session.actor.tool_context.monitor_event_buffer.as_ref().unwrap();
    let unpinned = buffer.release_view(xai_interjection_core::events::next_occurrence(),
        |event| event.owned_by_session(Some(fx.session.actor.session_info.id.0.as_ref()))).iter().count();
    assert_eq!(unpinned, 50, "H6_L11_PRODUCER_CAP_COUNTS_ONLY_UNPINNED");
    turn.running.abort();
    let _ = turn.running.await;
    json!({"constructed":false,"source":"SessionCommand::InjectNotification -> real push_capped",
        "pinned_occurrences":1,"unpinned_residents":50,"pinned_key":pinned})
}

fn u6b_r7_watch_native_ack<T: Send + 'static>(
    received: tokio::sync::oneshot::Receiver<Result<T, k2_history::NativeUnavailable>>,
    operation: &'static str,
    log: K2lgNLog,
) -> tokio::sync::oneshot::Receiver<Result<T, k2_history::NativeUnavailable>> {
    let started = std::time::Instant::now();
    log.lock().unwrap().push(("R7 native acknowledgement".into(),
        json!({"operation":operation,"phase":"started","elapsed_ms":0})));
    let (sent, returned) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let outcome = received.await;
        let code = match &outcome {
            Ok(Ok(_)) => "accepted",
            Ok(Err(_)) => "refused",
            Err(_) => "channel_closed",
        };
        let unavailable = outcome.as_ref().ok().and_then(|result| result.as_ref().err())
            .map(|reason| serde_json::to_value(reason).unwrap());
        log.lock().unwrap().push(("R7 native acknowledgement".into(),
            json!({"operation":operation,"phase":"finished",
                "elapsed_ms":started.elapsed().as_millis(),"result":code,"unavailable":unavailable})));
        if let Ok(result) = outcome {
            let _ = sent.send(result);
        }
    });
    returned
}

impl Drop for U6Fixture {
    fn drop(&mut self) {
        let requests = self.fx.server.requests().into_iter()
            .filter(|request| request.path == "/v1/messages")
            .filter_map(|request| request.body).collect::<Vec<_>>();
        let acknowledgements = self.fx.log.lock().unwrap().iter()
            .filter(|(kind, _)| kind == "R7 native acknowledgement")
            .map(|(_, value)| value.clone()).collect::<Vec<_>>();
        let _ = U6_ARM_OBSERVATIONS.try_with(|observations| {
            observations.borrow_mut().push(json!({"stage":"R7 fixture drop snapshot",
                "requests":requests,"native_acknowledgements":acknowledgements}));
        });
    }
}

fn u6b_n12_maybe_drop_ack<T>(
    incoming: tokio::sync::oneshot::Receiver<T>,
    switch: &std::sync::atomic::AtomicBool,
    log: &K2lgNLog,
    operation: &'static str,
) -> tokio::sync::oneshot::Receiver<T> {
    if !switch.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return incoming;
    }
    drop(incoming);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    drop(sender);
    log.lock().unwrap().push(("N12_ACK_DROPPED".into(), json!({"operation": operation})));
    receiver
}

async fn u6b_n12_route_qualification() -> Value {
    let (fixture, mut turn, stepper, continuation) = u6b_n12_tool_call_fixture().await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    let prefix = "U6 N12 route held human:";
    let human = format!("{prefix}{}", "h".repeat(256 * 1024 - prefix.len()));
    u6_queue_resident(fx, &human, Vec::new()).await;
    let original = u6_resident_keys(&fx.session.actor);
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N12_ROUTE_LIVE_HOLD");
    let before_http = k2lg_n_bodies(&fx.server).len();
    assert_eq!(before_http, 3, "H6_N12_ROUTE_POST_HTTP_PRECONDITION");
    fixture.persistence_control.n12_drop_next_native_ack.store(true, std::sync::atomic::Ordering::SeqCst);
    continuation.release();
    stepper.resume();
    let ended = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N12_ROUTE_TURN_OUTCOME");
    let names = u6_turn_end_names(&ended);
    let owned_error = ended.as_ref().ok().and_then(|outcome| outcome.as_ref().err());
    let owned_error_code = owned_error.and_then(|error| error.data.as_ref())
        .and_then(|data| data.get("code")).cloned();
    let owned_error_reason = owned_error.and_then(|error| error.data.as_ref())
        .and_then(|data| data.get("message")).cloned();
    let read = tokio::time::timeout(K0_TIMEOUT, fx.session.actor.chat_state_handle.read_native_history())
        .await.expect("H6_N12_ROUTE_NATIVE_READ");
    let reason = read.as_ref().err().copied();
    let sampling = tokio::time::timeout(K0_TIMEOUT, fx.session.actor.chat_state_handle.get_sampling_config())
        .await.expect("H6_N12_ROUTE_CHAT_STATUS");
    let dropped: Vec<Value> = fx.log.lock().unwrap().iter()
        .filter(|(kind, _)| kind == "N12_ACK_DROPPED")
        .map(|(_, value)| value.clone()).collect();
    let after_http = k2lg_n_bodies(&fx.server).len();
    let held_keys = {
        let state = fx.session.actor.state.lock().await;
        state.message_delivery.held_input.entries.iter()
            .filter(|entry| entry.source == super::interjection::HeldSource::HumanInterjection)
            .map(|entry| entry.key.as_u64()).collect::<Vec<_>>()
    };
    let expected_keys = original.iter().map(|entry| entry["key"].as_u64().unwrap()).collect::<Vec<_>>();
    u6_observe(json!({"stage":"N12 W11 route qualification", "before_http_count":before_http,
        "after_http_count":after_http, "dropped_ack_operations":dropped.clone(),
        "native_unavailable":reason, "chat_open":sampling.is_some(), "turn_outcome":names.clone(),
        "held_keys":held_keys.clone(), "original_keys":expected_keys.clone()}));
    assert_eq!(dropped.len(), 1, "H6_N12_ROUTE_CHOSEN_APPEND");
    assert_eq!(reason, Some(k2_history::NativeUnavailable::Indeterminate),
        "H6_N12_ROUTE_INDETERMINATE");
    assert!(sampling.is_some(), "H6_N12_ROUTE_CHAT_OPEN");
    assert!(["error_code", "error_kind", "error_type"].iter()
        .any(|key| names[*key].as_str() == Some("native_history_read_refused")),
        "H6_N12_ROUTE_NAMED_READ_REFUSAL");
    assert!(names["native_unavailable"].as_array().is_some_and(|values|
        values.iter().any(|value| value.as_str() == Some("Indeterminate"))),
        "H6_N12_ROUTE_NAMED_INDETERMINATE");
    assert_eq!(after_http, before_http, "H6_N12_ROUTE_NO_HTTP_AFTER_REFUSAL");
    assert_eq!(held_keys, expected_keys, "H6_N12_ROUTE_CUSTODY_RETAINED");
    continuation.assert_satisfied();
    json!({"kind":"N12_W11_ROUTE_QUALIFICATION", "native_operation":dropped[0]["operation"],
        "native_unavailable":reason, "turn_outcome":names, "post_http_request_count":before_http,
        "owned_error_code":owned_error_code, "owned_error_reason":owned_error_reason,
        "held_key_count":held_keys.len(), "credit_scope":"route qualification only; no unit RED or GREEN credit"})
}

async fn u6b_n12_tool_call_fixture() -> (
    U6Fixture,
    K2lgNTurn,
    U6TurnStepper,
    xai_grok_test_support::InferenceExpectation,
) {
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let stepper = U6TurnStepper::default();
    let mut turn =
        u6_handoff_with_salvage(&mut fixture.fx, None, None, None, Some(stepper.clone())).await;
    stepper.pause();
    let mut continuation = fixture.fx.server.expect_response_blocked(
        "U6 N12 tool-call checkpoint continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2_frames(
            "msg_u6b_n12_checkpoint",
            vec![
                k2_result("srvtoolu_k2_pending", "alpha__lookup"),
                (json!({"type":"tool_use","id":"client_u6b_n12_must_not_run",
                    "name":"alpha__lookup","input":{"value":"N12 refused before execution"}}), Vec::new()),
            ],
            "tool_use",
        )),
    );
    fixture.fx.session.releases[1].add_permits(1);
    u6_step_to_http(&stepper, &mut continuation).await;
    assert!(!turn.running.is_finished(), "H6_N12_ROUTE_CONTINUATION_IN_FLIGHT");
    (fixture, turn, stepper, continuation)
}

async fn u6b_n12_end_fixture() -> (
    U6Fixture,
    K2lgNTurn,
    U6TurnStepper,
    xai_grok_test_support::InferenceExpectation,
    std::rc::Rc<K0IdleObserver>,
) {
    let idle = std::rc::Rc::new(K0IdleObserver::default());
    let mut fixture = u6_fixture_controlled(None, K2lgNOptions::default(), U6PersistenceControl {
        plan_tools: true, idle_observer: Some(idle.clone()), ..Default::default()
    }).await;
    let stepper = U6TurnStepper::default();
    let turn =
        u6_handoff_with_salvage(&mut fixture.fx, None, None, None, Some(stepper.clone())).await;
    stepper.pause();
    let mut continuation = fixture.fx.server.expect_response_blocked(
        "U6 N12 end tool-call checkpoint continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2_frames("msg_u6b_n12_end_checkpoint", vec![
            k2_result("srvtoolu_k2_pending", "alpha__lookup"),
            (json!({"type":"tool_use","id":"client_u6b_n12_end_must_not_run",
                "name":"alpha__lookup","input":{"value":"N12 owned refusal"}}), Vec::new()),
        ], "tool_use")),
    );
    fixture.fx.session.releases[1].add_permits(1);
    u6_step_to_http(&stepper, &mut continuation).await;
    assert!(!turn.running.is_finished(), "H6_N12_END_LIVE_CONTINUATION");
    (fixture, turn, stepper, continuation, idle)
}

fn u6b_collect_host_records(value: &Value, records: &mut Vec<Value>) {
    match value {
        Value::String(text) => {
            if let Ok(record) = serde_json::from_str::<Value>(text)
                && record["kind"] == "held_input_host_record"
            {
                records.push(record);
            }
        }
        Value::Array(values) => {
            for value in values { u6b_collect_host_records(value, records); }
        }
        Value::Object(values) => {
            for value in values.values() { u6b_collect_host_records(value, records); }
        }
        _ => {}
    }
}

async fn u6b_host_records(fx: &K2lgN) -> Vec<Value> {
    let (respond_to, persisted) = tokio::sync::oneshot::channel();
    fx.session.actor.notifications.persistence_tx.send(PersistenceMsg::FlushAndAck { respond_to }).unwrap();
    tokio::time::timeout(K0_TIMEOUT, persisted).await
        .expect("H6_U66_HOST_RECORD_PERSISTENCE_BARRIER").unwrap().unwrap();
    let raw = tokio::fs::read_to_string(fx.session.directory.path().join("updates.jsonl"))
        .await.expect("H6_U66_HOST_RECORD_STREAM");
    let mut records = Vec::new();
    for line in raw.lines() {
        let value: Value = serde_json::from_str(line).expect("H6_U66_HOST_RECORD_JSONL");
        u6b_collect_host_records(&value, &mut records);
    }
    records
}

async fn u6b_u66_next_prompt(
    fx: &K2lgN,
    prompt: &'static str,
    tag: &'static str,
) -> (xai_grok_test_support::InferenceExpectation, K0Abort) {
    let mut next = fx.server.expect_response_blocked(
        "U6.6 next admitted user prompt",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_after_frames("msg_u6b_u66_next")),
    );
    let (respond_to, mut response) = tokio::sync::oneshot::channel();
    let (ready, mut ready_rx) = tokio::sync::oneshot::channel();
    let mut request = super::prompt_queue::QueueInputRequest::from_input_origin(
        vec![acp::ContentBlock::Text(acp::TextContent::new(prompt))],
        "u6b-u66-next-owner".into(), InputOrigin::new(PromptOrigin::User),
        PromptMode::Agent, respond_to,
    );
    request.client_identifier = Some("U6".into());
    request.initial_child_prompt_ready = Some(ready);
    assert!(!fx.session.actor.queue_input(request).await, "H6_U66_NEXT_REAL_ADMISSION");
    let (completion, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
    fx.session.actor.clone().maybe_start_running_task(completion).await;
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut ready_rx => {
                let release = result.expect("H6_U66_NEXT_READINESS");
                release.send(()).expect("H6_U66_NEXT_RELEASE");
            },
            _ = &mut response => panic!("{tag}"),
        }
    }).await.expect("H6_U66_NEXT_READY_BOUND");
    let handle = fx.session.actor.state.lock().await.running_task.as_ref()
        .expect("H6_U66_NEXT_RUNNING_OWNER").handle.clone();
    let stop = K0Abort(handle);
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = next.wait_blocked() => {},
            _ = &mut response => panic!("{tag}"),
        }
    }).await.unwrap_or_else(|_| panic!("{tag}"));
    (next, stop)
}

fn u6b_band_record_counts(record: &Value) -> Value {
    json!([
        record["bands"]["human_interjection"],
        record["bands"]["parent_steer"],
        record["bands"]["monitor_event"],
        record["bands"]["deferred_followup"],
        record["bands"]["stationarity_reminder"],
        record["bands"]["skill_reminder"],
        record["bands"]["plan_activation"],
    ])
}

async fn u6b_n12_turn_end() -> Value {
    use super::interjection::HeldSource;
    const HUMAN: &str = "U6 N12 large held human:";
    const RESIDENT: &str = "U6 N12 resident after custody";
    const PROMPT: &str = "U6 N12 next admitted user prompt";
    const MONITOR: &str = "U6 N12 held monitor";
    const DEFERRED: &str = "U6 N12 held deferred";
    const STATIONARITY: &str = "U6 N12 held stationarity";
    const SKILL: &str = "U6 N12 held skill";
    let (fixture, mut turn, stepper, continuation, idle) = u6b_n12_end_fixture().await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    let human = format!("{HUMAN}{}", "h".repeat(256 * 1024 - HUMAN.len()));
    u6_queue_resident(fx, &human, Vec::new()).await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N12_HUMAN_CUSTODY");
    u6_monitor(fx, "u6b-n12-monitor", MONITOR).await;
    k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
    for (text, source) in [(DEFERRED, HeldSource::DeferredFollowUp),
        (STATIONARITY, HeldSource::StationarityReminder)]
    {
        k2lg_n_selected(fx, fx.session.actor.hold_or_push_user_item(
            ConversationItem::system_reminder(text), source,
        )).await.expect("H6_N12_MACHINE_CUSTODY");
    }
    let (responds_to, mut replied) = tokio::sync::oneshot::channel();
    fx.loop_tx.send(SessionCommand::SessionMode {
        session_mode: acp::SessionModeId::new("plan"), responds_to,
    }).unwrap();
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut replied => result.expect("H6_N12_PLAN_MODE_REPLY"),
            _ = &mut turn.running => panic!("H6_N12_PLAN_TURN_ENDED"),
        }
    }).await.expect("H6_N12_PLAN_COMMAND_BOUND");
    fx.session.actor.apply_skill_update_effects(
        xai_grok_tools::types::skill_discovery_tracker::SkillUpdateEffects {
            system_reminder: Some(SKILL.into()), send_available_commands: false,
            kind: xai_grok_tools::types::skill_discovery_tracker::SkillUpdateKind::Discovery,
        },
    ).await;
    k2lg_n_selected(fx, fx.session.actor.flush_skills_at_native_safe_point()).await;
    let mut keys = u6_mixed_keys(fx).await;
    assert_eq!(keys.len(), 6, "H6_N12_EVERY_PRESENT_BAND_ADMITTED");
    assert!(keys.iter().all(|entry| entry["source"] != "parent_steer"), "H6_N12_S4_EXCLUDED");
    let plan = fx.session.actor.plan_mode.lock().pending_activation_text()
        .expect("H6_N12_HELD_PLAN_REFERENCE").to_owned();
    u6_queue_resident(fx, RESIDENT, Vec::new()).await;
    let residents = u6_resident_keys(&fx.session.actor);
    assert_eq!(residents.len(), 1, "H6_N12_ONE_LATE_RESIDENT");
    let resident_key = residents[0]["key"].as_u64().unwrap();
    keys.push(json!({"key":resident_key,"source":"late_resident"}));
    keys.sort_by_key(|entry| entry["key"].as_u64().unwrap());
    fixture.persistence_control.n12_drop_next_native_ack.store(true, std::sync::atomic::Ordering::SeqCst);
    continuation.release();
    stepper.resume();
    let ended = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N12_OWNED_ERROR_OUTCOME").expect("H6_N12_OWNED_ERROR_JOIN");
    let error = ended.err().expect("H6_N12_OWNED_ERROR_REQUIRED");
    assert_eq!(error.data.as_ref().and_then(|data| data.get("code")),
        Some(&json!("native_history_read_refused")), "H6_N12_QUALIFIED_OWNED_REFUSAL");
    let read = fx.session.actor.chat_state_handle.read_native_history().await;
    assert_eq!(read.as_ref().err(), Some(&k2_history::NativeUnavailable::Indeterminate),
        "H6_N12_CHECKPOINT_INDETERMINATE");
    u6_observe(json!({"stage":"N12 checkpoint before turn-end progress",
        "native_unavailable":"Indeterminate","error_code":"native_history_read_refused",
        "custody_and_resident_keys":keys.clone(),"http_count":fx.server.messages_request_count()}));
    fx.session.actor.state.lock().await.notifications_suppressed = true;
    let (processed, acknowledgement) = tokio::sync::oneshot::channel();
    fx.loop_tx.send(SessionCommand::InjectTurnCompletion {
        prompt_id: KOXD_U6_PROMPT.into(), epoch: turn.epoch,
        result: Box::new(Err(error)), elapsed_ms: Some(0), processed,
    }).unwrap();
    tokio::time::timeout(K0_TIMEOUT, acknowledgement).await
        .expect("H6_N12_COMPLETION_COMMAND_BOUND").unwrap();
    tokio::time::timeout(K0_TIMEOUT, idle.notification.notified()).await
        .expect("H6_N12_SETTLED_OR_REPORTED");
    let records = u6b_host_records(fx).await;
    let retained: Vec<_> = records.iter().filter(|record|
        record["reason"] == "retained_view_unavailable").collect();
    assert_eq!(retained.len(), 1, "H6_N12_SETTLED_OR_REPORTED");
    assert_eq!(u6b_band_record_counts(retained[0]), json!([2, 0, 1, 1, 1, 1, 1]),
        "H6_N12_SETTLED_OR_REPORTED");
    {
        let state = fx.session.actor.state.lock().await;
        assert!(state.running_task.is_none() && state.pending_inputs.is_empty(),
            "H6_N12_SETTLED_OR_REPORTED");
    }
    assert_eq!(fx.server.messages_request_count(), 3, "H6_N12_SETTLED_OR_REPORTED");
    let mut retained_keys = u6_mixed_keys(fx).await;
    retained_keys.sort_by_key(|entry| entry["key"].as_u64().unwrap());
    assert_eq!(retained_keys.iter().map(|entry| &entry["key"]).collect::<Vec<_>>(),
        keys.iter().map(|entry| &entry["key"]).collect::<Vec<_>>(), "H6_N12_SETTLED_OR_REPORTED");
    let (mut next, _next_stop) = u6b_u66_next_prompt(fx, PROMPT, "H6_N12_ORDER").await;
    let read = fx.session.actor.chat_state_handle.read_native_history().await;
    assert_eq!(read.as_ref().err(), Some(&k2_history::NativeUnavailable::Indeterminate),
        "H6_N12_STILL_UNREADABLE_AT_RELEASE");
    let bodies = k2lg_n_bodies(&fx.server);
    let body = bodies.last().expect("H6_N12_NEXT_ACTUAL_REQUEST");
    let text = body["messages"].as_array().unwrap().iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n");
    let prompt_at = text.find(PROMPT).expect("H6_N12_NEXT_PROMPT_PRESENT");
    let mut positions = Vec::new();
    for entry in &keys {
        let needle = match entry["source"].as_str().unwrap() {
            "human_interjection" => HUMAN,
            "monitor_event" => MONITOR,
            "deferred_followup" => DEFERRED,
            "stationarity_reminder" => STATIONARITY,
            "skill_reminder" => SKILL,
            "plan_activation" => plan.as_str(),
            "late_resident" => RESIDENT,
            _ => panic!("H6_N12_UNEXPECTED_BAND"),
        };
        assert_eq!(text.matches(needle).count(), 1, "H6_N12_ORDER");
        let at = text.find(needle).expect("H6_N12_ORDER");
        assert!(at > prompt_at, "H6_N12_ORDER");
        positions.push(at);
    }
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "H6_N12_ORDER");
    {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert!(ledger.entries.is_empty() && ledger.latch.is_none(), "H6_N12_EMPTY");
    }
    assert!(u6_pinned_monitor_keys(fx).await.is_empty(), "H6_N12_EMPTY");
    assert!(fx.session.actor.pending_interjections.is_empty(), "H6_N12_EMPTY");
    next.release();
    tokio::time::timeout(K0_TIMEOUT, next.wait_satisfied()).await.expect("H6_N12_NEXT_SATISFIED");
    next.assert_satisfied();
    continuation.assert_satisfied();
    json!({"kind":"N12_UNAVAILABLE_TURN_END", "constructed":false,
        "native_unavailable":"Indeterminate", "error_code":"native_history_read_refused",
        "retained_band_counts":u6b_band_record_counts(retained[0]),
        "custody_and_resident_keys":keys,"positions_in_next_request":positions,
        "post_drain_idle_observer":true,"next_prompt_http_count":bodies.len(),"ledger_empty":true})
}

async fn u6b_n12_u66_scoped_turn_end() -> Value {
    use super::interjection::HeldSource;
    const HUMAN: &str = "U6 N12 large held human:";
    const RESIDENT: &str = "U6 N12 resident after custody";
    const PROMPT: &str = "U6 N12 next admitted user prompt";
    const MONITOR: &str = "U6 N12 held monitor";
    const DEFERRED: &str = "U6 N12 held deferred";
    const STATIONARITY: &str = "U6 N12 held stationarity";
    let (fixture, mut turn, stepper, continuation, idle) = u6b_n12_end_fixture().await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    let human = format!("{HUMAN}{}", "h".repeat(256 * 1024 - HUMAN.len()));
    u6_queue_resident(fx, &human, Vec::new()).await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N12_HUMAN_CUSTODY");
    u6_monitor(fx, "u6b-n12-monitor", MONITOR).await;
    k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
    for (text, source) in [(DEFERRED, HeldSource::DeferredFollowUp),
        (STATIONARITY, HeldSource::StationarityReminder)]
    {
        k2lg_n_selected(fx, fx.session.actor.hold_or_push_user_item(
            ConversationItem::system_reminder(text), source,
        )).await.expect("H6_N12_MACHINE_CUSTODY");
    }
    let mut keys = u6_mixed_keys(fx).await;
    assert_eq!(keys.len(), 4, "H6_N12_EVERY_PRESENT_BAND_ADMITTED");
    assert!(keys.iter().all(|entry| entry["source"] != "parent_steer"), "H6_N12_S4_EXCLUDED");
    u6_queue_resident(fx, RESIDENT, Vec::new()).await;
    let residents = u6_resident_keys(&fx.session.actor);
    assert_eq!(residents.len(), 1, "H6_N12_ONE_LATE_RESIDENT");
    let resident_key = residents[0]["key"].as_u64().unwrap();
    keys.push(json!({"key":resident_key,"source":"late_resident"}));
    keys.sort_by_key(|entry| entry["key"].as_u64().unwrap());
    fixture.persistence_control.n12_drop_next_native_ack.store(true, std::sync::atomic::Ordering::SeqCst);
    continuation.release();
    stepper.resume();
    let ended = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N12_OWNED_ERROR_OUTCOME").expect("H6_N12_OWNED_ERROR_JOIN");
    let error = ended.err().expect("H6_N12_OWNED_ERROR_REQUIRED");
    assert_eq!(error.data.as_ref().and_then(|data| data.get("code")),
        Some(&json!("native_history_read_refused")), "H6_N12_QUALIFIED_OWNED_REFUSAL");
    let read = fx.session.actor.chat_state_handle.read_native_history().await;
    assert_eq!(read.as_ref().err(), Some(&k2_history::NativeUnavailable::Indeterminate),
        "H6_N12_CHECKPOINT_INDETERMINATE");
    u6_observe(json!({"stage":"N12 checkpoint before turn-end progress",
        "native_unavailable":"Indeterminate","error_code":"native_history_read_refused",
        "custody_and_resident_keys":keys.clone(),"http_count":fx.server.messages_request_count()}));
    fx.session.actor.state.lock().await.notifications_suppressed = true;
    let (processed, acknowledgement) = tokio::sync::oneshot::channel();
    fx.loop_tx.send(SessionCommand::InjectTurnCompletion {
        prompt_id: KOXD_U6_PROMPT.into(), epoch: turn.epoch,
        result: Box::new(Err(error)), elapsed_ms: Some(0), processed,
    }).unwrap();
    tokio::time::timeout(K0_TIMEOUT, acknowledgement).await
        .expect("H6_N12_COMPLETION_COMMAND_BOUND").unwrap();
    tokio::time::timeout(K0_TIMEOUT, idle.notification.notified()).await
        .expect("H6_N12_SETTLED_OR_REPORTED");
    let records = u6b_host_records(fx).await;
    let retained: Vec<_> = records.iter().filter(|record|
        record["reason"] == "retained_view_unavailable").collect();
    assert_eq!(retained.len(), 1, "H6_N12_SETTLED_OR_REPORTED");
    assert_eq!(u6b_band_record_counts(retained[0]), json!([2, 0, 1, 1, 1, 0, 0]),
        "H6_N12_SETTLED_OR_REPORTED");
    {
        let state = fx.session.actor.state.lock().await;
        assert!(state.running_task.is_none() && state.pending_inputs.is_empty(),
            "H6_N12_SETTLED_OR_REPORTED");
    }
    assert_eq!(fx.server.messages_request_count(), 3, "H6_N12_SETTLED_OR_REPORTED");
    let mut retained_keys = u6_mixed_keys(fx).await;
    retained_keys.sort_by_key(|entry| entry["key"].as_u64().unwrap());
    assert_eq!(retained_keys.iter().map(|entry| &entry["key"]).collect::<Vec<_>>(),
        keys.iter().map(|entry| &entry["key"]).collect::<Vec<_>>(), "H6_N12_SETTLED_OR_REPORTED");
    let (mut next, _next_stop) = u6b_u66_next_prompt(fx, PROMPT, "H6_N12_ORDER").await;
    let read = fx.session.actor.chat_state_handle.read_native_history().await;
    assert_eq!(read.as_ref().err(), Some(&k2_history::NativeUnavailable::Indeterminate),
        "H6_N12_STILL_UNREADABLE_AT_RELEASE");
    let bodies = k2lg_n_bodies(&fx.server);
    let body = bodies.last().expect("H6_N12_NEXT_ACTUAL_REQUEST");
    let text = body["messages"].as_array().unwrap().iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n");
    let prompt_at = text.find(PROMPT).expect("H6_N12_NEXT_PROMPT_PRESENT");
    let mut positions = Vec::new();
    for entry in &keys {
        let needle = match entry["source"].as_str().unwrap() {
            "human_interjection" => HUMAN,
            "monitor_event" => MONITOR,
            "deferred_followup" => DEFERRED,
            "stationarity_reminder" => STATIONARITY,
            "late_resident" => RESIDENT,
            _ => panic!("H6_N12_UNEXPECTED_BAND"),
        };
        assert_eq!(text.matches(needle).count(), 1, "H6_N12_ORDER");
        let at = text.find(needle).expect("H6_N12_ORDER");
        assert!(at > prompt_at, "H6_N12_ORDER");
        positions.push(at);
    }
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "H6_N12_ORDER");
    {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert!(ledger.entries.is_empty() && ledger.latch.is_none(), "H6_N12_EMPTY");
    }
    assert!(u6_pinned_monitor_keys(fx).await.is_empty(), "H6_N12_EMPTY");
    assert!(fx.session.actor.pending_interjections.is_empty(), "H6_N12_EMPTY");
    next.release();
    tokio::time::timeout(K0_TIMEOUT, next.wait_satisfied()).await.expect("H6_N12_NEXT_SATISFIED");
    next.assert_satisfied();
    continuation.assert_satisfied();
    json!({"kind":"N12_U66_SCOPED_UNAVAILABLE_TURN_END", "constructed":false,
        "native_unavailable":"Indeterminate", "error_code":"native_history_read_refused",
        "retained_band_counts":u6b_band_record_counts(retained[0]),
        "custody_and_resident_keys":keys,"positions_in_next_request":positions,
        "post_drain_idle_observer":true,"next_prompt_http_count":bodies.len(),"ledger_empty":true})
}

#[derive(Clone, Copy)]
enum U6bCloseCause {
    ChatClose,
    ActorDrop,
    HardTeardown,
}

async fn u6b_n12c(cause: U6bCloseCause) -> Value {
    use super::interjection::HeldSource;
    use xai_message_delivery_core::TerminalCause;
    const FIRST: &str = "U6 N12c first held human";
    const SECOND: &str = "U6 N12c second held human";
    const RESIDENT: &str = "U6 N12c later resident";
    let (fixture, mut turn, stepper, continuation, idle) = u6b_n12_end_fixture().await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    let actor = &fx.session.actor;
    u6_queue_resident(fx, FIRST, vec![u6_image(1)]).await;
    let first_key = u6_resident_keys(actor)[0]["key"].clone();
    u6_queue_resident(fx, SECOND, Vec::new()).await;
    let second_key = u6_resident_keys(actor)[1]["key"].clone();
    assert!(!k2lg_n_selected(fx, actor.drain_interjections_at_safe_point()).await,
        "H6_N12C_HUMAN_CUSTODY");
    u6_monitor(fx, "u6b-n12c-monitor", "U6 N12c held monitor").await;
    k2lg_n_selected(fx, actor.inject_pending_monitor_events()).await;
    for (text, source) in [("U6 N12c deferred", HeldSource::DeferredFollowUp),
        ("U6 N12c stationarity", HeldSource::StationarityReminder)]
    {
        k2lg_n_selected(fx, actor.hold_or_push_user_item(
            ConversationItem::system_reminder(text), source,
        )).await.expect("H6_N12C_MACHINE_CUSTODY");
    }
    u6_queue_resident(fx, RESIDENT, Vec::new()).await;
    let resident_key = u6_resident_keys(actor)[0]["key"].clone();
    fixture.persistence_control.n12_drop_next_native_ack.store(true, std::sync::atomic::Ordering::SeqCst);
    continuation.release();
    stepper.resume();
    let error = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N12C_OWNED_ERROR_OUTCOME").expect("H6_N12C_OWNED_ERROR_JOIN")
        .err().expect("H6_N12C_OWNED_ERROR_REQUIRED");
    assert_eq!(error.data.as_ref().and_then(|data| data.get("code")),
        Some(&json!("native_history_read_refused")), "H6_N12C_QUALIFIED_REFUSAL");
    actor.state.lock().await.notifications_suppressed = true;
    let (processed, acknowledgement) = tokio::sync::oneshot::channel();
    fx.loop_tx.send(SessionCommand::InjectTurnCompletion {
        prompt_id: KOXD_U6_PROMPT.into(), epoch: turn.epoch,
        result: Box::new(Err(error)), elapsed_ms: Some(0), processed,
    }).unwrap();
    tokio::time::timeout(K0_TIMEOUT, acknowledgement).await
        .expect("H6_N12C_COMPLETION_COMMAND_BOUND").unwrap();
    tokio::time::timeout(K0_TIMEOUT, idle.notification.notified()).await
        .expect("H6_N12C_RETAINED_END_BOUND");
    assert_eq!(u6_mixed_keys(fx).await.len(), 6, "H6_N12C_RETAINED_BEFORE_CLOSE");
    assert_eq!(fx.server.messages_request_count(), 3, "H6_N12C_NO_RESTART");
    match cause {
        U6bCloseCause::ChatClose => {
            fx.chat_token.cancel();
            assert!(tokio::time::timeout(K0_TIMEOUT, actor.chat_state_handle.get_sampling_config())
                .await.expect("H6_N12C_CHAT_CLOSE_BOUND").is_none(), "H6_N12C_CHAT_CLOSED");
            assert_eq!(actor.flush_stranded_interjections().await, 0, "H6_N12C_NO_FALLBACK");
        }
        U6bCloseCause::ActorDrop => actor.settle_all_parent_messages(TerminalCause::ActorDrop).await,
        U6bCloseCause::HardTeardown => actor.settle_all_parent_messages(TerminalCause::HardTeardown).await,
    }
    let records = u6b_host_records(fx).await;
    let closed: Vec<_> = records.iter().filter(|record|
        record["reason"] == "settled_at_chat_close").collect();
    assert_eq!(closed.len(), 1, "H6_N12C_RECORD");
    assert_eq!(u6b_band_record_counts(closed[0]), json!([3, 0, 1, 1, 1, 0, 0]),
        "H6_N12C_RECORD");
    assert_eq!(closed[0]["held_human"], json!([
        {"key": first_key, "text": FIRST, "attachment_count": 1},
        {"key": second_key, "text": SECOND, "attachment_count": 0},
        {"key": resident_key, "text": RESIDENT, "attachment_count": 0},
    ]), "H6_N12C_HUMAN_TEXT_IN_RECORD");
    assert!(!fixture.persistence_control.user_chunks.lock().unwrap().iter().any(|chunk|
        chunk.to_string().contains("U6 N12c")), "H6_N12C_NO_USER_CHUNK");
    {
        let state = actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert!(ledger.entries.is_empty() && ledger.latch.is_none(), "H6_N12C_EMPTY");
        assert!(ledger.parent_stamps.is_empty() && ledger.pin_owner.is_none(), "H6_N12C_EMPTY");
    }
    assert!(u6_pinned_monitor_keys(fx).await.is_empty(), "H6_N12C_EMPTY");
    assert!(actor.pending_interjections.is_empty(), "H6_N12C_EMPTY");
    continuation.assert_satisfied();
    json!({"kind": "N12C_LOGICAL_CLOSE", "constructed": false,
        "cause": match cause { U6bCloseCause::ChatClose => "ChatClose",
            U6bCloseCause::ActorDrop => "ActorDrop", U6bCloseCause::HardTeardown => "HardTeardown" },
        "band_counts": u6b_band_record_counts(closed[0]), "saved_human_count": 3,
        "saved_attachment_counts": [1, 0, 0], "no_user_chunk": true, "ledger_empty": true})
}

async fn u6b_n14_u66_scoped() -> Value {
    use super::interjection::HeldSource;
    const CUSTODY: &str = "U6 N14 held custody";
    const RESIDENT: &str = "U6 N14 later resident";
    const PROMPT: &str = "U6 N14 next admitted prompt";
    const MONITOR: &str = "U6 N14 held monitor";
    const DEFERRED: &str = "U6 N14 held deferred";
    const STATIONARITY: &str = "U6 N14 held stationarity";
    let idle = std::rc::Rc::new(K0IdleObserver::default());
    let mut fixture = u6_fixture_controlled(None, K2lgNOptions::default(), U6PersistenceControl {
        idle_observer: Some(idle.clone()), ..Default::default()
    }).await;
    let stepper = U6TurnStepper::default();
    let mut turn = u6_handoff_with_salvage(
        &mut fixture.fx, None, None, None, Some(stepper.clone()),
    ).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    u6_queue_resident(fx, CUSTODY, Vec::new()).await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N14_HUMAN_CUSTODY");
    u6_monitor(fx, "u6b-n14-monitor", MONITOR).await;
    k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
    for (text, source) in [(DEFERRED, HeldSource::DeferredFollowUp),
        (STATIONARITY, HeldSource::StationarityReminder)]
    {
        k2lg_n_selected(fx, fx.session.actor.hold_or_push_user_item(
            ConversationItem::system_reminder(text), source,
        )).await.expect("H6_N14_MACHINE_CUSTODY");
    }
    let mut keys = u6_mixed_keys(fx).await;
    stepper.pause();
    let mut continuation = fx.server.expect_response_blocked(
        "U6 N14 readable closure",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ), k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    u6_step_to_http(&stepper, &mut continuation).await;
    u6_queue_resident(fx, RESIDENT, vec![u6_image(64)]).await;
    let resident_key = u6_resident_keys(&fx.session.actor)[0]["key"].clone();
    keys.push(json!({"key": resident_key, "source": "late_resident"}));
    keys.sort_by_key(|entry| entry["key"].as_u64().unwrap());
    let barriers = fixture.persistence_control.barriers.load(std::sync::atomic::Ordering::SeqCst);
    continuation.release();
    u6_u63_step_to_preparation(&fixture, &stepper, barriers).await;
    turn.running.abort();
    let ended = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N14_PREPARATION_ABORT_BOUND");
    assert!(matches!(ended, Err(error) if error.is_cancelled()), "H6_N14_REAL_PREPARATION_ABORT");
    let read = tokio::time::timeout(K0_TIMEOUT, fx.session.actor.chat_state_handle.read_native_history())
        .await.expect("H6_N14_READABLE_VIEW_BOUND");
    assert!(read.is_ok_and(|read| read.checkpoint.is_some()), "H6_N14_READABLE_VIEW");
    fx.session.actor.state.lock().await.notifications_suppressed = true;
    let (processed, acknowledgement) = tokio::sync::oneshot::channel();
    fx.loop_tx.send(SessionCommand::InjectTurnCompletion {
        prompt_id: KOXD_U6_PROMPT.into(), epoch: turn.epoch,
        result: Box::new(Err(acp::Error::internal_error().data("U6 N14 constructed owned error"))),
        elapsed_ms: Some(0), processed,
    }).unwrap();
    tokio::time::timeout(K0_TIMEOUT, acknowledgement).await
        .expect("H6_N14_COMPLETION_COMMAND_BOUND").unwrap();
    tokio::time::timeout(K0_TIMEOUT, idle.notification.notified()).await
        .expect("H6_N14_NO_RESTART");
    {
        let state = fx.session.actor.state.lock().await;
        assert!(state.running_task.is_none() && state.pending_inputs.is_empty(), "H6_N14_NO_RESTART");
    }
    assert_eq!(fx.server.messages_request_count(), 3, "H6_N14_NO_RESTART");
    let mut retained = u6_mixed_keys(fx).await;
    retained.sort_by_key(|entry| entry["key"].as_u64().unwrap());
    assert_eq!(retained.iter().map(|entry| &entry["key"]).collect::<Vec<_>>(),
        keys.iter().map(|entry| &entry["key"]).collect::<Vec<_>>(), "H6_N14_NO_RESTART");
    assert!(!fixture.persistence_control.user_chunks.lock().unwrap().iter().any(|chunk|
        chunk.to_string().contains("U6 N14")), "H6_N14_NO_RESTART");
    let (mut next, _next_stop) = u6b_u66_next_prompt(fx, PROMPT, "H6_N14_ORDER").await;
    let bodies = k2lg_n_bodies(&fx.server);
    let body = bodies.last().expect("H6_N14_NEXT_ACTUAL_REQUEST");
    let text = body["messages"].as_array().unwrap().iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n");
    let prompt_at = text.find(PROMPT).expect("H6_N14_ORDER");
    let mut positions = Vec::new();
    for entry in &keys {
        let needle = match entry["source"].as_str().unwrap() {
            "human_interjection" => CUSTODY,
            "monitor_event" => MONITOR,
            "deferred_followup" => DEFERRED,
            "stationarity_reminder" => STATIONARITY,
            "late_resident" => RESIDENT,
            _ => panic!("H6_N14_UNEXPECTED_BAND"),
        };
        assert_eq!(text.matches(needle).count(), 1, "H6_N14_ORDER");
        let at = text.find(needle).expect("H6_N14_ORDER");
        assert!(at > prompt_at, "H6_N14_ORDER");
        positions.push(at);
    }
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "H6_N14_ORDER");
    {
        let state = fx.session.actor.state.lock().await;
        assert!(state.message_delivery.held_input.entries.is_empty()
            && state.message_delivery.held_input.latch.is_none(), "H6_N14_EMPTY");
    }
    assert!(u6_pinned_monitor_keys(fx).await.is_empty(), "H6_N14_EMPTY");
    assert!(fx.session.actor.pending_interjections.is_empty(), "H6_N14_EMPTY");
    next.release();
    tokio::time::timeout(K0_TIMEOUT, next.wait_satisfied()).await.expect("H6_N14_NEXT_SATISFIED");
    next.assert_satisfied();
    continuation.assert_satisfied();
    json!({"kind": "N14_U66_READABLE_ERROR_END_RETAIN", "constructed": true,
        "construction": "owned error completion injected after a real preparation abort and committed readable closure",
        "native_view_readable": true, "custody_and_resident_keys": keys,
        "positions_in_next_request": positions, "no_restart": true, "ledger_empty": true,
        "skill_and_plan_legs": "owed at U6.7 under R13-4"})
}

#[derive(Clone, Copy)]
enum U6bTerminalAwait {
    Preparation,
    PersistenceBarrier,
}

async fn u6b_l13b(site: U6bTerminalAwait, cause: xai_message_delivery_core::TerminalCause) -> Value {
    use xai_grok_tools::implementations::grok_build::monitor::types::MonitorEventNotification;
    const CUSTODY: &str = "U6 L13b held human custody";
    const RESIDENT: &str = "U6 L13b guarded later resident";
    const A_MONITOR: &str = "U6 L13b owner A monitor";
    const B_MONITOR: &str = "U6 L13b owner B monitor";
    const LEGACY: &str = "U6 L13b ownerless legacy monitor";
    let (fixture, mut turn, stepper, continuation) = u6_u63_prepare_fixture(Some(CUSTODY)).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    let extra_actor = fx.session.actor.clone();
    let buffer = extra_actor.tool_context.monitor_event_buffer.as_ref().unwrap().clone();
    let mut b = K0Shell::new(&fx.server, false).await;
    let sampling = b.tasks.pop().expect("H6_L13B_B_SAMPLER_TASK");
    sampling.abort();
    let stopped = tokio::time::timeout(K0_TIMEOUT, sampling).await
        .expect("H6_L13B_B_SAMPLER_STOP_BOUND");
    assert!(stopped.is_err_and(|error| error.is_cancelled()), "H6_L13B_B_SAMPLER_STOPPED");
    Arc::get_mut(&mut b.actor).expect("H6_L13B_B_SETUP_EXCLUSIVE")
        .tool_context.monitor_event_buffer = Some(buffer.clone());
    assert_ne!(extra_actor.session_id_string(), b.actor.session_id_string(), "H6_L13B_DISTINCT_OWNERS");
    u6_monitor(fx, "u6b-l13b-A", A_MONITOR).await;
    b.buffer("u6b-l13b-B", B_MONITOR);
    buffer.push_capped(MonitorEventNotification {
        task_id: "u6b-l13b-legacy".into(),
        event_text: format!("<monitor-event description=\"legacy\" task_id=\"u6b-l13b-legacy\">\n{LEGACY}\n</monitor-event>"),
        owner_session_id: None,
    }, 128);
    k2lg_n_selected(fx, extra_actor.inject_pending_monitor_events()).await;
    let owner = extra_actor.state.lock().await.message_delivery.held_input.pin_owner
        .expect("H6_L13B_REAL_PIN_OWNER");
    assert_eq!(buffer.release_view(owner, |_| false).iter().count(), 2,
        "H6_L13B_REAL_OWNER_AND_LEGACY_PINS");
    u6_queue_resident(fx, RESIDENT, Vec::new()).await;
    let barriers = fixture.persistence_control.barriers.load(std::sync::atomic::Ordering::SeqCst);
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_L13B_CLOSED_CHECKPOINT").await;
    u6_u63_step_to_preparation(&fixture, &stepper, barriers).await;
    if matches!(site, U6bTerminalAwait::PersistenceBarrier) {
        fixture.persistence_control.hold_barriers.store(true, std::sync::atomic::Ordering::SeqCst);
        u6_step_to_persistence_barrier(&stepper, &fixture.persistence_control).await;
    }
    assert!(!turn.running.is_finished(), "H6_L13B_SUSPENDED_REAL_RELEASE");
    let before_http = fx.server.messages_request_count();
    let strong_count = Arc::strong_count(&extra_actor);
    assert!(strong_count > 1, "H6_L13B_EXTRA_ARC_BEFORE_TERMINAL");
    tokio::time::timeout(K0_TIMEOUT, extra_actor.settle_all_parent_messages(cause)).await
        .expect("H6_L13B_LOGICAL_SETTLEMENT_BOUND");
    {
        let state = extra_actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert!(ledger.terminal && ledger.entries.is_empty() && ledger.latch.is_none(),
            "H6_L13B_LOGICAL_SETTLEMENT_BEFORE_DROP");
        assert!(ledger.pin_owner.is_none() && ledger.parent_stamps.is_empty(),
            "H6_L13B_NO_PINS_BEFORE_FINAL_ARC_DROP");
    }
    assert_eq!(buffer.release_view(owner, |_| false).iter().count(), 0,
        "H6_L13B_NO_PINS_BEFORE_FINAL_ARC_DROP");
    assert!(Arc::strong_count(&extra_actor) > 1, "H6_L13B_EXTRA_ARC_STILL_ALIVE");
    let before_b = tokio::time::timeout(K0_TIMEOUT, b.actor.chat_state_handle.get_conversation())
        .await.expect("H6_L13B_B_BEFORE_READ_BOUND").len();
    b.actor.inject_pending_monitor_events().await;
    let b_before = tokio::time::timeout(K0_TIMEOUT, b.actor.chat_state_handle.get_conversation())
        .await.expect("H6_L13B_B_AFTER_READ_BOUND");
    assert!(b_before.len() > before_b, "H6_L13B_REAL_B_DRAIN");
    let b_text = b_before.iter().map(ConversationItem::text_content).collect::<Vec<_>>().join("\n");
    assert_eq!(b_text.matches(B_MONITOR).count(), 1, "H6_L13B_B_OWN_EVENT_ONCE");
    assert_eq!(b_text.matches(LEGACY).count(), 1, "H6_L13B_B_LEGACY_ONCE");
    assert!(!b_text.contains(A_MONITOR), "H6_L13B_NO_FOREIGN_TAKE");
    let b_digest = native_content_digest(&serde_json::to_vec(&b_before).unwrap());
    fixture.persistence_control.hold_barriers.store(false, std::sync::atomic::Ordering::SeqCst);
    while !fixture.persistence_control.held_barriers.lock().unwrap().is_empty() {
        u6_forward_held_barrier(&fixture);
    }
    fx.server.enqueue_response("/v1/messages", k2_script(k2lg_n_after_frames("msg_u6b_l13b_forbidden")));
    stepper.resume();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_L13B_RESUMED_OUTCOME_BOUND").expect("H6_L13B_RESUMED_TURN_JOIN");
    assert_eq!(fx.server.messages_request_count(), before_http, "H6_L13B_NO_TERMINAL_RELEASE");
    assert_eq!(u6_user_persists(&fixture.persistence_control, CUSTODY), 0,
        "H6_L13B_NO_TERMINAL_RELEASE");
    assert_eq!(u6_user_persists(&fixture.persistence_control, RESIDENT), 0,
        "H6_L13B_NO_TERMINAL_RELEASE");
    let a_after = extra_actor.chat_state_handle.get_conversation().await;
    assert!(!a_after.iter().any(|item| [CUSTODY, RESIDENT, A_MONITOR, LEGACY].iter()
        .any(|text| item.text_content().contains(text))), "H6_L13B_NO_TERMINAL_RELEASE");
    assert_eq!(buffer.release_view(owner, |_| false).iter().count(), 0,
        "H6_L13B_NO_TERMINAL_RELEASE");
    let b_after = b.actor.chat_state_handle.get_conversation().await;
    assert_eq!(native_content_digest(&serde_json::to_vec(&b_after).unwrap()), b_digest,
        "H6_L13B_NO_TERMINAL_RELEASE");
    {
        let state = extra_actor.state.lock().await;
        assert!(state.message_delivery.held_input.terminal
            && state.message_delivery.held_input.entries.is_empty()
            && state.message_delivery.held_input.latch.is_none(), "H6_L13B_NO_TERMINAL_RELEASE");
    }
    assert!(Arc::strong_count(&extra_actor) > 1, "H6_L13B_EXTRA_ARC_AT_FINAL_ASSERTION");
    continuation.assert_satisfied();
    json!({"kind": "L13B_LOGICAL_TERMINAL", "constructed": false,
        "phase": match site { U6bTerminalAwait::Preparation => "preparation",
            U6bTerminalAwait::PersistenceBarrier => "persistence-barrier" },
        "cause": format!("{cause:?}"), "extra_arc_alive": true,
        "shared_real_actor_buffer": true, "b_sampler_drainer_stopped_in_setup": true,
        "b_sampling_not_claimed": true, "pins_released_before_last_arc": true,
        "b_drained_before_a_resumed": true, "b_own_and_legacy_once": true,
        "resumed_release_had_no_effect": true, "resumed_turn_error": outcome.is_err(),
        "http_before": before_http, "http_after": fx.server.messages_request_count(),
        "native_handoff_observation_credited": false})
}

async fn u6b_u66_progress<T>(
    running: &mut U6Running,
    progress: impl std::future::Future<Output = T>,
    tag: &'static str,
) -> T {
    tokio::select! {
        biased;
        value = tokio::time::timeout(K0_TIMEOUT, progress) => value.expect(tag),
        outcome = running => {
            let kind = match outcome {
                Ok(Ok(_)) => "Completed",
                Ok(Err(error)) => {
                    let code = error.data.as_ref().and_then(|data| data.get("code"))
                        .and_then(Value::as_str).filter(|code| *code == "native_history_read_refused");
                    let indeterminate = error.data.as_ref().is_some_and(|data| data.to_string().contains("Indeterminate"));
                    u6_observe(json!({"stage":"U66 owned turn error", "tag":tag,
                        "native_error_code":code,
                        "native_unavailable_reason":indeterminate.then_some("Indeterminate")}));
                    "TurnError"
                }
                Err(error) if error.is_cancelled() => "JoinCancelled",
                Err(_) => "JoinError",
            };
            u6_observe(json!({"stage":"U66 owned wait", "tag":tag, "turn_outcome_kind":kind}));
            panic!("{tag}");
        }
    }
}

struct U6bIdAdmission {
    admission: k0_coordinator::ActiveMessageAdmission,
    receipt: Option<crate::agent::subagent::PromptTurnReceipt>,
}

async fn u6b_admit_steer_id(
    fx: &K2lgN,
    running: &mut U6Running,
    id: &str,
    text: &str,
    tag: &'static str,
) -> U6bIdAdmission {
    let (receipt_sink, mut receipt_rx) = tokio::sync::mpsc::channel(1);
    let (respond_to, response_rx) = tokio::sync::oneshot::channel();
    let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = &fx.session.actor;
    assert!(actor.state.lock().await.running_task.is_some(), "H6_U66_STEER_RUNNING_OWNER");
    u6b_u66_progress(running, k2lg_n_selected(fx, actor.admit_parent_agent_message_for_test(
        k0_task::ActiveAgentMessage {
            message_id: id.into(),
            sender_session_id: "u6b-test-parent".into(),
            text: Arc::from(text),
        },
        k0_task::ActiveAgentMessageSource::Agent,
        k0_task::ActiveAgentMessageOperation::Steer,
        receipt_sink,
        respond_to,
        completion_tx,
    )), tag).await;
    let admission = u6b_u66_progress(running, response_rx, tag).await.expect(tag);
    let receipt = if admission == k0_coordinator::ActiveMessageAdmission::Admitted {
        Some(u6b_u66_progress(running, receipt_rx.recv(), tag).await.expect(tag))
    } else {
        None
    };
    U6bIdAdmission { admission, receipt }
}

async fn u6b_n6_long_id_steer() -> Value {
    const STEER: &str = "U6 N6 long id Steer";
    const HUMAN: &str = "U6 N6 later human";
    let (fixture, mut turn, stepper, continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    let id = "s".repeat(127);
    let admitted = u6b_admit_steer_id(fx, &mut turn.running, &id, STEER,
        "H6_N6_REAL_ADMISSION").await;
    assert_eq!(admitted.admission, k0_coordinator::ActiveMessageAdmission::Admitted,
        "H6_N6_REAL_ADMISSION");
    let mut receipt = admitted.receipt.expect("H6_N6_REAL_ADMISSION");
    assert!(matches!(receipt.result.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)), "H6_N6_NO_UNSUBMITTED_SETTLEMENT");
    let stamp = fx.session.actor.state.lock().await.message_delivery.held_input
        .parent_key(&id).map(|key| key.as_u64());
    u6_queue_resident(fx, HUMAN, Vec::new()).await;
    let human_key = u6_resident_keys(&fx.session.actor)[0]["key"].as_u64()
        .expect("H6_N6_HUMAN_ARRIVAL_KEY");
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N6_PENDING_NATIVE_HOLD");
    fx.server.enqueue_response("/v1/messages", k2_script(k2lg_n_after_frames("msg_u6b_n6_after")));
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_N6_CLOSED_CHECKPOINT").await;
    stepper.resume();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N6_FIRST_RELEASE_PROGRESS").expect("H6_N6_FIRST_RELEASE_JOIN");
    assert!(outcome.is_ok(), "H6_N6_FIRST_RELEASE_PROGRESS");
    let bodies = k2lg_n_bodies(&fx.server);
    let body = bodies.last().expect("H6_N6_ACTUAL_REQUEST");
    let text = body["messages"].as_array().unwrap().iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n");
    let steer_at = text.find(STEER).expect("H6_N6_LONG_ID_STEER_ORDER");
    let human_at = text.find(HUMAN).expect("H6_N6_LONG_ID_STEER_ORDER");
    assert_eq!(text.matches(STEER).count(), 1, "H6_N6_LONG_ID_STEER_ORDER");
    assert_eq!(text.matches(HUMAN).count(), 1, "H6_N6_LONG_ID_STEER_ORDER");
    assert!(steer_at < human_at, "H6_N6_LONG_ID_STEER_ORDER");
    assert!(stamp.is_some_and(|key| key < human_key), "H6_N6_STAMPED_AT_ADMISSION");
    let state = fx.session.actor.state.lock().await;
    let ledger = &state.message_delivery.held_input;
    assert!(ledger.parent_stamps.len() <= fx.session.actor.live_parent_steer_slots(&state),
        "H6_N6_STAMPS_BOUNDED_BY_LIVE_IDENTITIES");
    assert!(ledger.entries.is_empty() && ledger.latch.is_none(), "H6_N6_EMPTY_AFTER_RELEASE");
    drop(state);
    continuation.assert_satisfied();
    json!({"kind":"N6_LONG_ID_STEER", "constructed":false, "id_bytes":id.len(),
        "stamp_at_admission":stamp, "human_arrival_key":human_key,
        "steer_position":steer_at, "human_position":human_at,
        "actual_request_count":bodies.len(), "once_only":true})
}

async fn u6b_n11_steer_receipt() -> Value {
    use super::interjection::{HeldSource, HoldRefusalReason};
    let (fixture, mut turn, _stepper, _continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    let mut receipts = Vec::new();
    for index in 0..32 {
        let id = format!("u6b-steer-{index}");
        let admitted = u6b_admit_steer_id(fx, &mut turn.running, &id,
            "U6 N11 held Steer", "H6_N11_STEER_ADMISSION_BOUND").await;
        assert_eq!(admitted.admission, k0_coordinator::ActiveMessageAdmission::Admitted,
            "H6_N11_STEER_ADMISSION");
        receipts.push(admitted.receipt.expect("H6_N11_STEER_ADMISSION"));
    }
    let overflow = u6b_admit_steer_id(fx, &mut turn.running, "u6b-steer-overflow",
        "U6 N11 refused Steer", "H6_N11_STEER_RECEIPT_BOUND").await;
    u6_observe(json!({"stage":"N11 full-slot host outcome", "admission":format!("{:?}", overflow.admission)}));
    assert_ne!(overflow.admission, k0_coordinator::ActiveMessageAdmission::Rejected,
        "H6_N11_STEER_RECEIPT");
    assert!(overflow.receipt.is_none(), "H6_N11_STEER_RECEIPT");
    {
        let state = fx.session.actor.state.lock().await;
        assert_eq!(fx.session.actor.live_parent_steer_slots(&state), 32,
            "H6_N11_STEER_RECEIPT");
        let refusal = state.message_delivery.held_input.refusal.as_ref()
            .expect("H6_N11_STEER_REFUSAL_RECORD");
        assert_eq!(refusal.source, HeldSource::ParentSteer, "H6_N11_STEER_REFUSAL_RECORD");
        assert_eq!(refusal.reason, HoldRefusalReason::SteerSlotsFull,
            "H6_N11_STEER_REFUSAL_RECORD");
        assert_eq!(refusal.observed_len, 32, "H6_N11_STEER_REFUSAL_RECORD");
        u6_observe(json!({"stage":"N11 named refusal", "reason":"steer_slots_full",
            "observed_slots":refusal.observed_len, "key":refusal.key}));
    }
    let duplicate = u6b_admit_steer_id(fx, &mut turn.running, "u6b-steer-0",
        "U6 N11 duplicate Steer", "H6_N11_DUPLICATE_BOUND").await;
    assert_eq!(duplicate.admission, k0_coordinator::ActiveMessageAdmission::Rejected,
        "H6_N11_OTHER_REJECTION_PRESERVED");
    assert!(receipts.iter_mut().all(|receipt| matches!(receipt.result.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty))), "H6_N11_NO_UNSUBMITTED_SETTLEMENT");
    json!({"kind":"N11_STEER_RECEIPT", "constructed":false, "admitted":receipts.len(),
        "overflow_admission":format!("{:?}", overflow.admission), "refusal_reason":"steer_slots_full",
        "duplicate_kept_rejected":duplicate.admission == k0_coordinator::ActiveMessageAdmission::Rejected,
        "max_slots":32})
}

async fn u6b_n5_start_turn(
    fx: &mut K2lgN,
    user: &'static str,
    stepper: U6TurnStepper,
) -> K2lgNTurn {
    U6_ARM_STEPPERS.with(|steppers| steppers.borrow_mut().push(stepper.clone()));
    fx.session.actor.chat_state_handle.begin_turn_capture();
    tokio::time::timeout(K0_TIMEOUT, fx.session.actor.chat_state_handle.get_conversation())
        .await.expect("H6_N5_START_INBOX_FENCE");
    let turn_start_ops = fx.log.lock().unwrap().len();
    tokio::time::timeout(K0_TIMEOUT, fx.session.actor.chat_state_handle
        .push_user_message_and_ack(ConversationItem::user(user)))
        .await.expect("H6_N5_START_USER_ACK");
    let submits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let record = submits.clone();
    let actor = fx.session.actor.clone();
    let domain = fx.session.domain(&fx.server);
    let observations = U6_ARM_OBSERVATIONS.with(Clone::clone);
    let guard = super::turn_task::TurnActiveGuard::activate(Some(&actor.session_turn_active));
    let running = tokio::task::spawn_local(async move {
        let _guard = guard;
        let mut prepare = move |request: &mut ConversationRequest,
            binding: &xai_chat_state::RequestTransportBinding| {
            assert!(record.borrow().len() < 16, "H6_N5_REQUEST_BUDGET");
            record.borrow_mut().push(json!({
                "items":serde_json::to_value(&request.items).unwrap(),
                "tools":serde_json::to_value(&request.tools).unwrap(),
                "model":request.model, "bound_model":binding.config.model,
                "body_digest":request.encoded.as_ref().map(|body| native_content_digest(body.as_bytes())),
            }));
            Ok(())
        };
        let mut salvage = super::length_salvage::LengthSalvage::new(None);
        let mut sampling = Default::default();
        let future = super::switch_projection::HISTORY_S3_TEST_DOMAIN.scope(
            domain,
            super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
                k2_history::NativeHistoryRequirement::Selected,
                actor.process_conversation_turn_for_native_history_test(
                    KOXD_U6_PROMPT, None, None, None,
                    &mut salvage, &mut sampling, &mut prepare,
                ),
            ),
        );
        U6_ARM_OBSERVATIONS.scope(observations, U6SteppedTurn {
            future: Box::pin(future), stepper: Some(stepper),
        }).await
    });
    let (epoch, identity) = u6_install_running(fx, running.abort_handle()).await;
    K2lgNTurn { running, epoch, identity, submits, turn_start_ops }
}

async fn u6b_n5_native_handoff(fx: &mut K2lgN, stepper: U6TurnStepper) -> K2lgNTurn {
    let before = fx.server.messages_request_count();
    let permit = tokio::time::timeout(K0_TIMEOUT,
        fx.session.releases[1].clone().acquire_many_owned(16))
        .await.expect("H6_N5_CLIENT_GATE_BOUND").expect("H6_N5_CLIENT_GATE_OPEN");
    permit.forget();
    fx.server.enqueue_response("/v1/messages", k2_script(k2_u5_frames()));
    let mut turn = u6b_n5_start_turn(fx, "U6 N5 later native turn", stepper).await;
    let captured = u6b_u66_progress(&mut turn.running, fx.session.source_acks.recv(),
        "H6_N5_NATIVE_CAPTURE_BOUND").await;
    assert_eq!(captured, Some(k2_history::NativeResponseStatus::Accepted), "H6_N5_NATIVE_CAPTURE");
    let mut entered = u6_client_dispatch(
        &mut fx.session.entered, &mut turn.running, json!("Accepted"),
        "N5 real native handoff", "H6_N5_NATIVE_DISPATCH_BOUND", "H6_N5_NATIVE_DISPATCH_ENDED",
    ).await;
    entered.sort();
    assert_eq!(entered, vec!["alpha__lookup", "zeta__fetch"], "H6_N5_REAL_CLIENT_EXECUTION");
    let conversation = &fx.session.actor.chat_state_handle;
    u6_await_turn_progress(
        &mut turn.running, "N5 first actual client result", "H6_N5_FIRST_ACTUAL_RESULT",
        "H6_N5_FIRST_ACTUAL_RESULT_ENDED", || async move {
            conversation.get_conversation().await.iter().any(|item| {
                matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")
            })
        },
    ).await;
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_N5_HANDOFF_RELOAD").await;
    assert!(k2lg_n_live(&checkpoint), "H6_N5_REAL_LIVE_PENDING");
    assert_eq!(fx.server.messages_request_count(), before + 1, "H6_N5_ZERO_EARLY_CONTINUATION");
    u6_observe(json!({"stage":"N5 native handoff", "before_requests":before,
        "after_requests":fx.server.messages_request_count()}));
    turn
}

async fn u6b_n5_33rd_steer() -> Value {
    const EARLY: &str = "U6 N5 prepared early human";
    const LATE: &str = "U6 N5 later unprepared human";
    const STEER: &str = "U6 N5 33rd Steer";
    let idle = std::rc::Rc::new(K0IdleObserver::default());
    let mut fixture = u6_fixture_controlled(None, K2lgNOptions::default(), U6PersistenceControl {
        idle_observer: Some(idle.clone()), ..Default::default()
    }).await;
    k2_seed(&mut fixture.fx.session, &fixture.fx.server).await;
    let mut first = fixture.fx.server.expect_response_blocked(
        "N5 earlier ordinary turn", xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ), k2_script(k2_frames("msg_u6b_n5_prior_first", vec![
            (json!({"type":"text","text":"N5 earlier first sample"}), Vec::new()),
        ], "end_turn")),
    );
    let prior_stepper = U6TurnStepper::default();
    let mut prior = u6b_n5_start_turn(&mut fixture.fx, "U6 N5 earlier ordinary prompt", prior_stepper).await;
    let _prior_stop = K0Abort(prior.running.abort_handle());
    u6b_u66_progress(&mut prior.running, first.wait_blocked(), "H6_N5_PRIOR_FIRST_HTTP").await;
    let mut receipts = Vec::new();
    let mut prior_texts = Vec::new();
    for index in 0..32 {
        let id = format!("u6b-n5-prior-{index}");
        let text = format!("U6 N5 prior Steer {index:02}");
        let admitted = u6b_admit_steer_id(&fixture.fx, &mut prior.running, &id, &text,
            "H6_N5_PRIOR_ADMISSION_BOUND").await;
        assert_eq!(admitted.admission, k0_coordinator::ActiveMessageAdmission::Admitted,
            "H6_N5_PRIOR_REAL_ADMISSION");
        receipts.push(admitted.receipt.expect("H6_N5_PRIOR_REAL_ADMISSION"));
        prior_texts.push(text);
    }
    let mut second = fixture.fx.server.expect_response_blocked(
        "N5 actual prior Steer submission", xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ), k2_script(k2_frames("msg_u6b_n5_prior_complete", vec![
            (json!({"type":"text","text":"N5 prior turn complete"}), Vec::new()),
        ], "end_turn")),
    );
    first.release();
    u6b_u66_progress(&mut prior.running, second.wait_blocked(), "H6_N5_PRIOR_STEERS_SUBMITTED").await;
    let bodies = k2lg_n_bodies(&fixture.fx.server);
    let prior_body = bodies.last().expect("H6_N5_PRIOR_ACTUAL_REQUEST");
    let text = prior_body["messages"].as_array().unwrap().iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n");
    assert!(prior_texts.iter().all(|needle| text.matches(needle.as_str()).count() == 1),
        "H6_N5_PRIOR_32_SUBMITTED_ONCE");
    second.release();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut prior.running).await
        .expect("H6_N5_PRIOR_TURN_BOUND").expect("H6_N5_PRIOR_TURN_JOIN")
        .expect("H6_N5_PRIOR_TURN_SUCCESS");
    {
        let state = fixture.fx.session.actor.state.lock().await;
        assert_eq!(fixture.fx.session.actor.live_parent_steer_slots(&state), 32,
            "H6_N5_PRIOR_DELIVERED_LIVE_SLOTS");
    }
    let structured_output = match outcome {
        TurnOutcome::Completed { stop: CompletedStop::EndTurn, structured_output, .. } => structured_output,
        _ => panic!("H6_N5_PRIOR_EXPECTED_END_TURN"),
    };
    let total_tokens = tokio::time::timeout(K0_TIMEOUT,
        fixture.fx.session.actor.chat_state_handle.get_total_tokens()).await
        .expect("H6_N5_PRIOR_TOKEN_READ_BOUND");
    let prompt_outcome = crate::session::commands::PromptTurnOk {
        stop_reason: acp::StopReason::EndTurn,
        total_tokens,
        turn_snapshot: None,
        completion_kind: crate::session::commands::PromptCompletionKind::Completed,
        structured_output,
        usage: None,
        tool_overrides: None,
    };
    fixture.fx.session.actor.state.lock().await.notifications_suppressed = true;
    let (processed, completed) = tokio::sync::oneshot::channel();
    fixture.fx.loop_tx.send(SessionCommand::InjectTurnCompletion {
        prompt_id: KOXD_U6_PROMPT.into(), epoch: prior.epoch,
        result: Box::new(Ok(prompt_outcome)), elapsed_ms: Some(0), processed,
    }).unwrap();
    tokio::time::timeout(K0_TIMEOUT, completed).await.expect("H6_N5_PRIOR_COMPLETION_BOUND").unwrap();
    tokio::time::timeout(K0_TIMEOUT, idle.notification.notified()).await.expect("H6_N5_PRIOR_POST_DRAIN_IDLE");
    let settled_count = receipts.len();
    for receipt in receipts {
        let delivered = tokio::time::timeout(K0_TIMEOUT, receipt.result).await
            .expect("H6_N5_PRIOR_RECEIPT_BOUND").expect("H6_N5_PRIOR_RECEIPT_OPEN");
        assert!(delivered.is_ok(), "H6_N5_PRIOR_SETTLED_DELIVERED");
    }
    let (previous_stamps, previous_live_slots) = {
        let state = fixture.fx.session.actor.state.lock().await;
        assert!(state.running_task.is_none() && state.pending_inputs.is_empty(), "H6_N5_PRIOR_IDLE_NO_RESTART");
        assert_eq!(fixture.fx.session.actor.live_parent_steer_slots(&state), 0,
            "H6_N5_PRIOR_IDENTITIES_SETTLED");
        (state.message_delivery.held_input.parent_stamps.len(),
            fixture.fx.session.actor.live_parent_steer_slots(&state))
    };
    u6_observe(json!({"stage":"R19 real Completion pruning", "constructed":false,
        "stamp_count":previous_stamps, "live_parent_slots":previous_live_slots}));
    let mut prior_acks = 0;
    while let Ok(status) = fixture.fx.session.source_acks.try_recv() {
        assert_eq!(status, k2_history::NativeResponseStatus::Accepted, "H6_N5_PRIOR_NATIVE_ACK");
        prior_acks += 1;
    }
    first.assert_satisfied();
    second.assert_satisfied();
    let history_before = tokio::time::timeout(K0_TIMEOUT,
        fixture.fx.session.actor.chat_state_handle.get_conversation()).await.expect("H6_N5_PRIOR_HISTORY_BOUND");
    let native_stepper = U6TurnStepper::default();
    let mut turn = u6b_n5_native_handoff(&mut fixture.fx, native_stepper.clone()).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    let history_after = u6b_u66_progress(&mut turn.running,
        fx.session.actor.chat_state_handle.get_conversation(), "H6_N5_HISTORY_PRESERVED_BOUND").await;
    assert!(history_after.len() >= history_before.len(), "H6_N5_PRIOR_HISTORY_PRESERVED");
    assert_eq!(native_content_digest(&serde_json::to_vec(&history_before).unwrap()),
        native_content_digest(&serde_json::to_vec(&history_after[..history_before.len()]).unwrap()),
        "H6_N5_PRIOR_HISTORY_PRESERVED");
    native_stepper.pause();
    let mut continuation = fx.server.expect_response_blocked(
        "N5 matching native closure", xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ), k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    u6b_u66_progress(&mut turn.running, u6_step_to_http(&native_stepper, &mut continuation),
        "H6_N5_NATIVE_CONTINUATION_BOUND").await;
    u6_queue_resident(fx, EARLY, Vec::new()).await;
    let barriers = fixture.persistence_control.barriers.load(std::sync::atomic::Ordering::SeqCst);
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_N5_NATIVE_CLOSED_CHECKPOINT").await;
    u6b_u66_progress(&mut turn.running, u6_u63_step_to_preparation(&fixture, &native_stepper, barriers),
        "H6_N5_REAL_PREPARATION_BOUND").await;
    u6_queue_resident(fx, LATE, Vec::new()).await;
    let admitted = u6b_admit_steer_id(fx, &mut turn.running, "u6b-n5-33rd", STEER,
        "H6_N5_LATE_STEER_ADMISSION_BOUND").await;
    assert_eq!(admitted.admission, k0_coordinator::ActiveMessageAdmission::Admitted,
        "H6_N5_LATE_STEER_ADMITTED");
    let mut receipt = admitted.receipt.expect("H6_N5_LATE_STEER_ADMITTED");
    let late_stamp = fx.session.actor.state.lock().await.message_delivery.held_input
        .parent_key("u6b-n5-33rd").map(|key| key.as_u64());
    u6_observe(json!({"stage":"N5 33rd admission", "earlier_settled":settled_count,
        "previous_stamps":previous_stamps, "late_stamp":late_stamp,
        "prior_requests":bodies.len(), "prior_native_acks":prior_acks}));
    let mut first_prefix = Vec::new();
    for _ in 0..256 {
        assert!(u6b_u66_progress(&mut turn.running, native_stepper.step(),
            "H6_N5_FIRST_PREFIX_PROGRESS").await, "H6_N5_FIRST_PREFIX_PROGRESS");
        first_prefix = u6b_u66_progress(&mut turn.running,
            fx.session.actor.chat_state_handle.get_conversation(), "H6_N5_FIRST_PREFIX_READ").await;
        if first_prefix.iter().any(|item| item.text_content().contains(EARLY)) { break; }
    }
    let first_labels: Vec<_> = first_prefix.iter().flat_map(|item| [EARLY, LATE, STEER]
        .into_iter().filter(move |label| item.text_content().contains(label))).collect();
    assert_eq!(first_labels, vec![EARLY], "H6_N5_FIRST_PREFIX_CUTOFF");
    assert!(!fixture.persistence_control.hold_barriers.load(std::sync::atomic::Ordering::SeqCst),
        "H6_N5_PERSISTENCE_FENCE_NOT_HELD");
    u6b_u66_progress(&mut turn.running, u6b_host_records(fx),
        "H6_N5_FIRST_PREFIX_PERSISTENCE_FENCE").await;
    let unsubmitted_persists = u6_user_persists(&fixture.persistence_control, STEER);
    u6_observe(json!({"stage":"N5 unsubmitted Steer", "first_prefix":first_labels,
        "parent_user_chunks":unsubmitted_persists}));
    assert_eq!(unsubmitted_persists, 0, "H6_N5_NO_UNSUBMITTED_PARENT_PERSIST");
    assert!(matches!(receipt.result.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)), "H6_N5_NO_UNSUBMITTED_SETTLEMENT");
    fx.server.enqueue_response("/v1/messages", k2_script(k2lg_n_after_frames("msg_u6b_n5_after")));
    native_stepper.resume();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N5_FINAL_RELEASE_BOUND").expect("H6_N5_FINAL_RELEASE_JOIN");
    assert!(outcome.is_ok(), "H6_N5_FINAL_RELEASE_PROGRESS");
    let all_bodies = k2lg_n_bodies(&fx.server);
    let body = all_bodies.last().expect("H6_N5_FINAL_ACTUAL_REQUEST");
    let final_text = body["messages"].as_array().unwrap().iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n");
    let positions: Vec<_> = [EARLY, LATE, STEER].iter().map(|needle| {
        assert_eq!(final_text.matches(*needle).count(), 1, "H6_N5_GLOBAL_STEER_ORDER");
        final_text.find(*needle).expect("H6_N5_GLOBAL_STEER_ORDER")
    }).collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "H6_N5_GLOBAL_STEER_ORDER");
    assert!(late_stamp.is_some(), "H6_N5_STAMPED_AT_ADMISSION");
    assert!(previous_stamps <= previous_live_slots, "H6_R19_COMPLETION_STAMP_PRUNE");
    let state = fx.session.actor.state.lock().await;
    assert!(state.message_delivery.held_input.parent_stamps.len()
        <= fx.session.actor.live_parent_steer_slots(&state), "H6_N5_STAMPS_BOUNDED_BY_LIVE_IDENTITIES");
    drop(state);
    continuation.assert_satisfied();
    json!({"kind":"N5_33RD_STEER", "constructed":false, "prior_settled":settled_count,
        "previous_stamp_count":previous_stamps, "stamp_at_33rd_admission":late_stamp,
        "first_prefix":first_labels, "final_positions":positions,
        "unsubmitted_parent_persists":unsubmitted_persists,
        "history_prefix_preserved":native_content_digest(&serde_json::to_vec(&history_before).unwrap())
            == native_content_digest(&serde_json::to_vec(&history_after[..history_before.len()]).unwrap()),
        "actual_request_count":all_bodies.len()})
}

async fn u6b_r19_constructed_orphan_release() -> Value {
    use super::interjection::{HeldBody, HeldEntry, HeldSource};
    const ID: &str = "u6-r19-never-admitted-parent";
    let (fixture, mut turn, _stepper, continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_R19_ORPHAN_CLOSED_CHECKPOINT").await;
    let history_before = u6b_u66_progress(&mut turn.running,
        fx.session.actor.chat_state_handle.get_conversation(), "H6_R19_ORPHAN_HISTORY_BOUND").await;
    let key = xai_interjection_core::events::next_occurrence();
    let parent_entries_before = {
        let mut state = fx.session.actor.state.lock().await;
        state.message_delivery.held_input.entries.push_back(HeldEntry {
            key, source: HeldSource::ParentSteer, body: HeldBody::Parent(ID.to_owned()),
            meta: json!({"constructed":true}), preparing: None,
        });
        state.message_delivery.held_input.entries.iter()
            .filter(|entry| matches!(&entry.body, HeldBody::Parent(_))).count()
    };
    let observation_start = U6_ARM_OBSERVATIONS.with(|rows| rows.borrow().len());
    let delivered = u6b_u66_progress(&mut turn.running,
        k2lg_n_selected(fx, fx.session.actor.drain_admitted_messages_at_safe_point()),
        "H6_R19_ORPHAN_RELEASE_BOUND").await;
    let committed = U6_ARM_OBSERVATIONS.with(|rows| rows.borrow().iter().skip(observation_start)
        .any(|row| row["stage"] == "merged release outcome" && row["release_path"] == "submitted"));
    let (entry_remains, parent_entries_after) = {
        let state = fx.session.actor.state.lock().await;
        (state.message_delivery.held_input.entries.iter().any(|entry| entry.key == key),
            state.message_delivery.held_input.entries.iter()
                .filter(|entry| matches!(&entry.body, HeldBody::Parent(_))).count())
    };
    u6_observe(json!({"stage":"R19 constructed orphan release", "constructed":true,
        "release_committed":committed, "parent_entries_before":parent_entries_before,
        "parent_entries_after":parent_entries_after, "entry_remains":entry_remains,
        "delivered":delivered}));
    assert!(committed, "H6_R19_ORPHAN_COMMITTED");
    assert!(!entry_remains, "H6_R19_ORPHAN_EXPIRED");
    assert!(!delivered, "H6_R19_ORPHAN_NO_RELEASE");
    let history_after = u6b_u66_progress(&mut turn.running,
        fx.session.actor.chat_state_handle.get_conversation(), "H6_R19_ORPHAN_HISTORY_BOUND").await;
    assert_eq!(native_content_digest(&serde_json::to_vec(&history_before).unwrap()),
        native_content_digest(&serde_json::to_vec(&history_after).unwrap()), "H6_R19_ORPHAN_NO_RELEASE");
    continuation.assert_satisfied();
    json!({"constructed":true, "committed":committed, "expired":!entry_remains,
        "nothing_released":!delivered})
}

async fn u6b_u66_constructed_missing_stamp() -> Value {
    const ID: &str = "u6-u66-constructed-missing-stamp";
    const STEER: &str = "U6 U66 unstamped Steer";
    let (fixture, mut turn, _stepper, continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    let admitted = u6b_admit_steer_id(fx, &mut turn.running, ID, STEER,
        "H6_U66_MISSING_STAMP_ADMISSION").await;
    assert_eq!(admitted.admission, k0_coordinator::ActiveMessageAdmission::Admitted,
        "H6_U66_MISSING_STAMP_ADMISSION");
    let mut receipt = admitted.receipt.expect("H6_U66_MISSING_STAMP_ADMISSION");
    {
        let mut state = fx.session.actor.state.lock().await;
        assert!(state.message_delivery.held_input.parent_key(ID).is_some(),
            "H6_U66_MISSING_STAMP_REAL_STAMP");
        state.message_delivery.held_input.parent_stamps.retain(|(id, _)| id != ID);
    }
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_U66_MISSING_STAMP_CLOSED_CHECKPOINT").await;
    let history_before = u6b_u66_progress(&mut turn.running,
        fx.session.actor.chat_state_handle.get_conversation(),
        "H6_U66_MISSING_STAMP_HISTORY_BOUND").await;
    let delivered = u6b_u66_progress(&mut turn.running,
        k2lg_n_selected(fx, fx.session.actor.drain_admitted_messages_at_safe_point()),
        "H6_U66_MISSING_STAMP_RELEASE_BOUND").await;
    let records = u6b_u66_progress(&mut turn.running, async {
        let (respond_to, persisted) = tokio::sync::oneshot::channel();
        fx.session.actor.notifications.persistence_tx.send(PersistenceMsg::FlushAndAck { respond_to }).unwrap();
        tokio::time::timeout(K0_TIMEOUT, persisted).await
            .expect("H6_U66_HOST_RECORD_PERSISTENCE_BARRIER").unwrap().unwrap();
        let raw = match tokio::fs::read_to_string(fx.session.directory.path().join("updates.jsonl")).await {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => panic!("H6_U66_HOST_RECORD_STREAM: {error}"),
        };
        let mut records = Vec::new();
        for line in raw.lines() {
            let value: Value = serde_json::from_str(line).expect("H6_U66_HOST_RECORD_JSONL");
            u6b_collect_host_records(&value, &mut records);
        }
        records
    }, "H6_U66_MISSING_STAMP_RECORD_BOUND").await;
    let missing: Vec<_> = records.iter().filter(|record|
        record["reason"] == "missing_parent_stamp").collect();
    assert_eq!(missing.len(), 1, "H6_U66_MISSING_STAMP_RECORDED");
    assert_eq!(missing[0]["missing_parent_stamps"], 1,
        "H6_U66_MISSING_STAMP_RECORDED");
    assert!(!delivered, "H6_U66_MISSING_STAMP_WITHHELD");
    assert!(matches!(receipt.result.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)),
        "H6_U66_MISSING_STAMP_UNSETTLED");
    {
        let state = fx.session.actor.state.lock().await;
        assert_eq!(fx.session.actor.live_parent_steer_slots(&state), 1,
            "H6_U66_MISSING_STAMP_LIVE_SLOT");
        assert!(state.message_delivery.held_input.parent_key(ID).is_none(),
            "H6_U66_MISSING_STAMP_NO_FRESH_KEY");
        assert!(state.message_delivery.held_input.retaining(),
            "H6_U66_MISSING_STAMP_RETAINED");
        assert_eq!(state.message_delivery.held_input.parent_persisted, 0,
            "H6_U66_MISSING_STAMP_NOT_PERSISTED");
    }
    let history_after = u6b_u66_progress(&mut turn.running,
        fx.session.actor.chat_state_handle.get_conversation(),
        "H6_U66_MISSING_STAMP_HISTORY_BOUND").await;
    assert_eq!(native_content_digest(&serde_json::to_vec(&history_after).unwrap()),
        native_content_digest(&serde_json::to_vec(&history_before).unwrap()),
        "H6_U66_MISSING_STAMP_NOT_PERSISTED");
    json!({"kind":"U66_MISSING_PARENT_STAMP", "constructed":true,
        "construction":"remove one admitted Steer's stamp while retaining its live slot",
        "withheld":!delivered, "recorded":missing.len(), "fresh_key_drawn":false,
        "persisted":false, "settled":false})
}

async fn u6b_n9_projecting_straddle() -> Value {
    use super::interjection::{HeldBindingMeta, HeldCommitError, HeldParentSelection};
    const SA: &str = "U6 N9 earlier Projecting Steer";
    const EARLY: &str = "U6 N9 early prepared human";
    const LATE: &str = "U6 N9 later unprepared human";
    const SB: &str = "U6 N9 later Pending Steer";
    let (fixture, mut turn, stepper, continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    let sa = u6b_admit_steer_id(fx, &mut turn.running, "u6b-n9-Sa", SA,
        "H6_N9_SA_ADMISSION_BOUND").await;
    assert_eq!(sa.admission, k0_coordinator::ActiveMessageAdmission::Admitted, "H6_N9_SA_ADMITTED");
    let _sa_receipt = sa.receipt.expect("H6_N9_SA_ADMITTED");
    {
        let actor = &fx.session.actor;
        let mut state = actor.state.lock().await;
        let task = state.running_task.as_ref().expect("H6_N9_RUNNING_BINDING");
        let binding = xai_message_delivery_core::TurnBinding::new(task.prompt_id.clone(), task.epoch);
        let meta = HeldBindingMeta { prompt_id: task.prompt_id.clone(), epoch: format!("{:?}", task.epoch) };
        let result = actor.commit_held_parent_batch(&mut state, &binding, &meta,
            HeldParentSelection::All, |parents, _| {
                assert_eq!(parents.len(), 1, "H6_N9_REAL_PROJECTING_MEMBERSHIP");
                Err(HeldCommitError::ChatStateUnavailable)
            });
        assert!(matches!(result, Err(HeldCommitError::ChatStateUnavailable)), "H6_N9_CONSTRUCTED_FAILURE");
        assert!(actor.held_parent_views(&state, &binding, &meta).is_empty(), "H6_N9_PROJECTING_NOT_PENDING");
        assert_eq!(actor.live_parent_steer_slots(&state), 1, "H6_N9_PROJECTING_STILL_LIVE");
        let view_rows = |views: Vec<super::interjection::HeldParentView>| {
            views.into_iter().map(|parent| {
                (parent.key, parent.message_id, parent.text, parent.payload)
            }).collect::<Vec<_>>()
        };
        let pending_before = view_rows(actor.held_parent_views(&state, &binding, &meta));
        let live_before = actor.live_parent_steer_slots(&state);
        let projecting_before = view_rows(actor.held_projecting_parent_views(&mut state, &binding, &meta));
        assert_eq!(view_rows(actor.held_parent_views(&state, &binding, &meta)), pending_before,
            "H6_N9_PROJECTING_READ_PRESERVES_PENDING");
        assert_eq!(view_rows(actor.held_projecting_parent_views(&mut state, &binding, &meta)), projecting_before,
            "H6_N9_PROJECTING_READ_PRESERVES_PROJECTING");
        assert_eq!(actor.live_parent_steer_slots(&state), live_before,
            "H6_N9_PROJECTING_READ_PRESERVES_LIVE_SLOTS");
    }
    u6_queue_resident(fx, EARLY, Vec::new()).await;
    let barriers = fixture.persistence_control.barriers.load(std::sync::atomic::Ordering::SeqCst);
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_N9_CLOSED_CHECKPOINT").await;
    u6b_u66_progress(&mut turn.running, u6_u63_step_to_preparation(&fixture, &stepper, barriers),
        "H6_N9_REAL_PREPARATION_BOUND").await;
    u6_queue_resident(fx, LATE, Vec::new()).await;
    let sb = u6b_admit_steer_id(fx, &mut turn.running, "u6b-n9-Sb", SB,
        "H6_N9_SB_ADMISSION_BOUND").await;
    assert_eq!(sb.admission, k0_coordinator::ActiveMessageAdmission::Admitted, "H6_N9_SB_ADMITTED");
    let _sb_receipt = sb.receipt.expect("H6_N9_SB_ADMITTED");
    let commits_before = U6_ARM_OBSERVATIONS.with(|rows| rows.borrow().iter()
        .filter(|row| row["stage"] == "merged release outcome").count());
    let mut reached = false;
    for _ in 0..256 {
        assert!(u6b_u66_progress(&mut turn.running, stepper.step(), "H6_N9_FIRST_COMMIT_BOUND").await,
            "H6_N9_FIRST_COMMIT_BOUND");
        reached = U6_ARM_OBSERVATIONS.with(|rows| rows.borrow().iter()
            .filter(|row| row["stage"] == "merged release outcome").count() > commits_before);
        if reached { break; }
    }
    assert!(reached, "H6_N9_FIRST_COMMIT_BOUND");
    let first = u6b_u66_progress(&mut turn.running, fx.session.actor.chat_state_handle.get_conversation(),
        "H6_N9_FIRST_PREFIX_READ_BOUND").await;
    let first_labels: Vec<_> = first.iter().flat_map(|item| [SA, EARLY, LATE, SB]
        .into_iter().filter(move |label| item.text_content().contains(label))).collect();
    u6_observe(json!({"stage":"N9 first release prefix", "constructed":true, "labels":first_labels}));
    assert!(first_labels.is_empty(), "H6_N9_PENDING_PROJECTING_STRADDLE");
    fx.server.enqueue_response("/v1/messages", k2_script(k2lg_n_after_frames("msg_u6b_n9_after")));
    stepper.resume();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N9_FINAL_RELEASE_BOUND").expect("H6_N9_FINAL_RELEASE_JOIN");
    assert!(outcome.is_ok(), "H6_N9_FINAL_RELEASE_PROGRESS");
    let bodies = k2lg_n_bodies(&fx.server);
    let body = bodies.last().expect("H6_N9_FINAL_ACTUAL_REQUEST");
    let text = body["messages"].as_array().unwrap().iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n");
    let positions: Vec<_> = [SA, EARLY, LATE, SB].iter().map(|needle| {
        assert_eq!(text.matches(*needle).count(), 1, "H6_N9_GLOBAL_ORDER");
        text.find(*needle).expect("H6_N9_GLOBAL_ORDER")
    }).collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "H6_N9_GLOBAL_ORDER");
    continuation.assert_satisfied();
    json!({"kind":"N9_PROJECTING_STRADDLE", "constructed":true,
        "construction":"real admitted Sa moved to Projecting by a deliberately rejected owned commit",
        "first_prefix":first_labels, "final_positions":positions,
        "actual_request_count":bodies.len()})
}

async fn u6b_l9_cancel_rebuild() -> Value {
    const CUSTODY: &str = "U6 L9 custody across real rebuild";
    const MONITOR: &str = "U6 L9 monitor across real rebuild";
    const NEXT: &str = "U6 L9 next admitted user turn";
    let (fixture, mut turn, _stepper, mut continuation) = u6_u63_prepare_fixture(Some(CUSTODY)).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    let definition = fx.session.actor.agent.borrow().definition().clone();
    let old_bridge = fx.session.actor.agent.borrow().tool_bridge().clone();
    u6_monitor(fx, "u6b-l9-monitor", MONITOR).await;
    k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
    let before = u6_mixed_keys(fx).await;
    assert!(before.iter().any(|entry| entry["source"] == "monitor_event"), "H6_L9_MONITOR_HELD");
    assert!(before.iter().any(|entry| entry["source"] == "human_interjection")
        && before.iter().any(|entry| entry["source"] == "monitor_event"), "H6_L9_REAL_CUSTODY");
    let old_epoch = turn.epoch;
    let requests_before_cancel = fx.server.messages_request_count();
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_L9_CLOSED_CHECKPOINT_BEFORE_CANCEL").await;
    fx.loop_tx.send(SessionCommand::Cancel(crate::session::CancelOptions {
        user_initiated: true,
        trigger: Some(crate::session::CancelTrigger::CtrlC),
        ..Default::default()
    })).unwrap();
    let joined = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_L9_IMMEDIATE_CANCEL");
    assert!(joined.is_err_and(|error| error.is_cancelled()), "H6_L9_IMMEDIATE_CANCEL");
    tokio::time::timeout(K0_TIMEOUT, async {
        while fx.session.actor.state.lock().await.running_task.is_some() {
            tokio::task::yield_now().await;
        }
    }).await.expect("H6_L9_OLD_OWNER_SETTLED");
    assert_eq!(u6_mixed_keys(fx).await, before, "H6_L9_CANCEL_RETAINS_CUSTODY");
    assert_eq!(fx.server.messages_request_count(), requests_before_cancel, "H6_L9_NO_SELF_START");
    tokio::time::timeout(K0_TIMEOUT, continuation.wait_satisfied()).await
        .expect("H6_L9_OLD_REQUEST_SATISFIED");
    continuation.assert_satisfied();
    let (responds_to, rebuilt) = tokio::sync::oneshot::channel();
    fx.loop_tx.send(SessionCommand::RebuildAgentForDefinition { definition, responds_to }).unwrap();
    tokio::time::timeout(K0_TIMEOUT, rebuilt).await
        .expect("H6_L9_REBUILD_BOUND").expect("H6_L9_REBUILD_REPLY").expect("H6_L9_REAL_REBUILD");
    assert!(!Arc::ptr_eq(&old_bridge, fx.session.actor.agent.borrow().tool_bridge()),
        "H6_L9_AGENT_ACTUALLY_REBUILT");
    assert_eq!(u6_mixed_keys(fx).await, before, "H6_L9_REBUILD_RETAINS_IDENTITIES");
    assert!(fx.session.actor.state.lock().await.running_task.is_none(), "H6_L9_NO_SELF_START");
    assert_eq!(fx.server.messages_request_count(), requests_before_cancel, "H6_L9_NO_SELF_START");
    let (mut next, next_stop) = u6b_u66_next_prompt(fx, NEXT, "H6_L9_NEXT_PROMPT_BOUND").await;
    let new_epoch = fx.session.actor.state.lock().await.running_task.as_ref()
        .expect("H6_L9_NEW_ADMITTED_OWNER").epoch;
    assert_ne!(new_epoch, old_epoch, "H6_L9_NEW_ADMITTED_OWNER");
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), requests_before_cancel + 1, "H6_L9_RELEASE_ONCE");
    let text = bodies.last().expect("H6_L9_ACTUAL_NEXT_REQUEST")["messages"].as_array().unwrap().iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n");
    assert_eq!(text.matches(CUSTODY).count(), 1, "H6_L9_REBUILT_AGENT_RELEASE");
    assert_eq!(text.matches(MONITOR).count(), 1, "H6_L9_REBUILT_AGENT_RELEASE");
    assert_eq!(text.matches(NEXT).count(), 1, "H6_L9_REBUILT_AGENT_RELEASE");
    assert!(u6_mixed_keys(fx).await.is_empty(), "H6_L9_RELEASE_EMPTIES_CUSTODY");
    next.release();
    tokio::time::timeout(K0_TIMEOUT, next.wait_satisfied()).await
        .expect("H6_L9_NEXT_REQUEST_SATISFIED");
    next.assert_satisfied();
    tokio::time::timeout(K0_TIMEOUT, async {
        while !next_stop.0.is_finished() {
            tokio::task::yield_now().await;
        }
    }).await.expect("H6_L9_NEXT_TURN_FINISHED");
    assert_eq!(fx.server.messages_request_count(), requests_before_cancel + 1, "H6_L9_RELEASE_ONCE");
    assert!(u6_mixed_keys(fx).await.is_empty(), "H6_L9_RELEASE_EMPTIES_CUSTODY");
    json!({"kind":"L9_CANCEL_REBUILD_NEXT_ADMISSION", "constructed":false,
        "cancel":"SessionCommand::Cancel CtrlC", "rebuild":"SessionCommand::RebuildAgentForDefinition",
        "actual_agent_bridge_replaced":true, "custody_identities_preserved":before,
        "requests_before_cancel":requests_before_cancel, "requests_after_next_turn":fx.server.messages_request_count(),
        "released_once_on_rebuilt_agent":true, "self_start":false,
        "next_task_finished":next_stop.0.is_finished(),
        "completion_observation":"production AbortHandle; run-loop finalization not claimed"})
}

#[derive(Clone, Copy)]
enum U6bL13Operation {
    Release,
    LogicalEnd,
    KillBackground,
}

async fn u6b_l13_shared_leaders(operation: U6bL13Operation) -> Value {
    use xai_grok_tools::implementations::grok_build::monitor::types::MonitorEventNotification;
    const HUMAN_A: &str = "U6 L13 leader A held human";
    const MONITOR_A: &str = "U6 L13 leader A owned monitor";
    const MONITOR_B: &str = "U6 L13 leader B owned monitor";
    const LEGACY_B: &str = "U6 L13 legacy event pinned by B";
    const NATIVE_B: &str = "U6 L13 real parent to shell child B";
    let (fixture, mut turn, stepper, continuation) = u6_u63_prepare_fixture(Some(HUMAN_A)).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &fixture.fx;
    let extra_actor = fx.session.actor.clone();
    let buffer = extra_actor.tool_context.monitor_event_buffer.as_ref().unwrap().clone();
    u6b_u66_progress(&mut turn.running, u6_monitor(fx, "u6b-l13-A", MONITOR_A),
        "H6_L13_A_MONITOR_INGRESS").await;
    k2lg_n_selected(fx, extra_actor.inject_pending_monitor_events()).await;
    let owner_a = extra_actor.state.lock().await.message_delivery.held_input.pin_owner
        .expect("H6_L13_A_REAL_PIN_OWNER");
    assert_eq!(buffer.release_view(owner_a, |_| false).iter().count(), 1,
        "H6_L13_A_REAL_PIN");
    let barriers = fixture.persistence_control.barriers.load(std::sync::atomic::Ordering::SeqCst);
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_L13_A_CLOSED_CHECKPOINT").await;
    u6b_u66_progress(&mut turn.running, u6_u63_step_to_preparation(&fixture, &stepper, barriers),
        "H6_L13_A_PREPARATION").await;

    let mut b = u6_fixture(None, K2lgNOptions::default()).await;
    let sampler = b.fx.session.tasks.remove(2);
    sampler.abort();
    let stopped = tokio::time::timeout(K0_TIMEOUT, sampler).await
        .expect("H6_L13_B_OLD_DRAINER_STOP_BOUND");
    assert!(stopped.is_err_and(|error| error.is_cancelled()), "H6_L13_B_OLD_DRAINER_STOPPED");
    let (sampler_tx, mut sampler_rx) = tokio::sync::mpsc::unbounded_channel();
    {
        let actor = Arc::get_mut(&mut b.fx.session.actor).expect("H6_L13_B_EXCLUSIVE_SETUP");
        actor.tool_context.monitor_event_buffer = Some(buffer.clone());
        actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
            xai_grok_sampler::SamplerConfig {
                base_url: b.fx.server.url(),
                model: "k2-replay".into(),
                api_key: Some("nonsecret-k2-key".into()),
                api_backend: xai_grok_sampler::ApiBackend::Messages,
                max_completion_tokens: Some(4096),
                max_retries: Some(0),
                ..Default::default()
            },
            xai_grok_sampler::RetryPolicy { max_retries: 0, ..Default::default() },
            sampler_tx,
        );
    }
    let drainer = b.fx.session.actor.clone();
    let (source_tx, source_acks) = tokio::sync::mpsc::unbounded_channel();
    b.fx.session.source_acks = source_acks;
    b.fx.session.tasks.push(tokio::task::spawn_local(async move {
        while let Some(event) = sampler_rx.recv().await {
            let status = match &event {
                xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                    evidence: k2_history::NativeEvidenceEvent::Attempt(attempt), ..
                } => Some(attempt.status),
                _ => None,
            };
            drainer.handle_sampling_event(event).await;
            if let Some(status) = status {
                let _ = source_tx.send(status);
            }
        }
    }));
    let mut b_turn = u6_handoff_with_salvage(&mut b.fx, None, None, None, None).await;
    let _b_stop = K0Abort(b_turn.running.abort_handle());
    assert_ne!(extra_actor.session_id_string(), b.fx.session.actor.session_id_string(),
        "H6_L13_DISTINCT_REAL_LEADERS");
    u6b_u66_progress(&mut b_turn.running, u6_monitor(&b.fx, "u6b-l13-B", MONITOR_B),
        "H6_L13_B_MONITOR_INGRESS").await;
    buffer.push_capped(MonitorEventNotification {
        task_id: "u6b-l13-legacy-B".into(),
        event_text: format!("<monitor-event description=\"legacy\" task_id=\"u6b-l13-legacy-B\">\n{LEGACY_B}\n</monitor-event>"),
        owner_session_id: None,
    }, 128);
    k2lg_n_selected(&b.fx, b.fx.session.actor.inject_pending_monitor_events()).await;
    let owner_b = b.fx.session.actor.state.lock().await.message_delivery.held_input.pin_owner
        .expect("H6_L13_B_REAL_PIN_OWNER");
    assert_ne!(owner_a, owner_b, "H6_L13_OWNER_IDENTITIES_DISTINCT");
    assert_eq!(buffer.release_view(owner_b, |_| false).iter().count(), 2,
        "H6_L13_B_REAL_OWN_AND_LEGACY_PINS");
    let b_keys = u6_mixed_keys(&b.fx).await;
    assert!(b_keys.iter().any(|entry| entry["source"] == "monitor_event"), "H6_L13_B_REAL_HOLD");
    u6b_u66_progress(&mut b_turn.running, k2lg_n_send(&mut b.fx, NATIVE_B),
        "H6_L13_REAL_PARENT_MAILBOX").await;
    assert_eq!(k2lg_n_resident(&b.fx.session).len(), 1, "H6_L13_PARENT_NATIVE_RETAINED");
    let b_requests = b.fx.server.messages_request_count();
    assert!(Arc::strong_count(&extra_actor) > 1, "H6_L13_EXTRA_ARC");
    match operation {
        U6bL13Operation::Release => {
            let mut after = fx.server.expect_response_blocked(
                "U6 L13 leader A post-release request",
                xai_grok_test_support::InferenceRequestMatcher::foreground(
                    xai_grok_test_support::InferenceEndpoint::Messages,
                ),
                k2_script(k2lg_n_after_frames("msg_u6b_l13_A_after")),
            );
            stepper.resume();
            u6b_u66_progress(&mut turn.running, after.wait_blocked(), "H6_L13_A_RELEASE_HTTP").await;
            let bodies = k2lg_n_bodies(&fx.server);
            let text = u6_body_text(bodies.last().expect("H6_L13_A_RELEASE_HTTP"));
            assert_eq!(text.matches(HUMAN_A).count(), 1, "H6_L13_A_OWN_HUMAN_ONCE");
            assert_eq!(text.matches(MONITOR_A).count(), 1, "H6_L13_A_OWN_MONITOR_ONCE");
            assert!(![MONITOR_B, LEGACY_B, NATIVE_B].iter().any(|needle| text.contains(needle)),
                "H6_L13_NO_FOREIGN_TAKE");
            after.release();
            let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
                .expect("H6_L13_A_OUTCOME_BOUND").expect("H6_L13_A_TURN_JOIN");
            assert!(outcome.is_ok(), "H6_L13_A_RELEASE_SUCCEEDED");
            tokio::time::timeout(K0_TIMEOUT, after.wait_satisfied()).await
                .expect("H6_L13_A_REQUEST_SATISFIED");
            after.assert_satisfied();
        }
        U6bL13Operation::LogicalEnd => {
            u6b_u66_progress(&mut turn.running,
                extra_actor.settle_all_parent_messages(xai_message_delivery_core::TerminalCause::ActorDrop),
                "H6_L13_LOGICAL_UNPIN_BOUND").await;
            let state = extra_actor.state.lock().await;
            assert!(state.message_delivery.held_input.terminal
                && state.message_delivery.held_input.pin_owner.is_none(), "H6_L13_A_LOGICALLY_ENDED");
            assert_eq!(buffer.release_view(owner_b, |_| false).iter().count(), 2,
                "H6_L13_NO_FOREIGN_UNPIN");
        }
        U6bL13Operation::KillBackground => {
            let outcome = extra_actor.cancel_running_task(crate::session::CancelOptions {
                cancel_subagents: false,
                kill_background_tasks: true,
                user_initiated: true,
                trigger: Some(crate::session::CancelTrigger::CtrlC),
                history: crate::session::CancelHistoryDisposition::Keep,
            }).await;
            assert!(outcome.turn_stopped, "H6_L13_A_REAL_KILL_CANCEL");
            let stopped = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
                .expect("H6_L13_A_KILL_OUTCOME_BOUND");
            assert!(stopped.is_err_and(|error| error.is_cancelled()), "H6_L13_A_KILL_OWNS_ABORT");
            assert_eq!(buffer.release_view(owner_b, |_| false).iter().count(), 2,
                "H6_L13_NO_FOREIGN_DISCARD");
        }
    }
    assert_eq!(buffer.release_view(owner_b, |_| false).iter().count(), 2,
        "H6_L13_FOREIGN_PINS_STILL_OWNED");
    assert_eq!(u6_mixed_keys(&b.fx).await, b_keys, "H6_L13_FOREIGN_LEDGER_UNCHANGED");
    assert_eq!(k2lg_n_resident(&b.fx.session).len(), 1, "H6_L13_PARENT_NATIVE_RETAINED");
    assert_eq!(b.fx.server.messages_request_count(), b_requests, "H6_L13_NO_FOREIGN_RESTART");
    assert!(!b_turn.running.is_finished(), "H6_L13_OTHER_LEADER_STILL_RUNNING");
    continuation.assert_satisfied();
    json!({"kind": "L13_REAL_SHARED_LEADERS", "constructed": false,
        "operation": match operation { U6bL13Operation::Release => "take",
            U6bL13Operation::LogicalEnd => "unpin", U6bL13Operation::KillBackground => "discard" },
        "two_real_held_leaders": true, "same_actor_monitor_buffer": true,
        "ownerless_legacy_pinned_by_other_actor": true, "real_parent_shell_child_mailbox": true,
        "foreign_pin_count": 2, "foreign_ledger_unchanged": true,
        "other_leader_still_running": true, "b_sampler_recreated_before_its_first_turn": true})
}

#[derive(Clone, Copy)]
enum U6bL18Resume {
    NextAdmittedTurn,
    SendNow,
}

async fn u6b_l18_wait_cancel(resume: U6bL18Resume) -> Value {
    const CUSTODY: &str = "U6 L18 custody before client wait";
    const RESIDENT: &str = "U6 L18 arrival after last held safe point";
    const PROMPT: &str = "U6 L18 next admitted or send-now prompt";
    let mut fixture = u6_wait_fixture().await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff_with_salvage(fx, None, None, None, None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let initial_bodies = k2lg_n_bodies(&fx.server);
    assert!(u6_body_text(initial_bodies.last().expect("H6_L18_USER_PROMPT_ALREADY_SENT"))
        .contains("U6 real mixed handoff"), "H6_L18_USER_PROMPT_ALREADY_SENT");
    let front_before = fx.session.actor.state.lock().await.front_message_committed;
    u6_observe(json!({"stage":"L18 direct-turn intake commit", "front_before":front_before,
        "prompt_already_in_actual_http":true}));
    assert!(!front_before, "H6_L18_DIRECT_FIXTURE_COMMIT_PRECONDITION");
    fx.session.actor.mark_front_message_committed().await;
    assert!(fx.session.actor.state.lock().await.front_message_committed,
        "H6_L18_RUNNING_PROMPT_COMMITTED");
    let definition = fx.session.actor.agent.borrow().definition().clone();
    let old_bridge = fx.session.actor.agent.borrow().tool_bridge().clone();
    u6b_u66_progress(&mut turn.running, u6_interject(fx, CUSTODY, Vec::new()),
        "H6_L18_CUSTODY_INGRESS").await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_L18_CUSTODY_HELD");
    let custody_keys = u6_mixed_keys(fx).await;
    assert_eq!(custody_keys.len(), 1, "H6_L18_ONE_HELD_HUMAN");
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L18 real interruptible client wait",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(u6_wait_frames(u6_next_pending(true), "client_u6_l18_wait", 60_000)),
    );
    fx.session.releases[1].add_permits(1);
    u6b_u66_progress(&mut turn.running, continuation.wait_blocked(),
        "H6_L18_REAL_WAIT_REQUEST").await;
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, true,
        "H6_L18_READABLE_PENDING_CHECKPOINT").await;
    u6_wait_started(fx, &mut turn.running).await;
    let stepper = u6_current_stepper();
    stepper.pause();
    assert!(fx.session.actor.tool_context.blocking_wait_depth.depth() != 0,
        "H6_L18_REAL_WAIT_IN_FLIGHT");
    u6b_u66_progress(&mut turn.running, u6_interject(fx, RESIDENT, Vec::new()),
        "H6_L18_AFTER_LAST_SAFE_POINT_INGRESS").await;
    assert_eq!(u6_mixed_keys(fx).await, custody_keys, "H6_L18_LATE_IS_UNADMITTED");
    assert_eq!(fx.session.actor.pending_interjections.len(), 1, "H6_L18_LATE_RESIDENT");
    let before_requests = fx.server.messages_request_count();
    let (mut next, next_stop) = match resume {
        U6bL18Resume::NextAdmittedTurn => {
            fx.loop_tx.send(SessionCommand::Cancel(crate::session::CancelOptions {
                user_initiated: true,
                trigger: Some(crate::session::CancelTrigger::CtrlC),
                ..Default::default()
            })).unwrap();
            let stopped = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
                .expect("H6_L18_IMMEDIATE_CANCEL");
            assert!(stopped.is_err_and(|error| error.is_cancelled()), "H6_L18_IMMEDIATE_CANCEL");
            tokio::time::timeout(K0_TIMEOUT, async {
                while fx.session.actor.state.lock().await.running_task.is_some() {
                    tokio::task::yield_now().await;
                }
            }).await.expect("H6_L18_OLD_OWNER_SETTLED");
            assert!(fx.session.actor.pending_interjections.is_empty(), "H6_L18_CANCEL_CLEARS_UNADMITTED");
            assert_eq!(u6_mixed_keys(fx).await, custody_keys, "H6_L18_CANCEL_RETAINS_CUSTODY");
            assert_eq!(fx.server.messages_request_count(), before_requests, "H6_L18_NO_SELF_START");
            let (responds_to, rebuilt) = tokio::sync::oneshot::channel();
            fx.loop_tx.send(SessionCommand::RebuildAgentForDefinition { definition, responds_to }).unwrap();
            tokio::time::timeout(K0_TIMEOUT, rebuilt).await.expect("H6_L18_REBUILD_BOUND")
                .expect("H6_L18_REBUILD_REPLY").expect("H6_L18_REAL_REBUILD");
            assert!(!Arc::ptr_eq(&old_bridge, fx.session.actor.agent.borrow().tool_bridge()),
                "H6_L18_AGENT_ACTUALLY_REBUILT");
            assert_eq!(u6_mixed_keys(fx).await, custody_keys, "H6_L18_REBUILD_RETAINS_CUSTODY");
            assert_eq!(fx.server.messages_request_count(), before_requests, "H6_L18_NO_SELF_START");
            u6b_u66_next_prompt(fx, PROMPT, "H6_L18_NEXT_ADMITTED_PROMPT").await
        }
        U6bL18Resume::SendNow => {
            let mut next = fx.server.expect_response_blocked(
                "U6 L18 real send-now prompt",
                xai_grok_test_support::InferenceRequestMatcher::foreground(
                    xai_grok_test_support::InferenceEndpoint::Messages,
                ),
                k2_script(k2lg_n_after_frames("msg_u6b_l18_send_now")),
            );
            let (respond_to, mut response) = tokio::sync::oneshot::channel();
            let (ready, mut ready_rx) = tokio::sync::oneshot::channel();
            fx.loop_tx.send(SessionCommand::Prompt {
                prompt_id: "u6b-l18-send-now".into(),
                prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(PROMPT))],
                prompt_mode: PromptMode::Agent,
                artifact_upload_ctx: None,
                client_identifier: Some("U6".into()),
                screen_mode: None,
                verbatim: false,
                traceparent: None,
                json_schema: None,
                send_now: true,
                admission: None,
                tool_overrides_update: None,
                respond_to,
                prompt_admitted: Some(ready),
                persist_ack: None,
                parsed_prompt_tx: None,
            }).unwrap();
            let stopped = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
                .expect("H6_L18_SEND_NOW_IMMEDIATE_CANCEL");
            assert!(stopped.is_err_and(|error| error.is_cancelled()), "H6_L18_SEND_NOW_IMMEDIATE_CANCEL");
            tokio::time::timeout(K0_TIMEOUT, async {
                tokio::select! {
                    result = &mut ready_rx => {
                        result.expect("H6_L18_SEND_NOW_READINESS").send(())
                            .expect("H6_L18_SEND_NOW_PUBLICATION_RELEASE");
                    }
                    _ = &mut response => panic!("H6_L18_SEND_NOW_ENDED_BEFORE_READY"),
                }
            }).await.expect("H6_L18_SEND_NOW_READY_BOUND");
            let handle = fx.session.actor.state.lock().await.running_task.as_ref()
                .expect("H6_L18_SEND_NOW_NEW_OWNER").handle.clone();
            let stop = K0Abort(handle);
            tokio::time::timeout(K0_TIMEOUT, async {
                tokio::select! {
                    _ = next.wait_blocked() => {},
                    _ = &mut response => panic!("H6_L18_SEND_NOW_ENDED_BEFORE_HTTP"),
                }
            }).await.expect("H6_L18_SEND_NOW_HTTP_BOUND");
            (next, stop)
        }
    };
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), before_requests + 1, "H6_L18_RELEASE_ONCE");
    let text = u6_body_text(bodies.last().expect("H6_L18_ACTUAL_NEXT_REQUEST"));
    assert_eq!(text.matches(PROMPT).count(), 1, "H6_L18_PROMPT_ONCE");
    assert_eq!(text.matches(CUSTODY).count(), 1, "H6_L18_CUSTODY_RELEASE_ONCE");
    let prompt_at = text.find(PROMPT).expect("H6_L18_PROMPT_BEFORE_CUSTODY");
    let custody_at = text.find(CUSTODY).expect("H6_L18_PROMPT_BEFORE_CUSTODY");
    assert!(prompt_at < custody_at, "H6_L18_PROMPT_BEFORE_CUSTODY");
    match resume {
        U6bL18Resume::NextAdmittedTurn => {
            assert!(!text.contains(RESIDENT), "H6_L18_CANCEL_CLEARS_UNADMITTED");
        }
        U6bL18Resume::SendNow => {
            assert_eq!(text.matches(RESIDENT).count(), 1, "H6_L18_SEND_NOW_RESIDENT_ONCE");
            assert!(text.find(RESIDENT).expect("H6_L18_SEND_NOW_ORDER") > custody_at,
                "H6_L18_SEND_NOW_ORDER");
        }
    }
    assert!(u6_mixed_keys(fx).await.is_empty(), "H6_L18_CUSTODY_EMPTY_AFTER_RELEASE");
    next.release();
    tokio::time::timeout(K0_TIMEOUT, next.wait_satisfied()).await
        .expect("H6_L18_NEXT_REQUEST_SATISFIED");
    next.assert_satisfied();
    continuation.assert_satisfied();
    tokio::time::timeout(K0_TIMEOUT, async {
        while !next_stop.0.is_finished() {
            tokio::task::yield_now().await;
        }
    }).await.expect("H6_L18_NEXT_TURN_FINISHED");
    assert_eq!(fx.server.messages_request_count(), before_requests + 1, "H6_L18_RELEASE_ONCE");
    assert!(u6_mixed_keys(fx).await.is_empty(), "H6_L18_CUSTODY_EMPTY_AFTER_RELEASE");
    json!({"kind": "L18_REAL_WAIT_CANCEL", "constructed": false,
        "resume": match resume { U6bL18Resume::NextAdmittedTurn => "Cancel-rebuild-next-admitted-turn",
            U6bL18Resume::SendNow => "Prompt send_now through real run loop" },
        "real_interruptible_wait": true, "later_arrival_after_last_held_safe_point": true,
        "custody_before_cancel": custody_keys, "requests_before_cancel": before_requests,
        "requests_after_next_turn": fx.server.messages_request_count(), "prompt_before_custody": prompt_at < custody_at,
        "once_only_release": bodies.len() == before_requests + 1 && text.matches(CUSTODY).count() == 1, "next_task_finished": next_stop.0.is_finished(),
        "budget_refused_resident": "NOT_REPRESENTABLE / NOT_RUN / NOT_CREDITED under signed OI-15"})
}

const U6B_L18_READY_OUTPUT: &str = "U6 L18 completed task wins ready-result race";

struct U6bL18ReadyRunner {
    inner: K2lgNRunner,
    finish: std::rc::Rc<tokio::sync::Notify>,
    published: std::rc::Rc<std::cell::Cell<bool>>,
}

impl k0_coordinator::ChildRunner for U6bL18ReadyRunner {
    type Control = crate::agent::subagent::ShellChildRuntime;
    type CompletionData = ();
    type RunFuture = k0_coordinator::LocalBoxFuture<k0_coordinator::ChildRunOutput<()>>;
    type ValidateFuture = std::future::Pending<k0_task::SubagentValidateTypeOutcome>;
    type DescribeFuture = std::future::Pending<k0_task::SubagentDescribeOutcome>;

    fn run(&self, request: k0_coordinator::ChildRunRequest<Self::Control>) -> Self::RunFuture {
        let id = request.request.id.clone();
        let running = <K2lgNRunner as k0_coordinator::ChildRunner>::run(&self.inner, request);
        let finish = self.finish.clone();
        Box::pin(async move {
            tokio::select! {
                output = running => output,
                _ = finish.notified() => k0_coordinator::ChildRunOutput {
                    result: k0_task::SubagentResult {
                        success: true, output: Arc::from(U6B_L18_READY_OUTPUT),
                        subagent_id: id.clone(), child_session_id: id,
                        turns: 1, ..Default::default()
                    },
                    completion_data: (), snapshot_ref: None,
                },
            }
        })
    }
    fn validate_type(&self, _: String, _: String) -> Self::ValidateFuture {
        std::future::pending()
    }
    fn describe_type(&self, _: String, _: Option<String>, _: String) -> Self::DescribeFuture {
        std::future::pending()
    }
    fn supports_wake(&self) -> bool { false }
    fn on_completed(&self, completed: k0_coordinator::ChildCompletion<()>, published: Box<dyn FnOnce() + Send>) {
        <K2lgNRunner as k0_coordinator::ChildRunner>::on_completed(&self.inner, completed, published);
        self.published.set(true);
    }
}

async fn u6b_l18_ready_result_priority() -> Value {
    use std::task::{Context, Poll};
    use xai_grok_tools::implementations::grok_build::task::backend::SubagentBackendResource;
    const HUMAN: &str = "U6 L18 resident at completed-result rendezvous";
    let mut fixture = u6_wait_fixture().await;
    let started = std::rc::Rc::new(tokio::sync::Notify::new());
    let finish = std::rc::Rc::new(tokio::sync::Notify::new());
    let published = std::rc::Rc::new(std::cell::Cell::new(false));
    let (sender, receiver) = k0_coordinator::SubagentCoordinator::<U6bL18ReadyRunner>::channel();
    let runner = U6bL18ReadyRunner {
        inner: K2lgNRunner { commands: fixture.fx.loop_tx.clone(), started: started.clone() },
        finish: finish.clone(), published: published.clone(),
    };
    let coordinator: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> = Box::pin(
        k0_coordinator::SubagentCoordinator::from_channel(receiver, runner, Default::default()).run(),
    );
    let shared = std::rc::Rc::new(std::cell::RefCell::new(coordinator));
    let manual = std::rc::Rc::new(std::cell::Cell::new(false));
    let saved_waker = std::rc::Rc::new(std::cell::RefCell::new(None::<std::task::Waker>));
    let background_shared = shared.clone();
    let background_manual = manual.clone();
    let background_waker = saved_waker.clone();
    let pump = tokio::task::spawn_local(futures_util::future::poll_fn(move |context| {
        *background_waker.borrow_mut() = Some(context.waker().clone());
        if background_manual.get() {
            Poll::Pending
        } else {
            std::future::Future::poll(background_shared.borrow_mut().as_mut(), context)
        }
    }));
    let _pump_stop = K0Abort(pump.abort_handle());
    let root = k2lg_n_root(&fixture.fx.server, sender).await;
    tokio::time::timeout(K0_TIMEOUT, k2lg_n_register_worker(&root, &started)).await
        .expect("H6_L18_READY_REAL_WORKER_REGISTERED");
    let bridge = fixture.fx.session.actor.agent.borrow().tool_bridge().clone();
    let backend = root.tool_bridge().read_resource::<SubagentBackendResource>().await
        .expect("H6_L18_READY_REAL_BACKEND");
    bridge.shared_resources().await.lock().await.insert(backend);
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff_with_salvage(fx, None, None, Some(60_000), None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    u6_wait_started(fx, &mut turn.running).await;
    let stepper = u6_current_stepper();
    stepper.pause();
    manual.set(true);
    finish.notify_one();
    let waker = futures_util::task::noop_waker();
    let mut context = Context::from_waker(&waker);
    let mut polls = 0;
    while !published.get() && polls < 128 {
        assert!(!turn.running.is_finished(), "H6_L18_READY_TURN_STILL_RUNNING");
        assert!(std::future::Future::poll(shared.borrow_mut().as_mut(), &mut context).is_pending(),
            "H6_L18_READY_COORDINATOR_STILL_RUNNING");
        polls += 1;
    }
    assert!(published.get(), "H6_L18_READY_SOURCE_SETTLED_UNDER_OWNED_BARRIER");
    assert!(fx.session.actor.tool_context.blocking_wait_depth.depth() != 0,
        "H6_L18_READY_RESULT_NOT_YET_CONSUMED");
    // The current-thread runtime cannot poll the dispatch drainer during this rendezvous.
    fx.session.actor.pending_interjections.push(xai_interjection_core::PendingInterjection {
        text: HUMAN.into(), attachments: Vec::new(),
    });
    assert_eq!(fx.session.actor.pending_interjections.len(), 1, "H6_L18_READY_COMPETING_RESIDENT");
    assert!(fx.session.actor.tool_context.blocking_wait_depth.depth() != 0,
        "H6_L18_READY_BOTH_INPUTS_BEFORE_CONSUMPTION");
    manual.set(false);
    saved_waker.borrow_mut().take().expect("H6_L18_READY_COORDINATOR_WAKER").wake();
    stepper.resume();
    u6_wait_result(fx, &mut turn.running, "client_u6_wait").await;
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    let result = history.iter().find(|item| matches!(item,
        ConversationItem::ToolResult(result) if result.tool_call_id == "client_u6_wait"))
        .expect("H6_L18_READY_ACTUAL_TOOL_RESULT");
    let output_present = result.text_content().contains(U6B_L18_READY_OUTPUT);
    assert!(output_present, "H6_L18_READY_RESULT_PRIORITY");
    let interrupted = fx.session.actor.tool_context.blocking_wait_depth.interrupted_wait_ids();
    assert!(interrupted.is_none(), "H6_L18_READY_RESULT_PRIORITY");
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L18 ready-result results-only continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.server.enqueue_response("/v1/messages", k2_script(k2lg_n_after_frames("msg_u6b_l18_ready_after")));
    fx.session.releases[1].add_permits(1);
    u6b_u66_progress(&mut turn.running, continuation.wait_blocked(),
        "H6_L18_READY_CONTINUATION").await;
    assert!(!fx.session.actor.chat_state_handle.get_conversation().await.iter()
        .any(|item| item.text_content().contains(HUMAN)), "H6_L18_READY_HUMAN_STILL_HELD");
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_L18_READY_CLOSED_CHECKPOINT").await;
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_L18_READY_OUTCOME_BOUND").expect("H6_L18_READY_TURN_JOIN");
    assert!(outcome.is_ok(), "H6_L18_READY_RELEASE_PROGRESS");
    let bodies = k2lg_n_bodies(&fx.server);
    let released_count = u6_body_text(bodies.last().expect("H6_L18_READY_RELEASE_REQUEST"))
        .matches(HUMAN).count();
    assert_eq!(released_count, 1, "H6_L18_READY_RELEASE_ONCE");
    continuation.assert_satisfied();
    json!({"kind": "L18_READY_RESULT_REAL_DISPATCH", "constructed": true,
        "construction": "resident input at a controlled current-thread rendezvous; no public arrival-producer claim",
        "real_task_output_backend": true, "real_dispatch_select": true,
        "coordinator_completion_published": published.get(), "manual_coordinator_polls": polls,
        "ready_output_present": output_present, "interrupted_wait_ids": interrupted,
        "resident_count_in_final_request": released_count})
}

async fn u6b_db11_native_handoff_after_terminal() -> Value {
    const HUMAN: &str = "U6 DB11 held human before logical end";
    const NATIVE: &str = "U6 DB11 native prefix must remain excluded";
    const STEER: &str = "U6 DB11 held parent before logical end";
    const DEFERRED: &str = "U6 DB11 follow-up after logical end";
    let (mut fixture, mut turn, stepper, continuation) = u6_u63_prepare_fixture(Some(HUMAN)).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let extra_actor = fixture.fx.session.actor.clone();
    let steer = u6b_admit_steer_id(&fixture.fx, &mut turn.running, "u6b-db11-parent", STEER,
        "H6_DB11_REAL_PARENT_ADMISSION").await;
    assert_eq!(steer.admission, k0_coordinator::ActiveMessageAdmission::Admitted,
        "H6_DB11_REAL_PARENT_ADMISSION");
    let _receipt = steer.receipt.expect("H6_DB11_REAL_PARENT_ADMISSION");
    k2lg_n_selected(&fixture.fx, extra_actor.drain_admitted_messages_at_safe_point()).await;
    assert!(extra_actor.state.lock().await.message_delivery.held_input.parent_key("u6b-db11-parent").is_some(),
        "H6_DB11_REAL_ADMISSION_STAMP");
    let barriers = fixture.persistence_control.barriers.load(std::sync::atomic::Ordering::SeqCst);
    u6_release_after_checkpoint(&fixture.fx, &mut turn.running, &continuation, false,
        "H6_DB11_READABLE_CLOSED_CHECKPOINT").await;
    u6b_u66_progress(&mut turn.running, u6_u63_step_to_preparation(&fixture, &stepper, barriers),
        "H6_DB11_PREPARATION_SUSPENDED").await;
    u6b_u66_progress(&mut turn.running, k2lg_n_send(&mut fixture.fx, NATIVE),
        "H6_DB11_REAL_NATIVE_MAILBOX").await;
    assert_eq!(k2lg_n_resident(&fixture.fx.session).len(), 1, "H6_DB11_REAL_NATIVE_RESIDENT");
    let before_requests = fixture.fx.server.messages_request_count();
    u6b_u66_progress(&mut turn.running,
        extra_actor.settle_all_parent_messages(xai_message_delivery_core::TerminalCause::ActorDrop),
        "H6_DB11_LOGICAL_SETTLEMENT").await;
    {
        let state = extra_actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert!(ledger.terminal && ledger.entries.is_empty() && ledger.latch.is_none()
            && ledger.parent_stamps.is_empty() && ledger.pin_owner.is_none(),
            "H6_DB11_TERMINAL_STATE");
    }
    let error = k2lg_n_selected(&fixture.fx, extra_actor.hold_or_push_user_item(
        ConversationItem::system_reminder(DEFERRED), super::interjection::HeldSource::DeferredFollowUp,
    )).await.expect_err("H6_DB11_TERMINAL_ADMISSION");
    assert_eq!(error.data.as_ref().and_then(|data| data.get("code")).and_then(Value::as_str),
        Some("native_held_input_incomplete"), "H6_DB11_TERMINAL_ADMISSION");
    let reason = error.data.as_ref().and_then(|data| data.get("message"))
        .and_then(Value::as_str).unwrap_or(&error.message);
    assert!(reason.contains("terminal"), "H6_DB11_TERMINAL_ADMISSION_REASON");
    assert!(!k2lg_n_selected(&fixture.fx, extra_actor.drain_admitted_messages_at_safe_point()).await,
        "H6_DB11_TERMINAL_LATCH_EXCLUSION");
    assert_eq!(k2lg_n_resident(&fixture.fx.session).len(), 1, "H6_DB11_NATIVE_HANDOFF_EXCLUDED");
    fixture.fx.server.enqueue_response("/v1/messages", k2_script(k2lg_n_after_frames("msg_u6b_db11_forbidden")));
    stepper.resume();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_DB11_RESUMED_OUTCOME").expect("H6_DB11_RESUMED_JOIN");
    assert_eq!(fixture.fx.server.messages_request_count(), before_requests, "H6_DB11_NATIVE_HANDOFF_EXCLUDED");
    assert_eq!(k2lg_n_resident(&fixture.fx.session).len(), 1, "H6_DB11_NATIVE_HANDOFF_EXCLUDED");
    let conversation = extra_actor.chat_state_handle.get_conversation().await;
    assert!(!conversation.iter().any(|item| [HUMAN, NATIVE, STEER, DEFERRED].iter()
        .any(|text| item.text_content().contains(text))), "H6_DB11_NATIVE_HANDOFF_EXCLUDED");
    assert!(Arc::strong_count(&extra_actor) > 1, "H6_DB11_EXTRA_ARC_STILL_ALIVE");
    continuation.assert_satisfied();
    json!({"kind": "DB11_NATIVE_HANDOFF_AFTER_TERMINAL", "constructed": false,
        "real_native_resident_count": k2lg_n_resident(&fixture.fx.session).len(),
        "requests_before": before_requests, "requests_after": fixture.fx.server.messages_request_count(),
        "resumed_turn_error": outcome.is_err(), "terminal_admission_code": "native_held_input_incomplete",
        "extra_actor_arc_count": Arc::strong_count(&extra_actor),
        "no_new_mutant": true})
}

async fn u6b_db11_constructed_terminal_stamp() -> Value {
    let fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let actor = &fixture.fx.session.actor;
    actor.settle_all_parent_messages(xai_message_delivery_core::TerminalCause::ActorDrop).await;
    let mut state = actor.state.lock().await;
    assert!(state.message_delivery.held_input.terminal, "H6_DB11_STAMP_TERMINAL_PRECONDITION");
    state.message_delivery.held_input.stamp_parent("u6b-db11-post-terminal-stamp");
    assert!(state.message_delivery.held_input.parent_stamps.is_empty(), "H6_DB11_TERMINAL_STAMP_GUARD");
    json!({"kind": "DB11_TERMINAL_STAMP_GUARD", "constructed": true,
        "construction": "direct admission-side stamp call after logical settlement, not a post-end parent producer",
        "stamp_count": state.message_delivery.held_input.parent_stamps.len(), "no_new_mutant": true})
}

async fn u6b_u67_send_peer(fx: &mut K2lgN, index: usize, native: bool) {
    let body = format!("U6b peer {} {index:03}", if native { "native" } else { "text" });
    if native {
        fx.calls += 1;
        let result = fx.root.tool_bridge().call("send_message",
            json!({"target":K2LG_N_TARGET,"message":body}),
            &format!("u6b_peer_{}", fx.calls)).await.expect("H6_N11_PEER_NATIVE_REAL_TOOL");
        let xai_grok_tools::types::output::ToolOutput::Dynamic(output) = result.output else {
            panic!("H6_N11_PEER_NATIVE_REAL_RECEIPT");
        };
        assert_eq!(output.value["status"].as_str().map(str::to_ascii_lowercase).as_deref(),
            Some("delivered"), "H6_N11_PEER_NATIVE_MAILBOX_DELIVERED");
    } else {
        use xai_grok_tools::implementations::grok_build::task::types::{AgentMailboxMessage, AgentMailboxMessageKind};
        fx.loop_tx.send(SessionCommand::AgentMessage { message: AgentMailboxMessage {
            message_id: format!("u6b-peer-text-{index}"), team_scope_id: "u6b-peer-team".into(),
            from_agent_id: "u6b-peer-source".into(), to_agent_id: K2LG_N_TARGET.into(),
            kind: AgentMailboxMessageKind::Message, body, created_at_ms: 0, native: None,
        }}).expect("H6_N11_PEER_REAL_MAILBOX_SEND");
    }
}

async fn u6b_u67_mailbox_barrier(fx: &K2lgN, turn: &mut K2lgNTurn) {
    let (responds_to, mut reply) = tokio::sync::oneshot::channel();
    fx.loop_tx.send(SessionCommand::GetCurrentModel { responds_to })
        .expect("H6_U67_REAL_MAILBOX_BARRIER_SEND");
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut reply => { result.expect("H6_U67_REAL_MAILBOX_BARRIER_REPLY"); },
            _ = &mut turn.running => panic!("H6_U67_REAL_MAILBOX_BARRIER_TURN_ENDED"),
        }
    }).await.expect("H6_U67_REAL_MAILBOX_BARRIER_BOUND");
}

async fn u6b_n11_peer(native_crossing: bool) -> Value {
    const HUMAN: &str = "U6b peer-band independent human";
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_N11_PEER_LIVE_RELOAD").await;
    assert!(k2lg_n_live(&checkpoint), "H6_N11_PEER_REAL_HOLD");
    for index in 0..50 {
        u6b_u67_send_peer(fx, index, index >= 25).await;
    }
    u6b_u67_mailbox_barrier(fx, &mut turn).await;
    assert_eq!(fx.session.actor.pending_interjections.snapshot().len(), 25,
        "H6_N11_PEER_TEXT_PREFIX_ADMITTED");
    assert_eq!(k2lg_n_resident(&fx.session).len(), 25,
        "H6_N11_PEER_NATIVE_PREFIX_ADMITTED");
    u6b_u67_send_peer(fx, 50, native_crossing).await;
    u6b_u67_mailbox_barrier(fx, &mut turn).await;
    {
        let state = fx.session.actor.state.lock().await;
        let refusal = state.message_delivery.held_input.refusal.as_ref()
            .expect("H6_N11_PEER_CROSSING");
        assert_eq!(format!("{:?}", refusal.reason), "PeerBandFull", "H6_N11_PEER_REFUSAL_VISIBLE");
        assert_eq!(u6_source_name(refusal.source), "peer_agent", "H6_N11_PEER_REFUSAL_VISIBLE");
    }
    assert_eq!(fx.session.actor.pending_interjections.snapshot().len(), 25,
        "H6_N11_PEER_TEXT_REFUSED_AT_PRODUCER");
    assert_eq!(k2lg_n_resident(&fx.session).len(), 25,
        "H6_N11_PEER_NATIVE_REFUSED_AT_PRODUCER");
    u6_interject(fx, HUMAN, Vec::new()).await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N11_PEER_NO_EARLY_ADMISSION");
    {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert_eq!(ledger.entries.iter().filter(|entry| u6_source_name(entry.source) == "peer_agent").count(),
            25, "H6_N11_PEER_ISOLATED_FROM_HUMAN");
        assert_eq!(ledger.entries.iter().filter(|entry| u6_source_name(entry.source) == "human_interjection").count(),
            1, "H6_N11_PEER_FULL_NEVER_REFUSES_HUMAN");
    }
    let mut continuation = fx.server.expect_response_blocked("U6b peer results-only continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(xai_grok_test_support::InferenceEndpoint::Messages),
        k2_script(k2lg_n_closure_frames()));
    fx.server.enqueue_response("/v1/messages", k2_script(k2lg_n_after_frames("msg_u6b_peer_after")));
    fx.session.releases[1].add_permits(1);
    let continued = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = continuation.wait_blocked() => true,
            _ = &mut turn.running => false,
        }
    }).await.expect("H6_N11_PEER_CONTINUATION_BOUND");
    assert!(continued, "H6_N11_PEER_RESULTS_ONLY_CONTINUATION");
    let bodies = k2lg_n_bodies(&fx.server);
    u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false, "H6_N11_PEER_CLOSURE").await;
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N11_PEER_RELEASE_BOUND").expect("H6_N11_PEER_RELEASE_JOIN");
    assert!(outcome.is_ok(), "H6_N11_PEER_RELEASE_OUTCOME");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    for index in 0..50 {
        let needle = format!("U6b peer {} {index:03}", if index >= 25 { "native" } else { "text" });
        assert_eq!(history.iter().filter(|item| item.text_content().contains(&needle)).count(), 1,
            "H6_N11_PEER_RELEASE_ONCE");
    }
    let rejected = format!("U6b peer {} 050", if native_crossing { "native" } else { "text" });
    assert!(!history.iter().any(|item| item.text_content().contains(&rejected)), "H6_N11_PEER_REJECTED_NEVER_DELIVERED");
    assert_eq!(history.iter().filter(|item| item.text_content().contains(HUMAN)).count(), 1,
        "H6_N11_PEER_HUMAN_RELEASE_ONCE");
    assert!(k2lg_n_resident(&fx.session).is_empty(), "H6_N11_PEER_NATIVE_PREFIX_REMOVED");
    json!({"constructed":false,"native_crossing":native_crossing,"peer_prefix":50,"refused":1,
        "human_delivered":1,"http":fx.server.messages_request_count()})
}

async fn u6b_l15_session(
    server: &MockInferenceServer,
    options: K2lgNOptions,
    agent_wide: &Arc<std::sync::atomic::AtomicBool>,
    chat_token: &tokio_util::sync::CancellationToken,
    log: &K2lgNLog,
    control: &U6PersistenceControl,
    image: Option<Arc<str>>,
) -> (K2ReplaySession, tokio::sync::mpsc::UnboundedReceiver<xai_chat_state::ChatStateEvent>) {
    let idle_observer = control.idle_observer.clone();
    let title_home = crate::util::grok_home::grok_home();
    assert!(title_home.starts_with(std::env::temp_dir()), "H6_R7_PRIVATE_TITLE_HOME");
    let title_cwd = title_home.to_string_lossy().into_owned();
    let session_parent = crate::util::grok_home::ensure_sessions_cwd_dir_in(&title_home, &title_cwd).unwrap();
    let directory = tempfile::Builder::new().prefix("k2lg-n-").tempdir_in(session_parent).unwrap();
    let config = xai_grok_sampler::SamplerConfig {
        base_url: server.url(),
        model: "k2-replay".into(),
        api_key: Some("nonsecret-k2-key".into()),
        api_backend: xai_grok_sampler::ApiBackend::Messages,
        max_completion_tokens: Some(4096),
        max_retries: Some(options.retries),
        ..Default::default()
    };
    let info = crate::session::info::Info {
        id: acp::SessionId::new(directory.path().file_name().unwrap().to_string_lossy().into_owned()),
        cwd: title_cwd,
    };
    let persistence = {
        let initial = crate::session::persistence::new_with_explicit_dir(
            &info, directory.path().to_path_buf(), acp::ModelId::new("k2-replay"),
            xai_grok_sampler::SamplingClient::new(config.clone()).unwrap(), "k2-replay".into(),
            crate::session::persistence::ExplicitSessionOpen::New { identity: None, next_trace_turn: None },
        ).await.unwrap();
        const TITLE: &str = "U6b already generated fixture session title";
        initial.tx.send(PersistenceMsg::GeneratedTitle(TITLE.into())).unwrap();
        let (respond_to, persisted) = tokio::sync::oneshot::channel();
        initial.tx.send(PersistenceMsg::FlushAndAck { respond_to }).unwrap();
        tokio::time::timeout(K0_TIMEOUT, persisted).await.unwrap().unwrap().unwrap();
        drop(initial);
        let (loaded, handle) = crate::session::persistence::load_light(
            &info, None, false, crate::session::persistence::SessionDeps {
                sampling_client: xai_grok_sampler::SamplingClient::new(config.clone()).unwrap(),
                storage_mode: crate::config::StorageMode::Local, auth_manager: None, relay_sync: None, gateway: None,
                session_summary_model: "k2-replay".into(), registry_title_sync: None,
                search_index: crate::session::storage::search::SharedSearchIndex::never_indexed(), session_kind: None,
            },
        ).await.unwrap();
        assert_eq!(loaded.summary.generated_title.as_deref(), Some(TITLE), "H6_R7_REAL_TITLED_RESUME");
        u6_observe(json!({"stage":"R7 title fixture state","loaded_title":true,"real_load_surface":"persistence::load_light"}));
        handle
    };
    let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let cleanup_control = control.clone();
    let gateway = tokio::task::spawn_local(async move {
        while let Some(message) = gateway_rx.recv().await {
            if let xai_acp_lib::AcpClientMessage::SessionNotification(arguments) = message {
                if matches!(&arguments.request.update, acp::SessionUpdate::Plan(plan)
                    if plan.entries.iter().any(|entry| entry.content == "U6 bookkeeping observer")
                        && plan.entries.iter().all(|entry| entry.status == acp::PlanEntryStatus::Completed))
                {
                    cleanup_control
                        .cleanup_count
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    cleanup_control.cleanup.notify_one();
                }
                let _ = arguments.response_tx.send(Ok(()));
            }
        }
    });
    let (persistence_tx, mut persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let real_persistence = persistence.tx.clone();
    let updates = control.updates.clone();
    let barriers = control.barriers.clone();
    let observation_control = control.clone();
    let persistence_proxy = tokio::task::spawn_local(async move {
        while let Some(message) = persistence_rx.recv().await {
            if matches!(&message, PersistenceMsg::FlushAndAck { .. }) {
                barriers.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if observation_control
                    .hold_barriers
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    observation_control
                        .held_barriers
                        .lock()
                        .unwrap()
                        .push(message);
                    observation_control.barrier_held.notify_one();
                    continue;
                }
            }
            if matches!(&message, PersistenceMsg::AnnouncementState(_)) {
                observation_control
                    .announcement_updates
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if matches!(&message, PersistenceMsg::PlanModeState(_)) {
                observation_control
                    .plan_updates
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if let PersistenceMsg::Update(SessionUpdate::Acp(notification)) = &message {
                if matches!(
                    &notification.update,
                    acp::SessionUpdate::UserMessageChunk(_)
                ) {
                    observation_control
                        .user_chunks
                        .lock()
                        .unwrap()
                        .push(serde_json::to_value(notification).unwrap());
                }
                updates
                    .lock()
                    .unwrap()
                    .push(serde_json::to_value(notification).unwrap());
            }
            if real_persistence.send(message).is_err() {
                break;
            }
        }
    });
    let (mut actor, _) = create_test_actor_with_chat_persistence(
        0,
        256_000,
        85,
        gateway_tx,
        persistence_tx.clone(),
        Box::new(xai_chat_state::NullChatPersistence),
    )
    .await;
    let sampling = actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .expect("K2LG setup chat-state sampling config");
    let (chat_event_tx, chat_event_rx) = tokio::sync::mpsc::unbounded_channel();
    actor.chat_state_handle = xai_chat_state::ChatStateActor::spawn(
        vec![],
        sampling,
        Box::new(K2lgNPersistence {
            inner: crate::session::chat_persistence::ChannelChatPersistence::new(
                persistence_tx.clone(),
            ),
            log: log.clone(),
            n12_drop_next_native_ack: control.n12_drop_next_native_ack.clone(),
        }),
        chat_event_tx,
        chat_token.clone(),
    );
    actor.tool_context.is_turn_active = Some(agent_wide.clone());
    actor.tool_context.monitor_event_buffer = Some(MonitorEventBuffer::new());
    actor.max_turns = options.max_turns;
    actor.session_info = info;
    actor.attach_non_interactive.set(true);
    if control.media_resample {
        *actor.agent.borrow_mut() = super::support::test_agent_with_tools(vec![
            xai_grok_tools::registry::types::ToolConfig::for_tool::<
                xai_grok_tools::implementations::grok_build::image_gen::ImageGenTool,
            >(),
        ])
        .await;
    }
    if control.plan_tools {
        *actor.agent.borrow_mut() = super::support::test_agent_with_plan_tools().await;
        *actor.plan_mode.lock() =
            crate::session::plan_mode::PlanModeTracker::new(directory.path().to_path_buf());
    }
    if control.bookkeeping_feedback {
        actor.feedback_manager = Arc::new(crate::session::feedback_manager::FeedbackManager::new(
            "u6-bookkeeping",
            None,
            crate::session::feedback_manager::FeedbackManagerConfig {
                feedback_enabled: true,
                ..Default::default()
            },
        ));
        actor
            .notifications
            .gateway_enabled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    actor.max_retries = 0;
    actor.compaction.compaction_mode = xai_chat_state::CompactionMode::Summary;
    {
        let mut slot = actor.agent.borrow_mut();
        let mut policy = slot.compaction_policy().clone();
        policy.two_pass_enabled = false;
        policy.remote_compaction_v2 = false;
        policy.memory_flush_enabled = false;
        *slot = xai_grok_agent::Agent::new(
            slot.definition().clone(),
            slot.prompt_context().clone(),
            slot.system_prompt().to_string(),
            slot.tool_bridge().clone(),
            slot.reminder_policy().clone(),
            policy,
            Vec::new(),
            true,
        );
    }
    actor.memory.flush_config.enabled = false;
    actor.memory.dream_config.enabled = false;
    actor.memory.save_on_end = false;
    let (manager, models) = manager_with_entries();
    let mut entry = entry_with_search_flag("k2-replay", true);
    entry.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
    entry.api_base_url = Some(server.url());
    entry.api_key = Some("nonsecret-k2-key".into());
    manager.insert_test_entry("k2-replay", entry);
    manager.set_current_model_id(acp::ModelId::new("k2-replay"));
    actor.models_manager = manager;
    let mut binding = actor
        .chat_state_handle
        .capture_transport_binding()
        .await
        .unwrap();
    binding.config.base_url = server.url();
    binding.config.model = "k2-replay".into();
    binding.config.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
    binding.config.supports_search_tool = true;
    binding.config.max_completion_tokens = Some(4096);
    binding.config.max_retries = Some(options.retries);
    binding.credentials.api_key = Some("nonsecret-k2-key".into());
    binding.credentials.auth_type = xai_chat_state::AuthType::ApiKey;
    actor
        .chat_state_handle
        .install_transport_binding(binding)
        .await
        .unwrap();
    let executions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (entered_tx, entered) = tokio::sync::mpsc::unbounded_channel();
    let releases = [
        Arc::new(tokio::sync::Semaphore::new(16)),
        Arc::new(tokio::sync::Semaphore::new(16)),
    ];
    for (index, name) in ["alpha__lookup", "zeta__fetch"].into_iter().enumerate() {
        actor
        .agent
        .borrow()
        .tool_bridge()
        .register_mcp_tools(
            name.into(),
            U6ClientTool {
                name,
                executions: executions.clone(),
                entered: entered_tx.clone(),
                release: releases[index].clone(),
                image: if index == 1 { image.clone() } else { None },
            },
            Some(json!({"type":"object","description":"Exact K2 original schema","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false})),
        )
        .unwrap();
    }
    {
        let mut mcp = actor.mcp_state.lock().await;
        mcp.finish_init();
        mcp.complete_init();
    }
    actor
        .workspace_ops
        .bind_local_session(
            &actor.session_id_string(),
            actor.tool_context.cwd.as_path().to_path_buf(),
            actor.tool_context.hunk_tracker_handle.clone(),
            actor.agent.borrow().tool_bridge().toolset(),
            None,
        )
        .unwrap();
    let (sampler_tx, mut sampler_rx) = tokio::sync::mpsc::unbounded_channel();
    actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
        config,
        xai_grok_sampler::RetryPolicy {
            max_retries: options.retries,
            ..Default::default()
        },
        sampler_tx,
    );
    actor.turn_summary_enabled = false;
    actor.title_refresh_enabled = false;
    if let Some(idle) = idle_observer.as_ref() {
        let mut extensions = xai_agent_lifecycle::LocalExtensionRegistryBuilder::default();
        extensions.session_lifecycle_contributor(idle.clone());
        actor.extension_registry = extensions.build();
    }
    let actor = Arc::new(actor);
    let drainer = actor.clone();
    let (source_tx, source_acks) = tokio::sync::mpsc::unbounded_channel();
    let sampler = tokio::task::spawn_local(async move {
        while let Some(event) = sampler_rx.recv().await {
            let status = match &event {
                xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                    evidence: k2_history::NativeEvidenceEvent::Attempt(attempt),
                    ..
                } => Some(attempt.status),
                _ => None,
            };
            drainer.handle_sampling_event(event).await;
            if let Some(status) = status {
                let _ = source_tx.send(status);
            }
        }
    });
    (K2ReplaySession {
        actor,
        persistence,
        directory,
        _models: models,
        tasks: vec![gateway, persistence_proxy, sampler],
        executions,
        entered,
        releases,
        source_acks,
    }, chat_event_rx)
}

async fn u6b_l15_fixture(
    image: Option<Arc<str>>,
    options: K2lgNOptions,
    persistence_control: U6PersistenceControl,
) -> (U6Fixture, tokio::sync::mpsc::UnboundedReceiver<xai_chat_state::ChatStateEvent>) {
    let server = MockInferenceServer::start().await.unwrap();
    let agent_wide = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let chat_token = tokio_util::sync::CancellationToken::new();
    let log = K2lgNLog::default();
    let (mut session, chat_rx) = u6b_l15_session(
        &server,
        options,
        &agent_wide,
        &chat_token,
        &log,
        &persistence_control,
        image,
    )
    .await;
    let (loop_tx, loop_rx) = tokio::sync::mpsc::unbounded_channel();
    let started = std::rc::Rc::new(tokio::sync::Notify::new());
    let (sender, receiver) = k0_coordinator::SubagentCoordinator::<K2lgNRunner>::channel();
    session.tasks.push(tokio::task::spawn_local(
        k0_coordinator::SubagentCoordinator::from_channel(
            receiver,
            K2lgNRunner {
                commands: loop_tx.clone(),
                started: started.clone(),
            },
            Default::default(),
        )
        .run(),
    ));
    let root = k2lg_n_root(&server, sender).await;
    k2lg_n_register_worker(&root, &started).await;
    (U6Fixture {
        fx: K2lgN {
            session,
            server,
            root,
            loop_tx,
            loop_rx: Some(loop_rx),
            log,
            chat_token,
            agent_wide,
            calls: 0,
        },
        persistence_control,
    }, chat_rx)
}

async fn u6b_l15_install_running(
    fx: &mut K2lgN,
    handle: tokio::task::AbortHandle,
    chat_rx: tokio::sync::mpsc::UnboundedReceiver<xai_chat_state::ChatStateEvent>,
) -> (TurnEpoch, std::rc::Rc<()>) {
    let epoch = fx.session.actor.turn_report.start_next_turn();
    let identity = {
        let mut state = fx.session.actor.state.lock().await;
        state.running_task = Some(AgentTask::new_at_epoch(KOXD_U6_PROMPT, epoch, handle));
        state
            .pending_inputs
            .push_front(user_item(KOXD_U6_PROMPT, "U6"));
        state.running_task.as_ref().unwrap().identity.clone()
    };
    *fx.session.actor.current_prompt_id.lock().unwrap() = Some(KOXD_U6_PROMPT.into());
    if !fx.session.actor.session_turn_active.load(std::sync::atomic::Ordering::SeqCst) {
        let guard = super::turn_task::TurnActiveGuard::activate(Some(&fx.session.actor.session_turn_active));
        fx.session.tasks.push(tokio::task::spawn_local(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        }));
        u6_observe(json!({"stage":"R7 fixture-owned manual turn guard","active":true}));
    }
    if let Some(command_rx) = fx.loop_rx.take() {
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let actor = fx.session.actor.clone();
        fx.session.tasks.push(tokio::task::spawn_local(async move {
            let _keep = event_tx;
            super::run_session(
                actor,
                command_rx,
                chat_rx,
                event_rx,
                None,
                Arc::new(parking_lot::Mutex::new(
                    xai_grok_workspace::file_system::CodebaseIndexManager::new(),
                )),
                std::env::temp_dir(),
                crate::session::fs_watch::FsWatchCapabilities::none(),
            )
            .await;
        }));
    }
    (epoch, identity)
}

async fn u6b_l15_handoff(
    fx: &mut K2lgN,
    budget: Option<u32>,
    schema: Option<Value>,
    wait_timeout: Option<u64>,
    stepper: Option<U6TurnStepper>,
    chat_rx: tokio::sync::mpsc::UnboundedReceiver<xai_chat_state::ChatStateEvent>,
) -> K2lgNTurn {
    let stepper = Some(stepper.unwrap_or_default());
    U6_ARM_STEPPERS.with(|steppers| {
        steppers
            .borrow_mut()
            .push(stepper.as_ref().unwrap().clone())
    });
    k2_seed(&mut fx.session, &fx.server).await;
    fx.session.releases[1]
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap()
        .forget();
    let frames = wait_timeout.map_or_else(k2_u5_frames, |timeout| {
        u6_wait_frames(k2_u5_frames(), "client_u6_wait", timeout)
    });
    fx.server
        .enqueue_response("/v1/messages", k2_script(frames));
    fx.session.actor.chat_state_handle.begin_turn_capture();
    let _ = fx.session.actor.chat_state_handle.get_conversation().await;
    let turn_start_ops = fx.log.lock().unwrap().len();
    fx.session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("U6 real mixed handoff"))
        .await;
    let submits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let record = submits.clone();
    let actor = fx.session.actor.clone();
    let domain = fx.session.domain(&fx.server);
    let observations = U6_ARM_OBSERVATIONS.with(Clone::clone);
    let session_turn_active_guard =
        super::turn_task::TurnActiveGuard::activate(Some(&actor.session_turn_active));
    let mut running = tokio::task::spawn_local(async move {
        let _session_turn_active_guard = session_turn_active_guard;
        let mut prepare =
            move |request: &mut ConversationRequest,
                  binding: &xai_chat_state::RequestTransportBinding| {
                assert!(record.borrow().len() < 16, "H6_REQUEST_BUDGET");
                record.borrow_mut().push(json!({
            "items": serde_json::to_value(&request.items).unwrap(),
            "tools": serde_json::to_value(&request.tools).unwrap(),
            "model": request.model,
            "bound_model": binding.config.model,
            "body_digest": request.encoded.as_ref().map(|body| native_content_digest(body.as_bytes())),
        }));
                Ok(())
            };
        let mut salvage = super::length_salvage::LengthSalvage::new(budget);
        let mut sampling = Default::default();
        let future = super::switch_projection::HISTORY_S3_TEST_DOMAIN.scope(
            domain,
            super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
                k2_history::NativeHistoryRequirement::Selected,
                actor.process_conversation_turn_for_native_history_test(
                    KOXD_U6_PROMPT,
                    None,
                    None,
                    schema,
                    &mut salvage,
                    &mut sampling,
                    &mut prepare,
                ),
            ),
        );
        U6_ARM_OBSERVATIONS.scope(observations, async {
            let outcome = U6SteppedTurn {
                future: Box::pin(future),
                stepper,
            }.await;
            if let Err(error) = &outcome {
                u6_observe(json!({
                    "stage": "direct turn error outcome",
                    "error_code": error.data.as_ref().and_then(|data| data.get("code")).and_then(Value::as_str),
                    "data_keys": error.data.as_ref().and_then(Value::as_object).map(|data| data.keys().cloned().collect::<Vec<_>>()),
                    "message_digest": native_content_digest(error.message.as_bytes()),
                    "local_message_digest": error.data.as_ref().and_then(|data| data.get("message")).and_then(Value::as_str).map(|message| native_content_digest(message.as_bytes())),
                }));
            }
            outcome
        }).await
    });
    let (epoch, identity) = u6b_l15_install_running(fx, running.abort_handle(), chat_rx).await;
    let captured = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut running => {
                let outcome = result.unwrap();
                u6_observe(json!({"stage":"real handoff setup rejected before native capture",
                    "error_code":outcome.as_ref().err().and_then(|error| error.data.as_ref())
                        .and_then(|data| data.get("code")).and_then(Value::as_str),
                    "local_message":outcome.as_ref().err().and_then(|error| error.data.as_ref())
                        .and_then(|data| data.get("message")).and_then(Value::as_str)
                        .map(|message| message.chars().take(256).collect::<String>()),
                    "error_message_digest":outcome.as_ref().err().map(|error| native_content_digest(error.message.as_bytes())),
                    "http_count":fx.server.messages_request_count()}));
                false
            },
            status = fx.session.source_acks.recv() => status == Some(k2_history::NativeResponseStatus::Accepted),
        }
    }).await.expect("H6_HANDOFF_CAPTURE_OUTCOME_RACE");
    assert!(captured, "H6_DECODED_NATIVE_HANDOFF");
    let mut entered = u6_client_dispatch(
        &mut fx.session.entered,
        &mut running,
        json!("Accepted"),
        "real handoff client dispatch",
        "H6_HANDOFF_DISPATCH_OUTCOME_RACE",
        "H6_HANDOFF_TURN_ENDED_BEFORE_DISPATCH",
    )
    .await;
    entered.sort();
    assert!(
        entered == vec!["alpha__lookup", "zeta__fetch"],
        "H6_REAL_CLIENT_EXECUTION"
    );
    let conversation = &fx.session.actor.chat_state_handle;
    u6_await_turn_progress(
        &mut running,
        "real handoff first actual client result",
        "H6_FIRST_ACTUAL_RESULT",
        "H6_FIRST_ACTUAL_RESULT_TURN_ENDED",
        || async move {
            conversation.get_conversation().await.iter().any(|item| {
                matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")
            })
        },
    )
    .await;
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_HANDOFF_RELOAD").await;
    assert!(k2lg_n_live(&checkpoint), "H6_REAL_LIVE_PENDING");
    assert_eq!(
        fx.server.messages_request_count(),
        2,
        "H6_ZERO_EARLY_CONTINUATION"
    );
    K2lgNTurn {
        running,
        epoch,
        identity,
        submits,
        turn_start_ops,
    }
}


#[derive(Clone, Copy, Debug)]
enum U6bL15Source { Memory, Mcp }

async fn u6b_l15(source: U6bL15Source) -> Value {
    const HUMAN: &str = "U6b L15 retained human forces the first closed-exchange build";
    let (mut fixture, chat_rx) = u6b_l15_fixture(None, K2lgNOptions::default(), U6PersistenceControl::default()).await;
    let fx = &mut fixture.fx;
    let mut turn = u6b_l15_handoff(fx, None, None, None, None, chat_rx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let initial_counter = fx.session.actor.memory.injection_count.load(std::sync::atomic::Ordering::Relaxed);
    match source {
        U6bL15Source::Memory => {
            let root = fx.session.directory.path().join("l15-memory-v2");
            let storage = crate::session::memory::MemoryStorage::new_for_mode(
                fx.session.actor.tool_context.cwd.as_path(), Some(&root), crate::config::MemoryMode::V2);
            storage.ensure_initialized().expect("H6_L15_MEMORY_STORAGE_INITIALIZED");
            fx.session.actor.memory.storage.replace(Some(storage));
            assert_eq!(fx.session.actor.memory.mode(), Some(crate::config::MemoryMode::V2),
                "H6_L15_MEMORY_REAL_STORAGE_MODE");
        }
        U6bL15Source::Mcp => {
            fx.session.actor.mcp_announcements.lock().fingerprints
                .insert("departed-u6b-l15".into(), (1, 2, 3));
            fx.session.actor.mcp_reminder_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    let retained_history = fx.session.actor.chat_state_handle.get_conversation().await;
    let retained_digest = native_content_digest(&serde_json::to_vec(&retained_history).unwrap());
    fx.session.actor.memory.context_injected.store(false, std::sync::atomic::Ordering::Relaxed);
    let actor = fx.session.actor.clone();
    u6_await_turn_progress(&mut turn.running, "L15 actual ConversationReset event",
        "H6_L15_REAL_RESET_BOUND", "H6_L15_RESET_TURN_ENDED", || {
            let actor = actor.clone();
            async move { !actor.memory.context_injected.load(std::sync::atomic::Ordering::Relaxed) }
        }).await;
    let current = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(native_content_digest(&serde_json::to_vec(&current).unwrap()), retained_digest,
        "H6_L15_RESET_KEEPS_IDENTICAL_HISTORY_BYTES");
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_L15_REAL_REARM_LIVE_VIEW").await;
    assert!(k2lg_n_live(&checkpoint), "H6_L15_REARM_STILL_REAL_HOLD");
    u6_interject(fx, HUMAN, Vec::new()).await;
    let mut first = fx.server.expect_response_blocked("U6b L15 first results-only continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(xai_grok_test_support::InferenceEndpoint::Messages),
        k2_script(u6_next_pending(true)));
    fx.session.releases[1].add_permits(1);
    let first_reached = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = first.wait_blocked() => true,
            _ = &mut turn.running => false,
        }
    }).await.expect("H6_L15_FIRST_CONTINUATION_BOUND");
    assert!(first_reached, "H6_L15_FIRST_RESULTS_ONLY_CONTINUATION");
    let bodies = k2lg_n_bodies(&fx.server);
    u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
    if matches!(source, U6bL15Source::Memory) {
        assert!(!fx.session.actor.memory.context_injected.load(std::sync::atomic::Ordering::Relaxed),
            "H6_L15_MEMORY_GENERATION_WITHHELD");
        assert_eq!(fx.session.actor.memory.injection_count.load(std::sync::atomic::Ordering::Relaxed),
            initial_counter, "H6_L15_MEMORY_COUNTER_WITHHELD");
        assert!(!serde_json::to_string(&bodies[2]).unwrap().contains("## Global memory manifest"),
            "H6_L15_MEMORY_BUILD_ARGUMENT_WITHHELD");
    }
    let mut second = fx.server.expect_response_blocked("U6b L15 ordinary tool-batch while another native call remains pending",
        xai_grok_test_support::InferenceRequestMatcher::foreground(xai_grok_test_support::InferenceEndpoint::Messages),
        k2_script(k2_frames("msg_u6b_l15_closed", vec![k2_result("srvtoolu_u6_next", "alpha__lookup"), (json!({"type":"text","text":"U6b L15 closing reply"}), Vec::new())], "end_turn")));
    fx.server.enqueue_response("/v1/messages", k2_script(k2lg_n_after_frames("msg_u6b_l15_after")));
    first.release();
    let mut entered = u6_client_dispatch(&mut fx.session.entered, &mut turn.running, json!("Accepted"),
        "L15 second real client batch", "H6_L15_SECOND_BATCH_BOUND", "H6_L15_SECOND_BATCH_TURN_ENDED").await;
    entered.sort();
    assert_eq!(entered, vec!["alpha__lookup", "zeta__fetch"], "H6_L15_REAL_SECOND_CLIENT_BATCH");
    fx.session.releases[1].add_permits(1);
    let second_reached = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = second.wait_blocked() => true,
            _ = &mut turn.running => false,
        }
    }).await.expect("H6_L15_SECOND_CONTINUATION_BOUND");
    assert!(second_reached, "H6_L15_SECOND_RESULTS_ONLY_CONTINUATION");
    let still_pending = u6_readable_checkpoint(fx, &mut turn.running, true, "H6_L15_ANOTHER_REAL_PENDING_CALL").await;
    assert!(k2lg_n_live(&still_pending), "H6_L15_STILL_REAL_HOLD");
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4, "H6_L15_HELD_BUILD_COUNT");
    assert!(!fx.session.actor.chat_state_handle.get_conversation().await.iter()
        .any(|item| item.text_content().contains(HUMAN)), "H6_L15_HUMAN_STILL_HELD");
    match source {
        U6bL15Source::Memory => {
            assert!(!fx.session.actor.memory.context_injected.load(std::sync::atomic::Ordering::Relaxed),
                "H6_L15_MEMORY_GENERATION_WITHHELD");
            assert_eq!(fx.session.actor.memory.injection_count.load(std::sync::atomic::Ordering::Relaxed),
                initial_counter, "H6_L15_MEMORY_COUNTER_WITHHELD");
            assert!(!serde_json::to_string(&bodies[3]).unwrap().contains("## Global memory manifest"),
                "H6_L15_MEMORY_BUILD_ARGUMENT_WITHHELD");
        }
        U6bL15Source::Mcp => {
            assert!(fx.session.actor.mcp_reminder_dirty.load(std::sync::atomic::Ordering::Relaxed),
                "H6_L15_MCP_DIRTY_UNCONSUMED");
            assert!(fx.session.actor.mcp_announcements.lock().fingerprints.contains_key("departed-u6b-l15"),
                "H6_L15_MCP_FACTS_NOT_CONSUMED");
            assert!(!fx.session.actor.chat_state_handle.get_conversation().await.iter()
                .any(|item| item.text_content().contains("MCP server disconnected: departed-u6b-l15")),
                "H6_L15_MCP_REMINDER_WITHHELD");
        }
    }
    u6_release_after_checkpoint(fx, &mut turn.running, &second, false, "H6_L15_MATCHING_CLOSURE").await;
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_L15_CLOSED_EXCHANGE_PROGRESS").expect("H6_L15_CLOSED_EXCHANGE_JOIN");
    assert!(outcome.is_ok(), "H6_L15_CLOSED_EXCHANGE_OUTCOME");
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 5, "H6_L15_FIRST_ELIGIBLE_BUILD");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(history.iter().filter(|item| item.text_content().contains(HUMAN)).count(), 1,
        "H6_L15_HUMAN_RELEASE_ONCE");
    match source {
        U6bL15Source::Memory => {
            assert!(fx.session.actor.memory.context_injected.load(std::sync::atomic::Ordering::Relaxed),
                "H6_L15_MEMORY_INJECTED_AFTER_CLOSURE");
            assert!(serde_json::to_string(&bodies[4]).unwrap().contains("## Global memory manifest"),
                "H6_L15_MEMORY_CURRENT_FACTS_IN_FIRST_ELIGIBLE_BUILD");
            let count = fx.session.actor.memory.injection_count.load(std::sync::atomic::Ordering::Relaxed);
            assert!(count > initial_counter, "H6_L15_MEMORY_COUNTER_AFTER_CLOSURE");
            assert!(fx.session.actor.first_turn_memory_reminder().await.is_none(), "H6_L15_MEMORY_GENERATION_ONCE");
            assert_eq!(fx.session.actor.memory.injection_count.load(std::sync::atomic::Ordering::Relaxed), count,
                "H6_L15_MEMORY_GENERATION_ONCE");
        }
        U6bL15Source::Mcp => {
            assert!(!fx.session.actor.mcp_reminder_dirty.load(std::sync::atomic::Ordering::Relaxed),
                "H6_L15_MCP_CONSUMED_AFTER_CLOSURE");
            let expected = xai_grok_tools::implementations::search_tool::fingerprint_servers(
                &fx.session.actor.connected_server_summaries());
            assert_eq!(fx.session.actor.mcp_announcements.lock().fingerprints, expected,
                "H6_L15_MCP_CURRENT_FACTS_AFTER_CLOSURE");
            let reminders = history.iter().filter(|item|
                item.text_content().contains("MCP server disconnected: departed-u6b-l15")).count();
            assert_eq!(reminders, 1, "H6_L15_MCP_REMINDER_ONCE");
            fx.session.actor.maybe_inject_mcp_reminder().await;
            assert_eq!(fx.session.actor.chat_state_handle.get_conversation().await.iter().filter(|item|
                item.text_content().contains("MCP server disconnected: departed-u6b-l15")).count(), reminders,
                "H6_L15_MCP_REMINDER_ONCE");
        }
    }
    json!({"constructed":false,"source":format!("{source:?}"),"rearm":"real ConversationReset from byte-identical compaction",
        "history_digest":retained_digest,"http":bodies.len(),"memory_counter_delta":
        fx.session.actor.memory.injection_count.load(std::sync::atomic::Ordering::Relaxed) - initial_counter})
}

#[derive(Clone, Copy, Debug)]
enum U6bL19Trigger { Cancel, OwnedError }

#[derive(Clone, Copy, Debug)]
enum U6bL19Skills { Late, RefusedResident }

#[derive(Clone, Copy, Debug)]
enum U6bL19Native { Absent, RetainedPrefix }

async fn u6b_l19_skill(actor: &SessionActor, text: &str) {
    actor.apply_skill_update_effects(
        xai_grok_tools::types::skill_discovery_tracker::SkillUpdateEffects {
            system_reminder: Some(text.into()),
            ..Default::default()
        },
    ).await;
}

async fn u6b_l19_running_fence(fx: &K2lgN, turn: &mut K2lgNTurn) {
    let (respond_to, pending) = tokio::sync::oneshot::channel();
    fx.session.actor.notifications.persistence_tx
        .send(PersistenceMsg::FlushAndAck { respond_to }).expect("H6_L19_RUNNING_FENCE_SEND");
    u6b_u66_progress(&mut turn.running, pending, "H6_L19_RUNNING_FENCE")
        .await.expect("H6_L19_RUNNING_FENCE_REPLY").expect("H6_L19_RUNNING_FENCE_OK");
}

async fn u6b_l19_ended_fence(fx: &K2lgN) {
    let (responds_to, reply) = tokio::sync::oneshot::channel();
    fx.loop_tx.send(SessionCommand::GetCurrentModel { responds_to })
        .expect("H6_L19_ENDED_MAILBOX_FENCE_SEND");
    tokio::time::timeout(K0_TIMEOUT, reply).await
        .expect("H6_L19_ENDED_MAILBOX_FENCE_BOUND").expect("H6_L19_ENDED_MAILBOX_FENCE_REPLY");
    let (respond_to, pending) = tokio::sync::oneshot::channel();
    fx.session.actor.notifications.persistence_tx
        .send(PersistenceMsg::FlushAndAck { respond_to }).expect("H6_L19_ENDED_PERSISTENCE_FENCE_SEND");
    tokio::time::timeout(K0_TIMEOUT, pending).await
        .expect("H6_L19_ENDED_PERSISTENCE_FENCE_BOUND")
        .expect("H6_L19_ENDED_PERSISTENCE_FENCE_REPLY").expect("H6_L19_ENDED_PERSISTENCE_FENCE_OK");
}

fn u6b_l19_enqueue_prompt(
    fx: &K2lgN,
    prompt: &str,
    id: &str,
    prompt_mode: PromptMode,
) -> tokio::sync::oneshot::Receiver<crate::session::commands::PromptTurnResult> {
    let (respond_to, response) = tokio::sync::oneshot::channel();
    fx.loop_tx.send(SessionCommand::Prompt {
        prompt_id: id.into(),
        prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(prompt))],
        prompt_mode,
        artifact_upload_ctx: None,
        client_identifier: Some("U6".into()),
        screen_mode: None,
        verbatim: false,
        traceparent: None,
        json_schema: None,
        send_now: false,
        admission: None,
        tool_overrides_update: None,
        respond_to,
        prompt_admitted: None,
        persist_ack: None,
        parsed_prompt_tx: None,
    }).expect("H6_L19_REAL_PROMPT_SEND");
    response
}

async fn u6b_l19_prompt_fence(
    fx: &K2lgN,
    end: &mut tokio::sync::oneshot::Receiver<crate::session::commands::PromptTurnResult>,
) {
    let (respond_to, mut pending) = tokio::sync::oneshot::channel();
    fx.session.actor.notifications.persistence_tx
        .send(PersistenceMsg::FlushAndAck { respond_to }).expect("H6_L19_PROMPT_FENCE_SEND");
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut pending => result.expect("H6_L19_PROMPT_FENCE_REPLY").expect("H6_L19_PROMPT_FENCE_OK"),
            _ = end => panic!("H6_L19_PROMPT_ENDED_DURING_FENCE"),
        }
    }).await.expect("H6_L19_PROMPT_FENCE_BOUND");
}

async fn u6b_l19_release_next(
    fx: &K2lgN,
    late: &str,
    plan: &str,
    control: &U6PersistenceControl,
    native_prefix: Option<&str>,
) -> Value {
    let mut next = fx.server.expect_response_blocked("U6b L19 next admitted prompt",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages),
        k2_script(k2lg_n_after_frames("msg_u6b_l19_next")));
    let mut end = u6b_l19_enqueue_prompt(fx, "U6b L19 next admitted prompt", "u6b-l19-next-owner", PromptMode::Plan);
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = next.wait_blocked() => {},
            _ = &mut end => panic!("H6_L19_NEXT_PROMPT_ENDED_BEFORE_HTTP"),
        }
    }).await.expect("H6_L19_NEXT_PROMPT_HTTP_BOUND");
    let handle = fx.session.actor.state.lock().await.running_task.as_ref()
        .expect("H6_L19_NEXT_RUNNING_OWNER").handle.clone();
    let _stop = K0Abort(handle);
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(history.iter().filter(|item| item.text_content().contains(late)).count(),
        1, "H6_L19_LATE_SKILL_RELEASE_ONCE");
    assert_eq!(history.iter().filter(|item| item.text_content() == plan).count(),
        1, "H6_L19_PLAN_RELEASE_ONCE");
    if let Some(prefix) = native_prefix {
        assert_eq!(history.iter().filter(|item| item.text_content().contains(prefix)).count(),
            1, "H6_L17_NATIVE_PREFIX_RELEASE_ONCE");
        let native_at = history.iter().position(|item| item.text_content().contains(prefix))
            .expect("H6_L17_NATIVE_PREFIX_RELEASE_ONCE");
        let late_at = history.iter().position(|item| item.text_content().contains(late))
            .expect("H6_L19_LATE_SKILL_RELEASE_ONCE");
        assert!(native_at < late_at, "H6_L17_NATIVE_PREFIX_BEFORE_MERGED_BATCH");
        assert!(k2lg_n_resident(&fx.session).is_empty(), "H6_L17_NATIVE_PREFIX_RESIDENT_CLEARED");
    }
    let bodies = k2lg_n_bodies(&fx.server);
    let body = u6_body_text(bodies.last().expect("H6_L19_NEXT_ACTUAL_BODY"));
    assert!(body.contains(late) && body.contains(plan), "H6_L19_NEXT_BODY_CURRENT_FACTS");
    if let Some(prefix) = native_prefix {
        assert!(body.contains(prefix), "H6_L17_NATIVE_PREFIX_IN_NEXT_BODY");
    }
    assert!(fx.session.actor.pending_skill_reminders.lock().is_empty(), "H6_L19_RESIDENT_SKILLS_RELEASED");
    assert!(fx.session.actor.plan_mode.lock().pending_activation_text().is_none(), "H6_L19_PLAN_RELEASED");
    let before = control.announcement_updates.load(std::sync::atomic::Ordering::SeqCst);
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = fx.session.actor.flush_pending_skill_reminders() => {},
            _ = &mut end => panic!("H6_L19_NEXT_TURN_ENDED_DURING_REPEAT"),
        }
    }).await.expect("H6_L19_REPEAT_FLUSH_BOUND");
    u6b_l19_prompt_fence(fx, &mut end).await;
    assert_eq!(control.announcement_updates.load(std::sync::atomic::Ordering::SeqCst), before,
        "H6_L19_REPEAT_NO_DELIVERY_ACK");
    let after = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(after.iter().filter(|item| item.text_content().contains(late)).count(), 1,
        "H6_L19_REPEAT_RELEASE_ONCE");
    if let Some(prefix) = native_prefix {
        assert_eq!(after.iter().filter(|item| item.text_content().contains(prefix)).count(), 1,
            "H6_L17_NATIVE_PREFIX_REPEAT_ONCE");
    }
    json!({"http":bodies.len(),"late_skill_once":true,"plan_once":true,
        "native_prefix_once":native_prefix.is_some()})
}

async fn u6b_l19_hold(trigger: U6bL19Trigger, skills: U6bL19Skills, native: U6bL19Native) -> Value {
    use super::interjection::HeldSource;
    const CUSTODY: &str = "U6b L19 human custody";
    const LATE: &str = "U6b L19 skill after the last held safe point";
    let mut fixture = u6_fixture_controlled(None, K2lgNOptions::default(), U6PersistenceControl {
        plan_tools: true, ..Default::default()
    }).await;
    let control = fixture.persistence_control.clone();
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    assert!(u6_body_text(k2lg_n_bodies(&fx.server).last().expect("H6_L19_INITIAL_REQUEST"))
        .contains("U6 real mixed handoff"), "H6_L19_PROMPT_WAS_SUBMITTED");
    fx.session.actor.mark_front_message_committed().await;
    fx.session.actor.state.lock().await.notifications_suppressed = true;
    u6b_u66_progress(&mut turn.running, u6_interject(fx, CUSTODY, Vec::new()),
        "H6_L19_HUMAN_INGRESS").await;
    if matches!(skills, U6bL19Skills::RefusedResident) {
        for index in 0..129 {
            u6b_u66_progress(&mut turn.running,
                u6b_l19_skill(&fx.session.actor, &format!("U6b L19 resident skill {index:03}")),
                "H6_L19_SKILL_BAND_PRODUCER").await;
        }
    }
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_L19_REAL_HELD_SAFE_POINT");
    if matches!(skills, U6bL19Skills::RefusedResident) {
        k2lg_n_selected(fx, fx.session.actor.flush_skills_at_native_safe_point()).await;
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert_eq!(ledger.entries.iter().filter(|entry| entry.source == HeldSource::SkillReminder).count(),
            128, "H6_L19_SKILL_BAND_HELD_PREFIX");
        let refusal = ledger.refusal.as_ref().expect("H6_L19_REFUSED_SKILL_RESIDENT");
        assert_eq!(format!("{:?}", refusal.reason), "SkillBandFull", "H6_L19_REFUSED_SKILL_REASON");
    }
    let frames = match trigger {
        U6bL19Trigger::Cancel => k2lg_n_closure_frames(),
        U6bL19Trigger::OwnedError => k2_frames("msg_u6b_l19_unknown_terminal",
            vec![(json!({"type":"text","text":"U6b L19 decoder error response"}), Vec::new())],
            "u6b_l19_unknown_terminal"),
    };
    let mut continuation = fx.server.expect_response_blocked("U6b L19 last held request before teardown",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages), k2_script(frames));
    fx.session.releases[1].add_permits(1);
    u6b_u66_progress(&mut turn.running, continuation.wait_blocked(), "H6_L19_LAST_HELD_HTTP").await;
    let checkpoint = u6_readable_checkpoint(fx, &mut turn.running, true, "H6_L19_LAST_HELD_CHECKPOINT").await;
    assert!(k2lg_n_live(&checkpoint), "H6_L19_NATIVE_HOLD_PRECONDITION");
    u6b_u66_progress(&mut turn.running,
        fx.session.actor.handle_session_mode(acp::SessionModeId::new("plan")),
        "H6_L19_REAL_MODE_ON").await;
    let plan = fx.session.actor.plan_mode.lock().pending_activation_text().map(str::to_owned)
        .expect("H6_L19_REAL_PENDING_PLAN");
    u6b_u66_progress(&mut turn.running, u6b_l19_skill(&fx.session.actor, LATE),
        "H6_L19_LATE_SKILL_PRODUCER").await;
    if matches!(native, U6bL19Native::RetainedPrefix) {
        u6b_u66_progress(&mut turn.running, u6b_u67_send_peer(fx, 900, true),
            "H6_L19_NATIVE_REAL_PRODUCER").await;
        u6b_u67_mailbox_barrier(fx, &mut turn).await;
        assert_eq!(k2lg_n_resident(&fx.session).len(), 1, "H6_L19_NATIVE_RESIDENT_PRECONDITION");
    }
    assert_eq!(fx.session.actor.pending_skill_reminders.lock().len(),
        if matches!(skills, U6bL19Skills::RefusedResident) { 2 } else { 1 },
        "H6_L19_LATE_SKILL_STILL_RESIDENT");
    assert!(!fx.session.actor.chat_state_handle.get_conversation().await.iter()
        .any(|item| item.text_content().contains(LATE)), "H6_L19_LATE_SKILL_NOT_YET_MODEL_VISIBLE");
    u6b_l19_running_fence(fx, &mut turn).await;
    let requests_before = fx.server.messages_request_count();
    let announcements_before = control.announcement_updates.load(std::sync::atomic::Ordering::SeqCst);
    let plan_writes_before = control.plan_updates.load(std::sync::atomic::Ordering::SeqCst);
    let mut error_code = None;
    match trigger {
        U6bL19Trigger::Cancel => {
            fx.loop_tx.send(SessionCommand::Cancel(crate::session::CancelOptions {
                user_initiated: true,
                trigger: Some(crate::session::CancelTrigger::CtrlC),
                ..Default::default()
            })).expect("H6_L19_REAL_CANCEL_SEND");
            let result = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
                .expect("H6_L19_REAL_CANCEL_BOUND");
            assert!(result.is_err_and(|error| error.is_cancelled()), "H6_L19_REAL_CANCEL_OUTCOME");
        }
        U6bL19Trigger::OwnedError => {
            continuation.release();
            let result = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
                .expect("H6_L19_REAL_ERROR_BOUND").expect("H6_L19_REAL_ERROR_JOIN");
            let error = match result { Err(error) => error, Ok(_) => panic!("H6_L19_REAL_ERROR_OUTCOME") };
            let encoded = serde_json::to_value(&error).expect("H6_L19_ERROR_ENCODING");
            error_code = encoded.get("data").and_then(|data| data.get("code"))
                .and_then(Value::as_str).map(str::to_owned);
            u6_observe(json!({"stage":"L19 error from actual native turn", "error_code":error_code,
                "message_digest":native_content_digest(&serde_json::to_vec(&encoded).unwrap())}));
            let (processed, acknowledgement) = tokio::sync::oneshot::channel();
            fx.loop_tx.send(SessionCommand::InjectTurnCompletion {
                prompt_id: KOXD_U6_PROMPT.into(),
                epoch: turn.epoch,
                result: Box::new(Err(error)),
                elapsed_ms: Some(0),
                processed,
            }).expect("H6_L19_ACTUAL_ERROR_COMPLETION_SEND");
            tokio::time::timeout(K0_TIMEOUT, acknowledgement).await
                .expect("H6_L19_ERROR_COMPLETION_BOUND").expect("H6_L19_ERROR_COMPLETION_REPLY");
        }
    }
    u6b_l19_ended_fence(fx).await;
    assert_eq!(fx.server.messages_request_count(), requests_before, "H6_L19_NO_TEARDOWN_REQUEST");
    {
        let state = fx.session.actor.state.lock().await;
        assert!(state.running_task.is_none() && state.pending_inputs.is_empty(), "H6_L19_NO_TEARDOWN_RESTART");
        assert!(state.message_delivery.held_input.retaining(), "H6_L19_CUSTODY_RETAINED");
    }
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(!history.iter().any(|item| item.text_content().contains(LATE)), "H6_L19_LATE_SKILL_WITHHELD");
    assert!(fx.session.actor.pending_skill_reminders.lock().iter()
        .any(|item| item.text_content().contains(LATE)), "H6_L19_LATE_SKILL_RETAINED");
    assert!(fx.session.actor.plan_mode.lock().pending_activation_text().as_deref() == Some(plan.as_str()),
        "H6_L19_PLAN_NONCONSUMING_TEARDOWN");
    assert!(!history.iter().any(|item| item.text_content() == plan), "H6_L19_PLAN_WITHHELD");
    assert_eq!(control.announcement_updates.load(std::sync::atomic::Ordering::SeqCst), announcements_before,
        "H6_L19_NO_TEARDOWN_ANNOUNCEMENT_ACK");
    assert_eq!(control.plan_updates.load(std::sync::atomic::Ordering::SeqCst), plan_writes_before,
        "H6_L19_NO_TEARDOWN_PLAN_WRITE");
    if matches!(skills, U6bL19Skills::RefusedResident) {
        assert_eq!(fx.session.actor.pending_skill_reminders.lock().len(), 2,
            "H6_L19_REFUSED_SKILL_STILL_RESIDENT");
    }
    let native_prefix = if matches!(native, U6bL19Native::RetainedPrefix) {
        assert_eq!(k2lg_n_resident(&fx.session).len(), 1, "H6_L19_NATIVE_PREFIX_WITHHELD");
        assert!(!history.iter().any(|item| item.text_content().contains("U6b peer native 900")),
            "H6_L19_NATIVE_PREFIX_NOT_APPENDED");
        Some("U6b peer native 900")
    } else {
        None
    };
    let release = u6b_l19_release_next(fx, LATE, &plan, &control, native_prefix).await;
    json!({"trigger":format!("{trigger:?}"),"skills":format!("{skills:?}"),"actual_error_code":error_code,
        "error_completion_delivery":"existing fixture mailbox command carrying actual turn error",
        "ordinary_mode_writes_before_teardown":plan_writes_before,"teardown_announcement_delta":0,
        "teardown_plan_write_delta":0,"next_release":release,"constructed_native_release":false,
        "native_release_and_grammar": if native_prefix.is_some() { "RELEASED_ONCE" } else { "ABSENT" },
        "L16_credit": false,
        "L16_status": "UNSCHEDULABLE_GAP"})
}

async fn u6b_l19_unheld(trigger: U6bL19Trigger) -> Value {
    const LATE: &str = "U6b L19 ordinary unheld skill";
    let mut fixture = u6_fixture_controlled(None, K2lgNOptions::default(), U6PersistenceControl {
        plan_tools: true, ..Default::default()
    }).await;
    let control = fixture.persistence_control.clone();
    let fx = &mut fixture.fx;
    if let Some(command_rx) = fx.loop_rx.take() {
        let (chat_tx, chat_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let actor = fx.session.actor.clone();
        fx.session.tasks.push(tokio::task::spawn_local(async move {
            let _keep = (chat_tx, event_tx);
            super::run_session(
                actor,
                command_rx,
                chat_rx,
                event_rx,
                None,
                Arc::new(parking_lot::Mutex::new(
                    xai_grok_workspace::file_system::CodebaseIndexManager::new(),
                )),
                std::env::temp_dir(),
                crate::session::fs_watch::FsWatchCapabilities::none(),
            )
            .await;
        }));
    }
    fx.session.actor.state.lock().await.notifications_suppressed = true;
    let frames = match trigger {
        U6bL19Trigger::Cancel => k2lg_n_after_frames("msg_u6b_l19_unheld"),
        U6bL19Trigger::OwnedError => k2_frames("msg_u6b_l19_unheld_error",
            vec![(json!({"type":"text","text":"U6b L19 ordinary error"}), Vec::new())],
            "u6b_l19_unknown_terminal"),
    };
    let mut response = fx.server.expect_response_blocked("U6b L19 ordinary unheld request",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages), k2_script(frames));
    let mut end = u6b_l19_enqueue_prompt(fx, "U6b L19 ordinary unheld prompt", "u6b-l19-unheld-owner", PromptMode::Agent);
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = response.wait_blocked() => {},
            _ = &mut end => panic!("H6_L19_UNHELD_TURN_ENDED_BEFORE_REQUEST"),
        }
    }).await.expect("H6_L19_UNHELD_HTTP_BOUND");
    let handle = fx.session.actor.state.lock().await.running_task.as_ref()
        .expect("H6_L19_UNHELD_RUNNING_OWNER").handle.clone();
    let _stop = K0Abort(handle);
    assert!(!fx.session.actor.state.lock().await.message_delivery.held_input.retaining(),
        "H6_L19_UNHELD_NO_CUSTODY");
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = fx.session.actor.handle_session_mode(acp::SessionModeId::new("plan")) => {},
            _ = &mut end => panic!("H6_L19_UNHELD_TURN_ENDED_DURING_MODE"),
        }
    }).await.expect("H6_L19_UNHELD_MODE_BOUND");
    let plan = fx.session.actor.plan_mode.lock().pending_activation_text().map(str::to_owned)
        .expect("H6_L19_UNHELD_REAL_PENDING_PLAN");
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = u6b_l19_skill(&fx.session.actor, LATE) => {},
            _ = &mut end => panic!("H6_L19_UNHELD_TURN_ENDED_DURING_SKILL"),
        }
    }).await.expect("H6_L19_UNHELD_SKILL_BOUND");
    u6b_l19_prompt_fence(fx, &mut end).await;
    let announcements_before = control.announcement_updates.load(std::sync::atomic::Ordering::SeqCst);
    let plan_before = control.plan_updates.load(std::sync::atomic::Ordering::SeqCst);
    match trigger {
        U6bL19Trigger::Cancel => fx.loop_tx.send(SessionCommand::Cancel(crate::session::CancelOptions {
            user_initiated: true,
            trigger: Some(crate::session::CancelTrigger::CtrlC),
            ..Default::default()
        })).expect("H6_L19_UNHELD_CANCEL_SEND"),
        U6bL19Trigger::OwnedError => { response.release(); }
    }
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut end).await
        .expect("H6_L19_UNHELD_END_BOUND").expect("H6_L19_UNHELD_END_REPLY");
    if matches!(trigger, U6bL19Trigger::OwnedError) {
        assert!(outcome.is_err(), "H6_L19_UNHELD_ACTUAL_ERROR");
    }
    u6b_l19_ended_fence(fx).await;
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(history.iter().filter(|item| item.text_content().contains(LATE)).count(), 1,
        "H6_L19_UNHELD_SKILL_FLUSH_UNCHANGED");
    assert_eq!(history.iter().filter(|item| item.text_content() == plan).count(), 1,
        "H6_L19_UNHELD_PLAN_FLUSH_UNCHANGED");
    assert!(fx.session.actor.pending_skill_reminders.lock().is_empty(), "H6_L19_UNHELD_SKILLS_CONSUMED");
    assert!(fx.session.actor.plan_mode.lock().pending_activation_text().is_none(), "H6_L19_UNHELD_PLAN_CONSUMED");
    assert!(fx.session.actor.state.lock().await.running_task.is_none(), "H6_L19_UNHELD_OWNER_ENDED");
    json!({"constructed":false,"trigger":format!("{trigger:?}"),"real_prompt_command":true,
        "real_turn_completion":true,"response_ok":outcome.is_ok(),
        "announcement_delta":control.announcement_updates.load(std::sync::atomic::Ordering::SeqCst) - announcements_before,
        "plan_delta":control.plan_updates.load(std::sync::atomic::Ordering::SeqCst) - plan_before})
}

#[derive(Clone, Copy, Debug)]
enum U6bL20Activation { Withdraw, Reenable, ApprovalWithdraw }

#[derive(Clone, Copy, Debug)]
enum U6bL20Completion { Closure, Cancel, OwnedError }

async fn u6b_l20(activation: U6bL20Activation, completion: U6bL20Completion) -> Value {
    use super::interjection::HeldSource;
    use crate::session::plan_mode::PlanModeState;
    use xai_grok_tools::types::SessionMode;
    const HUMAN: &str = "U6b L20 human retained through real plan revocation";
    let mut fixture = u6_fixture_controlled(None, K2lgNOptions::default(), U6PersistenceControl {
        plan_tools: true, ..Default::default()
    }).await;
    let control = fixture.persistence_control.clone();
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    fx.session.actor.mark_front_message_committed().await;
    fx.session.actor.state.lock().await.notifications_suppressed = true;
    let prior = fx.session.actor.plan_mode.lock().snapshot();
    assert_eq!(prior.state, PlanModeState::Inactive, "H6_L20_PRIOR_INACTIVE");
    let checkpoint = u6_readable_checkpoint(fx, &mut turn.running, true,
        "H6_L20_NATIVE_HOLD_PRECONDITION").await;
    assert!(k2lg_n_live(&checkpoint), "H6_L20_REAL_LATCH_PRECONDITION");
    u6b_u66_progress(&mut turn.running, u6_interject(fx, HUMAN, Vec::new()),
        "H6_L20_REAL_HUMAN_INGRESS").await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_L20_HUMAN_HELD");
    let before_on = control.plan_updates.load(std::sync::atomic::Ordering::SeqCst);
    u6b_u66_progress(&mut turn.running,
        fx.session.actor.handle_session_mode(acp::SessionModeId::new(SessionMode::Plan.as_id())),
        "H6_L20_REAL_MODE_ON").await;
    u6b_l19_running_fence(fx, &mut turn).await;
    let after_on = control.plan_updates.load(std::sync::atomic::Ordering::SeqCst);
    // Pending and mid-turn activation are two ordinary mode writes.
    assert_eq!(after_on - before_on, 2, "H6_L20_ORDINARY_ON_WRITES_ATTRIBUTED");
    let original_plan = fx.session.actor.plan_mode.lock().pending_activation_text()
        .map(str::to_owned).expect("H6_L20_REAL_BUFFERED_PLAN");
    u6b_u66_progress(&mut turn.running,
        k2lg_n_selected(fx, fx.session.actor.flush_skills_at_native_safe_point()),
        "H6_L20_SELECTED_HELD_PLAN_OBSERVATION").await;
    u6b_l19_running_fence(fx, &mut turn).await;
    let after_hold = control.plan_updates.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(after_hold, after_on, "H6_L20_NO_HOLD_PLAN_WRITE");
    {
        let state = fx.session.actor.state.lock().await;
        assert_eq!(state.message_delivery.held_input.entries.iter()
            .filter(|entry| entry.source == HeldSource::PlanActivation).count(), 1,
            "H6_L20_PLAN_IN_HELD_SOURCE");
    }
    let before_off = if matches!(activation, U6bL20Activation::ApprovalWithdraw) {
        let result = u6b_u66_progress(&mut turn.running,
            fx.session.actor.request_plan_approval(&acp::ToolCallId::new("u6b-l20-approval"),
                Some("# U6b L20 approval revocation".into())),
            "H6_L20_REAL_APPROVAL_REQUEST").await;
        assert!(result.is_err(), "H6_L20_APPROVAL_DISCONNECT_OUTCOME");
        assert!(fx.session.actor.plan_mode.lock().is_awaiting_plan_approval(),
            "H6_L20_REAL_APPROVAL_PARKED");
        u6b_l19_running_fence(fx, &mut turn).await;
        let after_approval = control.plan_updates.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(after_approval - after_hold, 1,
            "H6_L20_ORDINARY_APPROVAL_WRITE_ATTRIBUTED");
        after_approval
    } else { after_hold };
    u6b_u66_progress(&mut turn.running,
        fx.session.actor.handle_session_mode(acp::SessionModeId::new(SessionMode::Default.as_id())),
        "H6_L20_REAL_MODE_OFF").await;
    u6b_l19_running_fence(fx, &mut turn).await;
    let after_off = control.plan_updates.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(after_off - before_off, 1, "H6_L20_ORDINARY_OFF_WRITE_ATTRIBUTED");
    let revoked = fx.session.actor.plan_mode.lock().snapshot();
    assert_eq!(serde_json::to_value(&revoked).unwrap(), serde_json::to_value(&prior).unwrap(),
        "H6_L20_REVOKED_PRIOR_STATE");
    assert!(fx.session.actor.plan_mode.lock().pending_activation_text().is_none(),
        "H6_L20_REVOKED_ACTIVATION_ABSENT");
    let (live_plan, after_modes) = if matches!(activation, U6bL20Activation::Reenable) {
        u6b_u66_progress(&mut turn.running,
            fx.session.actor.handle_session_mode(acp::SessionModeId::new(SessionMode::Plan.as_id())),
            "H6_L20_REAL_MODE_REENABLE").await;
        u6b_l19_running_fence(fx, &mut turn).await;
        let after_reenable = control.plan_updates.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(after_reenable - after_off, 2, "H6_L20_ORDINARY_REENABLE_WRITES_ATTRIBUTED");
        let current_plan = fx.session.actor.plan_mode.lock().pending_activation_text()
            .map(str::to_owned).expect("H6_L20_REENABLED_LIVE_PLAN");
        u6b_u66_progress(&mut turn.running,
            k2lg_n_selected(fx, fx.session.actor.flush_skills_at_native_safe_point()),
            "H6_L20_SELECTED_REENABLED_OBSERVATION").await;
        u6b_l19_running_fence(fx, &mut turn).await;
        assert_eq!(control.plan_updates.load(std::sync::atomic::Ordering::SeqCst), after_reenable,
            "H6_L20_REENABLE_NO_HOLD_PLAN_WRITE");
        (Some(current_plan), after_reenable)
    } else { (None, after_off) };
    let frames = match completion {
        U6bL20Completion::Closure | U6bL20Completion::Cancel => k2lg_n_closure_frames(),
        U6bL20Completion::OwnedError => k2_frames("msg_u6b_l20_unknown_terminal",
            vec![(json!({"type":"text","text":"U6b L20 actual owned error"}), Vec::new())],
            "u6b_l20_unknown_terminal"),
    };
    let mut continuation = fx.server.expect_response_blocked("U6b L20 held request after mode revocation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages), k2_script(frames));
    fx.session.releases[1].add_permits(1);
    u6b_u66_progress(&mut turn.running, continuation.wait_blocked(),
        "H6_L20_HELD_REQUEST_REACHED").await;
    let pending = u6_readable_checkpoint(fx, &mut turn.running, true,
        "H6_L20_HELD_REQUEST_READABLE").await;
    assert!(k2lg_n_live(&pending), "H6_L20_HELD_REQUEST_LIVE");
    let held_bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(held_bodies.len(), 3, "H6_L20_HELD_REQUEST_COUNT");
    u6_assert_results_only(&checkpoint, &held_bodies[1], &held_bodies[2]);
    assert!(!u6_body_text(&held_bodies[2]).contains(&original_plan), "H6_L20_REVOKED_PLAN_WITHHELD");
    let mut actual_error_code = None;
    match completion {
        U6bL20Completion::Closure => {
            fx.server.enqueue_response("/v1/messages", k2_script(k2lg_n_after_frames("msg_u6b_l20_after")));
            u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
                "H6_L20_MATCHING_CLOSED_CHECKPOINT").await;
            let result = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
                .expect("H6_L20_CLOSED_EXCHANGE_BOUND").expect("H6_L20_CLOSED_EXCHANGE_JOIN");
            assert!(result.is_ok(), "H6_L20_CLOSED_EXCHANGE_OUTCOME");
        }
        U6bL20Completion::Cancel => {
            fx.loop_tx.send(SessionCommand::Cancel(crate::session::CancelOptions {
                user_initiated: true,
                trigger: Some(crate::session::CancelTrigger::CtrlC),
                ..Default::default()
            })).expect("H6_L20_REAL_CANCEL_SEND");
            let result = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
                .expect("H6_L20_CANCEL_BOUND");
            assert!(result.is_err_and(|error| error.is_cancelled()), "H6_L20_REAL_CANCEL_OUTCOME");
        }
        U6bL20Completion::OwnedError => {
            continuation.release();
            let result = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
                .expect("H6_L20_OWNED_ERROR_BOUND").expect("H6_L20_OWNED_ERROR_JOIN");
            let error = match result { Err(error) => error, Ok(_) => panic!("H6_L20_ACTUAL_OWNED_ERROR") };
            actual_error_code = error.data.as_ref().and_then(|data| data.get("code"))
                .and_then(Value::as_str).map(str::to_owned);
            let (processed, acknowledgement) = tokio::sync::oneshot::channel();
            fx.loop_tx.send(SessionCommand::InjectTurnCompletion {
                prompt_id: KOXD_U6_PROMPT.into(), epoch: turn.epoch,
                result: Box::new(Err(error)), elapsed_ms: Some(0), processed,
            }).expect("H6_L20_ACTUAL_ERROR_COMPLETION_SEND");
            tokio::time::timeout(K0_TIMEOUT, acknowledgement).await
                .expect("H6_L20_ERROR_COMPLETION_BOUND").expect("H6_L20_ERROR_COMPLETION_REPLY");
        }
    }
    u6b_l19_ended_fence(fx).await;
    if !matches!(completion, U6bL20Completion::Closure) {
        assert_eq!(control.plan_updates.load(std::sync::atomic::Ordering::SeqCst), after_modes,
            "H6_L20_NO_TEARDOWN_PLAN_WRITE");
        assert_eq!(fx.server.messages_request_count(), 3, "H6_L20_NO_TEARDOWN_REQUEST");
        assert!(!fx.session.actor.chat_state_handle.get_conversation().await.iter()
            .any(|item| item.text_content() == original_plan), "H6_L20_NO_TEARDOWN_REVOKED_TEXT");
        let mut next = fx.server.expect_response_blocked("U6b L20 next eligible admitted turn",
            xai_grok_test_support::InferenceRequestMatcher::foreground(
                xai_grok_test_support::InferenceEndpoint::Messages),
            k2_script(k2lg_n_after_frames("msg_u6b_l20_next")));
        let mut end = u6b_l19_enqueue_prompt(fx, "U6b L20 next admitted prompt",
            "u6b-l20-next-owner", PromptMode::Agent);
        tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                _ = next.wait_blocked() => {},
                _ = &mut end => panic!("H6_L20_NEXT_TURN_ENDED_BEFORE_REQUEST"),
            }
        }).await.expect("H6_L20_NEXT_REQUEST_BOUND");
        next.release();
        let result = tokio::time::timeout(K0_TIMEOUT, &mut end).await
            .expect("H6_L20_NEXT_TURN_BOUND").expect("H6_L20_NEXT_TURN_REPLY");
        assert!(result.is_ok(), "H6_L20_NEXT_TURN_OUTCOME");
        u6b_l19_ended_fence(fx).await;
    }
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4, "H6_L20_FIRST_ELIGIBLE_REQUEST_COUNT");
    let last_text = u6_body_text(bodies.last().expect("H6_L20_FINAL_BODY"));
    assert_eq!(last_text.matches(HUMAN).count(), 1, "H6_L20_HUMAN_RELEASE_ONCE");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(history.iter().filter(|item| item.text_content().contains(HUMAN)).count(), 1,
        "H6_L20_HUMAN_HISTORY_ONCE");
    let after_release = control.plan_updates.load(std::sync::atomic::Ordering::SeqCst);
    if let Some(current_plan) = &live_plan {
        assert_eq!(history.iter().filter(|item| item.text_content() == current_plan.as_str()).count(), 1,
            "H6_L20_REENABLED_TEXT_RELEASED_ONCE");
        assert_eq!(last_text.matches(current_plan.as_str()).count(), 1,
            "H6_L20_REENABLED_BODY_ONCE");
        assert!(fx.session.actor.plan_mode.lock().pending_activation_text().is_none(),
            "H6_L20_REENABLED_CONSUMED_AFTER_SUBMIT");
    } else {
        assert_eq!(after_release, after_modes, "H6_L20_NO_HOLD_RELEASE_PLAN_WRITE");
        assert_eq!(history.iter().filter(|item| item.text_content() == original_plan).count(), 0,
            "H6_L20_REVOKED_TEXT_NOT_DELIVERED");
        assert_eq!(last_text.matches(original_plan.as_str()).count(), 0,
            "H6_L20_REVOKED_BODY_ABSENT");
        let final_snapshot = fx.session.actor.plan_mode.lock().snapshot();
        assert_eq!(final_snapshot.reminder_count, prior.reminder_count,
            "H6_L20_REVOKED_REMINDER_COUNT_UNCHANGED");
        assert_eq!(serde_json::to_value(&final_snapshot).unwrap(), serde_json::to_value(&prior).unwrap(),
            "H6_L20_REVOKED_STATE_STAYS_PRIOR");
    }
    json!({"activation":format!("{activation:?}"),"completion":format!("{completion:?}"),
        "ordinary_on_writes":after_on-before_on,"hold_writes":after_hold-after_on,
        "ordinary_approval_writes":before_off-after_hold,"ordinary_off_writes":after_off-before_off,
        "ordinary_reenable_writes":after_modes-after_off,"release_plan_writes":after_release-after_modes,
        "raw_no_hold_totals_compared":false,"http":bodies.len(),"actual_error_code":actual_error_code,
        "reenabled":live_plan.is_some(),"source_attribution":"session_mode.rs:50-52,318,102-103; tool_calls.rs:2143-2144"})
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum U6bL21Writer {
    MediaResample,
    ToolCallLength,
    ToolFreeLength,
}

impl U6bL21Writer {
    fn needle(self) -> &'static str {
        match self {
            Self::MediaResample => "the max limit for `image_gen` is",
            Self::ToolCallLength => super::sampler_turn::OUTPUT_TOKEN_LIMIT_REMINDER,
            Self::ToolFreeLength => super::length_salvage::LENGTH_CONTINUE_REMINDER_BODY,
        }
    }

    fn held_source(self) -> &'static str {
        match self {
            Self::MediaResample => "MediaResample",
            Self::ToolCallLength => "LengthReminder",
            Self::ToolFreeLength => "ToolFreeLength",
        }
    }

    fn blocks(self) -> Vec<(Value, Vec<Value>)> {
        match self {
            Self::MediaResample => (0..2 * xai_grok_tools::media_gen_limits::DEFAULT_MAX_PARALLEL_IMAGE_GEN)
                .map(|index| (json!({"type":"tool_use","id":format!("client_u6b_l21_media_{index}"),
                    "name":"image_gen","input":{"prompt":"L21 real over-cap writer; never executed"}}), Vec::new()))
                .collect(),
            Self::ToolCallLength => vec![(json!({"type":"tool_use","id":"client_u6b_l21_length",
                "name":"alpha__lookup","input":{"value":"L21 real Length tool result"}}), Vec::new())],
            Self::ToolFreeLength => vec![(json!({"type":"text","text":"L21 real truncated answer"}), Vec::new())],
        }
    }

    fn stop(self) -> &'static str {
        match self {
            Self::MediaResample => "tool_use",
            Self::ToolCallLength | Self::ToolFreeLength => "max_tokens",
        }
    }
}

fn u6b_l21_media_guard(actor: &SessionActor) {
    let count = 2 * xai_grok_tools::media_gen_limits::DEFAULT_MAX_PARALLEL_IMAGE_GEN;
    let over = xai_grok_tools::media_gen_limits::over_cap_by_name(
        (0..count).map(|_| ("image_gen", actor.agent.borrow().tool_bridge().tool_kind("image_gen"))),
        &actor.rebuild_spec.media_gen_batch_limits,
    );
    assert!(xai_grok_tools::media_gen_limits::should_resample_egregious(&over, 0, 1),
        "H6_L21_MEDIA_RESAMPLE_PRECONDITION");
}

fn u6b_l21_user_texts(body: &Value) -> Vec<&str> {
    body["messages"].as_array().into_iter().flatten()
        .filter(|message| message["role"] == "user")
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str())
        .collect()
}

fn u6b_l21_writer_count(body: &Value, writer: U6bL21Writer) -> usize {
    u6b_l21_user_texts(body).iter().filter(|text| text.contains(writer.needle())).count()
}

async fn u6b_l21_held(writer: U6bL21Writer) -> Value {
    const HUMAN: &str = "U6b L21 human precedes the held real writer";
    let schema = json!({"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false});
    let mut fixture = u6_fixture_with_bookkeeping(None, K2lgNOptions::default(), false, writer == U6bL21Writer::MediaResample).await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff_with_salvage(fx, Some(1), Some(schema), None, None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let (checkpoint, _) = u6b_u66_progress(&mut turn.running,
        u6_quiescent_reload(fx, "H6_L21_INITIAL_CHECKPOINT_READ"), "H6_L21_INITIAL_CHECKPOINT_OUTCOME").await;
    assert!(k2lg_n_live(&checkpoint), "H6_L21_REAL_PENDING_PRECONDITION");
    assert!(k2lg_n_selected(fx, fx.session.actor.native_inputs_held()).await, "H6_L21_REAL_LATCH_PRECONDITION");
    u6_interject(fx, HUMAN, Vec::new()).await;
    let matcher = xai_grok_test_support::InferenceRequestMatcher::foreground(xai_grok_test_support::InferenceEndpoint::Messages);
    let mut produced = fx.server.expect_response_blocked("L21 actual production writer", matcher,
        k2_script(k2_frames("msg_u6b_l21_writer", writer.blocks(), writer.stop())));
    let mut closure = fx.server.expect_response_blocked("L21 pending continuation and later matching closure", matcher,
        k2_script(k2_frames("msg_u6b_l21_closed", vec![k2_result("srvtoolu_k2_pending", "alpha__lookup"),
            (json!({"type":"tool_use","id":"client_u6b_l21_invalid","name":"StructuredOutput","input":{"invalid":7}}), Vec::new())], "tool_use")));
    let mut after = fx.server.expect_response_blocked("L21 first eligible post-release request", matcher,
        k2_script(k2_frames("msg_u6b_l21_final", vec![(json!({"type":"tool_use","id":"client_u6b_l21_valid",
            "name":"StructuredOutput","input":{"answer":"L21 complete"}}), Vec::new())], "tool_use")));
    fx.session.releases[1].add_permits(1);
    u6b_u66_progress(&mut turn.running, produced.wait_blocked(), "H6_L21_WRITER_REQUEST_REACHED").await;
    let before = k2lg_n_bodies(&fx.server);
    assert_eq!(before.len(), 3, "H6_L21_WRITER_REQUEST_COUNT");
    assert_eq!(u6b_l21_writer_count(&before[2], writer), 0, "H6_L21_WRITER_NOT_PREGENERATED");
    if writer == U6bL21Writer::MediaResample { u6b_l21_media_guard(&fx.session.actor); }
    produced.release();
    if writer == U6bL21Writer::ToolFreeLength {
        let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
            .expect("H6_L21_TFL_TURN_END_BOUND").expect("H6_L21_TFL_TURN_JOIN");
        let error = match outcome {
            Err(error) => error,
            Ok(_) => panic!("H6_L21_TFL_REFUSED_OUTCOME"),
        };
        let code = error.data.as_ref().and_then(|data| data.get("code")).and_then(Value::as_str);
        assert_eq!(code, Some("native_history_compound_refused"), "H6_L21_TFL_COMPOUND_REFUSED");
        let bodies = k2lg_n_bodies(&fx.server);
        assert_eq!(bodies.len(), 3, "H6_L21_TFL_EXACTLY_THREE_REQUESTS");
        let no_writer_request = bodies.iter().all(|body| !body.to_string().contains(writer.needle()));
        assert!(no_writer_request, "H6_L21_WRITER_WITHHELD");
        let no_human_request = bodies.iter().all(|body| !body.to_string().contains(HUMAN));
        assert!(no_human_request, "H6_L21_HUMAN_WITHHELD");
        let custody_reminder_sources = {
            let state = fx.session.actor.state.lock().await;
            state.message_delivery.held_input.entries.iter().filter_map(|entry| {
                let body_has_reminder = match &entry.body {
                    super::interjection::HeldBody::Human(raw) => raw.text.contains(writer.needle()),
                    super::interjection::HeldBody::Owned(item) => item.text_content().contains(writer.needle()),
                    super::interjection::HeldBody::Parent(id) => id.contains(writer.needle()),
                    super::interjection::HeldBody::Monitor | super::interjection::HeldBody::Plan => false,
                };
                (body_has_reminder || entry.meta.to_string().contains(writer.needle()))
                    .then(|| format!("{:?}", entry.source))
            }).collect::<Vec<_>>()
        };
        assert_eq!(custody_reminder_sources.len(), 1, "H6_L21_WRITER_HELD_ONCE");
        assert_eq!(custody_reminder_sources[0], "ToolFreeLength", "H6_L21_WRITER_OWN_SOURCE");
        return json!({"custody_arm":"R51 held tool-free Length", "error_code":code,
            "refusal_reason":error.message.to_string(), "request_count":bodies.len(),
            "no_writer_request":no_writer_request,"no_human_request":no_human_request,
            "custody_reminder_count":custody_reminder_sources.len(),"custody_reminder_sources":custody_reminder_sources,
            "retired_legs":"continuation, closure, release: NOT_REPRESENTABLE_AT_BASE / NOT_RUN / NOT_CREDITED",
            "counterfactual_kind":"M4-writer-bypasses-hold tool-free Length application"});
    }
    u6b_u66_progress(&mut turn.running, closure.wait_blocked(), "H6_L21_WRITER_CONTINUATION_SEND").await;
    let continuation = k2lg_n_bodies(&fx.server);
    assert_eq!(continuation.len(), 4, "H6_L21_CONTINUATION_REQUEST_COUNT");
    assert_eq!(u6b_l21_writer_count(&continuation[3], writer), 0, "H6_L21_WRITER_WITHHELD");
    assert!(!u6b_l21_user_texts(&continuation[3]).iter().any(|text| text.contains(HUMAN)), "H6_L21_HUMAN_WITHHELD");
    let held_writer = {
        let state = fx.session.actor.state.lock().await;
        let entries: Vec<_> = state.message_delivery.held_input.entries.iter().filter_map(|entry| {
            match &entry.body {
                super::interjection::HeldBody::Owned(item) if item.text_content().contains(writer.needle()) =>
                    Some((entry.key, format!("{:?}", entry.source))),
                _ => None,
            }
        }).collect();
        assert_eq!(entries.len(), 1, "H6_L21_WRITER_HELD_ONCE");
        assert_eq!(entries[0].1, writer.held_source(), "H6_L21_WRITER_OWN_SOURCE");
        entries[0].0
    };
    let native_before_close = k2lg_n_selected(fx, fx.session.actor.chat_state_handle.read_native_history()).await
        .expect("H6_L21_CONTINUATION_NATIVE_READ");
    assert!(native_before_close.checkpoint.is_some(), "H6_L21_CONTINUATION_CHECKPOINT");
    closure.release();
    u6b_u66_progress(&mut turn.running, after.wait_blocked(), "H6_L21_RELEASE_REQUEST_REACHED").await;
    let released = k2lg_n_bodies(&fx.server);
    assert_eq!(released.len(), 5, "H6_L21_RELEASE_REQUEST_COUNT");
    assert_eq!(u6b_l21_writer_count(&released[4], writer), 1, "H6_L21_WRITER_RELEASE_ONCE");
    let texts = u6b_l21_user_texts(&released[4]);
    let human_at = texts.iter().position(|text| text.contains(HUMAN)).expect("H6_L21_HUMAN_RELEASE_ONCE");
    let writer_at = texts.iter().position(|text| text.contains(writer.needle())).expect("H6_L21_WRITER_RELEASE_ONCE");
    assert!(human_at < writer_at, "H6_L21_RELEASE_KEY_ORDER");
    assert_eq!(texts.iter().filter(|text| text.contains(HUMAN)).count(), 1, "H6_L21_HUMAN_RELEASE_ONCE");
    assert!(fx.session.actor.state.lock().await.message_delivery.held_input.entries.is_empty(), "H6_L21_RELEASE_EMPTIES_CUSTODY");
    if writer == U6bL21Writer::MediaResample {
        assert!(fx.session.entered.try_recv().is_err(), "H6_L21_MEDIA_NEVER_EXECUTED");
    }
    after.release();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await.expect("H6_L21_FINAL_JOIN_BOUND")
        .expect("H6_L21_FINAL_JOIN");
    assert!(outcome.is_ok(), "H6_L21_FINAL_OUTCOME");
    let (closed, _) = u6_quiescent_reload(fx, "H6_L21_CLOSED_CHECKPOINT_READ").await;
    assert!(!k2lg_n_live(&closed), "H6_L21_CLOSED_EXCHANGE_PRECONDITION");
    json!({"writer":format!("{writer:?}"),"held":true,"http":released.len(),"writer_key":held_writer.as_u64(),
        "real_writer_site":match writer { U6bL21Writer::MediaResample => "turn.rs:3526", U6bL21Writer::ToolCallLength => "turn.rs:3686-3694", U6bL21Writer::ToolFreeLength => "turn.rs:3815-3820" },
        "continuation_writer_count":0,"released_writer_count":u6b_l21_writer_count(&released[4],writer)})
}

async fn u6b_l21_unheld(writer: U6bL21Writer) -> Value {
    let mut fixture = u6_fixture_with_bookkeeping(None, K2lgNOptions::default(), false, writer == U6bL21Writer::MediaResample).await;
    let fx = &mut fixture.fx;
    let matcher = xai_grok_test_support::InferenceRequestMatcher::foreground(xai_grok_test_support::InferenceEndpoint::Messages);
    let mut produced = fx.server.expect_response_blocked("L21 unheld real writer", matcher,
        k2_script(k2_frames("msg_u6b_l21_unheld_writer", writer.blocks(), writer.stop())));
    let mut next = fx.server.expect_response_blocked("L21 unheld writer remains model-visible", matcher,
        k2_script(k2_frames("msg_u6b_l21_unheld_final", vec![(json!({"type":"text","text":"L21 unheld complete"}), Vec::new())], "end_turn")));
    let actor = fx.session.actor.clone();
    actor.chat_state_handle.begin_turn_capture();
    actor.chat_state_handle.get_conversation().await;
    actor.chat_state_handle.push_user_message_and_ack(ConversationItem::user("U6b L21 unheld prompt")).await;
    let domain = fx.session.domain(&fx.server);
    let observed = U6_ARM_OBSERVATIONS.with(Clone::clone);
    let counters = std::rc::Rc::new(std::cell::RefCell::new(None));
    let counters_out = counters.clone();
    let active = super::turn_task::TurnActiveGuard::activate(Some(&actor.session_turn_active));
    let mut running = tokio::task::spawn_local(async move {
        let _active = active;
        let mut salvage = super::length_salvage::LengthSalvage::new(Some(1));
        let mut sampling = Default::default();
        let mut prepare = |_: &mut ConversationRequest, _: &xai_chat_state::RequestTransportBinding| Ok(());
        U6_ARM_OBSERVATIONS.scope(observed, async {
            let outcome = super::switch_projection::HISTORY_S3_TEST_DOMAIN.scope(domain,
                super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(k2_history::NativeHistoryRequirement::Inactive,
                    actor.process_conversation_turn_for_native_history_test("U6b L21 unheld prompt", None, None, None,
                        &mut salvage, &mut sampling, &mut prepare))).await;
            *counters_out.borrow_mut() = Some(json!({"continues":salvage.continues(),"budget":salvage.budget(),"escalated":salvage.escalated()}));
            if let Err(error) = &outcome {
                u6_observe(json!({"stage":"direct turn error outcome","error_code":error.data.as_ref()
                    .and_then(|data| data.get("code")).and_then(Value::as_str)}));
            }
            outcome
        }).await
    });
    let _stop = K0Abort(running.abort_handle());
    u6_install_running(fx, running.abort_handle()).await;
    u6b_u66_progress(&mut running, produced.wait_blocked(), "H6_L21_UNHELD_WRITER_REQUEST").await;
    assert_eq!(fx.server.messages_request_count(), 1, "H6_L21_UNHELD_INITIAL_COUNT");
    assert!(fx.session.actor.state.lock().await.message_delivery.held_input.entries.is_empty(), "H6_L21_UNHELD_NO_CUSTODY");
    if writer == U6bL21Writer::MediaResample { u6b_l21_media_guard(&fx.session.actor); }
    produced.release();
    u6b_u66_progress(&mut running, next.wait_blocked(), "H6_L21_UNHELD_NEXT_REQUEST").await;
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 2, "H6_L21_UNHELD_RETRY_COUNT");
    assert_eq!(u6b_l21_writer_count(&bodies[1], writer), 1, "H6_L21_UNHELD_WRITER_BYTES");
    assert!(fx.session.actor.state.lock().await.message_delivery.held_input.entries.is_empty(), "H6_L21_UNHELD_NO_CUSTODY");
    if writer == U6bL21Writer::MediaResample {
        assert!(fx.session.entered.try_recv().is_err(), "H6_L21_UNHELD_MEDIA_NEVER_EXECUTED");
    }
    next.release();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut running).await.expect("H6_L21_UNHELD_JOIN_BOUND")
        .expect("H6_L21_UNHELD_JOIN");
    assert!(outcome.is_ok(), "H6_L21_UNHELD_FINAL_OUTCOME");
    let counters = counters.borrow().clone().expect("H6_L21_UNHELD_COUNTER_OBSERVATION");
    if writer == U6bL21Writer::ToolFreeLength {
        assert_eq!(counters["continues"], 1, "H6_L21_UNHELD_LENGTH_BUDGET");
        assert_eq!(counters["budget"], 1, "H6_L21_UNHELD_LENGTH_BUDGET");
        assert_eq!(counters["escalated"], false, "H6_L21_UNHELD_LENGTH_BUDGET");
    }
    json!({"writer":format!("{writer:?}"),"held":false,"bodies":bodies,"retry_counters":counters})
}

async fn u6b_l21_tool_call_length_tripwire() -> Value {
    const HUMAN: &str = "U6b L21 human precedes the held real writer";
    let writer = U6bL21Writer::ToolCallLength;
    let schema = json!({"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false});
    let mut fixture = u6_fixture_with_bookkeeping(None, K2lgNOptions::default(), false, false).await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff_with_salvage(fx, Some(1), Some(schema), None, None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let (checkpoint, _) = u6b_u66_progress(&mut turn.running,
        u6_quiescent_reload(fx, "H6_L21_TRIPWIRE_INITIAL_CHECKPOINT_READ"), "H6_L21_TRIPWIRE_INITIAL_CHECKPOINT_OUTCOME").await;
    assert!(k2lg_n_live(&checkpoint), "H6_L21_TRIPWIRE_REAL_PENDING_PRECONDITION");
    assert!(k2lg_n_selected(fx, fx.session.actor.native_inputs_held()).await, "H6_L21_TRIPWIRE_REAL_LATCH_PRECONDITION");
    u6_interject(fx, HUMAN, Vec::new()).await;
    let matcher = xai_grok_test_support::InferenceRequestMatcher::foreground(xai_grok_test_support::InferenceEndpoint::Messages);
    let mut produced = fx.server.expect_response_blocked("L21 actual production writer", matcher,
        k2_script(k2_frames("msg_u6b_l21_writer", writer.blocks(), writer.stop())));
    fx.session.releases[1].add_permits(1);
    u6b_u66_progress(&mut turn.running, produced.wait_blocked(), "H6_L21_TRIPWIRE_WRITER_REQUEST_REACHED").await;
    let before = k2lg_n_bodies(&fx.server);
    assert_eq!(before.len(), 3, "H6_L21_TRIPWIRE_WRITER_REQUEST_COUNT");
    assert_eq!(u6b_l21_writer_count(&before[2], writer), 0, "H6_L21_TRIPWIRE_WRITER_NOT_PREGENERATED");
    produced.release();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_L21_TRIPWIRE_TURN_END_BOUND").expect("H6_L21_TRIPWIRE_TURN_JOIN");
    let error = match outcome {
        Err(error) => error,
        Ok(_) => panic!("H6_L21_TRIPWIRE_REFUSED_OUTCOME"),
    };
    let code = error.data.as_ref().and_then(|data| data.get("code")).and_then(Value::as_str);
    assert_eq!(code, Some("native_waiting_set_refused"), "H6_L21_TRIPWIRE_WAITING_SET_REFUSED");
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 3, "H6_L21_TRIPWIRE_EXACTLY_THREE_REQUESTS");
    let no_length_reminder_request = bodies.iter().all(|body| !body.to_string().contains(writer.needle()));
    assert!(no_length_reminder_request, "H6_L21_TRIPWIRE_NO_LENGTH_REMINDER_REQUEST");
    let no_human_request = bodies.iter().all(|body| !body.to_string().contains(HUMAN));
    assert!(no_human_request, "H6_L21_TRIPWIRE_NO_HUMAN_REQUEST");
    let custody_reminder_sources = {
        let state = fx.session.actor.state.lock().await;
        state.message_delivery.held_input.entries.iter().filter_map(|entry| {
            let body_has_reminder = match &entry.body {
                super::interjection::HeldBody::Human(raw) => raw.text.contains(writer.needle()),
                super::interjection::HeldBody::Owned(item) => item.text_content().contains(writer.needle()),
                super::interjection::HeldBody::Parent(id) => id.contains(writer.needle()),
                super::interjection::HeldBody::Monitor | super::interjection::HeldBody::Plan => false,
            };
            (body_has_reminder || entry.meta.to_string().contains(writer.needle()))
                .then(|| format!("{:?}", entry.source))
        }).collect::<Vec<_>>()
    };
    assert_eq!(custody_reminder_sources.len(), 1, "H6_L21_S10_CUSTODY_ONCE");
    assert_eq!(custody_reminder_sources[0], "LengthReminder", "H6_L21_S10_OWN_SOURCE");
    json!({"custody_arm":"R49 held tool-call Length S10", "error_code":code,
        "request_count":bodies.len(), "no_length_reminder_request":no_length_reminder_request,
        "custody_length_reminder_count":custody_reminder_sources.len(),
        "custody_length_reminder_sources":custody_reminder_sources, "no_human_request":no_human_request,
        "retired_legs":"continuation, closure, release: NOT_REPRESENTABLE_AT_BASE / NOT_RUN / NOT_CREDITED",
        "counterfactual_kind":"M4-writer-bypasses-hold tool-call Length application; refusal guard control outside M4"})
}

async fn u6b_n11_u67_band_custody(
    source: super::interjection::HeldSource,
    reason_name: &str,
    reason_tag: &str,
) -> Value {
    let (mut fixture, mut turn, _, continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &mut fixture.fx;
    let source_tag = u6_source_name(source);
    let checkpoint = u6_readable_checkpoint(fx, &mut turn.running, true,
        "H6_N11_U67_REAL_LATCH_PRECONDITION").await;
    assert!(k2lg_n_live(&checkpoint), "H6_N11_U67_REAL_LATCH_PRECONDITION");
    for index in 0..128 {
        k2lg_n_selected(fx, fx.session.actor.hold_or_push_user_item(
            ConversationItem::system_reminder(format!("U67 N11 {source_tag} accepted {index:03}")), source,
        )).await.expect("H6_N11_U67_ACCEPTED_PREFIX");
    }
    let error = k2lg_n_selected(fx, fx.session.actor.hold_or_push_user_item(
        ConversationItem::system_reminder(format!("U67 N11 {source_tag} crossing")), source,
    )).await.expect_err("H6_N11_U67_BAND_LIMIT");
    assert_eq!(error.data.as_ref().and_then(|data| data.get("code")).and_then(Value::as_str),
        Some("native_held_input_incomplete"), "H6_N11_U67_TYPED_INCOMPLETE");
    let message = error.data.as_ref().and_then(|data| data.get("message"))
        .and_then(Value::as_str).unwrap_or(&error.message);
    assert!(message.contains(reason_tag), "H6_N11_U67_NAMED_REASON");
    {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert_eq!(ledger.entries.iter().filter(|entry| entry.source == source).count(), 128,
            "H6_N11_U67_PREFIX_RETAINED");
        let refusal = ledger.refusal.as_ref().expect("H6_N11_U67_REFUSAL_RECORD");
        assert_eq!(refusal.source, source, "H6_N11_U67_REFUSAL_SOURCE");
        assert_eq!(format!("{:?}", refusal.reason), reason_name, "H6_N11_U67_REFUSAL_REASON");
        assert_eq!(refusal.observed_len, 128, "H6_N11_U67_REFUSAL_COUNT");
    }
    u6_queue_resident(fx, "U67 N11 human admitted after full machine band", Vec::new()).await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N11_U67_HUMAN_HELD");
    {
        let state = fx.session.actor.state.lock().await;
        assert_eq!(state.message_delivery.held_input.entries.iter().filter(|entry|
            entry.source == super::interjection::HeldSource::HumanInterjection).count(), 1,
            "H6_N11_U67_FULL_BAND_NEVER_REFUSES_HUMAN");
    }
    assert_eq!(fx.server.messages_request_count(), 3, "H6_N11_U67_NO_NEW_CONTINUATION_HTTP");
    assert!(!turn.running.is_finished(), "H6_N11_U67_LATCH_REMAINS_ACTIVE");
    continuation.release();
    turn.running.abort();
    let _ = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N11_U67_FINAL_ABORT_BOUND");
    json!({"constructed":true,"kind":"U67 writer/stationarity custody-call crossing fallback",
        "source":source_tag,"reason":reason_tag,"typed_outcome":"native_held_input_incomplete",
        "accepted_prefix":128,"human_after_full_band":1,"new_http":0,
        "real_129_writer_invocations_proven":false,"restored_counters_required":true})
}

async fn u6b_n11_u67_skill_crossing() -> Value {
    use super::interjection::HeldSource;
    const PREFIX: &str = "U67 N11 real skill update ";
    let (mut fixture, mut turn, _, continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &mut fixture.fx;
    let checkpoint = u6_readable_checkpoint(fx, &mut turn.running, true,
        "H6_N11_SKILL_REAL_LATCH_PRECONDITION").await;
    assert!(k2lg_n_live(&checkpoint), "H6_N11_SKILL_REAL_LATCH_PRECONDITION");
    for index in 0..129 {
        u6b_u66_progress(&mut turn.running,
            u6b_l19_skill(&fx.session.actor, &format!("{PREFIX}{index:03}")),
            "H6_N11_SKILL_REAL_UPDATE_API").await;
    }
    assert_eq!(fx.session.actor.pending_skill_reminders.lock().len(), 129,
        "H6_N11_SKILL_REAL_RESIDENT_PREFIX");
    u6b_u66_progress(&mut turn.running,
        k2lg_n_selected(fx, fx.session.actor.flush_skills_at_native_safe_point()),
        "H6_N11_SKILL_SELECTED_SAFE_POINT").await;
    {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert_eq!(ledger.entries.iter().filter(|entry| entry.source == HeldSource::SkillReminder).count(), 128,
            "H6_N11_SKILL_BAND_LIMIT");
        let refusal = ledger.refusal.as_ref().expect("H6_N11_SKILL_REFUSAL_RECORD");
        assert_eq!(refusal.source, HeldSource::SkillReminder, "H6_N11_SKILL_REFUSAL_SOURCE");
        assert_eq!(format!("{:?}", refusal.reason), "SkillBandFull", "H6_N11_SKILL_REFUSAL_REASON");
        assert_eq!(refusal.observed_len, 128, "H6_N11_SKILL_REFUSAL_COUNT");
    }
    {
        let resident = fx.session.actor.pending_skill_reminders.lock();
        assert_eq!(resident.len(), 1, "H6_N11_SKILL_REFUSED_STAYS_RESIDENT");
        assert!(resident[0].text_content().contains(&format!("{PREFIX}128")),
            "H6_N11_SKILL_REFUSED_STAYS_RESIDENT");
    }
    u6_queue_resident(fx, "U67 N11 full skill band never refuses a human", Vec::new()).await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N11_SKILL_HUMAN_HELD");
    {
        let state = fx.session.actor.state.lock().await;
        assert_eq!(state.message_delivery.held_input.entries.iter().filter(|entry|
            entry.source == HeldSource::HumanInterjection).count(), 1,
            "H6_N11_SKILL_HUMAN_INDEPENDENT");
    }
    assert_eq!(fx.server.messages_request_count(), 3, "H6_N11_SKILL_NO_NEW_CONTINUATION_HTTP");
    continuation.release();
    turn.running.abort();
    let _ = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N11_SKILL_FINAL_ABORT_BOUND");
    json!({"constructed":false,"real_producer":"apply_skill_update_effects",
        "real_updates":129,"accepted_prefix":128,"resident_refused":1,
        "reason":"skill_band_full","human_independent":true,"new_http":0,
        "release_coverage":"separately preserved L19 refused-skill legs"})
}

async fn u6b_n11_u67_peer_at_flush() -> Value {
    use super::interjection::HeldSource;
    const HUMAN: &str = "U67 peer-at-flush held human";
    const PEER: &str = "U6b peer text 000";
    const PROMPT: &str = "U67 peer-at-flush next admitted prompt";
    let (mut fixture, mut turn, stepper, continuation, idle) = u6b_n12_end_fixture().await;
    let control = fixture.persistence_control.clone();
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &mut fixture.fx;
    u6_queue_resident(fx, HUMAN, Vec::new()).await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N11_PEER_FLUSH_HUMAN_HELD");
    u6b_u66_progress(&mut turn.running, u6b_u67_send_peer(fx, 0, false),
        "H6_N11_PEER_FLUSH_REAL_INGRESS").await;
    u6b_u67_mailbox_barrier(fx, &mut turn).await;
    let residents = u6_resident_keys(&fx.session.actor);
    assert_eq!(residents.len(), 1, "H6_N11_PEER_FLUSH_LATE_RESIDENT_PRECONDITION");
    let peer_key = residents[0]["key"].as_u64().expect("H6_N11_PEER_FLUSH_REAL_OCCURRENCE");
    {
        let state = fx.session.actor.state.lock().await;
        assert_eq!(state.message_delivery.held_input.entries.iter().filter(|entry|
            entry.source == HeldSource::PeerAgent).count(), 0, "H6_N11_PEER_FLUSH_NOT_DRAINED_EARLY");
        assert!(state.message_delivery.held_input.peer_origins.iter().any(|key| key.as_u64() == peer_key),
            "H6_N11_PEER_FLUSH_REAL_ORIGIN");
    }
    control.n12_drop_next_native_ack.store(true, std::sync::atomic::Ordering::SeqCst);
    continuation.release();
    stepper.resume();
    let ended = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N11_PEER_FLUSH_OWNED_ERROR_BOUND").expect("H6_N11_PEER_FLUSH_OWNED_ERROR_JOIN");
    let error = ended.err().expect("H6_N11_PEER_FLUSH_OWNED_ERROR_REQUIRED");
    assert_eq!(error.data.as_ref().and_then(|data| data.get("code")),
        Some(&json!("native_history_read_refused")), "H6_N11_PEER_FLUSH_QUALIFIED_ERROR");
    let read = fx.session.actor.chat_state_handle.read_native_history().await;
    assert_eq!(read.as_ref().err(), Some(&k2_history::NativeUnavailable::Indeterminate),
        "H6_N11_PEER_FLUSH_UNAVAILABLE_PRECONDITION");
    fx.session.actor.state.lock().await.notifications_suppressed = true;
    let (processed, acknowledgement) = tokio::sync::oneshot::channel();
    fx.loop_tx.send(SessionCommand::InjectTurnCompletion {
        prompt_id: KOXD_U6_PROMPT.into(), epoch: turn.epoch,
        result: Box::new(Err(error)), elapsed_ms: Some(0), processed,
    }).expect("H6_N11_PEER_FLUSH_COMPLETION_SEND");
    tokio::time::timeout(K0_TIMEOUT, acknowledgement).await
        .expect("H6_N11_PEER_FLUSH_COMPLETION_BOUND").expect("H6_N11_PEER_FLUSH_COMPLETION_ACK");
    tokio::time::timeout(K0_TIMEOUT, idle.notification.notified()).await
        .expect("H6_N11_PEER_FLUSH_POST_DRAIN_IDLE");
    {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        let peers: Vec<_> = ledger.entries.iter().filter(|entry| entry.source == HeldSource::PeerAgent).collect();
        assert_eq!(peers.len(), 1, "H6_N11_PEER_FLUSH_JOIN");
        assert_eq!(peers[0].key.as_u64(), peer_key, "H6_N11_PEER_FLUSH_KEY_PRESERVED");
        assert!(state.running_task.is_none() && state.pending_inputs.is_empty(), "H6_N11_PEER_FLUSH_NO_PROMPT_ROW");
    }
    assert!(fx.session.actor.pending_interjections.is_empty(), "H6_N11_PEER_FLUSH_RESIDENT_JOINED");
    assert_eq!(fx.server.messages_request_count(), 3, "H6_N11_PEER_FLUSH_NO_RESTART_HTTP");
    let (mut next, _next_stop) = u6b_u66_next_prompt(fx, PROMPT, "H6_N11_PEER_FLUSH_NEXT_PROMPT").await;
    let bodies = k2lg_n_bodies(&fx.server);
    let text = u6_body_text(bodies.last().expect("H6_N11_PEER_FLUSH_RELEASE_REQUEST"));
    assert_eq!(text.matches(HUMAN).count(), 1, "H6_N11_PEER_FLUSH_RELEASE_ONCE");
    assert_eq!(text.matches(PEER).count(), 1, "H6_N11_PEER_FLUSH_RELEASE_ONCE");
    let prompt_at = text.find(PROMPT).expect("H6_N11_PEER_FLUSH_PROMPT_PRESENT");
    let human_at = text.find(HUMAN).expect("H6_N11_PEER_FLUSH_HUMAN_PRESENT");
    let peer_at = text.find(PEER).expect("H6_N11_PEER_FLUSH_PEER_PRESENT");
    assert!(prompt_at < human_at && human_at < peer_at, "H6_N11_PEER_FLUSH_RELEASE_KEY_ORDER");
    {
        let state = fx.session.actor.state.lock().await;
        assert!(state.message_delivery.held_input.entries.is_empty(), "H6_N11_PEER_FLUSH_EMPTY_CUSTODY");
    }
    next.release();
    tokio::time::timeout(K0_TIMEOUT, next.wait_satisfied()).await.expect("H6_N11_PEER_FLUSH_NEXT_SATISFIED");
    next.assert_satisfied();
    continuation.assert_satisfied();
    json!({"constructed":false,"real_ingress":"SessionCommand::AgentMessage mailbox text",
        "real_completion":"InjectTurnCompletion owned native_history_read_refused / Indeterminate",
        "peer_key":peer_key,"peer_joined_band":1,"fallback_rows":0,"released_once":true,
        "release_order":"next prompt, held human, late peer","http_after_end":3,"http_at_release":bodies.len()})
}

async fn u6b_n11_u67_peer_flush_refusal() -> Value {
    use super::interjection::HeldSource;
    let (mut fixture, mut turn, stepper, continuation, _) = u6b_n12_end_fixture().await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &mut fixture.fx;
    u6_queue_resident(fx, "U67 constructed peer-flush existing human custody", Vec::new()).await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N11_PEER_FLUSH_DEFENSIVE_HUMAN_HELD");
    u6b_u66_progress(&mut turn.running, u6b_u67_send_peer(fx, 0, false),
        "H6_N11_PEER_FLUSH_DEFENSIVE_REAL_INGRESS").await;
    u6b_u67_mailbox_barrier(fx, &mut turn).await;
    let residents = u6_resident_keys(&fx.session.actor);
    assert_eq!(residents.len(), 1, "H6_N11_PEER_FLUSH_DEFENSIVE_ONE_RESIDENT");
    let key_u64 = residents[0]["key"].as_u64().expect("H6_N11_PEER_FLUSH_DEFENSIVE_KEY");
    let pending_before;
    let peer_key = {
        let mut state = fx.session.actor.state.lock().await;
        pending_before = state.pending_inputs.len();
        let ledger = &mut state.message_delivery.held_input;
        let peer_key = *ledger.peer_origins.iter().find(|key| key.as_u64() == key_u64)
            .expect("H6_N11_PEER_FLUSH_DEFENSIVE_REAL_ORIGIN");
        assert!(ledger.peer_origins.remove(&peer_key), "H6_N11_PEER_FLUSH_DEFENSIVE_CONSTRUCTION");
        peer_key
    };
    assert!(k2lg_n_selected(fx, fx.session.actor.native_inputs_held()).await,
        "H6_N11_PEER_FLUSH_DEFENSIVE_ACTIVE_HOLD");
    for index in 0..50 {
        assert!(k2lg_n_selected(fx, fx.session.actor.hold_or_push_user_item(
            ConversationItem::system_reminder(format!("U67 constructed peer full-band entry {index:03}")),
            HeldSource::PeerAgent)).await.is_ok(), "H6_N11_PEER_FLUSH_DEFENSIVE_FILL");
        let state = fx.session.actor.state.lock().await;
        assert_eq!(state.message_delivery.held_input.entries.iter()
            .filter(|entry| entry.source == HeldSource::PeerAgent).count(), index + 1,
            "H6_N11_PEER_FLUSH_DEFENSIVE_FILL_CUSTODY");
    }
    {
        let mut state = fx.session.actor.state.lock().await;
        let ledger = &mut state.message_delivery.held_input;
        ledger.peer_origins.insert(peer_key);
        assert_eq!(ledger.entries.iter().filter(|entry| entry.source == HeldSource::PeerAgent).count(), 50,
            "H6_N11_PEER_FLUSH_DEFENSIVE_FULL_PRECONDITION");
    }
    assert_eq!(k2lg_n_selected(fx, fx.session.actor.flush_stranded_interjections()).await, 0,
        "H6_N11_PEER_FLUSH_DEFENSIVE_NO_PROMPT_ROW");
    let after = u6_resident_keys(&fx.session.actor);
    assert_eq!(after.len(), 1, "H6_N11_PEER_FLUSH_REFUSED_RESIDENT");
    assert_eq!(after[0]["key"].as_u64(), Some(key_u64), "H6_N11_PEER_FLUSH_REFUSED_RESIDENT");
    {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        let refusal = ledger.refusal.as_ref().expect("H6_N11_PEER_FLUSH_DEFENSIVE_REFUSAL_RECORD");
        assert_eq!(refusal.source, HeldSource::PeerAgent, "H6_N11_PEER_FLUSH_DEFENSIVE_REFUSAL_SOURCE");
        assert_eq!(format!("{:?}", refusal.reason), "PeerBandFull", "H6_N11_PEER_FLUSH_DEFENSIVE_REFUSAL_REASON");
        assert_eq!(refusal.observed_len, 50, "H6_N11_PEER_FLUSH_DEFENSIVE_REFUSAL_COUNT");
        assert!(ledger.peer_origins.iter().any(|key| key.as_u64() == key_u64),
            "H6_N11_PEER_FLUSH_DEFENSIVE_ORIGIN_RETAINED");
        assert_eq!(state.pending_inputs.len(), pending_before, "H6_N11_PEER_FLUSH_DEFENSIVE_NO_PROMPT_ROW");
    }
    assert_eq!(fx.server.messages_request_count(), 3, "H6_N11_PEER_FLUSH_DEFENSIVE_NO_NEW_HTTP");
    continuation.release();
    stepper.resume();
    turn.running.abort();
    let _ = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N11_PEER_FLUSH_DEFENSIVE_FINAL_ABORT_BOUND");
    json!({"constructed":true,"construction":"50 held peer entries plus an already-admitted real mailbox peer at the flush band check",
        "real_resident_key":key_u64,"refused_keeps_key":true,"reason":"peer_band_full","new_prompt_rows":0,
        "real_full_peer_producer_bypass_claimed":false})
}

async fn u6b_n12_u67_band_extension() -> Value {
    use super::interjection::HeldSource;
    const HUMAN: &str = "U67 N12 large held human:";
    const RESIDENT: &str = "U67 N12 resident after all bands";
    const PROMPT: &str = "U67 N12 next admitted plan prompt";
    const MONITOR: &str = "U67 N12 held monitor";
    const DEFERRED: &str = "U67 N12 held deferred";
    const STATIONARITY: &str = "U67 N12 held stationarity";
    const SKILL: &str = "U67 N12 held real skill";
    const PEER: &str = "U6b peer text 000";
    const MEDIA: &str = "U67 N12 held media source";
    const TOOL_FREE: &str = "U67 N12 held tool-free Length source";
    const S10: &str = "U67 N12 held S10 source";
    let (mut fixture, mut turn, stepper, continuation, idle) = u6b_n12_end_fixture().await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &mut fixture.fx;
    let human = format!("{HUMAN}{}", "h".repeat(256 * 1024 - HUMAN.len()));
    u6_queue_resident(fx, &human, Vec::new()).await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N12_HUMAN_CUSTODY");
    u6_monitor(fx, "u67-n12-monitor", MONITOR).await;
    k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
    for (text, source) in [(DEFERRED, HeldSource::DeferredFollowUp),
        (STATIONARITY, HeldSource::StationarityReminder), (MEDIA, HeldSource::MediaResample),
        (TOOL_FREE, HeldSource::ToolFreeLength), (S10, HeldSource::LengthReminder)]
    {
        k2lg_n_selected(fx, fx.session.actor.hold_or_push_user_item(
            ConversationItem::system_reminder(text), source,
        )).await.expect("H6_N12_MACHINE_CUSTODY");
    }
    let (responds_to, mut replied) = tokio::sync::oneshot::channel();
    fx.loop_tx.send(SessionCommand::SessionMode {
        session_mode: acp::SessionModeId::new("plan"), responds_to,
    }).expect("H6_N12_PLAN_COMMAND_SEND");
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut replied => result.expect("H6_N12_PLAN_MODE_REPLY"),
            _ = &mut turn.running => panic!("H6_N12_PLAN_TURN_ENDED"),
        }
    }).await.expect("H6_N12_PLAN_COMMAND_BOUND");
    u6b_u66_progress(&mut turn.running, u6b_l19_skill(&fx.session.actor, SKILL),
        "H6_N12_REAL_SKILL_UPDATE").await;
    u6b_u66_progress(&mut turn.running,
        k2lg_n_selected(fx, fx.session.actor.flush_skills_at_native_safe_point()),
        "H6_N12_SKILL_AND_PLAN_HELD").await;
    u6b_u66_progress(&mut turn.running, u6b_u67_send_peer(fx, 0, false),
        "H6_N12_REAL_PEER_INGRESS").await;
    u6b_u67_mailbox_barrier(fx, &mut turn).await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N12_PEER_HELD");
    let mut keys = u6_mixed_keys(fx).await;
    assert_eq!(keys.len(), 10, "H6_N12_EVERY_PRESENT_BAND_ADMITTED");
    assert!(keys.iter().all(|entry| entry["source"] != "parent_steer"), "H6_N12_S4_EXCLUDED");
    let plan = fx.session.actor.plan_mode.lock().pending_activation_text()
        .expect("H6_N12_HELD_PLAN_REFERENCE").to_owned();
    u6_queue_resident(fx, RESIDENT, Vec::new()).await;
    let residents = u6_resident_keys(&fx.session.actor);
    assert_eq!(residents.len(), 1, "H6_N12_ONE_LATE_RESIDENT");
    let resident_key = residents[0]["key"].as_u64().expect("H6_N12_LATE_RESIDENT_KEY");
    keys.push(json!({"key":resident_key,"source":"late_resident"}));
    keys.sort_by_key(|entry| entry["key"].as_u64().unwrap());
    fixture.persistence_control.n12_drop_next_native_ack.store(true, std::sync::atomic::Ordering::SeqCst);
    continuation.release();
    stepper.resume();
    let ended = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N12_OWNED_ERROR_OUTCOME").expect("H6_N12_OWNED_ERROR_JOIN");
    let error = ended.err().expect("H6_N12_OWNED_ERROR_REQUIRED");
    assert_eq!(error.data.as_ref().and_then(|data| data.get("code")),
        Some(&json!("native_history_read_refused")), "H6_N12_QUALIFIED_OWNED_REFUSAL");
    let read = fx.session.actor.chat_state_handle.read_native_history().await;
    assert_eq!(read.as_ref().err(), Some(&k2_history::NativeUnavailable::Indeterminate),
        "H6_N12_CHECKPOINT_INDETERMINATE");
    u6_observe(json!({"stage":"N12 checkpoint before turn-end progress",
        "native_unavailable":"Indeterminate","error_code":"native_history_read_refused",
        "custody_and_resident_keys":keys.clone(),"http_count":fx.server.messages_request_count()}));
    fx.session.actor.state.lock().await.notifications_suppressed = true;
    let (processed, acknowledgement) = tokio::sync::oneshot::channel();
    fx.loop_tx.send(SessionCommand::InjectTurnCompletion {
        prompt_id: KOXD_U6_PROMPT.into(), epoch: turn.epoch,
        result: Box::new(Err(error)), elapsed_ms: Some(0), processed,
    }).expect("H6_N12_COMPLETION_COMMAND_SEND");
    tokio::time::timeout(K0_TIMEOUT, acknowledgement).await
        .expect("H6_N12_COMPLETION_COMMAND_BOUND").expect("H6_N12_COMPLETION_COMMAND_ACK");
    tokio::time::timeout(K0_TIMEOUT, idle.notification.notified()).await
        .expect("H6_N12_SETTLED_OR_REPORTED");
    let records = u6b_host_records(fx).await;
    let retained: Vec<_> = records.iter().filter(|record|
        record["reason"] == "retained_view_unavailable").collect();
    assert_eq!(retained.len(), 1, "H6_N12_SETTLED_OR_REPORTED");
    assert_eq!(retained[0]["bands"], json!({
        "human_interjection":2,"parent_steer":0,"monitor_event":1,"deferred_followup":1,
        "stationarity_reminder":1,"skill_reminder":1,"plan_activation":1,"peer_agent":1,
        "media_resample":1,"tool_free_length":1,"length_reminder":1,
    }), "H6_N12_SETTLED_OR_REPORTED");
    {
        let state = fx.session.actor.state.lock().await;
        assert!(state.running_task.is_none() && state.pending_inputs.is_empty(),
            "H6_N12_SETTLED_OR_REPORTED");
    }
    assert_eq!(fx.server.messages_request_count(), 3, "H6_N12_SETTLED_OR_REPORTED");
    let mut retained_keys = u6_mixed_keys(fx).await;
    retained_keys.sort_by_key(|entry| entry["key"].as_u64().unwrap());
    assert_eq!(retained_keys.iter().map(|entry| &entry["key"]).collect::<Vec<_>>(),
        keys.iter().map(|entry| &entry["key"]).collect::<Vec<_>>(), "H6_N12_SETTLED_OR_REPORTED");
    let mut next = fx.server.expect_response_blocked("U67 N12 next admitted plan prompt",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages),
        k2_script(k2lg_n_after_frames("msg_u67_n12_next")));
    let mut response = u6b_l19_enqueue_prompt(fx, PROMPT, "u67-n12-next-owner", PromptMode::Plan);
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = next.wait_blocked() => {},
            _ = &mut response => panic!("H6_N12_NEXT_PROMPT_ENDED_BEFORE_HTTP"),
        }
    }).await.expect("H6_N12_ORDER");
    let handle = fx.session.actor.state.lock().await.running_task.as_ref()
        .expect("H6_N12_NEXT_RUNNING_OWNER").handle.clone();
    let _next_stop = K0Abort(handle);
    let read = fx.session.actor.chat_state_handle.read_native_history().await;
    assert_eq!(read.as_ref().err(), Some(&k2_history::NativeUnavailable::Indeterminate),
        "H6_N12_STILL_UNREADABLE_AT_RELEASE");
    let bodies = k2lg_n_bodies(&fx.server);
    let text = u6_body_text(bodies.last().expect("H6_N12_NEXT_ACTUAL_REQUEST"));
    let prompt_at = text.find(PROMPT).expect("H6_N12_NEXT_PROMPT_PRESENT");
    let mut positions = Vec::new();
    for entry in &keys {
        let needle = match entry["source"].as_str().unwrap() {
            "human_interjection" => HUMAN, "monitor_event" => MONITOR,
            "deferred_followup" => DEFERRED, "stationarity_reminder" => STATIONARITY,
            "skill_reminder" => SKILL, "plan_activation" => plan.as_str(),
            "peer_agent" => PEER, "media_resample" => MEDIA,
            "tool_free_length" => TOOL_FREE, "length_reminder" => S10,
            "late_resident" => RESIDENT,
            _ => panic!("H6_N12_UNEXPECTED_BAND"),
        };
        assert_eq!(text.matches(needle).count(), 1, "H6_N12_ORDER");
        let at = text.find(needle).expect("H6_N12_ORDER");
        assert!(at > prompt_at, "H6_N12_ORDER");
        positions.push(at);
    }
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "H6_N12_ORDER");
    {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert!(ledger.entries.is_empty() && ledger.latch.is_none(), "H6_N12_EMPTY");
    }
    assert!(u6_pinned_monitor_keys(fx).await.is_empty(), "H6_N12_EMPTY");
    assert!(fx.session.actor.pending_interjections.is_empty(), "H6_N12_EMPTY");
    next.release();
    tokio::time::timeout(K0_TIMEOUT, next.wait_satisfied()).await.expect("H6_N12_NEXT_SATISFIED");
    next.assert_satisfied();
    continuation.assert_satisfied();
    json!({"kind":"N12_U67_ALL_BANDS_UNAVAILABLE_TURN_END","constructed":true,
        "constructed_scope":"single writer-band custody-call entries; real peer, skill, plan, monitor and owned error",
        "native_unavailable":"Indeterminate","retained_band_counts":retained[0]["bands"],
        "custody_and_resident_keys":keys,"positions_in_next_request":positions,
        "post_drain_idle_observer":true,"next_prompt_http_count":bodies.len(),"ledger_empty":true})
}

async fn u6b_n13_admit_band(
    fx: &K2lgN,
    turn: &mut K2lgNTurn,
    source: super::interjection::HeldSource,
    count: usize,
    markers: &mut std::collections::BTreeMap<u64, String>,
) -> Vec<U6bIdAdmission> {
    use super::interjection::HeldSource;
    let mut steers = Vec::new();
    for index in 0..count {
        let before = u6_mixed_keys(fx).await;
        let mut text = format!("U67 N13 {} item {index:03}", u6_source_name(source));
        match source {
            HeldSource::HumanInterjection => panic!("H6_N13_MACHINE_SOURCE_REQUIRED"),
            HeldSource::MonitorEvent => {
                u6b_u66_progress(&mut turn.running,
                    u6_monitor(fx, &format!("u67-n13-monitor-{index:03}"), &text),
                    "H6_N13_MONITOR_PRODUCER").await;
                k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
            }
            HeldSource::ParentSteer => {
                let admitted = u6b_admit_steer_id(fx, &mut turn.running,
                    &format!("u67-n13-steer-{index:03}"), &text, "H6_N13_STEER_ADMISSION").await;
                assert_eq!(admitted.admission, k0_coordinator::ActiveMessageAdmission::Admitted,
                    "H6_N13_OTHER_BAND_ADMITTED");
                assert!(admitted.receipt.is_some(), "H6_N13_STEER_RECEIPT");
                steers.push(admitted);
            }
            HeldSource::PlanActivation => {
                let (responds_to, replied) = tokio::sync::oneshot::channel();
                fx.loop_tx.send(SessionCommand::SessionMode {
                    session_mode: acp::SessionModeId::new("plan"), responds_to,
                }).expect("H6_N13_PLAN_COMMAND_SEND");
                u6b_u66_progress(&mut turn.running, replied, "H6_N13_PLAN_COMMAND_BOUND")
                    .await.expect("H6_N13_PLAN_COMMAND_REPLY");
                text = fx.session.actor.plan_mode.lock().pending_activation_text()
                    .expect("H6_N13_PLAN_RESIDENT_PRECONDITION").to_owned();
            }
            HeldSource::DeferredFollowUp | HeldSource::StationarityReminder |
            HeldSource::SkillReminder | HeldSource::PeerAgent | HeldSource::MediaResample |
            HeldSource::ToolFreeLength | HeldSource::LengthReminder => {
                k2lg_n_selected(fx, fx.session.actor.hold_or_push_user_item(
                    ConversationItem::system_reminder(&text), source,
                )).await.expect("H6_N13_OTHER_BAND_ADMITTED");
            }
        }
        k2lg_n_selected(fx, async {
            assert!(!fx.session.actor.drain_interjections_at_safe_point().await,
                "H6_N13_BAND_HELD");
            fx.session.actor.flush_skills_at_native_safe_point().await;
        }).await;
        let after = u6_mixed_keys(fx).await;
        let added: Vec<_> = after.iter().filter(|entry|
            !before.iter().any(|old| old["key"] == entry["key"])).collect();
        assert_eq!(added.len(), 1, "H6_N13_OTHER_BAND_ADMITTED");
        assert_eq!(added[0]["source"], u6_source_name(source), "H6_N13_SOURCE_INDEPENDENT");
        let key = added[0]["key"].as_u64().expect("H6_N13_ADMISSION_KEY");
        assert!(markers.insert(key, text).is_none(), "H6_N13_DISTINCT_OCCURRENCES");
    }
    steers
}

async fn u6b_n13_band_isolation(full_source: super::interjection::HeldSource) -> Value {
    use super::interjection::HeldSource;
    const HUMAN: &str = "U67 N13 human after a full machine band";
    let sources = [HeldSource::ParentSteer, HeldSource::MonitorEvent,
        HeldSource::DeferredFollowUp, HeldSource::StationarityReminder, HeldSource::SkillReminder,
        HeldSource::PlanActivation, HeldSource::PeerAgent, HeldSource::MediaResample,
        HeldSource::ToolFreeLength, HeldSource::LengthReminder];
    let limit = match full_source {
        HeldSource::HumanInterjection => panic!("H6_N13_MACHINE_SOURCE_REQUIRED"),
        HeldSource::ParentSteer => 32,
        HeldSource::PlanActivation => 1,
        HeldSource::PeerAgent => 50,
        HeldSource::MonitorEvent | HeldSource::DeferredFollowUp | HeldSource::StationarityReminder |
        HeldSource::SkillReminder | HeldSource::MediaResample | HeldSource::ToolFreeLength |
        HeldSource::LengthReminder => 128,
    };
    let mut fixture = u6_fixture_controlled(None, K2lgNOptions::default(), U6PersistenceControl {
        plan_tools: true, ..Default::default()
    }).await;
    let fx = &mut fixture.fx;
    let stepper = U6TurnStepper::default();
    let mut turn = u6_handoff_with_salvage(fx, None, None, None, Some(stepper.clone())).await;
    let _stop = K0Abort(turn.running.abort_handle());
    stepper.pause();
    let mut continuation = fx.server.expect_response_blocked("U67 N13 held continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages), k2_script(k2lg_n_closure_frames()));
    fx.session.releases[1].add_permits(1);
    u6_step_to_http(&stepper, &mut continuation).await;
    let checkpoint = u6_readable_checkpoint(fx, &mut turn.running, true,
        "H6_N13_REAL_LATCH_PRECONDITION").await;
    assert!(k2lg_n_live(&checkpoint), "H6_N13_REAL_LATCH_PRECONDITION");
    let mut markers = std::collections::BTreeMap::new();
    let mut steers = u6b_n13_admit_band(fx, &mut turn, full_source, limit, &mut markers).await;
    {
        let state = fx.session.actor.state.lock().await;
        assert_eq!(state.message_delivery.held_input.entries.iter()
            .filter(|entry| entry.source == full_source).count(), limit, "H6_N13_FULL_BAND_PRECONDITION");
    }
    u6_queue_resident(fx, HUMAN, Vec::new()).await;
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N13_HUMAN_ADMITTED_AFTER_FULL_BAND");
    let human_key = u6_mixed_keys(fx).await.into_iter().find(|entry|
        entry["source"] == "human_interjection").expect("H6_N13_HUMAN_INDEPENDENT")["key"]
        .as_u64().expect("H6_N13_HUMAN_KEY");
    assert!(markers.insert(human_key, HUMAN.to_owned()).is_none(), "H6_N13_DISTINCT_OCCURRENCES");
    for source in sources {
        if source != full_source {
            steers.extend(u6b_n13_admit_band(fx, &mut turn, source, 1, &mut markers).await);
        }
    }
    let held = u6_mixed_keys(fx).await;
    assert_eq!(held.len(), limit + 10, "H6_N13_ALL_OTHER_BANDS_ADMITTED");
    assert_eq!(markers.len(), held.len(), "H6_N13_DISTINCT_OCCURRENCES");
    {
        let state = fx.session.actor.state.lock().await;
        assert!(state.message_delivery.held_input.refusal.is_none(), "H6_N13_NO_CROSS_BAND_REFUSAL");
    }
    assert_eq!(fx.server.messages_request_count(), 3, "H6_N13_NO_EARLY_RELEASE");
    fx.server.enqueue_response("/v1/messages", k2_script(k2lg_n_after_frames("msg_u67_n13_after")));
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_N13_READABLE_CLOSED_CHECKPOINT").await;
    stepper.resume();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N13_RELEASE_PROGRESS").expect("H6_N13_TURN_JOIN");
    assert!(outcome.is_ok(), "H6_N13_RELEASE_PROGRESS");
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4, "H6_N13_ONE_RELEASE_REQUEST");
    let text = u6_body_text(bodies.last().expect("H6_N13_ACTUAL_RELEASE_BODY"));
    let mut positions = Vec::new();
    for needle in markers.values() {
        assert_eq!(text.matches(needle.as_str()).count(), 1, "H6_N13_RELEASE_ONCE");
        positions.push(text.find(needle.as_str()).expect("H6_N13_RELEASE_KEY_ORDER"));
    }
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "H6_N13_RELEASE_KEY_ORDER");
    {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert!(ledger.entries.is_empty() && ledger.latch.is_none(), "H6_N13_EMPTY_CUSTODY");
    }
    assert!(u6_pinned_monitor_keys(fx).await.is_empty(), "H6_N13_EMPTY_PINS");
    assert!(fx.session.actor.pending_interjections.is_empty(), "H6_N13_EMPTY_RESIDENTS");
    continuation.assert_satisfied();
    json!({"constructed":true,"kind":"N13_PER_BAND_CONSTRUCTED_FILL",
        "full_band":u6_source_name(full_source),"full_count":limit,"other_machine_bands":9,
        "human_independent":true,"released_entries":markers.len(),"positions_in_key_order":positions,
        "constructed_scope":"per-band custody-call fills; real monitor, Steer and plan admissions; real latch and release",
        "held_steer_receipts":steers.len(),"http_count":bodies.len()})
}

async fn u6b_n13_real_peer_isolation() -> Value {
    use super::interjection::HeldSource;
    const HUMAN: &str = "U67 N13 human alongside real mailbox peers";
    const DEFERRED: &str = "U67 N13 deferred alongside real mailbox peers";
    const FIRST: &str = "U6b peer text 000";
    const SECOND: &str = "U6b peer text 001";
    let (mut fixture, mut turn, stepper, continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &mut fixture.fx;
    let checkpoint = u6_readable_checkpoint(fx, &mut turn.running, true,
        "H6_N13_REAL_PEER_LATCH_PRECONDITION").await;
    assert!(k2lg_n_live(&checkpoint), "H6_N13_REAL_PEER_LATCH_PRECONDITION");
    u6_queue_resident(fx, HUMAN, Vec::new()).await;
    for index in 0..2 {
        u6b_u66_progress(&mut turn.running, u6b_u67_send_peer(fx, index, false),
            "H6_N13_REAL_MAILBOX_PEER_INGRESS").await;
    }
    u6b_u67_mailbox_barrier(fx, &mut turn).await;
    let residents = u6_resident_keys(&fx.session.actor);
    assert_eq!(residents.len(), 3, "H6_N13_REAL_PEER_RESIDENT_PRECONDITION");
    assert!(!k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_N13_REAL_PEER_HELD");
    {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert_eq!(ledger.entries.iter().filter(|entry| entry.source == HeldSource::PeerAgent).count(),
            2, "H6_N13_PEER_ISOLATED_FROM_HUMAN");
        assert_eq!(ledger.entries.iter().filter(|entry| entry.source == HeldSource::HumanInterjection).count(),
            1, "H6_N13_PEER_ISOLATED_FROM_HUMAN");
    }
    k2lg_n_selected(fx, fx.session.actor.hold_or_push_user_item(
        ConversationItem::system_reminder(DEFERRED), HeldSource::DeferredFollowUp,
    )).await.expect("H6_N13_REAL_PEER_OTHER_BAND_ADMITTED");
    let mut keys = u6_mixed_keys(fx).await;
    keys.sort_by_key(|entry| entry["key"].as_u64().unwrap());
    assert_eq!(keys.len(), 4, "H6_N13_REAL_PEER_DISTINCT_KEYS");
    assert_eq!(fx.server.messages_request_count(), 3, "H6_N13_REAL_PEER_NO_EARLY_RELEASE");
    fx.server.enqueue_response("/v1/messages", k2_script(k2lg_n_after_frames("msg_u67_n13_real_peer_after")));
    u6_release_after_checkpoint(fx, &mut turn.running, &continuation, false,
        "H6_N13_REAL_PEER_CLOSED_CHECKPOINT").await;
    stepper.resume();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .expect("H6_N13_REAL_PEER_RELEASE_PROGRESS").expect("H6_N13_REAL_PEER_TURN_JOIN");
    assert!(outcome.is_ok(), "H6_N13_REAL_PEER_RELEASE_PROGRESS");
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4, "H6_N13_REAL_PEER_ONE_RELEASE_REQUEST");
    let text = u6_body_text(bodies.last().expect("H6_N13_REAL_PEER_ACTUAL_BODY"));
    let positions: Vec<_> = [HUMAN, FIRST, SECOND, DEFERRED].into_iter().map(|needle| {
        assert_eq!(text.matches(needle).count(), 1, "H6_N13_REAL_PEER_RELEASE_ONCE");
        text.find(needle).expect("H6_N13_REAL_PEER_RELEASE_KEY_ORDER")
    }).collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "H6_N13_REAL_PEER_RELEASE_KEY_ORDER");
    {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert!(ledger.entries.is_empty() && ledger.latch.is_none(), "H6_N13_REAL_PEER_EMPTY_CUSTODY");
    }
    continuation.assert_satisfied();
    json!({"constructed":false,"kind":"N13_REAL_MAILBOX_PEER_HUMAN_ISOLATION",
        "real_producer":"SessionCommand::AgentMessage mailbox text", "peer_entries":2,"human_entries":1,
        "deferred_entries":1,"custody_keys":keys,"positions_in_key_order":positions,"http_count":bodies.len()})
}

#[derive(Clone, Copy)]
enum U6bL15mArm { Durable, Rearm }

async fn u6b_l15m_native_memory(arm: U6bL15mArm) -> Value {
    const FIRST_HUMAN: &str = "U6b L15m human forces the first closed-exchange build";
    const SECOND_HUMAN: &str = "U6b L15m second human forces the sixth build";
    let (prefix, outcome_tag, reached_tag, first_tag, second_tag) = match arm {
        U6bL15mArm::Durable => ("H6_L15M_DURABLE", "H6_L15M_DURABLE_CLOSED_EXCHANGE_OUTCOME", "H6_L15M_DURABLE_FIFTH_REQUEST_REACHED", "H6_L15M_DURABLE_FIRST_CONTINUATION_BOUND", "H6_L15M_DURABLE_SECOND_CONTINUATION_BOUND"),
        U6bL15mArm::Rearm => ("H6_L15M_REARM", "H6_L15M_REARM_CLOSED_EXCHANGE_OUTCOME", "H6_L15M_REARM_FIFTH_REQUEST_REACHED", "H6_L15M_REARM_FIRST_CONTINUATION_BOUND", "H6_L15M_REARM_SECOND_CONTINUATION_BOUND"),
    };
    let (mut fixture, chat_rx) = u6b_l15_fixture(None, K2lgNOptions::default(), U6PersistenceControl::default()).await;
    let fx = &mut fixture.fx;
    let stepper = U6TurnStepper::default();
    let mut turn = u6b_l15_handoff(fx, None, None, None, Some(stepper.clone()), chat_rx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let root = fx.session.directory.path().join("l15m-memory-v2");
    let storage = crate::session::memory::MemoryStorage::new_for_mode(
        fx.session.actor.tool_context.cwd.as_path(), Some(&root), crate::config::MemoryMode::V2);
    storage.ensure_initialized().unwrap_or_else(|_| panic!("{prefix}_STORAGE_INITIALIZED"));
    fx.session.actor.memory.storage.replace(Some(storage));
    assert_eq!(fx.session.actor.memory.mode(), Some(crate::config::MemoryMode::V2), "{prefix}_STORAGE_MODE");
    fx.session.actor.memory.context_injected.store(false, std::sync::atomic::Ordering::Relaxed);
    let before = fx.session.actor.chat_state_handle.get_conversation().await;
    let system_before = native_content_digest(&serde_json::to_vec(before.first().unwrap_or_else(|| panic!("{prefix}_SYSTEM_PRECONDITION"))).unwrap());
    let (checkpoint, _) = u6_quiescent_reload(fx, &format!("{prefix}_INITIAL_CHECKPOINT")).await;
    assert!(k2lg_n_live(&checkpoint), "{prefix}_LIVE_HOLD_PRECONDITION");
    u6_interject(fx, FIRST_HUMAN, Vec::new()).await;
    let mut first = fx.server.expect_response_blocked("L15m first held results-only continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(xai_grok_test_support::InferenceEndpoint::Messages),
        k2_script(u6_next_pending(true)));
    fx.session.releases[1].add_permits(1);
    u6b_u66_progress(&mut turn.running, first.wait_blocked(), first_tag).await;
    let bodies = k2lg_n_bodies(&fx.server);
    u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
    assert!(!fx.session.actor.memory.context_injected.load(std::sync::atomic::Ordering::Relaxed),
        "{prefix}_HELD_MEMORY_LATCH_OPEN");
    let mut second = fx.server.expect_response_blocked("L15m matching native closure",
        xai_grok_test_support::InferenceRequestMatcher::foreground(xai_grok_test_support::InferenceEndpoint::Messages),
        k2_script(k2_frames("msg_u6b_l15m_closed", vec![k2_result("srvtoolu_u6_next", "alpha__lookup"),
            (json!({"type":"text","text":"L15m closing reply"}), Vec::new())], "end_turn")));
    first.release();
    let mut entered = u6_client_dispatch(&mut fx.session.entered, &mut turn.running, json!("Accepted"),
        "L15m second real client batch", &format!("{prefix}_SECOND_BATCH_BOUND"),
        &format!("{prefix}_SECOND_BATCH_TURN_ENDED")).await;
    entered.sort();
    assert_eq!(entered, vec!["alpha__lookup", "zeta__fetch"], "{prefix}_SECOND_CLIENT_BATCH");
    fx.session.releases[1].add_permits(1);
    u6b_u66_progress(&mut turn.running, second.wait_blocked(), second_tag).await;
    assert_eq!(fx.server.messages_request_count(), 4, "{prefix}_FOUR_HELD_REQUESTS");
    stepper.pause();
    u6_release_after_checkpoint(fx, &mut turn.running, &second, false, &format!("{prefix}_MATCHING_CLOSURE")).await;
    let closed = fx.session.actor.chat_state_handle.read_native_history().await
        .unwrap_or_else(|_| panic!("{prefix}_CLOSED_CHECKPOINT_READ"));
    let closed = closed.checkpoint.unwrap_or_else(|| panic!("{prefix}_CLOSED_CHECKPOINT_PRESENT"));
    let covered_at_closure = closed.record.history_boundary.items;
    let mut fifth = fx.server.expect_response_blocked("L15m first eligible memory build",
        xai_grok_test_support::InferenceRequestMatcher::foreground(xai_grok_test_support::InferenceEndpoint::Messages),
        k2_script(k2lg_n_after_frames("msg_u6b_l15m_fifth")));
    stepper.resume();
    let reached = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            _ = fifth.wait_blocked() => true,
            result = &mut turn.running => {
                let outcome = result.unwrap_or_else(|_| panic!("{prefix}_FIFTH_TURN_JOIN"));
                assert!(outcome.is_ok(), "{outcome_tag}");
                false
            },
        }
    }).await.unwrap_or_else(|_| panic!("{prefix}_FIFTH_WAIT_BOUND"));
    assert!(reached, "{reached_tag}");
    let rearm_counter = if matches!(arm, U6bL15mArm::Rearm) {
        let count = fx.session.actor.memory.injection_count.load(std::sync::atomic::Ordering::Relaxed);
        fx.session.actor.memory.context_injected.store(false, std::sync::atomic::Ordering::Relaxed);
        Some(count)
    } else { None };
    u6_interject(fx, SECOND_HUMAN, Vec::new()).await;
    fx.server.enqueue_response("/v1/messages", k2_script(k2lg_n_after_frames("msg_u6b_l15m_sixth")));
    fifth.release();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
        .unwrap_or_else(|_| panic!("{prefix}_FINAL_TURN_BOUND"))
        .unwrap_or_else(|_| panic!("{prefix}_FINAL_TURN_JOIN"));
    assert!(outcome.is_ok(), "{outcome_tag}");
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 6, "{prefix}_SIX_REQUESTS");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    let tag = fx.session.actor.reminder_wrapper_tag();
    let locations: Vec<_> = history.iter().enumerate().filter(|(_, item)|
        crate::session::helpers::memory_context::conversation_has_memory_reminder_item(std::slice::from_ref(*item), tag)).collect();
    match arm {
        U6bL15mArm::Durable => {
            assert_eq!(locations.len(), 1, "H6_L15M_DURABLE_ONE_MEMORY_ITEM");
            let (index, item) = locations[0];
            assert!(index >= covered_at_closure.try_into().expect("covered count fits usize"),
                "H6_L15M_DURABLE_ONE_MEMORY_ITEM");
            assert_eq!(native_content_digest(&serde_json::to_vec(history.first().unwrap()).unwrap()),
                system_before, "H6_L15M_DURABLE_SYSTEM_HEAD_UNCHANGED");
            let (checkpoint, disk) = u6_quiescent_reload(fx, "H6_L15M_DURABLE_DISK_RELOAD_BOUND").await;
            assert!(k2lg_n_values(&disk).contains(&serde_json::to_value(item).unwrap()), "H6_L15M_DURABLE_ITEM_ON_DISK");
            assert!(serde_json::to_string(&bodies[5]).unwrap().contains("## Global memory manifest"),
                "H6_L15M_DURABLE_SIXTH_BODY_MANIFEST");
            assert_eq!(locations.len(), 1, "H6_L15M_DURABLE_NO_SECOND_MEMORY_ITEM");
            assert!(checkpoint.record.history_boundary.items > index.try_into().expect("memory index fits boundary"),
                "H6_L15M_DURABLE_CHECKPOINT_COVERS_ITEM");
        }
        U6bL15mArm::Rearm => {
            assert_eq!(locations.len(), 1, "H6_L15M_REARM_NO_SECOND_MEMORY_ITEM");
            assert_eq!(fx.session.actor.memory.injection_count.load(std::sync::atomic::Ordering::Relaxed),
                rearm_counter.unwrap(), "H6_L15M_REARM_COUNTER_UNCHANGED");
            assert!(fx.session.actor.memory.context_injected.load(std::sync::atomic::Ordering::Relaxed),
                "H6_L15M_REARM_LATCH_CLOSED");
            assert!(serde_json::to_string(&bodies[5]).unwrap().contains("## Global memory manifest"),
                "H6_L15M_REARM_REQUEST_MANIFEST");
        }
    }
    assert_eq!(history.iter().filter(|item| item.text_content().contains(FIRST_HUMAN)).count(), 1,
        "{prefix}_FIRST_HUMAN_RELEASE_ONCE");
    assert_eq!(history.iter().filter(|item| item.text_content().contains(SECOND_HUMAN)).count(), 1,
        "{prefix}_SECOND_HUMAN_RELEASE_ONCE");
    json!({"constructed":false,"arm":prefix,"http_count":bodies.len(),
        "covered_at_closure":covered_at_closure,"memory_items":locations.len(),"system_before":system_before})
}

async fn u6b_l15m_selected_no_checkpoint() -> Value {
    let (mut fixture, chat_rx) = u6b_l15_fixture(None, K2lgNOptions::default(), U6PersistenceControl::default()).await;
    let fx = &mut fixture.fx;
    let initial = fx.session.actor.chat_state_handle.read_native_history().await;
    assert!(initial.as_ref().ok().and_then(|read| read.checkpoint.as_ref()).is_none(),
        "H6_L15M_SELECTED_NO_CHECKPOINT_INITIAL_NATIVE_EMPTY");
    let root = fx.session.directory.path().join("l15m-first-build-memory-v2");
    let storage = crate::session::memory::MemoryStorage::new_for_mode(
        fx.session.actor.tool_context.cwd.as_path(), Some(&root), crate::config::MemoryMode::V2);
    storage.ensure_initialized().expect("H6_L15M_SELECTED_NO_CHECKPOINT_STORAGE_READY");
    fx.session.actor.memory.storage.replace(Some(storage));
    fx.session.actor.memory.context_injected.store(false, std::sync::atomic::Ordering::Relaxed);
    let mut first = fx.server.expect_response_blocked("L15m Selected first build without native checkpoint",
        xai_grok_test_support::InferenceRequestMatcher::foreground(xai_grok_test_support::InferenceEndpoint::Messages),
        k2_script(k2_frames("msg_u6b_l15m_selected_first", vec![(json!({"type":"text","text":"Selected first build complete"}), Vec::new())], "end_turn")));
    let actor = fx.session.actor.clone();
    actor.chat_state_handle.begin_turn_capture();
    actor.chat_state_handle.get_conversation().await;
    actor.chat_state_handle.push_user_message_and_ack(ConversationItem::user("U6b L15m Selected first memory build")).await;
    let domain = fx.session.domain(&fx.server);
    let observations = U6_ARM_OBSERVATIONS.with(Clone::clone);
    let active = super::turn_task::TurnActiveGuard::activate(Some(&actor.session_turn_active));
    let mut running = tokio::task::spawn_local(async move {
        let _active = active;
        let mut salvage = super::length_salvage::LengthSalvage::new(None);
        let mut sampling = Default::default();
        let mut prepare = |_: &mut ConversationRequest, _: &xai_chat_state::RequestTransportBinding| Ok(());
        U6_ARM_OBSERVATIONS.scope(observations, async {
            let outcome = super::switch_projection::HISTORY_S3_TEST_DOMAIN.scope(domain,
                super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(k2_history::NativeHistoryRequirement::Selected,
                    actor.process_conversation_turn_for_native_history_test(KOXD_U6_PROMPT, None, None, None,
                        &mut salvage, &mut sampling, &mut prepare))).await;
            if let Err(error) = &outcome {
                u6_observe(json!({"stage":"direct turn error outcome","error_code":error.data.as_ref()
                    .and_then(|data| data.get("code")).and_then(Value::as_str)}));
            }
            outcome
        }).await
    });
    let _stop = K0Abort(running.abort_handle());
    u6b_l15_install_running(fx, running.abort_handle(), chat_rx).await;
    u6b_u66_progress(&mut running, first.wait_blocked(), "H6_L15M_SELECTED_NO_CHECKPOINT_FIRST_REQUEST_BOUND").await;
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 1, "H6_L15M_SELECTED_NO_CHECKPOINT_ONE_BUILD");
    assert!(bodies[0]["system"].to_string().contains("## Global memory manifest"),
        "H6_L15M_SELECTED_NO_CHECKPOINT_BLOCK_IN_SYSTEM");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(!crate::session::helpers::memory_context::conversation_has_memory_reminder_item(
        &history, fx.session.actor.reminder_wrapper_tag()),
        "H6_L15M_SELECTED_NO_CHECKPOINT_NO_MEMORY_ITEM");
    first.release();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut running).await
        .expect("H6_L15M_SELECTED_NO_CHECKPOINT_FINAL_BOUND").expect("H6_L15M_SELECTED_NO_CHECKPOINT_FINAL_JOIN");
    assert!(outcome.is_ok(), "H6_L15M_SELECTED_NO_CHECKPOINT_FINAL_OUTCOME");
    first.assert_satisfied();
    json!({"constructed":false,"kind":"L15M_SELECTED_FIRST_BUILD_SYSTEM_HEAD",
        "native_checkpoint_before_build":false,"memory_item_added":false,"http_count":bodies.len()})
}

async fn u6b_l15m_configure_inactive_v1(shell: &mut K0Shell, server: &MockInferenceServer) -> tempfile::TempDir {
    let drainer = shell.tasks.pop().expect("H6_L15M_INACTIVE_GUARD_SAMPLER_DRAINER");
    drainer.abort();
    let _ = tokio::time::timeout(K0_TIMEOUT, drainer).await
        .expect("H6_L15M_INACTIVE_GUARD_SAMPLER_DRAINER_JOIN_BOUND");
    let actor = Arc::get_mut(&mut shell.actor).expect("H6_L15M_INACTIVE_GUARD_UNSHARED_SETUP");
    let temp = tempfile::TempDir::new().expect("H6_L15M_INACTIVE_GUARD_MEMORY_DIRECTORY");
    let root = temp.path().join("memory-v1");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("H6_L15M_INACTIVE_GUARD_WORKSPACE_DIRECTORY");
    let storage = crate::session::memory::MemoryStorage::with_paths(root, workspace.clone());
    crate::session::memory::index::init_sqlite_vec();
    let note = temp.path().join("note.md");
    std::fs::write(&note, "# Rust backend conventions\n\nI2_FRESH_V1_MEMORY_MARKER: Rust backend services use bounded tasks.")
        .expect("H6_L15M_INACTIVE_GUARD_MEMORY_NOTE");
    let mut index = crate::session::memory::index::MemoryIndex::open_or_create(
        &workspace.join("index.sqlite"), storage.clone(), crate::config::MemoryIndexConfig::default(), 4,
    ).expect("H6_L15M_INACTIVE_GUARD_MEMORY_INDEX");
    index.reindex_file(&note, "workspace").expect("H6_L15M_INACTIVE_GUARD_MEMORY_REINDEX");
    drop(index);
    actor.memory.configured_mode = Some(crate::config::MemoryMode::Legacy);
    actor.memory.configured_storage = Some(storage.clone());
    actor.memory.storage.replace(Some(storage));
    actor.memory.initial_injection_config.enabled = true;
    actor.memory.backend_params = Some(crate::session::memory::MemoryBackendParams {
        session_id: "u6b-l15m-inactive-guard".into(), embed_config: None,
        embed_base_url: "http://localhost".into(), embed_api_key: None,
        search_config: crate::config::MemorySearchConfig::default(), watcher: None,
        stale_claim_secs: 60, search_source: crate::session::memory::MemorySearchSource::Tool,
        observation_sink: crate::session::memory::noop_memory_observation_sink(),
        embedding_credentials: crate::session::memory::EndpointScopedCredentials::none(),
    });
    actor.memory.context_injected.store(false, std::sync::atomic::Ordering::Relaxed);
    let (sampler_tx, mut sampler_rx) = tokio::sync::mpsc::unbounded_channel();
    actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
        xai_grok_sampler::SamplerConfig {
            base_url: server.url(), model: "test".into(), api_key: Some("test-key".into()),
            api_backend: xai_grok_sampler::ApiBackend::Messages,
            max_completion_tokens: Some(4096), max_retries: Some(0), ..Default::default()
        },
        xai_grok_sampler::RetryPolicy { max_retries: 0, ..Default::default() }, sampler_tx,
    );
    let drainer = shell.actor.clone();
    shell.tasks.push(tokio::task::spawn_local(async move {
        while let Some(event) = sampler_rx.recv().await {
            drainer.handle_sampling_event(event).await;
        }
    }));
    temp
}

async fn u6b_l15m_inactive_guard() -> Value {
    let server = MockInferenceServer::start().await.expect("H6_L15M_INACTIVE_GUARD_SERVER");
    let mut shell = K0Shell::new(&server, false).await;
    let _memory_directory = u6b_l15m_configure_inactive_v1(&mut shell, &server).await;
    assert_eq!(super::switch_projection::select_native_history(), k2_history::NativeHistoryRequirement::Inactive,
        "H6_L15M_INACTIVE_GUARD_PRODUCTION_SELECTOR");
    assert!(shell.actor.memory.initial_injection_config.enabled && shell.actor.memory.backend_params.is_some(),
        "H6_L15M_INACTIVE_GUARD_INJECTION_PRECONDITION");
    assert_eq!(shell.actor.memory.mode(), Some(crate::config::MemoryMode::Legacy),
        "H6_L15M_INACTIVE_GUARD_V1_STORAGE");
    let existing = xai_grok_tools::reminders::wrap_reminder_with_tag(
        "<memory-context>\nI2_EXISTING_MEMORY_EPOCH\n</memory-context>", shell.actor.reminder_wrapper_tag());
    shell.actor.chat_state_handle.push_user_message_and_ack(ConversationItem::system_reminder(existing)).await;
    let before = shell.actor.chat_state_handle.get_conversation().await;
    assert_eq!(before.iter().filter(|item|
        crate::session::helpers::memory_context::conversation_has_memory_reminder_item(
            std::slice::from_ref(*item), shell.actor.reminder_wrapper_tag())).count(), 1,
        "H6_L15M_INACTIVE_GUARD_ITEM_PRESENT");
    server.enqueue_response("/v1/messages", k0_script(vec![json!({"type":"text","text":"Inactive memory guard complete"})], "end_turn"));
    let outcome = tokio::time::timeout(K0_TIMEOUT, shell.actor.handle_prompt(
        "u6b-l15m-inactive-guard",
        vec![acp::ContentBlock::Text(acp::TextContent::new("tell me about Rust backend services conventions"))],
        PromptMode::Agent, None, None, None, None, true, false, None, None, None,
    )).await.expect("H6_L15M_INACTIVE_GUARD_TURN_BOUND");
    assert!(outcome.is_ok(), "H6_L15M_INACTIVE_GUARD_TURN_OUTCOME");
    let history = shell.actor.chat_state_handle.get_conversation().await;
    let system = history.first().expect("H6_L15M_INACTIVE_GUARD_SYSTEM_HEAD").text_content();
    assert!(system.contains("<memory-context>") && system.contains("I2_FRESH_V1_MEMORY_MARKER"),
        "H6_L15M_INACTIVE_GUARD_SYSTEM_HEAD_UPSERTED");
    assert_eq!(server.messages_request_count(), 1, "H6_L15M_INACTIVE_GUARD_ONE_REQUEST");
    json!({"constructed":false,"kind":"L15M_INACTIVE_V1_EXISTING_REMINDER_GUARD",
        "production_selector":"Inactive","existing_memory_item":true,"fresh_system_head_upserted":true,
        "http_count":server.messages_request_count(),"embedding_configured":false})
}

#[tokio::test(flavor = "current_thread")]
async fn messages_latch_only_teardown_with_nonentry_skill_retains_native_prefix() {
    const SELECTOR: &str = "session::acp_session::native_messages_replay_tests::messages_latch_only_teardown_with_nonentry_skill_retains_native_prefix";
    const ARM: &str = "W03/nonentries-skill-owned-cancel";
    i7yx_report_child_names();
    if std::env::var_os("K2LG_N_CHILD").is_some() {
        tokio::task::LocalSet::new().run_until(async {
            println!("\nK2LG_N_BUILD {}", k2lg_n_compiled_sources());
            let passed = k2lg_n_arm(ARM, async {
                const BODY: &str = "W03 native prefix with a late non-entry skill";
                const SKILL: &str = "W03 late skill after the last held safe point";
                let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
                let mut turn = k2lg_n_handoff(&mut fx, None).await;
                let sender_turn = super::turn_task::TurnActiveGuard::activate(Some(&fx.agent_wide));
                u6b_u66_progress(&mut turn.running, k2lg_n_send(&mut fx, BODY),
                    "K2LG boundary H6_W03_REAL_NATIVE_PRODUCER").await;
                assert_eq!(k2lg_n_natives(&k2lg_n_resident(&fx.session)),
                    vec![k2lg_n_native_text(BODY)], "K2LG boundary H6_W03_NATIVE_PRODUCER_SHAPE");
                drop(sender_turn);
                let mut continuation = fx.server.expect_response_blocked(
                    "W03 last held continuation before the late skill",
                    xai_grok_test_support::InferenceRequestMatcher::foreground(
                        xai_grok_test_support::InferenceEndpoint::Messages,
                    ),
                    k2_script(k2lg_n_closure_frames()),
                );
                fx.session.releases[1].add_permits(1);
                u6b_u66_progress(&mut turn.running, continuation.wait_blocked(),
                    "K2LG boundary H6_W03_LAST_HELD_HTTP").await;
                u6b_u66_progress(&mut turn.running, u6b_l19_skill(&fx.session.actor, SKILL),
                    "K2LG boundary H6_W03_LATE_SKILL_PRODUCER").await;
                {
                    let state = fx.session.actor.state.lock().await;
                    let ledger = &state.message_delivery.held_input;
                    assert!(ledger.latch.is_some() && !ledger.terminal,
                        "K2LG boundary H6_W03_REAL_LATCH_PRECONDITION");
                    assert!(ledger.entries.is_empty(),
                        "K2LG boundary H6_W03_NO_ENTRY_CUSTODY_PRECONDITION");
                }
                assert!(fx.session.actor.pending_interjections.is_empty(),
                    "K2LG boundary H6_W03_NO_RESIDENT_HUMAN_PRECONDITION");
                assert!(!fx.session.actor.plan_mode.lock().has_pending_activation(),
                    "K2LG boundary H6_W03_NO_PENDING_PLAN_PRECONDITION");
                assert_eq!(fx.session.actor.pending_skill_reminders.lock().len(), 1,
                    "K2LG boundary H6_W03_NONENTRY_SKILL_PRECONDITION");
                assert_eq!(k2lg_n_resident(&fx.session).len(), 1,
                    "K2LG boundary H6_W03_NATIVE_PREFIX_PRECONDITION");
                let ops_before = k2lg_n_ops(&fx).len();
                let requests_before = fx.server.messages_request_count();
                tokio::time::timeout(K0_TIMEOUT, k2lg_n_selected(
                    &fx,
                    fx.session.actor.cancel_running_task(crate::session::CancelOptions::default()),
                )).await.expect("K2LG boundary H6_W03_OWNED_CANCEL_BOUND");
                continuation.release();
                let joined = tokio::time::timeout(K0_TIMEOUT, &mut turn.running).await
                    .expect("K2LG boundary H6_W03_CANCELLED_TURN_BOUND");
                assert!(joined.is_err_and(|error| error.is_cancelled()),
                    "K2LG boundary H6_W03_OWNED_CANCEL_OUTCOME");
                u6b_l19_ended_fence(&fx).await;
                assert!(fx.session.actor.state.lock().await.running_task.is_none(),
                    "K2LG boundary H6_W03_OWNED_TEARDOWN_COMPLETED");
                assert_eq!(k2lg_n_resident(&fx.session).len(), 1,
                    "K2LG boundary H6_W03_NONENTRY_NATIVE_PREFIX_WITHHELD");
                assert_eq!(k2lg_n_native_persists(&k2lg_n_ops(&fx)[ops_before..]), 0,
                    "K2LG boundary H6_W03_NONENTRY_NATIVE_PREFIX_NOT_APPENDED");
                assert_eq!(fx.session.actor.pending_skill_reminders.lock().len(), 1,
                    "K2LG boundary H6_W03_LATE_SKILL_RETAINED");
                assert!(!fx.session.actor.chat_state_handle.get_conversation().await.iter()
                    .any(|item| item.text_content().contains(SKILL)),
                    "K2LG boundary H6_W03_LATE_SKILL_NOT_APPENDED");
                assert_eq!(fx.server.messages_request_count(), requests_before,
                    "K2LG boundary H6_W03_NO_TEARDOWN_REQUEST");
            }).await;
            assert!(passed, "K2LG boundary H6_W03_NONENTRY_WITNESS_FAILED");
        }).await;
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (mut command, parent_names) =
        i7yx_shell_child_command(I7yxChildRoute::NativeN, home.path());
    command.args(["--exact", SELECTOR, "--nocapture"])
        .stdin(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut command);
    let output = tokio::task::spawn_blocking(move || command.output())
        .await.unwrap().unwrap();
    i7yx_assert_child_hygiene(I7yxChildRoute::NativeN, &output.stdout, &parent_names);
    i7yx_print_proof_records(&output.stdout, I7yxChildRoute::NativeN);
    assert!(output.status.success(), "K2LG boundary W03 witness child failed: {}",
        i7yx_output_diagnostic(I7yxChildRoute::NativeN, &output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
}

#[tokio::test(flavor = "current_thread")]
async fn messages_leftover_pending_is_projected_out() {
    tokio::task::LocalSet::new()
        .run_until(super::native_history_policy_tests::k3q_leftover_pending_projected_out())
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn messages_live_pending_blocks_destructive_compaction() {
    tokio::task::LocalSet::new()
        .run_until(super::native_history_policy_tests::k3q_pre_loss_compaction_and_backing())
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn messages_local_queue_joins_held_input() {
    const CHILD: &str = "K3Q_U3_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let home = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .env(CHILD, "1")
            .env("GROK_HOME", home.path())
            .args([
                "--exact",
                "session::acp_session::native_messages_replay_tests::messages_local_queue_joins_held_input",
                "--nocapture",
            ])
            .stdin(std::process::Stdio::null());
        xai_tty_utils::detach_std_command(&mut command);
        let output = tokio::task::spawn_blocking(move || command.output())
            .await
            .unwrap()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("running 1 test"),
            "local queue join child failed: status={:?} stdout={} stderr={}",
            output.status.code(),
            stdout,
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::task::LocalSet::new().run_until(async {
        let observations = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let steppers = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        U6_ARM_OBSERVATIONS.scope(observations, U6_ARM_STEPPERS.scope(steppers, async {
        let (mut fixture, mut turn, _, continuation) = u6_u63_prepare_fixture(None).await;
        let _stop = K0Abort(turn.running.abort_handle());
        let fx = &mut fixture.fx;
        let text = "K3Q local queue join";
        k2lg_n_selected(
            fx,
            fx.session.actor.hold_or_push_user_item(
                ConversationItem::user(text),
                super::interjection::HeldSource::HumanInterjection,
            ),
        )
        .await
        .expect("the local item joins the held-input ledger");
        {
            let state = fx.session.actor.state.lock().await;
            let ledger = &state.message_delivery.held_input;
            assert!(
                ledger.entries.iter().any(|entry| {
                    matches!(&entry.body, super::interjection::HeldBody::Owned(item)
                        if item.text_content().contains(text))
                }),
                "the held-input witness keeps the joined local item"
            );
            assert_eq!(
                ledger.shared_owner_debt.local_entries,
                ledger.entries.len(),
                "the debt counts the local queue, not a global owner"
            );
            assert!(
                !ledger.shared_owner_debt.global_owner_credited,
                "the global shared-control owner stays uncredited"
            );
        }
        continuation.release();
        turn.running.abort();
        let _ = turn.running.await;
        })).await;
    }).await;
}
