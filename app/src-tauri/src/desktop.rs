//! Manage independent Desktop and Hermes agents on the local Docker engine.
pub(crate) mod lan;
pub(crate) mod hermes;
pub(crate) mod hermes_discovery;
mod hermes_address;
mod lan_startup;
mod memory;
mod beta;
mod gpu;
mod instances;
pub(crate) mod removal;
mod update;
pub use instances::AgentKind;
use instances::{Slot, PRIMARY};
pub(crate) mod runtime;
use crate::{check_common, venice, Settings};
use serde_json::{json, Value};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};
use tempfile::tempdir;

const LABEL: &str = "org.ratspeak.desktop-agent";
const IMAGE: &str = "ratspeak/desktop-agent:setup-v1";
// static, not const: each use of a const embeds its own copy of the bytes.
static AMD64: &[u8] = include_bytes!("../../../build/dist/desktop-native-amd64.gz");
const DOCKERFILE: &str = include_str!("../../../desktop/Dockerfile");
const MODELS: &str = include_str!("../../../ratspeak_pi/desktop_models.json");
const STATE: &str = "/var/lib/ratspeak-pi";
// The receiver never needs root powers; changing these updates existing agents.
const RECEIVER_OPTIONS: [&str; 4] = ["--security-opt", "no-new-privileges", "--cap-drop", "ALL"];
/// Marks a receiver created with the graphics card, so updates recreate it the same way.
const GPU_LABEL: &str = "org.ratspeak.gpu=1";
fn uses_gpu(container: &Value) -> bool {
    container["Config"]["Labels"]["org.ratspeak.gpu"] == "1"
}
const DOCKER_HELP: &str = "Start your local Docker Linux engine and check again.";
const WINDOWS_CONTAINERS: &str = "In the Docker Desktop menu, choose “Switch to Linux containers”. Ratspeak Agent will continue when Docker is ready.";
const UNOWNED: &str = "A Docker resource with this name already exists outside Ratspeak Agent. Choose a different Docker context or rename that resource.";

fn cli() -> Command {
    let path = env::var_os("PATH").unwrap_or_default();
    #[cfg_attr(target_os = "linux", allow(unused_mut))] // Only macOS and Windows add folders.
    let mut dirs: Vec<PathBuf> = env::split_paths(&path).collect();
    #[cfg(target_os = "macos")]
    if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".docker/bin"));
    }
    #[cfg(target_os = "macos")]
    dirs.extend(
        [
            "/usr/local/bin",
            "/opt/homebrew/bin",
            "/Applications/Docker.app/Contents/Resources/bin",
        ]
        .map(PathBuf::from),
    );
    #[cfg(windows)]
    if let Some(programs) = env::var_os("ProgramFiles") {
        dirs.push(PathBuf::from(programs).join("Docker/Docker/resources/bin"));
    }
    #[cfg(windows)]
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Programs/DockerDesktop/resources/bin"));
    }
    let exe = if cfg!(windows) {
        "docker.exe"
    } else {
        "docker"
    };
    let file = dirs
        .iter()
        .map(|dir| dir.join(exe))
        .find(|p| p.is_file())
        .unwrap_or_else(|| exe.into());
    let mut cmd = Command::new(file);
    if let Ok(paths) = env::join_paths(dirs) {
        cmd.env("PATH", paths);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

#[derive(Clone)]
struct Engine {
    endpoint: String,
    slot: Slot,
}
impl Engine {
    fn local() -> Result<Self, String> {
        // The tray and the LAN helper ask every few seconds; each lookup is two docker processes.
        static CONTEXT: std::sync::Mutex<Option<(Instant, String)>> = std::sync::Mutex::new(None);
        if let Some(host) = env::var_os("DOCKER_HOST").filter(|_| env::var_os("DOCKER_CONTEXT").is_none()) {
            return Self::from_endpoint(host.to_string_lossy().into_owned());
        }
        let mut cached = CONTEXT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, host)) = cached.as_ref().filter(|(at, _)| at.elapsed() < Duration::from_secs(60)) {
            return Self::from_endpoint(host.clone());
        }
        let host = {
            let context = cli()
                .args(["context", "show"])
                .output()
                .map_err(|_| DOCKER_HELP)?;
            if !context.status.success() {
                return Err(DOCKER_HELP.into());
            }
            let name = String::from_utf8_lossy(&context.stdout);
            let endpoint = cli()
                .args([
                    "context",
                    "inspect",
                    "--format",
                    "{{.Endpoints.docker.Host}}",
                    "--",
                    name.trim(),
                ])
                .output()
                .map_err(|_| DOCKER_HELP)?;
            if !endpoint.status.success() {
                return Err("Couldn’t read the selected Docker context. Check it in Docker or select a working local context. Ratspeak Agent will continue when it’s ready.".into());
            }
            String::from_utf8_lossy(&endpoint.stdout).trim().to_owned()
        };
        let engine = Self::from_endpoint(host.clone())?;
        *cached = Some((Instant::now(), host));
        Ok(engine)
    }
    fn from_endpoint(host: String) -> Result<Self, String> {
        if !host.starts_with("unix:///") && !host.starts_with("npipe:////./pipe/") {
            return Err("Select a local Docker context to host the agent on this computer.".into());
        }
        Ok(Self { endpoint: host, slot: PRIMARY })
    }
    fn cmd(&self) -> Command {
        let mut cmd = cli();
        cmd.env_remove("DOCKER_CONTEXT")
            .env_remove("DOCKER_HOST")
            .arg("--host")
            .arg(&self.endpoint);
        cmd
    }
    fn call(&self, args: &[&str]) -> Result<Output, String> {
        self.cmd()
            .args(args)
            .output()
            .map_err(|_| DOCKER_HELP.into())
    }
    fn require(&self, args: &[&str], error: &str) -> Result<String, String> {
        let out = self.call(args)?;
        if !out.status.success() {
            return Err(error.into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
    /// Like `require`, explaining a failure with Docker's own reason.
    fn require_explained(&self, args: &[&str], failed: &str) -> Result<String, String> {
        let out = self.call(args)?;
        if !out.status.success() {
            return Err(docker_failure(failed, &String::from_utf8_lossy(&out.stderr)));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
    fn object(&self, kind: &str, name: &str) -> Result<Option<Value>, String> {
        let out = self.call(&[kind, "inspect", name])?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).to_ascii_lowercase();
            if err.contains("no such") || err.contains("not found") {
                return Ok(None);
            }
            return Err(DOCKER_HELP.into());
        }
        let data: Vec<Value> =
            serde_json::from_slice(&out.stdout).map_err(|_| "Could not inspect the agent.")?;
        let obj = data.into_iter().next().ok_or("Missing Docker metadata.")?;
        let labels = if kind == "volume" {
            &obj["Labels"]
        } else {
            &obj["Config"]["Labels"]
        };
        if labels[LABEL] != "1" {
            return Err(UNOWNED.into());
        }
        Ok(Some(obj))
    }
    fn input(&self, op: &str, data: &Value) -> Result<(), String> {
        self.input_reporting(op, data, &|_| {})
    }
    /// Like `input`, passing each JSON line the command prints (install progress) to `progress`.
    fn input_reporting(&self, op: &str, data: &Value, progress: &dyn Fn(Value)) -> Result<(), String> {
        use std::io::BufRead;
        let target = self.object("container", &self.slot.container())?.ok_or("Missing managed container.")?;
        let id = target["Id"].as_str().ok_or("Missing container identity.")?;
        let mut cmd = self.cmd();
        cmd.args(["exec", "-i", id, "python3"]);
        if op == "access" {
            cmd.args(["-c", include_str!("../../../ratspeak_pi/access_control.py")]);
        } else {
            cmd.args(["-m", "ratspeak_pi.desktop", op]);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| DOCKER_HELP)?;
        let sent = serde_json::to_writer(child.stdin.take().unwrap(), data);
        let mut reported = None;
        for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                match value["error"].as_str() {
                    Some(error) => reported = Some(error.to_owned()),
                    None => progress(value),
                }
            }
        }
        let done = child.wait().map_err(|_| "The container stopped.")?;
        if done.code() == Some(42) {
            return Err("Venice could not return a test reply from this computer. Check the model, key permissions and available credit, then retry.".into());
        }
        if !done.success() || sent.is_err() {
            return Err(match reported {
                Some(error) if is_disk_full(&error) => DISK_FULL.into(),
                Some(error) => error,
                None => "The agent could not save its settings or prepare the model. Check the internet connection, free disk space and Docker memory, then retry.".into(),
            });
        }
        Ok(())
    }
    fn create_receiver(&self, image: &str, kind: AgentKind, gpu: bool) -> Result<(), String> {
        let label = format!("{LABEL}=1");
        let (name, mount, publish) = (
            self.slot.container(),
            format!("type=volume,source={},target={STATE}", self.slot.volume()),
            format!("{}:4243/tcp", self.slot.port),
        );
        let mut args = vec!["create", "--name", &name, "--label", &label, "--label", kind.label(), "--init",
            "--restart", "unless-stopped", "--mount", &mount, "--publish", &publish];
        if gpu {
            args.extend(["--gpus", "all", "--label", GPU_LABEL]);
        }
        args.extend(RECEIVER_OPTIONS);
        args.push(image);
        self.require(&args, "Could not create the agent container.").map(|_| ())
    }
    /// The host's NVIDIA card, with why Docker can't use it, if it can't.
    fn gpu(&self) -> Option<(gpu::Card, Option<&'static str>)> {
        let card = gpu::host()?;
        let info = self
            .require(&["info", "--format", "{{.Architecture}}|{{.OperatingSystem}}|{{.KernelVersion}}"], DOCKER_HELP)
            .ok()?;
        let problem = gpu::blocked(&info, &self.endpoint);
        Some((card, problem))
    }
    fn hardware(&self) -> Result<(u64, String), String> {
        let data = self.require(
            &[
                "info",
                "--format",
                "{{.OSType}} {{.MemTotal}} {{.Architecture}}",
            ],
            DOCKER_HELP,
        )?;
        let mut fields = data.split_whitespace();
        match fields.next() {
            Some("linux") => (),
            Some("windows") => return Err(WINDOWS_CONTAINERS.into()),
            _ => return Err(DOCKER_HELP.into()),
        }
        let bytes: u64 = fields
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or(DOCKER_HELP)?;
        let arch = fields
            .next()
            .filter(|s| matches!(*s, "x86_64" | "amd64" | "aarch64" | "arm64"))
            .ok_or("This engine needs an ARM64 or x86-64 Linux VM.")?;
        Ok((bytes >> 20, arch.to_owned()))
    }
    /// The slot's agent, or one to repair when a container exists but its settings can't be read.
    fn agent_or_broken(&self) -> Result<Option<Value>, String> {
        match self.agent() {
            Err(detail) => {
                let Some(container) = self.object("container", &self.slot.container())? else {
                    return Err(detail);
                };
                let hermes = AgentKind::from_labels(&container["Config"]["Labels"]) == AgentKind::Hermes;
                Ok(Some(json!({"broken": true, "detail": detail, "hermes": hermes, "lan_port": self.slot.port})))
            }
            agent => agent,
        }
    }
    fn agent(&self) -> Result<Option<Value>, String> {
        let Some(obj) = self.object("container", &self.slot.container())? else {
            return Ok(None);
        };
        if obj["State"]["Running"] == true {
            let id = obj["Id"].as_str().ok_or("Missing container identity.")?;
            let output = self.call(&["exec", id, "python3", "-c", include_str!("../../../ratspeak_pi/dashboard.py")])?;
            if output.status.success() {
                let mut snapshot: Value = serde_json::from_slice(&output.stdout).map_err(|_| "Invalid agent status.")?;
                if snapshot["provider"] == "local" {
                    snapshot["stats"]["device_memory"] = json!(memory::reading());
                }
                snapshot["lan_port"] = json!(self.slot.port);
                return Ok(Some(snapshot));
            }
        }
        let tmp = tempdir().map_err(|e| e.to_string())?;
        let path = tmp.path().join("agent.json");
        let id = obj["Id"].as_str().ok_or("Missing container identity.")?;
        let copy = self.call(&[
            "cp",
            &format!("{id}:{STATE}/config.json"),
            path.to_str().ok_or("Invalid temporary folder.")?,
        ])?;
        if !copy.status.success() {
            let error = String::from_utf8_lossy(&copy.stderr);
            // Docker's wording for a missing path differs between versions.
            if error.contains("Could not find") || error.contains("No such container:path") {
                return Ok(None);
            }
            return Err(docker_failure("Could not read the desktop agent settings", &error));
        }
        let cfg: Value = serde_json::from_slice(&fs::read(path).map_err(|e| docker_failure("Could not read the desktop agent settings", &e.to_string()))?)
            .map_err(|_| "The agent’s settings are damaged.")?;
        let selected = if cfg["provider"] == "venice" {
            let public_model = tmp.path().join("model");
            let read_model = self.call(&[
                "cp",
                &format!("{id}:{STATE}/venice-model"),
                public_model.to_str().ok_or("Invalid temporary folder.")?,
            ])?;
            if read_model.status.success() {
                fs::read_to_string(public_model).ok()
            } else {
                None
            }
        } else {
            None
        };
        let state = if obj["State"]["Running"] != true {
            "stopped"
        } else if self
            .call(&["exec", &self.slot.container(), "test", "-f", "/tmp/ratspeak-agent-ready"])?
            .status
            .success()
        {
            "running"
        } else {
            "starting"
        };
        Ok(Some(
            json!({"name":cfg["name"],"provider":cfg["provider"],"model":selected.as_deref().or(cfg["model"].as_str()),
            "link":cfg["link"],"access":cfg["access"],"model_name":cfg["model_name"],"features":cfg["features"],"state":state,"lan_port":self.slot.port}),
        ))
    }
    fn lan_container(&self) -> Result<Option<String>, String> {
        let Some(obj) = self.object("container", &self.slot.container())? else {
            return Ok(None);
        };
        let published = obj["NetworkSettings"]["Ports"]["4243/tcp"]
            .as_array()
            .is_some_and(|ports| {
                ports.iter().any(|p| {
                    p["HostPort"].as_str().and_then(|s| s.parse::<u16>().ok()) == Some(self.slot.port)
                        && matches!(p["HostIp"].as_str(), Some("127.0.0.1" | "0.0.0.0" | ""))
                })
            });
        Ok((obj["State"]["Running"] == true && published)
            .then(|| obj["Id"].as_str().map(str::to_owned))
            .flatten())
    }
    fn announce_on_lan(&self, id: &str) {
        // Also supported by agents installed before the host LAN helper existed.
        let _ = self.call(&["exec", id, "python3", "-c",
            "from ratspeak_pi.desktop import units; from agent_rat import rpc; rpc(units.PROFILE, 'announce', rpc_timeout=5)"]);
    }
    fn with_lan(&self, app: &AppHandle, mut agent: Value) -> Value {
        crate::lifecycle::monitor_desktop();
        if agent["state"] == "running" {
            agent["lan_state"] = json!(enable_lan(app, self));
        }
        agent
    }
    fn ready(&self) -> Result<Value, String> {
        let deadline = Instant::now() + Duration::from_secs(250);
        while Instant::now() < deadline {
            if self
                .call(&["exec", &self.slot.container(), "test", "-f", "/tmp/ratspeak-agent-ready"])?
                .status
                .success()
            {
                if let Some(agent) = self.agent()? {
                    if agent["state"] == "running" {
                        return Ok(agent);
                    }
                }
            }
            if self
                .call(&["exec", &self.slot.container(), "test", "-f", "/tmp/ratspeak-agent-error"])?
                .status
                .success()
            {
                break;
            }
            thread::sleep(Duration::from_millis(750));
        }
        Err("The agent has not started. Check Docker memory and the agent’s status before retrying.".into())
    }
}

/// The CUDA runner and NVIDIA's runtime (`llama_cuda` in models.json).
const GPU_RUNNER_MB: u64 = 800;
/// The agent image, its build, and Docker's own growth.
const DOCKER_MARGIN_MB: u64 = 3072;

/// Disk a local model and its extras take where Docker keeps its data.
fn install_mb(model: &Value, gpu: bool, features: &std::collections::BTreeMap<String, bool>) -> u64 {
    let on = |id: &str| features.get(id) == Some(&true);
    let libraries: u64 = crate::offline::libraries().iter().filter(|(id, _)| on(id)).map(|(_, l)| l.size_mb).sum();
    // Voice messages are on unless chosen off.
    let voice = if features.get("voice") == Some(&false) { 0 } else { crate::offline::voice_engines()["whisper-base"].size_mb };
    model["size_mb"].as_u64().unwrap_or(0) + if gpu { GPU_RUNNER_MB } else { 30 } + libraries + voice + DOCKER_MARGIN_MB
}

/// Free space on the disk that holds this computer's Docker data, in MB: None for a remote engine or when unknown.
fn docker_disk_free_mb(endpoint: &str) -> Option<u64> {
    if !(endpoint.starts_with("unix://") || endpoint.starts_with("npipe://")) {
        return None;
    }
    #[cfg(windows)]
    let place = PathBuf::from(env::var_os("LOCALAPPDATA")?);
    #[cfg(target_os = "macos")]
    let place = PathBuf::from(env::var_os("HOME")?);
    #[cfg(target_os = "linux")]
    let place = ["/var/lib/docker", "/var/lib"].into_iter().map(PathBuf::from).find(|p| p.exists())?;
    free_mb(&place)
}

#[cfg(unix)]
fn free_mb(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // The path is NUL-terminated and stats is a writable statvfs for this synchronous call.
    (unsafe { libc::statvfs(path.as_ptr(), &mut stats) } == 0)
        .then(|| (stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64) >> 20)
}

#[cfg(windows)]
fn free_mb(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    // The path is NUL-terminated and free is writable for this synchronous call; the totals are not wanted.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free >> 20)
}

/// A failed Docker step explained: known causes name their fix; anything else carries Docker's own words.
const DISK_FULL: &str = "This computer or Docker is out of disk space. Free some space, then try again.";
fn is_disk_full(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("no space left") || lower.contains("not enough space")
}

fn docker_failure(failed: &str, error: &str) -> String {
    let lower = error.to_lowercase();
    if is_disk_full(error) {
        return DISK_FULL.into();
    }
    if lower.contains("read-only file system") {
        return "Docker’s storage became read-only, which usually follows a full disk. Free some space, restart Docker, then try again.".into();
    }
    if lower.contains("already in progress") {
        return "Docker is still removing the agent. Wait a moment, then try again.".into();
    }
    let said = error.lines().map(str::trim).filter(|line| !line.is_empty()).last().unwrap_or("");
    let said = said.trim_start_matches("Error response from daemon: ").trim_start_matches("Error: ");
    match said.char_indices().nth(240) {
        _ if said.is_empty() => format!("{failed}."),
        Some((cut, _)) => format!("{failed}. Docker said: {}…", &said[..cut]),
        None => format!("{failed}. Docker said: {said}"),
    }
}

fn host_ram() -> u64 {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("MemTotal:"))?
                    .split_whitespace()
                    .nth(1)?
                    .parse::<u64>()
                    .ok()
            })
            .unwrap_or(0)
            / 1024
    }
    #[cfg(target_os = "macos")]
    let cmd = Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.memsize"])
        .output();
    #[cfg(windows)]
    let cmd = {
        use std::os::windows::process::CommandExt;
        Command::new("powershell.exe")
            .creation_flags(0x0800_0000)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
            ])
            .output()
    };
    #[cfg(not(target_os = "linux"))]
    {
        cmd.ok()
            .and_then(|o| String::from_utf8(o.stdout).ok()?.trim().parse::<u64>().ok())
            .unwrap_or(0)
            >> 20
    }
}
/// Docker's VM still maps the model file while the card holds it.
const GPU_ENGINE_MB: u64 = 4096;

/// The models this computer can run: on a usable graphics card, those that fit its memory; else those that fit RAM.
fn offered(host: u64, engine: u64, card: Option<&gpu::Card>) -> Vec<Value> {
    let catalog: Vec<Value> = serde_json::from_str(MODELS).expect("desktop catalog");
    let need = |m: &Value, overhead: &str| m["size_mb"].as_u64().unwrap() + m[overhead].as_u64().unwrap();
    if let Some(card) = card {
        if engine < GPU_ENGINE_MB {
            return Vec::new();
        }
        return catalog
            .into_iter()
            .filter(|m| need(m, "gpu_overhead_mb") <= card.budget_mb())
            .map(|mut m| {
                m["on_gpu"] = json!(true);
                m
            })
            .collect();
    }
    let budget = if host > 0 {
        host.saturating_sub(1536).min(engine.saturating_sub(768))
    } else {
        engine.saturating_sub(1024)
    };
    catalog.into_iter().filter(|m| need(m, "overhead_mb") <= budget).collect()
}
#[tauri::command(async)]
pub fn desktop_status(app: AppHandle, kind: AgentKind, port: Option<u16>, link: Option<String>, create: Option<bool>) -> Result<Value, String> {
    let ram = host_ram();
    let mut info = json!({"platform":env::consts::OS,"arch":env::consts::ARCH,"ram_mb":ram,"models":[],"agent":null,
        "runtime_ready":false,"runtime_message":DOCKER_HELP});
    let engine = match Engine::local() {
        Ok(e) => e,
        Err(e) => {
            runtime::describe(&mut info, None, &e);
            return Ok(info);
        }
    };
    match engine.hardware() {
        Ok((mem, _)) => {
            info["runtime_ready"] = json!(true);
            info["runtime_ram_mb"] = json!(mem);
            let card = engine.gpu();
            info["gpu"] = json!(card.as_ref().map(|(card, problem)| gpu::describe(card, *problem)));
            let usable = card.as_ref().filter(|(_, problem)| problem.is_none()).map(|(card, _)| card);
            info["models"] = json!(offered(ram, mem, usable));
        }
        Err(e) => {
            runtime::describe(&mut info, Some(&engine.endpoint), &e);
            return Ok(info);
        }
    }
    let engine = if create == Some(true) { engine.vacant(kind)? } else { engine.selected(kind,port,link.as_deref())? };
    info["lan_port"] = json!(engine.slot.port);
    info["agent"] = match engine.agent_or_broken()? {
        Some(agent) if agent["broken"] == true => agent,
        agent => json!(agent.map(|a| engine.with_lan(&app, a))),
    };
    Ok(info)
}

#[tauri::command(async)]
pub fn desktop_snapshot(app: AppHandle, kind: AgentKind, port: Option<u16>, link: Option<String>) -> Result<Option<Value>, String> {
    let mut agent = Engine::local()?.selected(kind,port,link.as_deref())?.agent()?;
    if let Some(agent) = &mut agent {
        use tauri::Manager;
        let dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
        agent["lan_state"] = json!(fs::read_to_string(dir.join("agent-lan/state")).ok());
    }
    Ok(agent)
}

#[tauri::command(async)]
pub fn beta_unlocked(app: AppHandle) -> Result<bool, String> {
    use tauri::Manager;
    beta::enabled(&app.path().app_local_data_dir().map_err(|e| e.to_string())?)
}

#[tauri::command(async)]
pub fn beta_unlock(app: AppHandle, password: String) -> Result<(), String> {
    use tauri::Manager;
    beta::unlock(&app.path().app_local_data_dir().map_err(|e| e.to_string())?, &password)
}

#[tauri::command(async)]
pub fn desktop_contacts(app: AppHandle, kind: AgentKind, port: Option<u16>, request: Value) -> Result<Value, String> {
    use tauri::Manager;
    let link = request["link"].as_str().ok_or("Choose an agent first.")?;
    if serde_json::to_vec(&request).map_err(|_| "Invalid contact action.")?.len() > 32768 {
        return Err("Shorten this request before sending.".into());
    }
    let (engine, id) = contacts_target(kind, port, link)?;
    let dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    let remembered = beta::enabled(&dir)?;
    let mut next = contacts_request(&engine, &id, &request)?;
    if remembered && next["beta_locked"] == true {
        next = contacts_request(&engine, &id, &json!({"link":link,"choice":"unlock","password":beta::PASSWORD}))?;
    }
    if !remembered && next["beta_locked"] == false {
        beta::remember(&dir)?;
    }
    Ok(next)
}

fn contacts_target(kind: AgentKind, port: Option<u16>, link: &str) -> Result<(Engine, String), String> {
    let engine = Engine::local()?.selected(kind, port, Some(link))?;
    let target = engine.object("container", &engine.slot.container())?.ok_or("This agent is unavailable.")?;
    if target["State"]["Running"] != true {
        return Err("Start the agent to use A2A.".into());
    }
    let id = target["Id"].as_str().ok_or("Missing container identity.")?.to_owned();
    Ok((engine, id))
}

/// One file the owner attaches to an A2A request or reply. It waits on the agent until sent.
#[tauri::command]
pub async fn desktop_contact_attach(kind: AgentKind, port: Option<u16>, link: String, id: String) -> Result<Option<Value>, String> {
    use base64::Engine as _;
    let Some(file) = rfd::AsyncFileDialog::new().set_title("Attach a file").pick_file().await else { return Ok(None) };
    let path = file.path().to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let size = fs::metadata(&path).map_err(|_| "Could not read that file.")?.len();
        if size == 0 || size > 900_000 {
            return Err(if size == 0 { "That file is empty." } else { "Choose a file under 900 KB." }.to_string());
        }
        let data = fs::read(&path).map_err(|_| "Could not read that file.")?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (engine, container) = contacts_target(kind, port, &link)?;
        let staged = contacts_request(&engine, &container, &json!({"link":link,"choice":"stage","id":id,"name":name,
            "data":base64::engine::general_purpose::STANDARD.encode(data)}))?;
        Ok(Some(staged["attachment"].clone()))
    })
    .await
    .map_err(|_| "Could not attach that file.".to_string())?
}

/// Copy a request or reply file out of the agent to a place the owner chooses.
#[tauri::command]
pub async fn desktop_contact_save(kind: AgentKind, port: Option<u16>, link: String, id: String, part: String) -> Result<bool, String> {
    use base64::Engine as _;
    let file = tauri::async_runtime::spawn_blocking(move || {
        let (engine, container) = contacts_target(kind, port, &link)?;
        contacts_request(&engine, &container, &json!({"link":link,"choice":"file","id":id,"part":part}))
    })
    .await
    .map_err(|_| "Could not read that file.".to_string())??;
    let data = base64::engine::general_purpose::STANDARD
        .decode(file["data"].as_str().unwrap_or_default())
        .map_err(|_| "Could not read that file.")?;
    let name: String = file["name"].as_str().unwrap_or("file").chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_')).collect();
    let name = name.trim().trim_start_matches('.');
    let Some(target) = rfd::AsyncFileDialog::new().set_title("Save file")
        .set_file_name(if name.is_empty() { "file" } else { name }).save_file().await else { return Ok(false) };
    fs::write(target.path(), data).map_err(|_| "Could not save the file there.")?;
    Ok(true)
}

fn contacts_request(engine: &Engine, id: &str, request: &Value) -> Result<Value, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut input = tempfile::tempfile().map_err(|_| "Could not prepare contact action.")?;
    serde_json::to_writer(&mut input, &request).map_err(|_| "Invalid contact action.")?;
    input.rewind().map_err(|_| "Could not prepare contact action.")?;
    let mut output = tempfile::tempfile().map_err(|_| "Could not read Contacts.")?;
    let writer = output.try_clone().map_err(|_| "Could not read Contacts.")?;
    let mut child = engine.cmd().args(["exec", "-i", id, "python3", "-m", "ratspeak_pi.contacts"])
        .stdin(input).stdout(writer).stderr(Stdio::null())
        .spawn().map_err(|_| DOCKER_HELP)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    const MAX_BYTES: u64 = 32 * 1024 * 1024;
    let status = loop {
        if Instant::now() >= deadline || output.metadata().map_err(|_| "Could not read Contacts.")?.len() > MAX_BYTES {
            let _ = child.kill();
            let _ = child.wait();
            return Err("The agent stopped responding. Check its status and try again.".into());
        }
        if let Some(status) = child.try_wait().map_err(|_| "Could not read Contacts.")? { break status; }
        thread::sleep(Duration::from_millis(40));
    };
    if !status.success() {
        return Err("Contacts needs the current agent runtime. Update this agent and try again.".into());
    }
    output.seek(SeekFrom::Start(0)).map_err(|_| "Could not read Contacts.")?;
    let result: Value = serde_json::from_reader(output.take(MAX_BYTES)).map_err(|_| "Could not read Contacts.")?;
    if result["ok"] != true {
        return Err(result["error"].as_str().unwrap_or("Could not complete this contact action.").into());
    }
    Ok(result["state"].clone())
}

#[tauri::command(async)]
pub fn desktop_access(app: AppHandle, kind: AgentKind, port: Option<u16>, request: Value) -> Result<Value, String> {
    let _guard = lock(&app)?;
    let engine = Engine::local()?.selected(kind,port,request["link"].as_str())?;
    let agent = engine.agent()?.ok_or("No desktop agent.")?;
    if agent["state"] != "running" || agent["link"] != request["link"] || agent["access"] != request["expected"] {
        return Err("Access changed or the agent stopped. Check the current settings and try again.".into());
    }
    engine.input("access", &request).map_err(|_| "Could not save access. Check the current settings and try again.".to_string())?;
    engine.agent()?.ok_or("The desktop agent is unavailable.".into())
}

#[tauri::command(async)]
pub fn managed_agents(app: AppHandle) -> Result<Value,String> {
    let mut rows = crate::pi_pairing::directory(&app)?;
    let engine = match Engine::local() { Ok(engine) => engine, Err(_) => return Ok(json!({"agents":rows,"desktop_available":false})) };
    let slots = match engine.installed_slots() { Ok(slots) => slots, Err(_) => return Ok(json!({"agents":rows,"desktop_available":false})) };
    for slot in slots {
        // An agent that can't be read is repaired from its setup card, so it never hides the others.
        if let Ok(Some(agent)) = engine.in_slot(slot).agent() {
            let kind = if agent["provider"] == "hermes" { "hermes" } else { "desktop" };
            let model = if agent["provider"] == "venice" { agent["model"].as_str() } else { agent["model_name"].as_str() };
            rows.push(json!({"kind":kind,"link":agent["link"],"name":agent["name"],"provider":agent["provider"],
                "state":agent["state"],"lan_port":slot.port,"model":model.or_else(||agent["model"].as_str())}));
        }
    }
    Ok(json!({"agents":rows,"desktop_available":true}))
}

pub fn running_states() -> Result<Vec<(AgentKind,bool)>, String> {
    let engine = Engine::local()?;
    let mut states = Vec::new();
    for slot in engine.installed_slots()? {
        if let Some(c) = engine.object("container", &slot.container())? {
            states.push((AgentKind::from_labels(&c["Config"]["Labels"]), c["State"]["Running"] == true));
        }
    }
    Ok(states)
}

pub fn stop_for_exit(app: &AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let _guard = lock(app)?;
    let engine = Engine::local()?;
    for slot in engine.installed_slots()? {
        if let Some(obj) = engine.object("container", &slot.container())? {
            if obj["State"]["Running"] == true {
                engine.require(&["stop", obj["Id"].as_str().ok_or("Missing container identity.")?], "Could not stop the agent. Ratspeak Agent is still open.")?;
            }
        }
    }
    let dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?.join("agent-lan");
    if dir.is_dir() { lan::write_changed(&dir.join("enabled"), b"no").map_err(|e| e.to_string())?; }
    Ok(())
}

fn enable_lan(app: &AppHandle, engine: &Engine) -> String {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    lan_startup::enable(app, engine).unwrap_or_else(|_| "unavailable".into())
}

pub fn attach_existing(app: AppHandle) {
    thread::spawn(move || {
        if let Ok(engine) = Engine::local() {
            if let Ok(_guard) = lock(&app) {
                for slot in engine.installed_slots().unwrap_or_default() {
                    let _ = engine.in_slot(slot).update_receiver();
                }
            }
            if engine.installed_slots().unwrap_or_default().into_iter().any(|s| matches!(engine.in_slot(s).lan_container(), Ok(Some(_)))) {
                let _ = enable_lan(&app, &engine);
            }
        }
    });
}
fn lock(app: &AppHandle) -> Result<fs::File, String> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(crate::cache(app)?.join("desktop.lock"))
        .map_err(|e| e.to_string())?;
    file.try_lock()
        .map_err(|_| "Another setup operation is running.")?;
    Ok(file)
}
#[tauri::command]
pub async fn desktop_install(app: AppHandle, settings: Settings, hermes: Option<hermes::Connection>, create: Option<bool>, attempt: Option<String>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || install(&app, settings, hermes, create, attempt.as_deref())).await.map_err(|e| e.to_string())?
}

fn install(app: &AppHandle, settings: Settings, hermes: Option<hermes::Connection>, create: Option<bool>, attempt: Option<&str>) -> Result<Value, String> {
    let _guard = lock(app)?;
    let hermes_config = if settings.provider == "hermes" {
        if settings.access.mode != "later" || !settings.access.owners.is_empty() || settings.model.is_some()
            || settings.venice_model.is_some() || settings.venice_api_key.is_some() {
            return Err("Hermes requires a single owner paired through Ratspeak Agent.".into());
        }
        use tauri::Manager;
        if !beta::enabled(&app.path().app_local_data_dir().map_err(|e| e.to_string())?)? {
            return Err("Connect Hermes is in beta. Unlock it with the beta password first.".into());
        }
        let mut connection = hermes.ok_or("Enter the Hermes API server key.")?;
        connection.host = hermes_address::normalize(connection.host.as_deref())?;
        let model = connection.verify()?;
        Some(hermes::Gateway { connection, model })
    } else { None };
    // Hermes is desktop-only. Pi settings keep the existing provider restriction.
    let mut common = settings.clone();
    if hermes_config.is_some() { common.provider = "local".into(); }
    check_common(&common)?;
    let kind = AgentKind::from_provider(&settings.provider);
    let local = Engine::local()?;
    let attempt = if create == Some(true) {
        Some(attempt.filter(|s| s.len()==32 && s.bytes().all(|b| b.is_ascii_hexdigit())).ok_or("Reopen Add agent to start a new setup.")?)
    } else { None };
    let pending = match attempt { Some(token) => local.pending_creation(kind,token)?, None => None };
    let retry = pending.is_some();
    let engine = match pending {
        Some(engine) => engine,
        None if attempt.is_some() => local.vacant(kind)?,
        None => local.for_kind(kind)?,
    };
    let slot = engine.slot;
    let (mem, _) = engine.hardware()?;
    // Only local models use the card; the container keeps it for later model changes.
    let usable_gpu = engine.gpu().filter(|(_, problem)| problem.is_none()).map(|(card, _)| card)
        .filter(|_| settings.provider == "local");
    let on_gpu = usable_gpu.is_some();
    if settings.provider == "local"
        && !offered(host_ram(), mem, usable_gpu.as_ref())
            .iter()
            .any(|m| m["id"].as_str() == settings.model.as_deref())
    {
        return Err("Choose a local model that fits this Docker engine.".into());
    }
    if settings.provider == "venice" {
        venice::verify_choice(
            settings.venice_api_key.as_deref().unwrap(),
            settings.venice_model.as_deref().unwrap(),
        )?;
    }
    if !retry && engine.agent()?.is_some() {
        return Err("This agent is already set up. Open its card to manage it.".into());
    }
    let container = engine.object("container", &slot.container())?;
    let label = format!("{LABEL}=1");
    if let Some(existing) = &container {
        if (existing["Config"]["Labels"]["org.ratspeak.hermes"] == "1") != hermes_config.is_some() {
            return Err("A different desktop agent installation is pending. Finish that setup first.".into());
        }
    }
    // A first install checks the disk before any download; a full one can leave Docker's storage read-only.
    let model = serde_json::from_str::<Vec<Value>>(MODELS).expect("desktop catalog").into_iter()
        .find(|m| settings.provider == "local" && m["id"].as_str() == settings.model.as_deref());
    if let (None, Some(model), Some(free)) = (&container, model, docker_disk_free_mb(&engine.endpoint)) {
        let need = install_mb(&model, on_gpu, &settings.features);
        if free < need {
            return Err(format!("{} and its extras need about {} GB free on this computer, which has {} GB. Free some space, then try again.",
                model["name"].as_str().unwrap_or("This model"), need.div_ceil(1024), free / 1024));
        }
    }
    // Three stages, each reported with its own measure: build steps, then bytes, then the start.
    let report = |stage: u8, mut detail: Value| {
        detail["stage"] = json!(stage);
        detail["stages"] = json!(3);
        let _ = app.emit("desktop-progress", detail);
    };
    report(1, json!({"text":"Preparing the Linux container…"}));
    if container.is_some() {
        // A container left by an unfinished setup may predate this Setup's runtime.
        engine.update_receiver_reporting(&|step| report(1, step))?;
    } else {
        engine.current_image_reporting(&|step| report(1, step))?;
        if engine.object("volume", &slot.volume())?.is_none() {
            let volume = slot.volume();
            let mut args = vec!["volume","create","--label",&label,"--label",kind.label()];
            let attempt_label;
            if let Some(token) = attempt {
                attempt_label = format!("org.ratspeak.setup-attempt={token}");
                args.extend(["--label",attempt_label.as_str()]);
            }
            args.push(&volume);
            engine.require(&args, "Could not create the private agent volume.")?;
        }
        engine.create_receiver(IMAGE, kind, usable_gpu.is_some())?;
    }
    crate::lifecycle::monitor_desktop();
    engine.require(
        &["start", &slot.container()],
        &format!("Could not start the agent. Check that TCP port {} is available.", slot.port),
    )?;
    // A recovered volume can contain an agent even when its container was removed.
    if let Some(existing) = engine.agent()? {
        if AgentKind::from_provider(existing["provider"].as_str().unwrap_or("")) != kind {
            engine.require(&["stop", &slot.container()], "Could not stop the recovered agent.")?;
            return Err("The saved agent belongs to the other setup card. Its data was preserved.".into());
        }
        return engine.ready().map(|a| engine.with_lan(app, a));
    }
    report(2, json!({"text": if settings.provider == "local" {
            "Downloading and verifying the local model…"
        } else if hermes_config.is_some() {
            "Connecting Hermes and preparing your Ratspeak address…"
        } else {
            "Configuring Venice and the mesh identity…"
        }}));
    if let Some(gateway) = &hermes_config {
        use tauri::Manager;
        let dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?.join("agent-lan");
        hermes::remember(&dir, &slot.volume(), gateway)?;
    }
    let hermes_model = hermes_config.as_ref().map(|gateway| json!({"model": gateway.model}));
    let input = json!({"name":settings.name,"provider":settings.provider,"model":settings.model,"gpu":container.as_ref().map_or(on_gpu, uses_gpu),"venice_model":settings.venice_model,
        "venice_api_key":settings.venice_api_key,"access":settings.access,"features":settings.features,"hubs":settings.hubs,"hermes":hermes_model});
    engine.input_reporting("configure", &input, &|line| report(2, line))?;
    report(3, json!({"text":"Starting the agent and its mesh receiver…"}));
    engine.ready().map(|a| engine.with_lan(app, a))
}
#[tauri::command(async)]
pub fn desktop_set_running(app: AppHandle, kind: AgentKind, running: bool, port: Option<u16>, link: Option<String>) -> Result<Value, String> {
    let _guard = lock(&app)?;
    let local = Engine::local()?;
    let engine = if link.is_none() && port.is_none() {
        let mut receivers = Vec::new();
        for slot in local.installed_slots()? {
            if let Some(c) = local.object("container",&slot.container())? {
                if AgentKind::from_labels(&c["Config"]["Labels"]) == kind { receivers.push(slot); }
            }
        }
        if receivers.len() != 1 { return Err("Select the agent in the Agents tab.".into()); }
        local.in_slot(receivers[0])
    } else { local.selected(kind,port,link.as_deref())? };
    let installed = engine.agent()?.ok_or("No configured desktop agent.")?;
    if AgentKind::from_provider(installed["provider"].as_str().unwrap_or("")) != kind {
        return Err("The selected agent no longer matches this installation.".into());
    }
    if running {
        engine.update_receiver()?;
    }
    let container = engine.object("container",&engine.slot.container())?.ok_or("Missing managed container.")?;
    let id = container["Id"].as_str().ok_or("Missing container identity.")?;
    engine.require(
        &[if running { "start" } else { "stop" }, id],
        "Could not change the agent’s running state.",
    )?;
    if running {
        engine.ready()?;
    }
    Ok(json!({"agent":engine.with_lan(&app,engine.agent()?.ok_or("Missing agent.")?)}))
}
/// Which libraries and tools a local desktop agent keeps. The running agent downloads or removes them.
#[tauri::command(async)]
pub fn desktop_extras(app: AppHandle, kind: AgentKind, port: Option<u16>, link: String,
    features: std::collections::BTreeMap<String, bool>) -> Result<Value, String> {
    let _guard = lock(&app)?;
    let engine = Engine::local()?.selected(kind, port, Some(&link))?;
    let agent = engine.agent()?.ok_or("No desktop agent.")?;
    if agent["state"] != "running" {
        return Err("Start the agent to change its extras.".into());
    }
    engine.input("libraries", &json!({"features": features}))
        .map_err(|_| "Could not save the extras. Try again.".to_string())?;
    Ok(json!({"agent": engine.with_lan(&app, engine.agent()?.ok_or("Missing agent.")?)}))
}
#[tauri::command(async)]
pub fn desktop_rename(app: AppHandle, kind: AgentKind, port: Option<u16>, link: String, name: String) -> Result<Value,String> {
    crate::check_name(&name)?;
    let _guard = lock(&app)?;
    let engine = Engine::local()?.selected(kind,port,Some(&link))?;
    let slot = engine.slot;
    let container = engine.object("container",&slot.container())?.ok_or("Missing container.")?;
    let volume = slot.volume();
    let owned_mount = container["Mounts"].as_array().is_some_and(|mounts| mounts.iter().any(|m|
        m["Type"] == "volume" && m["Name"] == volume && m["Destination"] == STATE));
    if !owned_mount || engine.object("volume",&volume)?.is_none() { return Err("The agent’s storage changed.".into()); }
    let image = container["Image"].as_str().ok_or("Missing receiver image.")?;
    let id = container["Id"].as_str().ok_or("Missing receiver identity.")?;
    engine.require(&["run","--rm","--pull=never","--network=none","--read-only","--security-opt","no-new-privileges","--cap-drop","ALL","--mount",
        &format!("type=volume,source={volume},target={STATE},volume-nocopy"),"--entrypoint","python3",image,
        "-c",include_str!("../../../ratspeak_pi/rename.py"),&link,&name], "Could not save the agent’s new name.")?;
    if container["State"]["Running"] == true {
        engine.require(&["exec",id,"python3","-c",include_str!("../../../ratspeak_pi/rename.py"),&link,&name,"--announce"],
            "Name saved. The receiver couldn’t confirm its announcement.")?;
    }
    engine.agent()?.ok_or("Agent unavailable.".into())
}

#[tauri::command(async)]
pub fn desktop_configure(app: AppHandle, api_key: String, model: String, port: Option<u16>, link: String) -> Result<Value, String> {
    let _guard = lock(&app)?;
    let engine = Engine::local()?.selected(AgentKind::Desktop,port,Some(&link))?;
    let agent = engine.agent()?.ok_or("No configured desktop agent.")?;
    if agent["provider"] != "venice" || agent["state"] != "running" {
        return Err("Start the Venice agent before changing its key.".into());
    }
    venice::verify_choice(&api_key, &model)?;
    engine.input("rotate", &json!({"model":model,"key":api_key}))?;
    engine
        .agent()?
        .map(|a| engine.with_lan(&app, a))
        .ok_or_else(|| "Could not read the updated agent.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_usable_card_sizes_models_by_its_memory() {
        let card = |memory_mb| gpu::Card { name: "NVIDIA GeForce RTX".into(), memory_mb };
        let rtx3090 = offered(16 * 1024, 32 * 1024, Some(&card(24576)));
        assert_eq!(rtx3090[0]["id"], "qwen3-32b");
        assert!(rtx3090.iter().all(|m| m["on_gpu"] == true));
        assert_eq!(offered(64 * 1024, 32 * 1024, Some(&card(8192)))[0]["id"], "qwen3-8b");
        assert_eq!(offered(64 * 1024, 32 * 1024, Some(&card(12288)))[0]["id"], "qwen3-14b");
        assert_eq!(offered(64 * 1024, 32 * 1024, Some(&card(16384)))[0]["id"], "qwen3-14b");
        assert!(offered(64 * 1024, 2048, Some(&card(24576))).is_empty());
    }

    #[test]
    fn installs_count_the_model_runner_and_extras() {
        let model = json!({"size_mb": 19762});
        let none = std::collections::BTreeMap::from([("voice".to_string(), false)]);
        assert_eq!(install_mb(&model, false, &none), 19762 + 30 + DOCKER_MARGIN_MB);
        assert_eq!(install_mb(&model, true, &none), 19762 + GPU_RUNNER_MB + DOCKER_MARGIN_MB);
        let wiki = std::collections::BTreeMap::from([("wikipedia".to_string(), true)]);
        assert!(install_mb(&model, false, &wiki) > install_mb(&model, false, &none) + 1000);
        assert!(free_mb(Path::new(".")).is_some_and(|mb| mb > 0));
        assert_eq!(docker_disk_free_mb("tcp://10.0.0.2:2376"), None);
    }

    #[test]
    fn docker_failures_name_their_cause() {
        let read = |error| docker_failure("Could not read the desktop agent settings", error);
        assert!(read("Error response from daemon: write /tmp/x: no space left on device").contains("disk space"));
        assert!(read("There is not enough space on the disk. (os error 112)").contains("disk space"));
        assert!(read("Error response from daemon: unlinkat /var/lib/docker/x: read-only file system").contains("restart Docker"));
        assert_eq!(read("Error response from daemon: permission denied\n"),
            "Could not read the desktop agent settings. Docker said: permission denied");
        assert_eq!(read(""), "Could not read the desktop agent settings.");
        assert!(read(&"x".repeat(500)).ends_with('…'));
    }

    #[test]
    fn recommendations_fit_the_smaller_memory_limit() {
        assert!(offered(64 * 1024, 512, None).is_empty());
        assert_eq!(offered(64 * 1024, 4096, None)[0]["id"], "qwen3.5-4b");
        assert_eq!(offered(64 * 1024, 64 * 1024, None)[0]["id"], "qwen3-32b");
        let host_limited = offered(8192, 64 * 1024, None);
        let ids: Vec<_> = host_limited.iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["qwen3-8b", "qwen3.5-4b", "qwen3.5-2b", "lfm2.5-1.2b", "smollm2-360m"]);
        assert_eq!(offered(64 * 1024, 1536, None).len(), 2);
    }
}
