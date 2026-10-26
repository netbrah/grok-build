// crates/codegen/xai-grok-mods/src/process.rs
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

use crate::manifest::ValidatedMod;

const STDERR_RING_CAP: usize = 64 * 1024;

pub struct SidecarProcess {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
    stderr_ring: Arc<Mutex<Vec<u8>>>,
    stderr_task: Option<JoinHandle<()>>,
    pgid: Option<i32>,
}

impl SidecarProcess {
    pub fn spawn(
        validated: &ValidatedMod,
        subagent_id: &str,
        cwd: &Path,
    ) -> std::io::Result<Self> {
        let mut cmd = Command::new(&validated.program);
        cmd.args(&validated.args)
            .current_dir(cwd)
            .env_clear()
            .env("GROK_MOD_NAMESPACE", &validated.config.namespace)
            .env("GROK_MOD_SUBAGENT_ID", subagent_id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for key in &validated.config.runtime.env_allowlist {
            if let Some(val) = std::env::var_os(key) {
                cmd.env(key, val);
            }
        }

        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn()?;
        let pgid = child.id().and_then(|pid| i32::try_from(pid).ok());
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut stderr = child.stderr.take().expect("piped stderr");
        let stderr_ring = Arc::new(Mutex::new(Vec::new()));
        let ring_clone = Arc::clone(&stderr_ring);
        let stderr_task = tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut g = ring_clone.lock();
                g.extend_from_slice(&buf[..n]);
                if g.len() > STDERR_RING_CAP {
                    let excess = g.len() - STDERR_RING_CAP;
                    g.drain(..excess);
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            stdout,
            stderr_ring,
            stderr_task: Some(stderr_task),
            pgid,
        })
    }

    pub fn into_parts(
        mut self,
    ) -> (
        Child,
        ChildStdin,
        BufReader<ChildStdout>,
        Arc<Mutex<Vec<u8>>>,
        Option<JoinHandle<()>>,
        Option<i32>,
    ) {
        (
            self.child,
            self.stdin,
            self.stdout,
            self.stderr_ring,
            self.stderr_task.take(),
            self.pgid,
        )
    }

    pub fn stderr_tail(&self) -> String {
        String::from_utf8_lossy(&self.stderr_ring.lock())
            .trim()
            .to_owned()
    }

    pub async fn terminate_group(&mut self, graceful_ms: u64) {
        let _ = terminate_child(&mut self.child, self.pgid, graceful_ms).await;
        if let Some(task) = self.stderr_task.take() {
            let _ = tokio::time::timeout(Duration::from_millis(500), task).await;
        }
    }
}

pub async fn terminate_child(child: &mut Child, pgid: Option<i32>, graceful_ms: u64) -> Option<i32> {
    let grace = Duration::from_millis(graceful_ms);
    if let Ok(status) = tokio::time::timeout(grace, child.wait()).await {
        return status.ok().and_then(|s| s.code());
    }
    #[cfg(unix)]
    if let Some(pgid) = pgid {
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
    tokio::time::timeout(Duration::from_millis(500), child.wait())
        .await
        .ok()
        .and_then(|r| r.ok())
        .and_then(|s| s.code())
}
