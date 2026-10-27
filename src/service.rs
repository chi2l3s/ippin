//! Authenticated loopback control avoids signaling an unrelated reused PID.
use crate::{
    config::{Store, atomic_json},
    transport::{Route, Transport, full},
};
use anyhow::{Context, Result, bail, ensure};
use http_body_util::{BodyExt, Limited};
use hyper::{Method, Request};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    process::{Command, Stdio},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize, Deserialize)]
pub struct Instance {
    pub pid: u32,
    pub port: u16,
    pub token: String,
    #[serde(default)]
    pub route: String,
    #[serde(default)]
    pub config: Option<crate::config::Config>,
}

impl Instance {
    pub fn new(port: u16) -> Self {
        let token: [u8; 32] = rand::random();
        Self {
            pid: std::process::id(),
            port,
            token: token.iter().map(|b| format!("{b:02x}")).collect(),
            route: String::new(),
            config: None,
        }
    }
    pub fn save(&self, store: &Store) -> Result<()> {
        atomic_json(&store.path("instance.json"), self)
    }
}

pub fn load_instance(store: &Store) -> Result<Option<Instance>> {
    match fs::read(store.path("instance.json")) {
        Ok(data) => Ok(Some(
            serde_json::from_slice(&data).context("invalid instance state")?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

async fn control(instance: &Instance, stop: bool) -> Result<()> {
    let cancel = CancellationToken::new();
    let _cleanup = cancel.clone().drop_guard();
    tokio::time::timeout(Duration::from_secs(2), async {
        let req = Request::builder()
            .method(if stop { Method::POST } else { Method::GET })
            .uri(format!(
                "http://127.0.0.1:{}/_ippin/{}",
                instance.port,
                if stop { "stop" } else { "control" }
            ))
            .header("host", format!("127.0.0.1:{}", instance.port))
            .header("authorization", format!("Bearer {}", instance.token))
            .header("connection", "close")
            .body(full(""))?;
        let response = Transport::new(Route::Direct, false, 0)
            .request(req, cancel)
            .await?;
        ensure!(
            response.status().is_success(),
            "instance control returned {}",
            response.status()
        );
        let data = Limited::new(response.into_body(), 1024)
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("control response: {e}"))?
            .to_bytes();
        let reply: serde_json::Value = serde_json::from_slice(&data)?;
        ensure!(
            reply.get("pid").and_then(|p| p.as_u64()) == Some(instance.pid as u64),
            "instance identity mismatch"
        );
        Ok(())
    })
    .await
    .context("instance control timed out")?
}

pub async fn running(store: &Store) -> Result<Option<Instance>> {
    if let Some(instance) = load_instance(store)?
        && control(&instance, false).await.is_ok()
    {
        return Ok(Some(instance));
    }
    Ok(None)
}

pub async fn start(store: &Store, use_pac: bool) -> Result<Instance> {
    if let Some(instance) = running(store).await? {
        return Ok(instance);
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--home")
        .arg(&store.dir)
        .args(["start", "--headless"]);
    if use_pac {
        command.arg("-w");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child =
        crate::platform::spawn_background(command).context("start background process")?;
    for _ in 0..80 {
        if let Some(status) = child.try_wait()? {
            bail!("background process exited ({status}); see 'ippin logs'");
        }
        if let Some(instance) = running(store).await? {
            return Ok(instance);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Only terminate the child handle we created, never a PID read from disk.
    child.kill().context("stop child after startup timeout")?;
    child.wait()?;
    bail!("background process did not become ready; see 'ippin logs'")
}

pub async fn stop(store: &Store) -> Result<()> {
    let Some(instance) = load_instance(store)? else {
        return Ok(());
    };
    if let Err(error) = control(&instance, true).await {
        // A locked instance must never be mistaken for a stale PID record.
        let _lock = store
            .lock("instance.lock", true)
            .with_context(|| format!("cannot contact running instance: {error}"))?;
        if cfg!(windows) {
            crate::platform::disable_pac(store)?;
        }
        let _ = fs::remove_file(store.path("instance.json"));
        return Ok(());
    }
    for _ in 0..100 {
        if let Ok(_lock) = store.lock("instance.lock", true) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!("instance has not finished shutting down; inspect logs")
}
