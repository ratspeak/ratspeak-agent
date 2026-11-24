use super::{progress, run, Offer};
use std::{
    env, fs,
    os::unix::{fs::MetadataExt, net::UnixStream},
    path::{Path, PathBuf},
    process::Command,
};
use tauri::AppHandle;

enum Packages {
    Apt,
    Dnf,
    Pacman,
}

fn packages() -> Option<Packages> {
    let release = fs::read_to_string("/etc/os-release").ok()?;
    let id = release
        .lines()
        .find_map(|line| line.strip_prefix("ID="))?
        .trim_matches('"');
    match id {
        "debian" | "ubuntu" | "linuxmint" | "pop" if Path::new("/usr/bin/apt-get").is_file() => {
            Some(Packages::Apt)
        }
        "fedora" if Path::new("/usr/bin/dnf").is_file() => Some(Packages::Dnf),
        "arch" | "manjaro" if Path::new("/usr/bin/pacman").is_file() => Some(Packages::Pacman),
        _ => None,
    }
}

fn user_service(endpoint: &str) -> Option<&'static str> {
    let socket = Path::new(endpoint.strip_prefix("unix://")?);
    if env::var_os("HOME")
        .is_some_and(|h| PathBuf::from(h).join(".docker/desktop/docker.sock") == socket)
    {
        return Some("docker-desktop.service");
    }
    if env::var_os("XDG_RUNTIME_DIR")
        .is_some_and(|p| PathBuf::from(p).join("docker.sock") == socket)
    {
        return Some("docker.service");
    }
    None
}

fn username() -> Option<String> {
    let o = Command::new("/usr/bin/id").arg("-un").output().ok()?;
    if !o.status.success() {
        return None;
    }
    let name = String::from_utf8(o.stdout).ok()?.trim().to_owned();
    (!name.is_empty()).then_some(name)
}

fn access_offer(socket: &Path) -> Offer {
    let group = Command::new("/usr/bin/getent")
        .args(["group", "docker"])
        .output()
        .ok();
    let gid = group
        .as_ref()
        .and_then(|o| std::str::from_utf8(&o.stdout).ok())
        .and_then(|s| s.split(':').nth(2)?.parse::<u32>().ok());
    let active_group = gid.is_some_and(|group| {
        Command::new("/usr/bin/id")
            .arg("-G")
            .output()
            .is_ok_and(|o| {
                o.status.success()
                    && String::from_utf8_lossy(&o.stdout)
                        .split_whitespace()
                        .any(|g| g.parse::<u32>().ok() == Some(group))
            })
    });
    if gid.is_none() || fs::metadata(socket).ok().map(|m| m.gid()) != gid || active_group {
        return Offer::new("blocked","This account can’t access the selected Docker socket. Ask its administrator to grant access.");
    }
    if username()
        .and_then(|name| {
            Command::new("/usr/bin/id")
                .args(["-Gn", "--", &name])
                .output()
                .ok()
        })
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .any(|g| g == "docker")
        })
    {
        return Offer::new(
            "wait",
            "Sign out and sign in again for Docker access to take effect, then reopen Ratspeak Agent.",
        );
    }
    Offer::new("access","Allowing access gives this account administrator-level control of Docker and this computer. You’ll need to sign out and back in.")
}

fn system_service_installed() -> bool {
    [
        "/usr/lib/systemd/system/docker.service",
        "/lib/systemd/system/docker.service",
        "/etc/systemd/system/docker.service",
    ]
    .iter()
    .any(|f| Path::new(f).is_file())
}

pub(super) fn offer(endpoint: Option<&str>) -> Offer {
    if let Some(socket) = endpoint {
        if user_service(socket).is_some() {
            return Offer::new("start", "");
        }
        if !matches!(
            socket,
            "unix:///var/run/docker.sock" | "unix:///run/docker.sock"
        ) {
            return Offer::new("blocked","Start the engine for your selected Docker context. Ratspeak Agent will continue when it’s ready.");
        }
        let path = Path::new(socket.trim_start_matches("unix://"));
        if UnixStream::connect(path)
            .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
        {
            return access_offer(path);
        }
    }
    if !Path::new("/run/systemd/system").is_dir() || !Path::new("/usr/bin/pkexec").is_file() {
        return Offer::new("blocked","This system needs a running local Docker engine. Ratspeak Agent will continue when it becomes available.");
    }
    if system_service_installed()
        && super::super::cli()
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    {
        return Offer::new("start", "");
    }
    if packages().is_some() {
        return Offer::new("install", "");
    }
    Offer::new("blocked","Automatic Docker installation isn’t available for this Linux distribution. Ratspeak Agent will detect a running local Docker engine.")
}

fn elevated(tool: &str, args: &[&str]) -> Result<(), String> {
    run(
        Command::new("/usr/bin/pkexec").arg(tool).args(args),
        "Docker setup didn’t finish. Check the system prompt and try again.",
    )
}

fn install_packages() -> Result<(), String> {
    match packages().ok_or("Automatic Docker installation is unavailable on this distribution.")? {
        Packages::Apt => {
            elevated("/usr/bin/apt-get", &["update"])?;
            let mut args = vec!["install", "--yes", "--no-remove", "docker.io"];
            // Some releases bundle the CLI in docker.io; newer ones split it out.
            for name in ["docker-cli", "docker-buildx"] {
                if Command::new("/usr/bin/apt-cache")
                    .args(["show", name])
                    .output()
                    .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
                {
                    args.push(name);
                }
            }
            elevated("/usr/bin/apt-get", &args)
        }
        Packages::Dnf => elevated(
            "/usr/bin/dnf",
            &[
                "install",
                "-y",
                "moby-engine",
                "docker-cli",
                "docker-buildx",
            ],
        ),
        Packages::Pacman => elevated(
            "/usr/bin/pacman",
            &["-S", "--needed", "--noconfirm", "docker", "docker-buildx"],
        ),
    }
}

pub(super) fn prepare(app: &AppHandle, action: &str) -> Result<(), String> {
    if action == "access" {
        let name = username().ok_or("Couldn’t identify the account requesting Docker access.")?;
        return elevated("/usr/sbin/usermod", &["-aG", "docker", "--", &name]);
    }
    if action == "install" {
        progress(
            app,
            "Installing Docker from your system’s software repository…",
        );
        install_packages()?;
    }
    progress(app, "Starting Docker…");
    if let Ok(engine) = super::super::Engine::local() {
        if let Some(service) = user_service(&engine.endpoint) {
            return run(
                Command::new("/usr/bin/systemctl").args(["--user", "start", service]),
                "The Docker service for this account couldn’t start.",
            );
        }
    }
    elevated("/usr/bin/systemctl", &["start", "docker.service"])
}
