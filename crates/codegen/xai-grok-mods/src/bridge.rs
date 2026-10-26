// crates/codegen/xai-grok-mods/src/bridge.rs
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::ModHost;
use crate::protocol::{BridgeInfo, BridgeRpcRequest, BridgeRpcResponse, BridgedTool};

const MAX_IN_FLIGHT_CALLS: usize = 8;
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

pub struct BridgeHandle {
    info: BridgeInfo,
    socket_path: PathBuf,
    task: JoinHandle<()>,
}

impl BridgeHandle {
    pub fn start(tools: &[BridgedTool], host: Arc<dyn ModHost>) -> std::io::Result<Self> {
        let suffix = format!("gm-{:08x}.sock", rand::random::<u32>());
        let candidate = std::env::temp_dir().join(&suffix);
        let socket_path = if candidate.as_os_str().len() < 100 {
            candidate
        } else {
            PathBuf::from("/tmp").join(&suffix)
        };
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600));
        }
        let token = format!(
            "{:016x}{:016x}{:016x}{:016x}",
            rand::random::<u64>(),
            rand::random::<u64>(),
            rand::random::<u64>(),
            rand::random::<u64>()
        );
        let allowed: Arc<HashSet<(String, String)>> = Arc::new(
            tools
                .iter()
                .map(|t| (t.server.clone(), t.tool.clone()))
                .collect(),
        );
        let sem = Arc::new(Semaphore::new(MAX_IN_FLIGHT_CALLS));
        let token_clone = token.clone();

        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let host = Arc::clone(&host);
                let allowed = Arc::clone(&allowed);
                let sem = Arc::clone(&sem);
                let expected_token = token_clone.clone();
                tokio::spawn(async move {
                    let (rd, mut wr) = stream.into_split();
                    let mut lines = BufReader::new(rd).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let Ok(req) = serde_json::from_str::<BridgeRpcRequest>(&line) else {
                            break;
                        };
                        if req.token != expected_token {
                            break;
                        }
                        let resp = if req.method != "tools/call"
                            || !allowed.contains(&(req.server.clone(), req.tool.clone()))
                        {
                            BridgeRpcResponse {
                                id: req.id,
                                ok: false,
                                result: None,
                                error: Some("unknown or disallowed tool".into()),
                            }
                        } else {
                            let Ok(_permit) = sem.acquire().await else {
                                break;
                            };
                            match tokio::time::timeout(
                                CALL_TIMEOUT,
                                host.call_mcp_tool(&req.server, &req.tool, req.arguments),
                            )
                            .await
                            {
                                Ok(Ok(val)) => BridgeRpcResponse {
                                    id: req.id,
                                    ok: true,
                                    result: Some(val),
                                    error: None,
                                },
                                Ok(Err(err)) => BridgeRpcResponse {
                                    id: req.id,
                                    ok: false,
                                    result: None,
                                    error: Some(err),
                                },
                                Err(_) => BridgeRpcResponse {
                                    id: req.id,
                                    ok: false,
                                    result: None,
                                    error: Some("MCP tool call timed out".into()),
                                },
                            }
                        };
                        let Ok(mut bytes) = serde_json::to_vec(&resp) else {
                            break;
                        };
                        bytes.push(b'\n');
                        if wr.write_all(&bytes).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });

        Ok(Self {
            info: BridgeInfo {
                socket_path: socket_path.to_string_lossy().into_owned(),
                token,
            },
            socket_path,
            task,
        })
    }

    pub fn info(&self) -> BridgeInfo {
        self.info.clone()
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BorrowedSendFuture, ModChildControl, ModStarted};
    use xai_grok_tools::implementations::grok_build::task::coordinator::SubagentProgress;

    struct DummyHost;
    impl ModHost for DummyHost {
        fn on_started<'a>(
            &'a self,
            _: ModStarted,
            _: ModChildControl,
        ) -> BorrowedSendFuture<'a, bool> {
            Box::pin(std::future::ready(true))
        }
        fn on_progress(&self, _: &SubagentProgress, _: Option<&crate::protocol::VendorToolActivity>) {}
        fn finalizing<'a>(&'a self) -> BorrowedSendFuture<'a, bool> {
            Box::pin(std::future::ready(true))
        }
        fn call_mcp_tool<'a>(
            &'a self,
            server: &'a str,
            tool: &'a str,
            args: serde_json::Value,
        ) -> BorrowedSendFuture<'a, Result<serde_json::Value, String>> {
            Box::pin(async move {
                Ok(serde_json::json!({ "server": server, "tool": tool, "args": args }))
            })
        }
    }

    #[tokio::test]
    async fn t5_bridge_socket_auth_and_allowlist() {
        let tools = vec![BridgedTool {
            server: "github".into(),
            tool: "search_issues".into(),
            name: "github__search_issues".into(),
            description: None,
            input_schema: serde_json::json!({}),
        }];
        let handle = BridgeHandle::start(&tools, Arc::new(DummyHost)).unwrap();
        let info = handle.info();
        assert!(info.socket_path.len() < 100);

        // Allowed call succeeds
        let stream = tokio::net::UnixStream::connect(&info.socket_path)
            .await
            .unwrap();
        let (rd, mut wr) = stream.into_split();
        let mut lines = BufReader::new(rd).lines();
        let req = serde_json::json!({
            "id": "r1",
            "token": info.token,
            "method": "tools/call",
            "server": "github",
            "tool": "search_issues",
            "arguments": {"q": "bug"}
        });
        wr.write_all(format!("{req}\n").as_bytes()).await.unwrap();
        let resp: BridgeRpcResponse =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert!(resp.ok);

        // Unlisted tool rejected
        let req2 = serde_json::json!({
            "id": "r2",
            "token": info.token,
            "method": "tools/call",
            "server": "github",
            "tool": "delete_repo",
            "arguments": {}
        });
        wr.write_all(format!("{req2}\n").as_bytes()).await.unwrap();
        let resp2: BridgeRpcResponse =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert!(!resp2.ok);

        // Bad token closes connection
        let bad = serde_json::json!({
            "id": "r3",
            "token": "wrong",
            "method": "tools/call",
            "server": "github",
            "tool": "search_issues",
            "arguments": {}
        });
        wr.write_all(format!("{bad}\n").as_bytes()).await.unwrap();
        assert!(lines.next_line().await.unwrap().is_none());
    }
}
