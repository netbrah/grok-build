//! Tracing layer for `target: "sampling_log"` → `~/.grok/logs/sampling.jsonl`.
//! Enable with `--log-sampling` or `GROK_LOG_SAMPLING=1`.

use std::sync::Mutex;

use tracing::Subscriber;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::registry::LookupSpan;

use xai_grok_config::grok_home;

use crate::instrumentation::{NoOpLayer, TargetFilterLayer};

const ENV_VAR: &str = "GROK_LOG_SAMPLING";
const LOG_FILE: &str = "sampling.jsonl";
const TARGET: &str = "sampling_log";

static GUARD: std::sync::OnceLock<Mutex<Option<tracing_appender::non_blocking::WorkerGuard>>> =
    std::sync::OnceLock::new();

pub fn layer<S>() -> Box<dyn Layer<S> + Send + Sync>
where
    S: Subscriber + for<'span> LookupSpan<'span> + Send + Sync + 'static,
{
    if !std::env::var(ENV_VAR).is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "on")) {
        return Box::new(NoOpLayer::new());
    }

    let path = grok_home().join(crate::unified_log::LOG_DIR).join(LOG_FILE);

    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("failed to create sampling log dir: {e}");
        return Box::new(NoOpLayer::new());
    }

    if crate::unified_log::file_size(&path) >= crate::unified_log::MAX_SIZE {
        crate::unified_log::trim_file(&path);
    }

    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("failed to open sampling log: {e}");
            return Box::new(NoOpLayer::new());
        }
    };

    let (non_blocking, guard) = tracing_appender::non_blocking(file);
    let guard_slot = GUARD.get_or_init(|| Mutex::new(None));
    if let Ok(mut slot) = guard_slot.lock() {
        *slot = Some(guard);
    }

    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(false) // `spans` array already carries the full ancestor list
        .with_ansi(false)
        .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
        .with_target(false)
        .with_writer(BoxMakeWriter::new(non_blocking));

    Box::new(TargetFilterLayer::new(fmt_layer, TARGET))
}

#[cfg(test)]
mod tests {
    use tracing::Instrument;
    use xai_grok_sampler::{ApiBackend, AuthScheme, SamplerConfig, SamplingClient};
    const MDS_SECRET: &str = "MSS_STOP_EXTENSION_SENTINEL";
    const MDS_ORDINARY: &str = "MSS_ORDINARY_DIAG_SENTINEL";

    struct MdsSuccessCase {
        name: &'static str,
        frames: Vec<(String, String)>,
    }

    fn mds_start(details: &str) -> String {
        format!(
            r#"{{"type":"message_start","message":{{"id":"mds-message","type":"message","role":"assistant","model":"mds-model","content":[],"usage":{{"input_tokens":17,"output_tokens":0}}{details}}}}}"#
        )
    }

    fn mds_terminal(details: &str, extra: &str) -> String {
        format!(
            r#"{{"type":"message_delta","delta":{{"stop_reason":"refusal","stop_sequence":"ordinary-end"{details}}},"usage":{{"input_tokens":19,"output_tokens":23,"cache_read_input_tokens":11,"cache_creation_input_tokens":13}}{extra}}}"#
        )
    }

    fn mds_success_cases() -> Vec<MdsSuccessCase> {
        const DETAILS: &str = r#","stop_details":{"type":"refusal","category":"ordinary-category","explanation":"MSS_ORDINARY_DIAG_SENTINEL","fallback_has_prefill_claim":true,"resumable":false}"#;
        const TOKEN: &str = r#","stop_details":{"type":"refusal","category":"ordinary-category","explanation":"MSS_ORDINARY_DIAG_SENTINEL","fallback_credit_token":"MSS_STOP_EXTENSION_SENTINEL","resumable":false}"#;
        const MASKED: &str = r#","stop_details":{"type":"refusal","category":"ordinary-category","explanation":"MSS_ORDINARY_DIAG_SENTINEL","fallback_credit_token":"[REDACTED_STOP_EXTENSION]","resumable":false}"#;
        const ESCAPED: &str = r#", "stop\u005fdetails" : {"fallback_credit_\u0074oken" : "MSS_STOP_EXTENSION_SEN\u0054INEL", "explanation":"MSS_ORDINARY_DIAG_SENTINEL"}"#;
        const ESCAPED_MASKED: &str = r#", "stop\u005fdetails" : {"fallback_credit_\u0074oken" : "[REDACTED_STOP_EXTENSION]", "explanation":"MSS_ORDINARY_DIAG_SENTINEL"}"#;
        const DUPLICATES: &str = r#","stop_details":{"fallback_credit_token":"MSS_STOP_EXTENSION_SENTINEL","n":1e+02,"n":-0,"fallback_credit_token":{"nested":["MSS_STOP_EXTENSION_SENTINEL",false]},"explanation":"MSS_ORDINARY_DIAG_SENTINEL"}"#;
        const DUPLICATES_MASKED: &str = r#","stop_details":{"fallback_credit_token":"[REDACTED_STOP_EXTENSION]","n":1e+02,"n":-0,"fallback_credit_token":"[REDACTED_STOP_EXTENSION]","explanation":"MSS_ORDINARY_DIAG_SENTINEL"}"#;
        const UNRELATED: &str = r#","tool_input":{"stop_details":{"fallback_credit_token":"MSS_ORDINARY_DIAG_SENTINEL"}},"text":"fallback_credit_token stop_details","number":1.2300e+02,"number":-0,"escaped":"\u0061\/b""#;
        let variants = [
            ("ordinary", "", "", DETAILS, DETAILS, UNRELATED, UNRELATED),
            (
                "null-details",
                ",\"stop_details\":null",
                ",\"stop_details\":null",
                ",\"stop_details\":null",
                ",\"stop_details\":null",
                "",
                "",
            ),
            (
                "null-token",
                "",
                "",
                r#","stop_details":{"fallback_credit_token":null}"#,
                r#","stop_details":{"fallback_credit_token":null}"#,
                "",
                "",
            ),
            ("start-token", TOKEN, MASKED, DETAILS, DETAILS, "", ""),
            (
                "terminal-token",
                "",
                "",
                TOKEN,
                MASKED,
                UNRELATED,
                UNRELATED,
            ),
            (
                "escaped-selected",
                ESCAPED,
                ESCAPED_MASKED,
                ESCAPED,
                ESCAPED_MASKED,
                "",
                "",
            ),
            (
                "duplicate-selected",
                "",
                "",
                DUPLICATES,
                DUPLICATES_MASKED,
                "",
                "",
            ),
            (
                "top-level-selected",
                "",
                "",
                DETAILS,
                DETAILS,
                TOKEN,
                MASKED,
            ),
            (
                "empty-token",
                "",
                "",
                r#","stop_details":{"fallback_credit_token":""}"#,
                r#","stop_details":{"fallback_credit_token":"[REDACTED_STOP_EXTENSION]"}"#,
                "",
                "",
            ),
            (
                "boolean-token",
                "",
                "",
                r#","stop_details":{"fallback_credit_token":false}"#,
                r#","stop_details":{"fallback_credit_token":"[REDACTED_STOP_EXTENSION]"}"#,
                "",
                "",
            ),
            (
                "numeric-token",
                "",
                "",
                r#","stop_details":{"fallback_credit_token":-0.00e+2}"#,
                r#","stop_details":{"fallback_credit_token":"[REDACTED_STOP_EXTENSION]"}"#,
                "",
                "",
            ),
        ];
        variants
            .into_iter()
            .map(|(name, start, start_view, terminal, terminal_view, extra, extra_view)| {
                let mut frames = vec![(mds_start(start), mds_start(start_view))];
                for raw in [
                    r#"{"type":"content_block_start","index":7,"content_block":{"type":"text","text":""}}"#,
                    r#"{ "type":"content_block_delta", "index":7, "delta":{"type":"text_delta","text":"MSS_ORDINARY_DIAG_SENTINEL"}, "ordinary":1e+02, "ordinary":-0 }"#,
                    r#"{"type":"content_block_stop","index":7}"#,
                ] {
                    frames.push((raw.to_owned(), raw.to_owned()));
                }
                frames.push((mds_terminal(terminal, extra), mds_terminal(terminal_view, extra_view)));
                frames.push((r#"{"type":"message_stop"}"#.to_owned(), r#"{"type":"message_stop"}"#.to_owned()));
                MdsSuccessCase { name, frames }
            })
            .collect()
    }

    #[derive(Clone, Default)]
    struct MdsLog(std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>);

    struct MdsRecordWriter {
        log: MdsLog,
        bytes: Vec<u8>,
    }

    impl std::io::Write for MdsRecordWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Drop for MdsRecordWriter {
        fn drop(&mut self) {
            self.log
                .0
                .lock()
                .unwrap()
                .push(std::mem::take(&mut self.bytes));
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for MdsLog {
        type Writer = MdsRecordWriter;

        fn make_writer(&'a self) -> Self::Writer {
            MdsRecordWriter {
                log: self.clone(),
                bytes: Vec::new(),
            }
        }
    }

    struct MdsOutput {
        text: String,
        records: Vec<String>,
    }

    impl MdsLog {
        fn output(&self, observations: &[MdsObservation]) -> MdsOutput {
            let records: Vec<_> = self
                .0
                .lock()
                .unwrap()
                .iter()
                .map(|bytes| {
                    let text = String::from_utf8(bytes.clone()).unwrap();
                    mds_normalize_endpoints(text, observations)
                })
                .collect();
            let text = records.concat();
            MdsOutput { text, records }
        }
    }

    struct MdsServer {
        base_url: String,
        shutdown: tokio::sync::oneshot::Sender<()>,
        task: tokio::task::JoinHandle<()>,
        requests: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    }

    impl MdsServer {
        async fn finish(mut self) -> Vec<serde_json::Value> {
            let _ = self.shutdown.send(());
            match tokio::time::timeout(std::time::Duration::from_secs(10), &mut self.task).await {
                Ok(result) => result.expect("MDS_HTTP_SERVER_COMPLETED"),
                Err(_) => {
                    self.task.abort();
                    let _ = self.task.await;
                    panic!("MDS_HTTP_SHUTDOWN_DEADLINE");
                }
            }
            self.requests.lock().unwrap().clone()
        }
    }

    async fn mds_server(body: Vec<u8>, content_type: &'static str) -> MdsServer {
        let marker = body
            .windows(18)
            .position(|w| w == &MDS_SECRET.as_bytes()[..18]);
        let cut = marker
            .map_or(body.len() / 2, |offset| offset + 9)
            .max(3)
            .min(body.len());
        let chunks = [body[..cut].to_vec(), body[cut..].to_vec()];
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = requests.clone();
        let app = axum::Router::new().route(
            "/v1/messages",
            axum::routing::post(move |request: axum::body::Bytes| {
                let chunks = chunks.clone();
                let captured = captured.clone();
                async move {
                    captured
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&request).unwrap());
                    let stream =
                        tokio_stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>));
                    axum::response::Response::builder()
                        .status(200)
                        .header("content-type", content_type)
                        .body(axum::body::Body::from_stream(stream))
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        MdsServer {
            base_url,
            shutdown,
            task,
            requests,
        }
    }

    fn mds_sse(frames: &[(String, String)]) -> Vec<u8> {
        let mut wire = String::from("\u{feff}");
        for (raw, _) in frames {
            for line in raw.split('\n') {
                wire.push_str("data: ");
                wire.push_str(line);
                wire.push_str("\r\n");
            }
            wire.push_str("\r\n");
        }
        wire.push_str("data: [DONE]\r\n\r\n");
        wire.into_bytes()
    }

    fn mds_config(base_url: &str) -> SamplerConfig {
        SamplerConfig {
            api_key: Some("mds-synthetic-test-key".to_owned()),
            base_url: base_url.to_owned(),
            model: "mds-model".to_owned(),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::Bearer,
            max_completion_tokens: Some(128),
            max_retries: Some(0),
            ..SamplerConfig::default()
        }
    }

    struct MdsObservation {
        name: &'static str,
        base_url: String,
        outcome: serde_json::Value,
    }

    // Infer the public request type without a sampler-types dependency in telemetry.
    fn mds_default_request<T: Default, R>(_: impl FnOnce(T) -> R) -> T {
        T::default()
    }

    fn mds_success_observations(dispatch: &tracing::Dispatch) -> Vec<MdsObservation> {
        tracing::dispatcher::with_default(dispatch, || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut observations = Vec::new();
                for case in mds_success_cases() {
                    let span = tracing::info_span!(target: "sampling_log", "mds_diagnostic", case = case.name);
                    let observed = async {
                        let server = mds_server(mds_sse(&case.frames), "text/event-stream").await;
                        let base_url = server.base_url.clone();
                        let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                            let client = SamplingClient::new(mds_config(&base_url)).map_err(|e| e.to_string())?;
                            let mut request = mds_default_request(|r| client.conversation_messages(r));
                            request.items.push(serde_json::from_value(serde_json::json!({
                                "type": "user", "content": [{"type": "text", "text": "diagnostic request"}]
                            })).map_err(|e| e.to_string())?);
                            let response = client.conversation_collect_with_idle_timeout(
                                request, std::time::Duration::from_secs(5),
                            ).await.map_err(|e| e.to_string())?;
                            Ok::<_, String>(serde_json::json!({
                                "text": response.assistant().map(|a| a.content.as_ref()),
                                "stop_reason": response.stop_reason,
                                "usage": response.usage,
                                "raw_stop_reason": response.raw_stop_reason,
                                "stop_sequence": response.stop_sequence,
                                "message_id": response.message_id,
                                "stop_message": response.stop_message,
                            }))
                        }).await;
                        let requests = server.finish().await;
                        assert_eq!(requests.len(), 1, "MDS_HTTP_REQUEST_COUNT: {} result={result:?}", case.name);
                        assert_eq!(requests[0]["model"], "mds-model");
                        assert_eq!(requests[0]["stream"], true);
                        let outcome = result.expect("MDS_HTTP_DEADLINE").expect("MDS_STRICT_PARSER_SUCCESS");
                        let terminal: serde_json::Value = serde_json::from_str(&case.frames[4].0).unwrap();
                        assert_eq!(outcome, serde_json::json!({
                            "text": MDS_ORDINARY,
                            "stop_reason": "content_filter",
                            "usage": {
                                "prompt_tokens": 43, "completion_tokens": 23, "total_tokens": 66,
                                "reasoning_tokens": 0, "cached_prompt_tokens": 11,
                                "cache_creation_prompt_tokens": 13, "cache_creation_5m_input_tokens": 0,
                                "cache_creation_1h_input_tokens": 0,
                            },
                            "raw_stop_reason": "refusal", "stop_sequence": "ordinary-end",
                            "message_id": "mds-message",
                            "stop_message": terminal.pointer("/delta/stop_details/explanation"),
                        }), "MDS_SUCCESS_OUTCOME: {}", case.name);
                        MdsObservation { name: case.name, base_url, outcome }
                    }.instrument(span).await;
                    observations.push(observed);
                }
                observations
            })
        })
    }

    fn mds_normalize_endpoints(mut output: String, observations: &[MdsObservation]) -> String {
        for observation in observations {
            output = output.replace(&observation.base_url, "http://127.0.0.1:PORT/v1");
        }
        output
    }

    fn mds_ordinary_success() -> (MdsOutput, Vec<MdsObservation>) {
        let buffer = MdsLog::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(buffer.clone())
            .finish();
        let observations = mds_success_observations(&tracing::Dispatch::new(subscriber));
        (buffer.output(&observations), observations)
    }
    fn mds_expected_ordinary_success() -> Vec<String> {
        let mut records = Vec::new();
        for case in mds_success_cases() {
            let prefix = format!(
                " INFO mds_diagnostic{{case=\"{}\"}}: sampling_log: ",
                case.name
            );
            records.push(format!("{prefix}event=\"client_new\" base_url=http://127.0.0.1:PORT/v1 model=mds-model api_backend=Messages auth_scheme=Bearer reasoning_effort=\"unset\" has_api_key=true has_bearer_resolver=false has_authorization_header=true has_x_api_key_header=false\n"));
            for (_, view) in case.frames {
                records.push(format!(
                    "{prefix}event=\"sse_chunk\" backend=\"messages\" data={view}\n"
                ));
            }
        }
        records
    }

    fn mds_assert_ordinary_success(output: &MdsOutput, observations: &[MdsObservation]) {
        let records: Vec<_> = output
            .records
            .iter()
            .filter(|record| {
                record.contains("event=\"client_new\"") || record.contains("event=\"sse_chunk\"")
            })
            .cloned()
            .collect();
        println!(
            "MDS_SUCCESS_OBSERVATION {}",
            serde_json::json!({
                "mode": "ordinary", "records": records, "output": output.text,
                "cases": observations.iter().map(|o| serde_json::json!({
                    "name": o.name, "outcome": o.outcome, "status": 200,
                })).collect::<Vec<_>>(),
            })
        );
        let expected = mds_expected_ordinary_success();
        assert_eq!(
            records.len(),
            expected.len(),
            "MDS_ORDINARY_LIVE_RECORD_COUNT"
        );
        assert_eq!(
            &records[..21],
            &expected[..21],
            "MDS_ORDINARY_SSE_PRESERVATION"
        );
        println!("MDS_ORDINARY_PRESERVATION_PASSED cases=3 records=21");
        assert!(
            !output.text.contains("MSS_STOP_EXTENSION_"),
            "MDS_SSE_MARKER_ABSENCE ordinary output"
        );
        assert_eq!(records, expected, "MDS_SELECTIVE_SSE_PRESERVATION");
        println!(
            "MDS_SUCCESS_ASSERTIONS_PASSED mode=ordinary cases={}",
            observations.len()
        );
    }

    fn mds_run_child(selector: &str) {
        use tokio::io::AsyncReadExt;
        let home = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let scope = xai_tty_utils::ProcessScope::new();
            let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([selector, "--exact", "--nocapture"])
                .env_clear()
                .env("HOME", home.path())
                .env("GROK_HOME", home.path().join("grok"))
                .env("XDG_CONFIG_HOME", home.path().join("config"))
                .env("TMPDIR", home.path())
                .env("GROK_LOG_SAMPLING", "1")
                .env("MDS_LOG_CHILD_SELECTOR", selector)
                .env("RUST_MIN_STACK", "67108864")
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .kill_on_drop(true)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let (mut child, group) = scope.spawn(command).expect("MDS_CHILD_ENROLLED");
            let mut stdout = child.stdout.take().unwrap();
            let mut stderr = child.stderr.take().unwrap();
            let mut output = Vec::new();
            let mut errors = Vec::new();
            let result = tokio::time::timeout(std::time::Duration::from_secs(120), async {
                tokio::join!(
                    child.wait(),
                    stdout.read_to_end(&mut output),
                    stderr.read_to_end(&mut errors)
                )
            })
            .await;
            let (status, read_output, read_errors) = match result {
                Ok(result) => result,
                Err(_) => {
                    scope.kill_all();
                    tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
                        .await
                        .expect("MDS_CHILD_REAP_DEADLINE")
                        .expect("MDS_CHILD_REAP");
                    panic!("MDS_CHILD_DEADLINE");
                }
            };
            read_output.unwrap();
            read_errors.unwrap();
            let status = status.unwrap();
            drop(group);
            assert_eq!(scope.live_count(), 0, "MDS_CHILD_REAPED");
            let output = String::from_utf8(output).unwrap();
            let errors = String::from_utf8(errors).unwrap();
            println!(
                "MDS_CHILD_OUTPUT {}",
                serde_json::json!({
                    "selector": selector, "exit": status.code(), "stdout": output, "stderr": errors,
                    "home_is_temporary": true, "environment_cleared": true,
                })
            );
            assert!(
                output.contains("running 1 test"),
                "MDS_CHILD_EXACT_SELECTION"
            );
            assert_eq!(
                output.matches("MDS_CHILD_ASSERTIONS_BEGIN").count(),
                1,
                "MDS_CHILD_ASSERTION_LIVENESS"
            );
            assert!(status.success(), "MDS_CHILD_FAILURE: {selector}");
            assert!(
                output.contains("MDS_CHILD_ASSERTIONS_PASSED"),
                "MDS_CHILD_COMPLETED_ASSERTIONS"
            );
        });
    }

    fn mds_file_success() -> (String, Vec<MdsObservation>) {
        use tracing_subscriber::layer::SubscriberExt;
        let subscriber = tracing_subscriber::registry().with(super::layer());
        let dispatch = tracing::Dispatch::new(subscriber);
        let observations = mds_success_observations(&dispatch);
        drop(dispatch);
        let guard = super::GUARD
            .get()
            .and_then(|slot| slot.lock().unwrap().take());
        let had_guard = guard.is_some();
        drop(guard);
        let path = super::grok_home().join("logs").join("sampling.jsonl");
        assert!(path.is_file(), "MDS_SINK_ENABLED_FILE");
        assert!(had_guard, "MDS_SINK_WORKER_GUARD");
        let output = std::fs::read_to_string(path).unwrap();
        assert!(!output.is_empty(), "MDS_SINK_NONEMPTY_AFTER_GUARD_DROP");
        (mds_normalize_endpoints(output, &observations), observations)
    }
    fn mds_assert_file_success(output: &str, observations: &[MdsObservation]) {
        let records: Vec<serde_json::Value> = output
            .lines()
            .map(|line| {
                let mut record: serde_json::Value = serde_json::from_str(line).unwrap();
                assert!(
                    record["timestamp"].is_string(),
                    "MDS_SINK_TIMESTAMP_PRESENT"
                );
                record.as_object_mut().unwrap().remove("timestamp");
                record
            })
            .filter(|record| {
                matches!(
                    record["fields"]["event"].as_str(),
                    Some("client_new" | "sse_chunk")
                )
            })
            .collect();
        println!(
            "MDS_SUCCESS_OBSERVATION {}",
            serde_json::json!({
                "mode": "sampling_file", "records": records, "output": output,
                "cases": observations.iter().map(|o| serde_json::json!({
                    "name": o.name, "outcome": o.outcome, "status": 200,
                })).collect::<Vec<_>>(),
            })
        );
        let mut expected = Vec::new();
        for case in mds_success_cases() {
            let spans = serde_json::json!([{ "case": case.name, "name": "mds_diagnostic" }]);
            expected.push(serde_json::json!({
                "level": "INFO", "spans": spans,
                "fields": {
                    "event": "client_new", "base_url": "http://127.0.0.1:PORT/v1",
                    "model": "mds-model", "api_backend": "Messages", "auth_scheme": "Bearer",
                    "reasoning_effort": "unset", "has_api_key": true, "has_bearer_resolver": false,
                    "has_authorization_header": true, "has_x_api_key_header": false,
                },
            }));
            for (_, view) in case.frames {
                expected.push(serde_json::json!({
                    "level": "INFO", "spans": spans,
                    "fields": { "event": "sse_chunk", "backend": "messages", "data": view },
                }));
            }
        }
        assert_eq!(records.len(), expected.len(), "MDS_SINK_LIVE_RECORD_COUNT");
        assert_eq!(
            &records[..21],
            &expected[..21],
            "MDS_SINK_ORDINARY_SSE_PRESERVATION"
        );
        println!("MDS_SINK_ORDINARY_PRESERVATION_PASSED cases=3 records=21");
        assert!(
            !output.contains("MSS_STOP_EXTENSION_"),
            "MDS_SSE_MARKER_ABSENCE sampling_file output"
        );
        assert_eq!(records, expected, "MDS_SINK_SELECTIVE_SSE_PRESERVATION");
        println!(
            "MDS_SUCCESS_ASSERTIONS_PASSED mode=sampling_file cases={}",
            observations.len()
        );
    }

    #[test]
    fn mss_u4_stop_extension_not_in_success_sse_logs() {
        const SELECTOR: &str = "sampling_log::tests::mss_u4_stop_extension_not_in_success_sse_logs";
        if std::env::var("MDS_LOG_CHILD_SELECTOR").as_deref() != Ok(SELECTOR) {
            mds_run_child(SELECTOR);
            return;
        }
        println!("MDS_CHILD_ASSERTIONS_BEGIN {SELECTOR}");
        let (ordinary, ordinary_cases) = mds_ordinary_success();
        let (file, file_cases) = mds_file_success();
        let ordinary_outcomes: Vec<_> = ordinary_cases
            .iter()
            .map(|o| (&o.name, &o.outcome))
            .collect();
        let file_outcomes: Vec<_> = file_cases.iter().map(|o| (&o.name, &o.outcome)).collect();
        assert_eq!(
            ordinary_outcomes, file_outcomes,
            "MDS_OUTPUT_MODE_PARSER_PARITY"
        );
        mds_assert_file_success(&file, &file_cases);
        mds_assert_ordinary_success(&ordinary, &ordinary_cases);
        println!("MDS_CHILD_ASSERTIONS_PASSED {SELECTOR}");
    }

    const MDS_UNAVAILABLE: &str = "[UNAVAILABLE: unprojectable sensitive stop metadata]";

    struct MdsStreamErrorCase {
        name: &'static str,
        raw: String,
        view: String,
        required_error: &'static str,
        projection_reason: Option<&'static str>,
    }

    fn mds_error_delta(details: &str, usage: &str) -> String {
        format!(
            r#"{{"type":"message_delta","delta":{{"stop_reason":"refusal","stop_sequence":"ordinary-end"{details}}},"usage":{usage},"ordinary":1e+02,"ordinary":-0}}"#
        )
    }

    fn mds_stream_error_cases() -> Vec<MdsStreamErrorCase> {
        const ORDINARY: &str = r#","stop_details":{"type":"refusal","category":"ordinary-category","explanation":"MSS_ORDINARY_DIAG_SENTINEL","resumable":false}"#;
        const SECRET: &str = r#","stop_details":{"type":"refusal","category":"ordinary-category","explanation":"MSS_ORDINARY_DIAG_SENTINEL","fallback_credit_token":"MSS_STOP_EXTENSION_SENTINEL","resumable":false}"#;
        const MASKED: &str = r#","stop_details":{"type":"refusal","category":"ordinary-category","explanation":"MSS_ORDINARY_DIAG_SENTINEL","fallback_credit_token":"[REDACTED_STOP_EXTENSION]","resumable":false}"#;
        const ESCAPED: &str = r#", "stop\u005fdetails" : {"explanation":"MSS_ORDINARY_DIAG_SENTINEL", "fallback_credit_\u0074oken":"MSS_STOP_EXTENSION_SEN\u0054INEL"}"#;
        const ESCAPED_VIEW: &str = r#", "stop\u005fdetails" : {"explanation":"MSS_ORDINARY_DIAG_SENTINEL", "fallback_credit_\u0074oken":"[REDACTED_STOP_EXTENSION]"}"#;
        const WRONG: &str = r#"{"output_tokens":"MSS_ORDINARY_DIAG_SENTINEL"}"#;
        const MISSING: &str = "missing field `output_tokens`";
        const WRONG_REASON: &str =
            "invalid type: string \"MSS_ORDINARY_DIAG_SENTINEL\", expected u32";
        let ordinary_cases = [
            ("stream-ordinary-missing", ORDINARY, ORDINARY, "{}", MISSING),
            (
                "stream-ordinary-wrong-type",
                ORDINARY,
                ORDINARY,
                WRONG,
                WRONG_REASON,
            ),
            ("stream-mixed-missing", SECRET, MASKED, "{}", MISSING),
            (
                "stream-mixed-wrong-type",
                SECRET,
                MASKED,
                WRONG,
                WRONG_REASON,
            ),
            (
                "stream-escaped-selected",
                ESCAPED,
                ESCAPED_VIEW,
                "{}",
                MISSING,
            ),
            (
                "stream-null-selected",
                r#","stop_details":{"fallback_credit_token":null}"#,
                r#","stop_details":{"fallback_credit_token":null}"#,
                "{}",
                MISSING,
            ),
            (
                "stream-structured-selected",
                r#","stop_details":{"fallback_credit_token":{"nested":["MSS_STOP_EXTENSION_SENTINEL",false]},"explanation":"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                r#","stop_details":{"fallback_credit_token":"[REDACTED_STOP_EXTENSION]","explanation":"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                "{}",
                MISSING,
            ),
            (
                "stream-selected-number-error",
                r#","stop_details":{"fallback_credit_token":1e99999,"explanation":"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                r#","stop_details":{"fallback_credit_token":"[REDACTED_STOP_EXTENSION]","explanation":"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                r#"{"output_tokens":2}"#,
                "number out of range",
            ),
        ];
        let mut cases: Vec<_> = ordinary_cases
            .into_iter()
            .map(
                |(name, raw, view, usage, required_error)| MdsStreamErrorCase {
                    name,
                    raw: mds_error_delta(raw, usage),
                    view: mds_error_delta(view, usage),
                    required_error,
                    projection_reason: None,
                },
            )
            .collect();
        for (name, raw, view, required_error, projection_reason) in [
            (
                "stream-ordinary-truncated",
                r#"{"type":"message_delta","ordinary":"MSS_ORDINARY_DIAG_SENTINEL"#,
                r#"{"type":"message_delta","ordinary":"MSS_ORDINARY_DIAG_SENTINEL"#,
                "EOF while parsing a string",
                None,
            ),
            (
                "stream-sensitive-truncated",
                r#"{"type":"message_delta","delta":{"stop_details":{"fallback_credit_token":"MSS_STOP_EXTENSION_SENTINEL"#,
                MDS_UNAVAILABLE,
                "EOF while parsing a string",
                Some("truncated_selected_value"),
            ),
            (
                "stream-ordinary-invalid-key",
                r#"{"type":"message_delta","delta":{"stop_details":{"ordinary_\uZZZZ":"MSS_ORDINARY_DIAG_SENTINEL"}},"usage":{}}"#,
                r#"{"type":"message_delta","delta":{"stop_details":{"ordinary_\uZZZZ":"MSS_ORDINARY_DIAG_SENTINEL"}},"usage":{}}"#,
                "invalid escape",
                None,
            ),
            (
                "stream-sensitive-invalid-key",
                r#"{"type":"message_delta","delta":{"stop_details":{"fallback_credit_\uZZZZ":"MSS_STOP_EXTENSION_SENTINEL"}},"usage":{}}"#,
                MDS_UNAVAILABLE,
                "invalid escape",
                Some("ambiguous_relevant_key"),
            ),
        ] {
            cases.push(MdsStreamErrorCase {
                name,
                raw: raw.to_owned(),
                view: view.to_owned(),
                required_error,
                projection_reason,
            });
        }
        cases.push(MdsStreamErrorCase {
            name: "stream-mixed-trailing-malformed",
            raw: format!(r#"{{"type":"message_delta","delta":{{"stop_reason":"refusal"{SECRET}}},"usage":{{}},"ordinary":"#),
            view: format!(r#"{{"type":"message_delta","delta":{{"stop_reason":"refusal"{MASKED}}},"usage":{{}},"ordinary":"#),
            required_error: "EOF while parsing a value",
            projection_reason: None,
        });
        cases.push(MdsStreamErrorCase {
            name: "stream-multiline-mixed",
            raw: format!("{{\n\"type\":\"message_delta\",\n\"delta\":{{\"stop_reason\":\"refusal\"{SECRET}}},\n\"usage\":{{}}\n}}"),
            view: format!("{{\n\"type\":\"message_delta\",\n\"delta\":{{\"stop_reason\":\"refusal\"{MASKED}}},\n\"usage\":{{}}\n}}"),
            required_error: MISSING,
            projection_reason: None,
        });
        cases.extend(mds_stream_structural_error_cases());
        cases.extend(mds_stream_split_scalar_cases());
        cases
    }

    fn mds_stream_error_observations(dispatch: &tracing::Dispatch) -> Vec<MdsObservation> {
        tracing::dispatcher::with_default(dispatch, || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut observations = Vec::new();
                for case in mds_stream_error_cases() {
                    let span = tracing::info_span!(target: "sampling_log", "mds_diagnostic", case = case.name);
                    let observed = async {
                        let start = mds_start("");
                        let frames = vec![(start.clone(), start), (case.raw.clone(), case.view.clone())];
                        let server = mds_server(mds_sse(&frames), "text/event-stream").await;
                        let base_url = server.base_url.clone();
                        let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                            let client = SamplingClient::new(mds_config(&base_url)).map_err(|e| e.to_string())?;
                            let mut request = mds_default_request(|r| client.conversation_messages(r));
                            request.items.push(serde_json::from_value(serde_json::json!({
                                "type": "user", "content": [{"type": "text", "text": "diagnostic request"}]
                            })).map_err(|e| e.to_string())?);
                            let (mut events, _) = client.conversation_stream_messages(request)
                                .await.map_err(|e| e.to_string())?;
                            let first = std::future::poll_fn(|cx| events.as_mut().poll_next(cx)).await
                                .ok_or("MDS_STREAM_START_ABSENT")?.map_err(|e| e.to_string())?;
                            let first = serde_json::to_value(first).map_err(|e| e.to_string())?;
                            let error = match std::future::poll_fn(|cx| events.as_mut().poll_next(cx)).await {
                                Some(Err(error)) => error,
                                other => return Err(format!("MDS_EXPECTED_STREAM_SERIALIZATION: {other:?}")),
                            };
                            let info = xai_grok_sampler::SamplingErrorInfo::from(&error);
                            if info.kind != xai_grok_sampler::SamplingErrorKind::Serialization {
                                return Err(format!("MDS_WRONG_STREAM_ERROR: {:?}", info.kind));
                            }
                            Ok::<_, String>(serde_json::json!({
                                "kind": "serialization", "error": info.message,
                                "retryable": info.is_retryable, "first_event": first,
                            }))
                        }).await;
                        let requests = server.finish().await;
                        assert_eq!(requests.len(), 1, "MDS_STREAM_HTTP_REQUEST_COUNT: {} result={result:?}", case.name);
                        assert_eq!(requests[0]["stream"], true);
                        assert_eq!(requests[0]["model"], "mds-model");
                        let outcome = result.expect("MDS_STREAM_HTTP_DEADLINE").expect("MDS_STREAM_PRIMARY_SERIALIZATION");
                        assert_eq!(outcome["first_event"]["type"], "message_start");
                        assert_eq!(outcome["first_event"]["message"]["id"], "mds-message");
                        assert_eq!(outcome["retryable"], false);
                        assert!(outcome["error"].as_str().unwrap().contains(case.required_error), "MDS_STREAM_PRIMARY_REASON: {} {outcome}", case.name);
                        MdsObservation { name: case.name, base_url, outcome }
                    }.instrument(span).await;
                    observations.push(observed);
                }
                observations
            })
        })
    }

    fn mds_ordinary_stream_errors() -> (MdsOutput, Vec<MdsObservation>) {
        let buffer = MdsLog::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(buffer.clone())
            .finish();
        let observations = mds_stream_error_observations(&tracing::Dispatch::new(subscriber));
        (buffer.output(&observations), observations)
    }

    fn mds_stream_error_fields<'a>(record: &'a str, name: &str) -> (&'a str, &'a str) {
        let prefix = format!(
            "ERROR mds_diagnostic{{case=\"{name}\"}}: xai_grok_sampler::client: Failed to deserialize MessageStreamEvent from stream error="
        );
        let fields = record
            .trim_start()
            .strip_prefix(&prefix)
            .expect("MDS_STREAM_EVENT_IDENTITY");
        let fields = fields
            .strip_suffix('\n')
            .expect("MDS_STREAM_RECORD_BOUNDARY");
        fields
            .split_once(" raw_data=")
            .expect("MDS_STREAM_RAW_PRESERVATION")
    }

    fn mds_assert_stream_errors(output: &MdsOutput, observations: &[MdsObservation]) {
        let cases = mds_stream_error_cases();
        let errors: Vec<_> = output
            .records
            .iter()
            .filter(|record| {
                record.contains("Failed to deserialize MessageStreamEvent from stream")
            })
            .collect();
        let sse: Vec<_> = output
            .records
            .iter()
            .filter(|record| record.contains("event=\"sse_chunk\""))
            .collect();
        let clients: Vec<_> = output
            .records
            .iter()
            .filter(|record| record.contains("event=\"client_new\""))
            .collect();
        println!(
            "MDS_STREAM_ERROR_OBSERVATION {}",
            serde_json::json!({
                "mode": "ordinary_stream_error", "records": errors, "sse_records": sse,
                "client_records": clients, "output": output.text,
                "cases": cases.iter().zip(observations).map(|(case, observed)| serde_json::json!({
                    "name": case.name, "raw": case.raw, "view": case.view,
                    "projection_reason": case.projection_reason, "outcome": observed.outcome,
                    "status": 200,
                })).collect::<Vec<_>>(),
            })
        );
        assert_eq!(observations.len(), cases.len(), "MDS_STREAM_CASE_COUNT");
        assert_eq!(clients.len(), cases.len(), "MDS_STREAM_CLIENT_NEW_LIVENESS");
        assert_eq!(errors.len(), cases.len(), "MDS_STREAM_ERROR_CARDINALITY");
        assert_eq!(sse.len(), cases.len() * 2, "MDS_STREAM_PREDECODE_LIVENESS");
        for index in 0..2 {
            let (error, raw) = mds_stream_error_fields(errors[index], cases[index].name);
            let primary = observations[index].outcome["error"]
                .as_str()
                .unwrap()
                .strip_prefix("serialization error: ")
                .unwrap();
            assert_eq!(
                error, primary,
                "MDS_STREAM_ERROR_PRESERVATION: {}",
                cases[index].name
            );
            assert_eq!(
                raw, cases[index].raw,
                "MDS_STREAM_RAW_PRESERVATION: {}",
                cases[index].name
            );
        }
        println!("MDS_STREAM_ORDINARY_PRESERVATION_PASSED cases=2");
        assert!(
            !sse[5].contains("MSS_STOP_EXTENSION_"),
            "MDS_STREAM_D1_PREDECODE_PROTECTED"
        );
        assert!(
            !output.text.contains("MSS_STOP_EXTENSION_"),
            "MDS_STREAM_MARKER_ABSENCE raw_data/error output"
        );
        for (index, (case, observed)) in cases.iter().zip(observations).enumerate() {
            let (error, raw) = mds_stream_error_fields(errors[index], case.name);
            let primary = observed.outcome["error"]
                .as_str()
                .unwrap()
                .strip_prefix("serialization error: ")
                .unwrap();
            let status = case
                .projection_reason
                .map(|reason| format!(" stop_metadata_projection=\"{reason}\""))
                .unwrap_or_default();
            assert_eq!(
                error, primary,
                "MDS_STREAM_DETAILED_ERROR_PRESERVATION: {}",
                case.name
            );
            assert_eq!(
                raw,
                format!("{}{status}", case.view),
                "MDS_STREAM_SELECTIVE_RAW_PRESERVATION: {}",
                case.name
            );
            let prefix = format!(
                " INFO mds_diagnostic{{case=\"{}\"}}: sampling_log: ",
                case.name
            );
            assert_eq!(
                *sse[index * 2],
                format!(
                    "{prefix}event=\"sse_chunk\" backend=\"messages\" data={}\n",
                    mds_start("")
                ),
                "MDS_STREAM_START_DATA_PRESERVATION"
            );
            assert_eq!(
                *sse[index * 2 + 1],
                format!(
                    "{prefix}event=\"sse_chunk\" backend=\"messages\" data={}{status}\n",
                    case.view
                ),
                "MDS_STREAM_PREDECODE_DATA_PRESERVATION: {}",
                case.name
            );
            assert_eq!(
                *clients[index],
                format!(
                    "{prefix}event=\"client_new\" base_url=http://127.0.0.1:PORT/v1 model=mds-model api_backend=Messages auth_scheme=Bearer reasoning_effort=\"unset\" has_api_key=true has_bearer_resolver=false has_authorization_header=true has_x_api_key_header=false\n"
                ),
                "MDS_STREAM_CLIENT_NEW_PRESERVATION"
            );
        }
        println!("MDS_STREAM_ERROR_ASSERTIONS_PASSED cases={}", cases.len());
    }

    fn mds_stream_structural_error_cases() -> Vec<MdsStreamErrorCase> {
        let mut cases = Vec::new();
        let pairs = [
            (
                "stream-ordinary-unbalanced",
                r#","stop_details":{"ordinary":["MSS_ORDINARY_DIAG_SENTINEL"}"#,
                r#","stop_details":{"ordinary":["MSS_ORDINARY_DIAG_SENTINEL"}"#,
                "expected `,` or `]`",
                None,
            ),
            (
                "stream-selected-unbalanced",
                r#","stop_details":{"fallback_credit_token":["MSS_STOP_EXTENSION_SENTINEL"}"#,
                MDS_UNAVAILABLE,
                "expected `,` or `]`",
                Some("unbalanced_selected_value"),
            ),
            (
                "stream-ordinary-value-boundary",
                r#","stop_details":{"ordinary":oops"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                r#","stop_details":{"ordinary":oops"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                "expected value",
                None,
            ),
            (
                "stream-selected-value-boundary",
                r#","stop_details":{"fallback_credit_token":oops"MSS_STOP_EXTENSION_SENTINEL"}"#,
                MDS_UNAVAILABLE,
                "expected value",
                Some("ambiguous_selected_value"),
            ),
            (
                "stream-ordinary-missing-colon",
                r#","stop_details":{"ordinary" "MSS_ORDINARY_DIAG_SENTINEL"}"#,
                r#","stop_details":{"ordinary" "MSS_ORDINARY_DIAG_SENTINEL"}"#,
                "expected `:`",
                None,
            ),
            (
                "stream-selected-missing-colon",
                r#","stop_details":{"fallback_credit_token" "MSS_STOP_EXTENSION_SENTINEL"}"#,
                MDS_UNAVAILABLE,
                "expected `:`",
                Some("ambiguous_selected_member_separator"),
            ),
            (
                "stream-ordinary-unquoted-key",
                r#","stop_details":{ordinary:"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                r#","stop_details":{ordinary:"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                "key must be a string",
                None,
            ),
            (
                "stream-selected-unquoted-key",
                r#","stop_details":{fallback_credit_token:"MSS_STOP_EXTENSION_SENTINEL"}"#,
                MDS_UNAVAILABLE,
                "key must be a string",
                Some("ambiguous_object_member"),
            ),
            (
                "stream-selected-safe-tail",
                r#","stop_details":{"fallback_credit_token":"MSS_STOP_EXTENSION_SENTINEL" nonsense}"#,
                r#","stop_details":{"fallback_credit_token":"[REDACTED_STOP_EXTENSION]" nonsense}"#,
                "expected `,` or `}`",
                None,
            ),
            (
                "stream-selected-ambiguous-tail",
                r#","stop_details":{"fallback_credit_token":"MSS_STOP_EXTENSION_SENTINEL" fallback_credit_token:"MSS_STOP_EXTENSION_SENTINEL"}"#,
                MDS_UNAVAILABLE,
                "expected `,` or `}`",
                Some("ambiguous_object_boundary"),
            ),
        ];
        for (name, raw, view, required_error, projection_reason) in pairs {
            cases.push(MdsStreamErrorCase {
                name,
                raw: mds_error_delta(raw, "{}"),
                view: if projection_reason.is_some() {
                    view.to_owned()
                } else {
                    mds_error_delta(view, "{}")
                },
                required_error,
                projection_reason,
            });
        }
        for (name, key, marker, reason) in [
            ("stream-ordinary-depth", "ordinary", MDS_ORDINARY, None),
            (
                "stream-selected-depth",
                "fallback_credit_token",
                MDS_SECRET,
                Some("selected_container_depth_unresolved"),
            ),
        ] {
            let details = format!(
                ",\"stop_details\":{{\"{key}\":{}\"{marker}\"{}}}",
                "[".repeat(129),
                "]".repeat(129)
            );
            let raw = mds_error_delta(&details, "{}");
            cases.push(MdsStreamErrorCase {
                name,
                view: if reason.is_some() {
                    MDS_UNAVAILABLE.to_owned()
                } else {
                    raw.clone()
                },
                raw,
                required_error: "recursion limit exceeded",
                projection_reason: reason,
            });
        }
        for (name, details, view, tail, reason) in [
            ("stream-ordinary-trailing-data", "", "", " ordinary", None),
            (
                "stream-selected-trailing-data",
                r#","stop_details":{"fallback_credit_token":"MSS_STOP_EXTENSION_SENTINEL"}"#,
                r#","stop_details":{"fallback_credit_token":"[REDACTED_STOP_EXTENSION]"}"#,
                " ordinary",
                None,
            ),
            (
                "stream-ordinary-trailing-object",
                "",
                "",
                r#" {"ordinary":"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                None,
            ),
            (
                "stream-selected-trailing-object",
                "",
                "",
                r#" {"stop_details":{"fallback_credit_token":"MSS_STOP_EXTENSION_SENTINEL"}}"#,
                Some("ambiguous_trailing_input"),
            ),
        ] {
            let raw = format!(
                "{}{tail}",
                mds_error_delta(details, r#"{"output_tokens":2}"#)
            );
            let view = if reason.is_some() {
                MDS_UNAVAILABLE.to_owned()
            } else {
                format!("{}{tail}", mds_error_delta(view, r#"{"output_tokens":2}"#))
            };
            cases.push(MdsStreamErrorCase {
                name,
                raw,
                view,
                required_error: "trailing characters",
                projection_reason: reason,
            });
        }
        cases
    }

    struct MdsBodyErrorCase {
        name: &'static str,
        raw: Vec<u8>,
        view: String,
        required_error: &'static str,
        projection_reason: Option<&'static str>,
    }

    fn mds_error_body(details: &str, usage: &str) -> String {
        format!(
            r#"{{"id":"mds-body","type":"message","role":"assistant","content":[{{"type":"text","text":"MSS_ORDINARY_DIAG_SENTINEL"}}],"model":"mds-model","stop_reason":"refusal","stop_sequence":"ordinary-end"{details}{usage},"ordinary":1.2300e+02,"ordinary":-0}}"#
        )
    }

    fn mds_body_error_cases() -> Vec<MdsBodyErrorCase> {
        const ORDINARY: &str = r#","stop_details":{"type":"refusal","category":"ordinary-category","explanation":"MSS_ORDINARY_DIAG_SENTINEL","resumable":false}"#;
        const SECRET: &str = r#","stop_details":{"type":"refusal","category":"ordinary-category","explanation":"MSS_ORDINARY_DIAG_SENTINEL","fallback_credit_token":"MSS_STOP_EXTENSION_SENTINEL","resumable":false}"#;
        const MASKED: &str = r#","stop_details":{"type":"refusal","category":"ordinary-category","explanation":"MSS_ORDINARY_DIAG_SENTINEL","fallback_credit_token":"[REDACTED_STOP_EXTENSION]","resumable":false}"#;
        const WRONG: &str =
            r#","usage":{"input_tokens":"MSS_ORDINARY_DIAG_SENTINEL","output_tokens":2}"#;
        const MISSING: &str = "missing field `usage`";
        let pairs = [
            ("body-ordinary-missing", ORDINARY, ORDINARY, "", MISSING),
            (
                "body-ordinary-wrong-type",
                ORDINARY,
                ORDINARY,
                WRONG,
                "invalid type: string \"MSS_ORDINARY_DIAG_SENTINEL\", expected u32",
            ),
            ("body-mixed-missing", SECRET, MASKED, "", MISSING),
            (
                "body-mixed-wrong-type",
                SECRET,
                MASKED,
                WRONG,
                "invalid type: string \"MSS_ORDINARY_DIAG_SENTINEL\", expected u32",
            ),
            (
                "body-token-equals-field",
                r#","stop_details":{"fallback_credit_token":"usage","explanation":"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                r#","stop_details":{"fallback_credit_token":"[REDACTED_STOP_EXTENSION]","explanation":"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                "",
                MISSING,
            ),
            (
                "body-escaped-duplicates",
                r#", "stop\u005fdetails" : {"fallback_credit_\u0074oken":"MSS_STOP_EXTENSION_SEN\u0054INEL","ordinary":1e+02,"ordinary":-0,"fallback_credit_token":{"nested":["MSS_STOP_EXTENSION_SENTINEL"]},"explanation":"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                r#", "stop\u005fdetails" : {"fallback_credit_\u0074oken":"[REDACTED_STOP_EXTENSION]","ordinary":1e+02,"ordinary":-0,"fallback_credit_token":"[REDACTED_STOP_EXTENSION]","explanation":"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                "",
                MISSING,
            ),
            (
                "body-null-selected",
                r#","stop_details":{"fallback_credit_token":null}"#,
                r#","stop_details":{"fallback_credit_token":null}"#,
                "",
                MISSING,
            ),
        ];
        let mut cases: Vec<_> = pairs
            .into_iter()
            .map(
                |(name, raw, view, usage, required_error)| MdsBodyErrorCase {
                    name,
                    raw: mds_error_body(raw, usage).into_bytes(),
                    view: mds_error_body(view, usage),
                    required_error,
                    projection_reason: None,
                },
            )
            .collect();
        for (name, raw, view, reason) in [
            (
                "body-ordinary-truncated",
                r#"{"type":"message","ordinary":"MSS_ORDINARY_DIAG_SENTINEL"#,
                r#"{"type":"message","ordinary":"MSS_ORDINARY_DIAG_SENTINEL"#,
                None,
            ),
            (
                "body-selected-truncated",
                r#"{"type":"message","stop_details":{"fallback_credit_token":"MSS_STOP_EXTENSION_SENTINEL"#,
                MDS_UNAVAILABLE,
                Some("truncated_selected_value"),
            ),
        ] {
            cases.push(MdsBodyErrorCase {
                name,
                raw: raw.as_bytes().to_vec(),
                view: view.to_owned(),
                required_error: "EOF while parsing a string",
                projection_reason: reason,
            });
        }
        for (name, details, reason) in [
            (
                "body-ordinary-invalid-utf8",
                r#","stop_details":{"ordinary_UTF8_KEY":"MSS_ORDINARY_DIAG_SENTINEL"}"#,
                None,
            ),
            (
                "body-selected-invalid-utf8",
                r#","stop_details":{"fallback_credit_toUTF8_KEYken":"MSS_STOP_EXTENSION_SENTINEL"}"#,
                Some("invalid_relevant_key_utf8"),
            ),
        ] {
            let bytes = mds_error_body(details, "").into_bytes();
            let index = bytes
                .windows(8)
                .position(|bytes| bytes == b"UTF8_KEY")
                .unwrap();
            let mut raw = bytes[..index].to_vec();
            raw.push(0xff);
            raw.extend_from_slice(&bytes[index + 8..]);
            let view = if reason.is_some() {
                MDS_UNAVAILABLE.to_owned()
            } else {
                String::from_utf8_lossy(&raw).into_owned()
            };
            cases.push(MdsBodyErrorCase {
                name,
                raw,
                view,
                required_error: "invalid unicode code point",
                projection_reason: reason,
            });
        }
        for (name, raw, view) in [
            ("body-ordinary-trailing-data", ORDINARY, ORDINARY),
            ("body-selected-trailing-data", SECRET, MASKED),
        ] {
            const USAGE: &str = r#","usage":{"input_tokens":3,"output_tokens":2}"#;
            cases.push(MdsBodyErrorCase {
                name,
                raw: format!("{} ordinary", mds_error_body(raw, USAGE)).into_bytes(),
                view: format!("{} ordinary", mds_error_body(view, USAGE)),
                required_error: "trailing characters",
                projection_reason: None,
            });
        }
        cases.extend(mds_body_split_scalar_cases());
        cases
    }

    fn mds_body_error_observations(dispatch: &tracing::Dispatch) -> Vec<MdsObservation> {
        tracing::dispatcher::with_default(dispatch, || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut observations = Vec::new();
                for case in mds_body_error_cases() {
                    let span = tracing::info_span!(target: "sampling_log", "mds_diagnostic", case = case.name);
                    let observed = async {
                        let server = mds_server(case.raw.clone(), "application/json").await;
                        let base_url = server.base_url.clone();
                        let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                            let client = SamplingClient::new(mds_config(&base_url)).map_err(|e| e.to_string())?;
                            let mut request = mds_default_request(|r| client.conversation_messages(r));
                            request.items.push(serde_json::from_value(serde_json::json!({
                                "type": "user", "content": [{"type": "text", "text": "diagnostic request"}]
                            })).map_err(|e| e.to_string())?);
                            let error = match client.conversation_messages(request).await {
                                Err(error) => error,
                                Ok(_) => return Err("MDS_EXPECTED_BODY_SERIALIZATION".to_owned()),
                            };
                            let info = xai_grok_sampler::SamplingErrorInfo::from(&error);
                            if info.kind != xai_grok_sampler::SamplingErrorKind::Serialization {
                                return Err(format!("MDS_WRONG_BODY_ERROR: {:?}", info.kind));
                            }
                            Ok::<_, String>(serde_json::json!({
                                "kind": "serialization", "error": info.message,
                                "retryable": info.is_retryable,
                            }))
                        }).await;
                        let requests = server.finish().await;
                        assert_eq!(requests.len(), 1, "MDS_BODY_HTTP_REQUEST_COUNT: {} result={result:?}", case.name);
                        assert_eq!(requests[0]["model"], "mds-model");
                        assert!(matches!(requests[0].get("stream"), None | Some(serde_json::Value::Null | serde_json::Value::Bool(false))), "MDS_BODY_NONSTREAM_REQUEST");
                        let outcome = result.expect("MDS_BODY_HTTP_DEADLINE").expect("MDS_BODY_PRIMARY_SERIALIZATION");
                        assert_eq!(outcome["retryable"], false);
                        assert!(outcome["error"].as_str().unwrap().contains(case.required_error), "MDS_BODY_PRIMARY_REASON: {} {outcome}", case.name);
                        MdsObservation { name: case.name, base_url, outcome }
                    }.instrument(span).await;
                    observations.push(observed);
                }
                observations
            })
        })
    }

    fn mds_ordinary_body_errors() -> (MdsOutput, Vec<MdsObservation>) {
        let buffer = MdsLog::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(buffer.clone())
            .finish();
        let observations = mds_body_error_observations(&tracing::Dispatch::new(subscriber));
        (buffer.output(&observations), observations)
    }
    fn mds_body_error_fields<'a>(record: &'a str, name: &str) -> (&'a str, &'a str) {
        let prefix = format!(
            "ERROR mds_diagnostic{{case=\"{name}\"}}: xai_grok_sampler::client: Failed to deserialize MessagesResponse error="
        );
        let fields = record
            .trim_start()
            .strip_prefix(&prefix)
            .expect("MDS_BODY_EVENT_IDENTITY");
        let fields = fields.strip_suffix('\n').expect("MDS_BODY_RECORD_BOUNDARY");
        fields
            .split_once(" raw_body=")
            .expect("MDS_BODY_RAW_PRESERVATION")
    }

    fn mds_assert_body_errors(output: &MdsOutput, observations: &[MdsObservation]) {
        let cases = mds_body_error_cases();
        let errors: Vec<_> = output
            .records
            .iter()
            .filter(|record| record.contains("Failed to deserialize MessagesResponse"))
            .collect();
        let clients: Vec<_> = output
            .records
            .iter()
            .filter(|record| record.contains("event=\"client_new\""))
            .collect();
        println!(
            "MDS_BODY_ERROR_OBSERVATION {}",
            serde_json::json!({
                "mode": "ordinary_body_error", "records": errors, "client_records": clients,
                "output": output.text,
                "cases": cases.iter().zip(observations).map(|(case, observed)| serde_json::json!({
                    "name": case.name, "raw_bytes": case.raw,
                    "raw_lossy": String::from_utf8_lossy(&case.raw), "view": case.view,
                    "projection_reason": case.projection_reason, "outcome": observed.outcome,
                    "status": 200,
                })).collect::<Vec<_>>(),
            })
        );
        assert_eq!(observations.len(), cases.len(), "MDS_BODY_CASE_COUNT");
        assert_eq!(clients.len(), cases.len(), "MDS_BODY_CLIENT_NEW_LIVENESS");
        assert_eq!(errors.len(), cases.len(), "MDS_BODY_ERROR_CARDINALITY");
        assert!(
            !output.text.contains("event=\"sse_chunk\""),
            "MDS_BODY_IS_NONSTREAM"
        );
        for index in 0..2 {
            let (error, raw) = mds_body_error_fields(errors[index], cases[index].name);
            let primary = observations[index].outcome["error"]
                .as_str()
                .unwrap()
                .strip_prefix("serialization error: ")
                .unwrap();
            assert_eq!(
                error, primary,
                "MDS_BODY_ERROR_PRESERVATION: {}",
                cases[index].name
            );
            assert_eq!(
                raw,
                String::from_utf8_lossy(&cases[index].raw),
                "MDS_BODY_RAW_PRESERVATION: {}",
                cases[index].name
            );
        }
        println!("MDS_BODY_ORDINARY_PRESERVATION_PASSED cases=2");
        assert!(
            !output.text.contains("MSS_STOP_EXTENSION_"),
            "MDS_BODY_MARKER_ABSENCE raw_body/error output"
        );
        for (index, (case, observed)) in cases.iter().zip(observations).enumerate() {
            let (error, raw) = mds_body_error_fields(errors[index], case.name);
            let primary = observed.outcome["error"]
                .as_str()
                .unwrap()
                .strip_prefix("serialization error: ")
                .unwrap();
            let status = case
                .projection_reason
                .map(|reason| format!(" stop_metadata_projection=\"{reason}\""))
                .unwrap_or_default();
            assert_eq!(
                error, primary,
                "MDS_BODY_DETAILED_ERROR_PRESERVATION: {}",
                case.name
            );
            assert_eq!(
                raw,
                format!("{}{status}", case.view),
                "MDS_BODY_SELECTIVE_RAW_PRESERVATION: {}",
                case.name
            );
            let prefix = format!(
                " INFO mds_diagnostic{{case=\"{}\"}}: sampling_log: ",
                case.name
            );
            assert_eq!(
                *clients[index],
                format!(
                    "{prefix}event=\"client_new\" base_url=http://127.0.0.1:PORT/v1 model=mds-model api_backend=Messages auth_scheme=Bearer reasoning_effort=\"unset\" has_api_key=true has_bearer_resolver=false has_authorization_header=true has_x_api_key_header=false\n"
                ),
                "MDS_BODY_CLIENT_NEW_PRESERVATION"
            );
        }
        println!("MDS_BODY_ERROR_ASSERTIONS_PASSED cases={}", cases.len());
    }

    fn mds_file_errors() -> (String, Vec<MdsObservation>, Vec<MdsObservation>) {
        use tracing_subscriber::layer::SubscriberExt;
        let subscriber = tracing_subscriber::registry().with(super::layer());
        let dispatch = tracing::Dispatch::new(subscriber);
        let streams = mds_stream_error_observations(&dispatch);
        let bodies = mds_body_error_observations(&dispatch);
        drop(dispatch);
        let guard = super::GUARD
            .get()
            .and_then(|slot| slot.lock().unwrap().take());
        let had_guard = guard.is_some();
        drop(guard);
        let path = super::grok_home().join("logs").join("sampling.jsonl");
        assert!(path.is_file(), "MDS_ERROR_SINK_ENABLED_FILE");
        assert!(had_guard, "MDS_ERROR_SINK_WORKER_GUARD");
        let output = std::fs::read_to_string(path).unwrap();
        assert!(
            !output.is_empty(),
            "MDS_ERROR_SINK_NONEMPTY_AFTER_GUARD_DROP"
        );
        let output = mds_normalize_endpoints(output, &streams);
        (mds_normalize_endpoints(output, &bodies), streams, bodies)
    }

    fn mds_expected_file_client(name: &str) -> serde_json::Value {
        serde_json::json!({
            "level": "INFO", "spans": [{"case": name, "name": "mds_diagnostic"}],
            "fields": {
                "event": "client_new", "base_url": "http://127.0.0.1:PORT/v1",
                "model": "mds-model", "api_backend": "Messages", "auth_scheme": "Bearer",
                "reasoning_effort": "unset", "has_api_key": true, "has_bearer_resolver": false,
                "has_authorization_header": true, "has_x_api_key_header": false,
            },
        })
    }

    fn mds_assert_file_errors(output: &str, streams: &[MdsObservation], bodies: &[MdsObservation]) {
        let records: Vec<serde_json::Value> = output
            .lines()
            .map(|line| {
                let mut record: serde_json::Value = serde_json::from_str(line).unwrap();
                assert!(
                    record["timestamp"].is_string(),
                    "MDS_ERROR_SINK_TIMESTAMP_PRESENT"
                );
                record.as_object_mut().unwrap().remove("timestamp");
                record
            })
            .filter(|record| {
                matches!(
                    record["fields"]["event"].as_str(),
                    Some("client_new" | "sse_chunk")
                )
            })
            .collect();
        println!(
            "MDS_ERROR_SINK_OBSERVATION {}",
            serde_json::json!({
                "mode": "sampling_file_errors", "records": records, "output": output,
                "streams": streams.iter().map(|o| serde_json::json!({"name":o.name,"outcome":o.outcome,"status":200})).collect::<Vec<_>>(),
                "bodies": bodies.iter().map(|o| serde_json::json!({"name":o.name,"outcome":o.outcome,"status":200})).collect::<Vec<_>>(),
            })
        );
        let mut expected = Vec::new();
        for case in mds_stream_error_cases() {
            expected.push(mds_expected_file_client(case.name));
            for (index, data) in [mds_start(""), case.view].into_iter().enumerate() {
                let mut record = serde_json::json!({
                    "level": "INFO", "spans": [{"case":case.name,"name":"mds_diagnostic"}],
                    "fields": {"event":"sse_chunk","backend":"messages","data":data},
                });
                if index == 1
                    && let Some(reason) = case.projection_reason
                {
                    record["fields"]["stop_metadata_projection"] = serde_json::json!(reason);
                }
                expected.push(record);
            }
        }
        for case in mds_body_error_cases() {
            expected.push(mds_expected_file_client(case.name));
        }
        assert_eq!(
            records.len(),
            expected.len(),
            "MDS_ERROR_SINK_LIVE_RECORD_COUNT"
        );
        assert_eq!(
            &records[..6],
            &expected[..6],
            "MDS_ERROR_SINK_ORDINARY_PRESERVATION"
        );
        println!("MDS_ERROR_SINK_ORDINARY_PRESERVATION_PASSED stream_cases=2 records=6");
        assert!(
            !output.contains("MSS_STOP_EXTENSION_"),
            "MDS_ERROR_SAMPLING_SINK_MARKER_ABSENCE"
        );
        assert_eq!(records, expected, "MDS_ERROR_SINK_SELECTIVE_PRESERVATION");
        assert!(
            !output.contains("Failed to deserialize"),
            "MDS_ERROR_SINK_TARGET_BOUNDARY"
        );
        println!(
            "MDS_ERROR_SINK_ASSERTIONS_PASSED streams={} bodies={} records={}",
            streams.len(),
            bodies.len(),
            records.len()
        );
    }

    #[test]
    fn mss_u4_stop_extension_not_in_parse_error_logs() {
        const SELECTOR: &str = "sampling_log::tests::mss_u4_stop_extension_not_in_parse_error_logs";
        if std::env::var("MDS_LOG_CHILD_SELECTOR").as_deref() != Ok(SELECTOR) {
            mds_run_child(SELECTOR);
            return;
        }
        println!("MDS_CHILD_ASSERTIONS_BEGIN {SELECTOR}");
        let (ordinary_stream, ordinary_streams) = mds_ordinary_stream_errors();
        let (ordinary_body, ordinary_bodies) = mds_ordinary_body_errors();
        let (file, file_streams, file_bodies) = mds_file_errors();
        let ordinary_stream_outcomes: Vec<_> = ordinary_streams
            .iter()
            .map(|o| (&o.name, &o.outcome))
            .collect();
        let file_stream_outcomes: Vec<_> =
            file_streams.iter().map(|o| (&o.name, &o.outcome)).collect();
        let ordinary_body_outcomes: Vec<_> = ordinary_bodies
            .iter()
            .map(|o| (&o.name, &o.outcome))
            .collect();
        let file_body_outcomes: Vec<_> =
            file_bodies.iter().map(|o| (&o.name, &o.outcome)).collect();
        assert_eq!(
            ordinary_stream_outcomes, file_stream_outcomes,
            "MDS_ERROR_STREAM_PARSER_PARITY"
        );
        assert_eq!(
            ordinary_body_outcomes, file_body_outcomes,
            "MDS_ERROR_BODY_PARSER_PARITY"
        );
        mds_assert_file_errors(&file, &file_streams, &file_bodies);
        mds_assert_stream_errors(&ordinary_stream, &ordinary_streams);
        mds_assert_body_errors(&ordinary_body, &ordinary_bodies);
        println!("MDS_CHILD_ASSERTIONS_PASSED {SELECTOR}");
    }

    fn mds_stream_split_scalar_cases() -> Vec<MdsStreamErrorCase> {
        [
            (
                "stream-ordinary-split-scalar",
                r#","stop_details":{"ordinary":MSS_ORDINARY_DIAG_ SENTINEL}"#,
                None,
            ),
            (
                "stream-selected-split-scalar",
                r#","stop_details":{"fallback_credit_token":MSS_STOP_EXTENSION_ SENTINEL}"#,
                Some("ambiguous_selected_value"),
            ),
        ]
        .into_iter()
        .map(|(name, details, reason)| {
            let raw = mds_error_delta(details, "{}");
            MdsStreamErrorCase {
                name,
                view: if reason.is_some() {
                    MDS_UNAVAILABLE.to_owned()
                } else {
                    raw.clone()
                },
                raw,
                required_error: "expected value",
                projection_reason: reason,
            }
        })
        .collect()
    }

    fn mds_body_split_scalar_cases() -> Vec<MdsBodyErrorCase> {
        [
            (
                "body-ordinary-split-scalar",
                r#","stop_details":{"ordinary":MSS_ORDINARY_DIAG_ SENTINEL}"#,
                None,
            ),
            (
                "body-selected-split-scalar",
                r#","stop_details":{"fallback_credit_token":MSS_STOP_EXTENSION_ SENTINEL}"#,
                Some("ambiguous_selected_value"),
            ),
        ]
        .into_iter()
        .map(|(name, details, reason)| {
            let raw = mds_error_body(details, "");
            MdsBodyErrorCase {
                name,
                view: if reason.is_some() {
                    MDS_UNAVAILABLE.to_owned()
                } else {
                    raw.clone()
                },
                raw: raw.into_bytes(),
                required_error: "expected value",
                projection_reason: reason,
            }
        })
        .collect()
    }
}
