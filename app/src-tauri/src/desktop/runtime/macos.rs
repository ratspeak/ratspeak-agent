use super::{download, progress, run, Offer};
use plist::Value;
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};
use tauri::AppHandle;

const DOCKER_APP: &str = "/Applications/Docker.app";
// codesign treats an inline requirement as a filename unless it begins with '='.
const REQUIREMENT: &str = r#"=anchor apple generic and identifier "com.docker.docker" and certificate leaf[subject.OU] = "9BNSXJN65R""#;

fn colima(endpoint: &str) -> Option<(PathBuf, String)> {
    let home = env::var_os("COLIMA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".colima")))?;
    let socket = Path::new(endpoint.strip_prefix("unix://")?);
    let path = socket
        .strip_prefix(&home)
        .ok()?;
    if path.file_name()?.to_str()? != "docker.sock" {
        return None;
    }
    let profile = path.parent()?.to_str()?;
    if profile.is_empty()
        || profile.starts_with('-')
        || !profile
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    {
        return None;
    }
    let lima = env::var_os("LIMA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join("_lima"));
    if !existing_instance(socket, &lima, profile) { return None; }
    ["/opt/homebrew/bin/colima", "/usr/local/bin/colima"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .map(|p| (p, profile.to_owned()))
}

pub(super) fn offer(endpoint: Option<&str>) -> Offer {
    if let Some(socket) = endpoint {
        if colima(socket).is_some() {
            return Offer::new(
                "start",
                "Start your existing Colima engine to run the agent on this Mac.",
            );
        }
        let desktop_socket =
            env::var_os("HOME").map(|p| PathBuf::from(p).join(".docker/run/docker.sock"));
        if socket != "unix:///var/run/docker.sock"
            && socket != "unix:///private/var/run/docker.sock"
            && socket.strip_prefix("unix://").map(Path::new) != desktop_socket.as_deref()
        {
            return Offer::new("blocked", "Start the engine for your selected Docker context. Ratspeak Agent will continue when it’s ready.");
        }
    }
    if Path::new(DOCKER_APP).exists() {
        Offer::new("start", "")
    } else {
        Offer::new("install", "")
    }
}

fn existing_instance(socket: &Path, lima: &Path, profile: &str) -> bool {
    let instance = if profile == "default" { "colima".into() } else { format!("colima-{profile}") };
    socket.with_file_name("colima.yaml").is_file() && lima.join(instance).join("lima.yaml").is_file()
}

pub(super) fn restore_context(app: &AppHandle, current: &str) -> Result<bool, String> {
    use tauri::Manager;
    if !matches!(current, "unix:///var/run/docker.sock" | "unix:///private/var/run/docker.sock")
        || env::var_os("DOCKER_HOST").is_some() || env::var_os("DOCKER_CONTEXT").is_some() {
        return Ok(false);
    }
    let dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    let saved = std::fs::read_to_string(dir.join("agent-lan/engine")).unwrap_or_default();
    let Some((exe, profile)) = colima(saved.trim()) else { return Ok(false); };
    progress(app, "Reconnecting to your agent’s Docker engine…");
    run(Command::new(exe).args(["start", &profile, "--activate=true"]),
        "The agent’s Colima engine couldn’t start. Check Colima and try again.")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_requires_the_original_default_or_named_vm() {
        let temp = tempfile::tempdir().unwrap();
        let lima = temp.path().join("_lima");
        for (profile, instance) in [("default", "colima"), ("work", "colima-work")] {
            let dir = temp.path().join(profile);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("colima.yaml"), "").unwrap();
            let socket = dir.join("docker.sock");
            assert!(!existing_instance(&socket, &lima, profile), "An orphan profile must not create a replacement VM");
            let vm = lima.join(instance);
            std::fs::create_dir_all(&vm).unwrap();
            std::fs::write(vm.join("lima.yaml"), "").unwrap();
            assert!(existing_instance(&socket, &lima, profile));
            assert!(!existing_instance(&socket, &lima, "another"));
        }
    }
}

struct MountedImage(PathBuf);
impl Drop for MountedImage {
    fn drop(&mut self) {
        let _ = Command::new("/usr/bin/hdiutil")
            .arg("detach")
            .arg(&self.0)
            .output();
    }
}

fn install(app: &AppHandle) -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let dmg = temp.path().join("Docker.dmg");
    let silicon = cfg!(target_arch = "aarch64")
        || Command::new("/usr/sbin/sysctl")
            .args(["-n", "hw.optional.arm64"])
            .output()
            .is_ok_and(|o| o.status.success() && o.stdout.starts_with(b"1"));
    let arch = if silicon { "arm64" } else { "amd64" };
    download(
        app,
        &format!("https://desktop.docker.com/mac/main/{arch}/Docker.dmg"),
        &dmg,
    )?;
    progress(app, "Verifying Docker Desktop…");
    let attach = Command::new("/usr/bin/hdiutil")
        .args(["attach", "-readonly", "-nobrowse", "-noautoopen", "-plist"])
        .arg(&dmg)
        .output()
        .map_err(|e| e.to_string())?;
    if !attach.status.success() {
        return Err("macOS couldn’t open the Docker disk image.".into());
    }
    let plist =
        Value::from_reader(std::io::Cursor::new(attach.stdout)).map_err(|e| e.to_string())?;
    let mount = plist
        .as_dictionary()
        .and_then(|d| d.get("system-entities"))
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .filter_map(|v| v.as_dictionary()?.get("mount-point")?.as_string())
                .map(PathBuf::from)
                .next()
        })
        .ok_or("macOS couldn’t locate the mounted Docker disk image.")?;
    let image = MountedImage(mount);
    let bundle = image.0.join("Docker.app");
    run(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict", "-R", REQUIREMENT])
            .arg(&bundle),
        "Docker Desktop’s publisher could not be verified. Installation stopped.",
    )?;
    let metadata =
        Value::from_file(bundle.join("Contents/Info.plist")).map_err(|e| e.to_string())?;
    if let Some(minimum) = metadata
        .as_dictionary()
        .and_then(|d| d.get("LSMinimumSystemVersion"))
        .and_then(Value::as_string)
    {
        let current = Command::new("/usr/bin/sw_vers")
            .arg("-productVersion")
            .output()
            .map_err(|e| e.to_string())?;
        let version = |s: &str| {
            let mut parts = s
                .trim()
                .split('.')
                .map(|part| part.parse::<u32>().unwrap_or(0));
            [
                parts.next().unwrap_or(0),
                parts.next().unwrap_or(0),
                parts.next().unwrap_or(0),
            ]
        };
        if current.status.success()
            && version(&String::from_utf8_lossy(&current.stdout)) < version(minimum)
        {
            return Err(format!(
                "This version of Docker Desktop requires macOS {minimum} or later."
            ));
        }
    }
    if Path::new(DOCKER_APP).exists() {
        return Ok(());
    }
    let user = std::env::var("USER")
        .ok()
        .filter(|u| {
            !u.is_empty()
                && u.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
        .ok_or("Couldn’t determine this Mac’s user account.")?;
    progress(
        app,
        "Installing Docker Desktop. Approve the macOS prompt to continue.",
    );
    // The user accepted Docker's agreement on the install screen; --user applies Docker's
    // privileged setup now, so its first start asks for nothing.
    // The privileged command rechecks Docker's signature from inside the mounted
    // bundle before running its signed installer. AppleScript handles path quoting.
    let script = r#"on run argv
set bundlePath to quoted form of (item 1 of argv)
set signatureRule to quoted form of (item 2 of argv)
set userFlag to quoted form of ("--user=" & item 3 of argv)
do shell script "cd " & bundlePath & " && test ! -e /Applications/Docker.app && /usr/bin/codesign --verify --deep --strict -R " & signatureRule & " . && ./Contents/MacOS/install --accept-license " & userFlag with administrator privileges
end run"#;
    run(
        Command::new("/usr/bin/osascript")
            .args(["-e", script])
            .arg(&bundle)
            .arg(REQUIREMENT)
            .arg(&user),
        "Docker Desktop wasn’t installed. Retry and approve the macOS prompt.",
    )
}

pub(super) fn prepare(app: &AppHandle, action: &str) -> Result<(), String> {
    if action == "install" {
        install(app)?;
    }
    if let Ok(engine) = super::super::Engine::local() {
        if let Some((exe, profile)) = colima(&engine.endpoint) {
            progress(app, "Starting Colima…");
            return run(
                Command::new(exe).args(["start", &profile, "--activate=false"]),
                "Colima couldn’t start the selected Docker engine.",
            );
        }
    }
    progress(app, "Starting Docker Desktop…");
    // Docker's CLI starts the engine without opening a window; older installs fall back to the app.
    let started = Command::new(Path::new(DOCKER_APP).join("Contents/Resources/bin/docker"))
        .args(["desktop", "start", "--detach"])
        .output()
        .is_ok_and(|o| o.status.success());
    if started {
        return Ok(());
    }
    run(
        Command::new("/usr/bin/open").arg(DOCKER_APP),
        "Docker Desktop couldn’t open on this Mac.",
    )
}
