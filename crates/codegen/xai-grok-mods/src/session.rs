// crates/codegen/xai-grok-mods/src/session.rs
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use xai_grok_tools::implementations::grok_build::task::coordinator::SubagentProgress;

use crate::bridge::BridgeHandle;
use crate::control::ModChildControl;
use crate::process::SidecarProcess;
use crate::protocol::{HostFrame, SidecarFrame};
use crate::store::{ModSessionStore, SessionRecord};
use crate::{ModHost, ModRunInput, ModRunOutcome, ModStarted};

async fn write_frame(stdin: &mut tokio::process::ChildStdin, frame: &HostFrame) -> bool {
    let Ok(mut bytes) = serde_json::to_vec(frame) else {
        return false;
    };
    bytes.push(b'\n');
    stdin.write_all(&bytes).await.is_ok() && stdin.flush().await.is_ok()
}

enum IoCmd {
    Frame(HostFrame),
    Shutdown {
        kill: bool,
        reply: oneshot::Sender<Option<i32>>,
    },
}

fn queue_frame(io_tx: &mpsc::UnboundedSender<IoCmd>, frame: HostFrame) {
    let _ = io_tx.send(IoCmd::Frame(frame));
}

async fn request_shutdown(io_tx: &mpsc::UnboundedSender<IoCmd>, kill: bool) -> Option<i32> {
    let (reply_tx, reply_rx) = oneshot::channel();
    if io_tx
        .send(IoCmd::Shutdown {
            kill,
            reply: reply_tx,
        })
        .is_err()
    {
        return None;
    }
    reply_rx.await.ok().flatten()
}

fn stderr_tail(ring: &Mutex<Vec<u8>>) -> String {
    String::from_utf8_lossy(&ring.lock()).trim().to_owned()
}

pub async fn run_mod_child_with_store(
    input: ModRunInput,
    host: Arc<dyn ModHost>,
    store: &ModSessionStore,
) -> ModRunOutcome {
    let ns = input.validated.config.namespace.clone();
    let mut warnings = Vec::new();
    let resume_vendor_session_id = if input.validated.config.capabilities.session_resume {
        if let Some(key) = input.resume_key.as_deref() {
            match store.lookup(&ns, key, &input.parent_session_id) {
                Some(vsid) => Some(vsid),
                None => {
                    warnings.push(format!("vendor session for {key} not found; started fresh"));
                    None
                }
            }
        } else {
            None
        }
    } else {
        if let Some(key) = input.resume_key.as_deref() {
            warnings.push(format!(
                "mod '{ns}' does not support sessionResume (ignored {key})"
            ));
        }
        None
    };

    let bridge_handle = if !input.bridged_tools.is_empty()
        && input.validated.config.capabilities.mcp_bridge
        && (!input.read_only || input.validated.config.capabilities.mcp_in_read_only)
    {
        BridgeHandle::start(&input.bridged_tools, Arc::clone(&host)).ok()
    } else {
        None
    };

    let mut proc = match SidecarProcess::spawn(&input.validated, &input.subagent_id, &input.cwd) {
        Ok(p) => p,
        Err(e) => {
            return ModRunOutcome {
                promoted: false,
                final_text: String::new(),
                error: Some(format!("failed to spawn mod sidecar '{ns}': {e}")),
                cancelled: false,
                turns: 0,
                tool_calls: 0,
                tokens_used: 0,
                vendor_session_id: None,
                admission_uncertain: false,
                warnings,
            };
        }
    };

    let start_frame = HostFrame::Start {
        protocol: 1,
        subagent_id: input.subagent_id.clone(),
        parent_session_id: input.parent_session_id.clone(),
        subagent_type: input.subagent_type.clone(),
        agent_name: input.agent_name.clone(),
        model: input.vendor_model.clone(),
        cwd: input.cwd.to_string_lossy().into_owned(),
        prompt: input.prompt.clone(),
        system_prompt: input.system_prompt.clone(),
        resume_vendor_session_id,
        read_only: input.read_only,
        reasoning_effort: input.reasoning_effort.clone(),
        bridge: bridge_handle.as_ref().map(BridgeHandle::info),
        bridged_tools: if bridge_handle.is_some() {
            input.bridged_tools.clone()
        } else {
            Vec::new()
        },
    };

    if !write_frame(&mut proc.stdin, &start_frame).await {
        proc.terminate_group(input.validated.config.runtime.graceful_kill_ms)
            .await;
        return ModRunOutcome {
            promoted: false,
            final_text: String::new(),
            error: Some(format!(
                "failed to write run.start to '{ns}': {}",
                proc.stderr_tail()
            )),
            cancelled: false,
            turns: 0,
            tool_calls: 0,
            tokens_used: 0,
            vendor_session_id: None,
            admission_uncertain: false,
            warnings,
        };
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<HostFrame>();
    let pending = Arc::new(Mutex::new(HashSet::<String>::new()));
    let progress = Arc::new(Mutex::new(SubagentProgress::default()));
    let control_cancel = CancellationToken::new();
    let closed = Arc::new(AtomicBool::new(false));
    let control = ModChildControl::new(
        tx,
        Arc::clone(&pending),
        Arc::clone(&progress),
        input.validated.config.capabilities.live_steer,
        control_cancel.clone(),
        Arc::clone(&closed),
    );

    let timeout = Duration::from_secs(input.validated.config.runtime.timeout_secs);
    let deadline = tokio::time::Instant::now() + timeout;
    let graceful_ms = input.validated.config.runtime.graceful_kill_ms;
    let (mut child, mut stdin, stdout, stderr_ring, mut stderr_task, pgid) = proc.into_parts();
    let (io_tx, mut io_rx) = mpsc::unbounded_channel::<IoCmd>();
    tokio::spawn(async move {
        while let Some(cmd) = io_rx.recv().await {
            match cmd {
                IoCmd::Frame(frame) => {
                    let _ = write_frame(&mut stdin, &frame).await;
                }
                IoCmd::Shutdown { kill, reply } => {
                    let code = if kill {
                        crate::process::terminate_child(&mut child, pgid, graceful_ms).await
                    } else {
                        child.wait().await.ok().and_then(|status| status.code())
                    };
                    let _ = reply.send(code);
                    break;
                }
            }
        }
    });
    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<Option<String>>();
    tokio::spawn(async move {
        let mut lines = stdout.lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if line_tx.send(Some(line)).is_err() {
                        break;
                    }
                }
                _ => {
                    let _ = line_tx.send(None);
                    break;
                }
            }
        }
    });

    let mut promoted = false;
    let mut finalizing_called = false;
    let mut admission_uncertain = false;
    let mut vendor_session_id: Option<String> = None;
    let mut last_idle_text = String::new();
    let mut turns = 0u32;
    let mut tool_calls = 0u32;
    let mut tokens_used = 0u64;

    let outcome = loop {
        tokio::select! {
            biased;
            _ = input.cancellation.cancelled() => {
                closed.store(true, Ordering::Release);
                queue_frame(&io_tx, HostFrame::Cancel { reason: Some("cancelled".into()) });
                let _ = request_shutdown(&io_tx, true).await;
                break ModRunOutcome {
                    promoted,
                    final_text: last_idle_text,
                    error: Some("Subagent was cancelled".into()),
                    cancelled: true,
                    turns,
                    tool_calls,
                    tokens_used,
                    vendor_session_id,
                    admission_uncertain,
                    warnings,
                };
            }
            _ = control_cancel.cancelled() => {
                closed.store(true, Ordering::Release);
                queue_frame(&io_tx, HostFrame::Cancel { reason: Some("cancelled".into()) });
                let _ = request_shutdown(&io_tx, true).await;
                break ModRunOutcome {
                    promoted,
                    final_text: last_idle_text,
                    error: Some("Subagent was cancelled".into()),
                    cancelled: true,
                    turns,
                    tool_calls,
                    tokens_used,
                    vendor_session_id,
                    admission_uncertain,
                    warnings,
                };
            }
            _ = tokio::time::sleep_until(deadline) => {
                closed.store(true, Ordering::Release);
                let reason = format!("mod sidecar timed out after {}s", timeout.as_secs());
                queue_frame(&io_tx, HostFrame::Cancel { reason: Some(reason.clone()) });
                let _ = request_shutdown(&io_tx, true).await;
                break ModRunOutcome {
                    promoted,
                    final_text: last_idle_text,
                    error: Some(reason),
                    cancelled: false,
                    turns,
                    tool_calls,
                    tokens_used,
                    vendor_session_id,
                    admission_uncertain,
                    warnings,
                };
            }
            Some(out_frame) = rx.recv() => {
                queue_frame(&io_tx, out_frame);
            }
            maybe_line = line_rx.recv() => {
                let Some(Some(line)) = maybe_line else {
                    closed.store(true, Ordering::Release);
                    let status = request_shutdown(&io_tx, false).await;
                    if let Some(task) = stderr_task.take() {
                        let _ = tokio::time::timeout(Duration::from_millis(500), task).await;
                    }
                    let tail = stderr_tail(&stderr_ring);
                    break ModRunOutcome {
                        promoted,
                        final_text: last_idle_text,
                        error: Some(format!("mod sidecar exited ({status:?}): {tail}")),
                        cancelled: false,
                        turns,
                        tool_calls,
                        tokens_used,
                        vendor_session_id,
                        admission_uncertain,
                        warnings,
                    };
                };
                if line.len() > 1024 * 1024 {
                    continue;
                }
                let Ok(frame) = serde_json::from_str::<SidecarFrame>(&line) else {
                    tracing::debug!(namespace = %ns, "ignoring non-JSON sidecar stdout line");
                    continue;
                };
                match frame {
                    SidecarFrame::Started { vendor_session_id: vsid, effective_model } => {
                        vendor_session_id = Some(vsid.clone());
                        store.upsert(&ns, &input.subagent_id, SessionRecord {
                            vendor_session_id: vsid.clone(),
                            parent_session_id: input.parent_session_id.clone(),
                            subagent_type: input.subagent_type.clone(),
                            updated_at_ms: now_ms(),
                        });
                        let eff = effective_model.unwrap_or_else(|| input.vendor_model.clone());
                        if !host.on_started(ModStarted { vendor_session_id: vsid, effective_model: eff }, control.clone()).await {
                            closed.store(true, Ordering::Release);
                            queue_frame(&io_tx, HostFrame::Cancel { reason: Some("admission refused".into()) });
                            let _ = request_shutdown(&io_tx, true).await;
                            break ModRunOutcome {
                                promoted: false,
                                final_text: String::new(),
                                error: Some("Subagent was cancelled".into()),
                                cancelled: true,
                                turns: 0,
                                tool_calls: 0,
                                tokens_used: 0,
                                vendor_session_id,
                                admission_uncertain: false,
                                warnings,
                            };
                        }
                        promoted = true;
                    }
                    SidecarFrame::Progress { turns: t, tool_calls: tc, tokens_used: tok, tool } => {
                        turns = t;
                        tool_calls = tc;
                        tokens_used = tok;
                        let snap = {
                            let mut p = progress.lock();
                            p.turn_count = t;
                            p.tool_call_count = tc;
                            p.tokens_used = tok;
                            if let Some(ref act) = tool {
                                if !p.tools_used.contains(&act.name) {
                                    p.tools_used.push(act.name.clone());
                                }
                            }
                            p.clone()
                        };
                        host.on_progress(&snap, tool.as_ref());
                    }
                    SidecarFrame::Consumed { message_id } => {
                        pending.lock().remove(&message_id);
                    }
                    SidecarFrame::Idle { final_text } => {
                        last_idle_text = final_text;
                        if !finalizing_called {
                            finalizing_called = true;
                            if promoted && !host.finalizing().await {
                                admission_uncertain = true;
                            }
                        }
                        while let Ok(queued) = rx.try_recv() {
                            queue_frame(&io_tx, queued);
                        }
                        if pending.lock().is_empty() {
                            queue_frame(&io_tx, HostFrame::Finish);
                        } else {
                            queue_frame(&io_tx, HostFrame::Drain);
                        }
                    }
                    SidecarFrame::Completed { final_text, turns: t, tool_calls: tc, tokens_used: tok } => {
                        closed.store(true, Ordering::Release);
                        turns = t;
                        tool_calls = tc;
                        tokens_used = tok;
                        if let Some(ref vsid) = vendor_session_id {
                            store.upsert(&ns, &input.subagent_id, SessionRecord {
                                vendor_session_id: vsid.clone(),
                                parent_session_id: input.parent_session_id.clone(),
                                subagent_type: input.subagent_type.clone(),
                                updated_at_ms: now_ms(),
                            });
                        }
                        break ModRunOutcome {
                            promoted,
                            final_text,
                            error: None,
                            cancelled: false,
                            turns,
                            tool_calls,
                            tokens_used,
                            vendor_session_id,
                            admission_uncertain,
                            warnings,
                        };
                    }
                    SidecarFrame::Failed { error, cancelled, turns: t, tool_calls: tc, tokens_used: tok } => {
                        closed.store(true, Ordering::Release);
                        break ModRunOutcome {
                            promoted,
                            final_text: last_idle_text,
                            error: Some(error),
                            cancelled,
                            turns: t,
                            tool_calls: tc,
                            tokens_used: tok,
                            vendor_session_id,
                            admission_uncertain,
                            warnings,
                        };
                    }
                }
            }
        }
    };

    drop(bridge_handle);
    outcome
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
