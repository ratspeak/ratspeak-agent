//! First-run Docker provisioning. Always act on the offer the user actually chose.
#[cfg(target_os = "linux")]
#[path = "runtime/linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "runtime/macos.rs"]
mod platform;
#[cfg(windows)]
#[path = "runtime/windows.rs"]
mod platform;

use serde_json::{json, Value};
use std::process::Command;
use tauri::{AppHandle, Emitter};

/// Docker Desktop's installer accepts this agreement for the user, who agrees on the install screen.
#[tauri::command]
pub fn open_docker_terms() -> Result<(), String> {
    crate::updates::open_in_browser("https://www.docker.com/legal/docker-subscription-service-agreement/")
}

pub(super) struct Offer {
    action: &'static str,
    message: &'static str,
}

impl Offer {
    fn new(action: &'static str, message: &'static str) -> Self {
        Self { action, message }
    }
}

pub(super) fn describe(info: &mut Value, endpoint: Option<&str>, error: &str) {
    if error != super::DOCKER_HELP {
        info["runtime_action"] = json!("blocked");
        info["runtime_message"] = json!(error);
        return;
    }
    let offer = platform::offer(endpoint);
    info["runtime_action"] = json!(offer.action);
    info["runtime_message"] = json!(offer.message);
}

#[tauri::command]
pub(crate) async fn prepare_desktop_runtime(app: AppHandle, action: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || prepare(app, &action))
        .await
        .map_err(|e| e.to_string())?
}

fn prepare(app: AppHandle, action: &str) -> Result<(), String> {
    let _guard = super::lock(&app)?;
    let info = super::desktop_status(app.clone(), super::AgentKind::Desktop,None,None,None)?;
    if info["runtime_ready"] == true || info["runtime_action"].as_str() != Some(action) {
        return Ok(());
    }
    if !matches!(action, "install" | "start" | "access") {
        return Err("Docker setup is unavailable for this configuration.".into());
    }
    platform::prepare(&app, action)
}

#[tauri::command]
pub(crate) async fn reconnect_desktop_agent(app: AppHandle, kind: super::AgentKind, port: Option<u16>, link: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || reconnect(&app, kind, port, &link))
        .await.map_err(|e| e.to_string())?
}

fn reconnect(app: &AppHandle, kind: super::AgentKind, port: Option<u16>, link: &str) -> Result<Value, String> {
    use std::{thread, time::{Duration, Instant}};
    let _guard = super::lock(app)?;
    let engine = super::Engine::local()?;
    if let Err(error) = engine.hardware() {
        if error != super::DOCKER_HELP { return Err(error); }
        // Colima removes its context when stopped. Restore only the engine this
        // app previously used, never create a new VM for an existing agent.
        #[cfg(target_os = "macos")]
        let restored = platform::restore_context(app, &engine.endpoint)?;
        #[cfg(not(target_os = "macos"))]
        let restored = false;
        if !restored {
            let offer = platform::offer(Some(&engine.endpoint));
            if offer.action != "start" {
                return Err(if offer.message.is_empty() {
                    "Open the Docker engine used by this agent, then reconnect. Your agent’s data has not been changed.".into()
                } else { offer.message.into() });
            }
            platform::prepare(app, "start")?;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let engine = super::Engine::local()?;
        if engine.hardware().is_ok() {
            let engine = engine.selected(kind, port, Some(link))?;
            let agent = engine.agent()?.ok_or("This agent was not found in the connected Docker engine.")?;
            crate::lifecycle::monitor_desktop();
            return Ok(engine.with_lan(app, agent));
        }
        if Instant::now() >= deadline {
            return Err("Docker is still unavailable. Check for a Docker or system prompt, then reconnect.".into());
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn progress(app: &AppHandle, text: &str) {
    let _ = app.emit("runtime-progress", text);
}

fn run(command: &mut Command, error: &str) -> Result<(), String> {
    command
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| ())
        .ok_or_else(|| error.to_owned())
}

#[cfg(any(target_os = "macos", windows))]
fn download(app: &AppHandle, url: &str, destination: &std::path::Path) -> Result<(), String> {
    use std::{fs, process::Stdio, thread, time::Duration};
    progress(app, "Downloading Docker Desktop…");
    let mut child = crate::network::curl()
        .args([
            "--disable",
            "--fail",
            "--silent",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "15",
            "--speed-limit",
            "1024",
            "--speed-time",
            "60",
            "--max-time",
            "3600",
            "--output",
        ])
        .arg(destination)
        .arg(url)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Couldn’t start the Docker Desktop download.")?;
    let mut last_mb = 0;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err("Download interrupted. Check your internet connection and available disk space, then try again.".into())
            };
        }
        let mb = fs::metadata(destination)
            .map(|f| f.len() / 1_000_000)
            .unwrap_or_default();
        if mb != last_mb {
            progress(app, &format!("Downloading Docker Desktop · {mb} MB"));
            last_mb = mb;
        }
        thread::sleep(Duration::from_millis(500));
    }
}
