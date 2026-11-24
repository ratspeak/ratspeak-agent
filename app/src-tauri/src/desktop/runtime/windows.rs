use super::{download, progress, Offer};
use std::{
    env,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
};
use tauri::AppHandle;
use windows_sys::Win32::System::{
    SystemInformation::{IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64},
    Threading::{GetCurrentProcess, IsWow64Process2},
};

fn installed() -> Option<PathBuf> {
    [
        ("LOCALAPPDATA", "Programs/DockerDesktop"),
        ("ProgramFiles", "Docker/Docker"),
    ]
    .into_iter()
    .filter_map(|(var, dir)| {
        Some(
            PathBuf::from(env::var_os(var)?)
                .join(dir)
                .join("Docker Desktop.exe"),
        )
    })
    .find(|path| path.is_file())
}

const NO_WINDOW: u32 = 0x08000000;

fn system32(tool: &str) -> Option<PathBuf> {
    Some(
        PathBuf::from(env::var_os("SystemRoot")?)
            .join("System32")
            .join(tool),
    )
}

/// WSL opens its welcome window the first time a distribution starts, unless the user has seen it.
fn skip_wsl_welcome() {
    let Some(reg) = system32("reg.exe") else {
        return;
    };
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss";
    let seen = Command::new(&reg)
        .creation_flags(NO_WINDOW)
        .args(["query", KEY, "/v", "OOBEComplete"])
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("0x1"));
    if !seen {
        let _ = Command::new(&reg)
            .creation_flags(NO_WINDOW)
            .args([
                "add",
                KEY,
                "/v",
                "OOBEComplete",
                "/t",
                "REG_DWORD",
                "/d",
                "1",
                "/f",
            ])
            .output();
    }
}

/// Starts the engine through Docker's CLI, which opens no window; older installs fall back to the app.
fn start(exe: &Path) -> Result<(), String> {
    let cli = exe.parent().map(|dir| dir.join("resources/bin/docker.exe"));
    if let Some(cli) = cli.filter(|cli| cli.is_file()) {
        let started = Command::new(cli)
            .creation_flags(NO_WINDOW)
            .args(["desktop", "start", "--detach"])
            .output()
            .is_ok_and(|o| o.status.success());
        if started {
            return Ok(());
        }
    }
    Command::new(exe)
        .spawn()
        .map(|_| ())
        .map_err(|_| "Docker Desktop couldn’t open.".into())
}

pub(super) fn offer(endpoint: Option<&str>) -> Offer {
    if endpoint == Some("npipe:////./pipe/dockerDesktopWindowsEngine") {
        return Offer::new("blocked", super::super::WINDOWS_CONTAINERS);
    }
    if endpoint.is_some_and(|p| {
        !matches!(
            p,
            "npipe:////./pipe/docker_engine" | "npipe:////./pipe/dockerDesktopLinuxEngine"
        )
    }) {
        return Offer::new("blocked","Start the engine for your selected Docker context. Ratspeak Agent will continue when it’s ready.");
    }
    if installed().is_some() {
        Offer::new("start", "")
    } else {
        Offer::new("install", "")
    }
}

pub(super) fn prepare(app: &AppHandle, action: &str) -> Result<(), String> {
    if action == "install" && installed().is_none() {
        let mut process_machine = 0;
        let mut native_machine = 0;
        // Windows reports the host CPU even when an x64 build runs under ARM emulation.
        if unsafe {
            IsWow64Process2(
                GetCurrentProcess(),
                &mut process_machine,
                &mut native_machine,
            )
        } == 0
        {
            return Err("Couldn’t determine which Docker Desktop installer this PC needs.".into());
        }
        let arch = match native_machine {
            IMAGE_FILE_MACHINE_AMD64 => "amd64",
            IMAGE_FILE_MACHINE_ARM64 => "arm64",
            _ => return Err("Docker Desktop requires a 64-bit Windows PC.".into()),
        };
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let file = dir.path().join("Docker Desktop Installer.exe");
        download(
            app,
            &format!("https://desktop.docker.com/win/main/{arch}/Docker%20Desktop%20Installer.exe"),
            &file,
        )?;
        progress(app, "Verifying Docker Desktop…");
        let ps = system32("WindowsPowerShell/v1.0/powershell.exe")
            .ok_or("Windows system folder not found.")?;
        let verify = r#"$ErrorActionPreference='Stop'; $s=Get-AuthenticodeSignature -LiteralPath $env:RATSPEAK_DOCKER_INSTALLER; if ($s.Status -ne 'Valid' -or $s.SignerCertificate.GetNameInfo('SimpleName',$false) -notin @('Docker Inc','Docker Inc.','Docker, Inc.')) {exit 1}"#;
        super::run(
            Command::new(ps)
                .creation_flags(NO_WINDOW)
                .args(["-NoProfile", "-NonInteractive", "-Command", verify])
                .env("RATSPEAK_DOCKER_INSTALLER", &file),
            "Docker Desktop’s publisher could not be verified. Installation stopped.",
        )?;
        progress(app, "Installing Docker Desktop…");
        // The user accepted Docker's agreement on the install screen.
        let status = Command::new(file)
            .args([
                "install",
                "--user",
                "--quiet",
                "--accept-license",
                "--backend=wsl-2",
                "--no-windows-containers",
            ])
            .status()
            .map_err(|_| "The Docker Desktop installer couldn’t open.")?;
        match status.code() {
            Some(0) => (),
            Some(3010 | 1641) => return Err(
                "Windows needs a restart to finish Docker setup. Reopen Ratspeak Agent afterward."
                    .into(),
            ),
            _ => {
                return Err(
                    "Docker Desktop wasn’t installed. Check the installer window and retry.".into(),
                )
            }
        }
    }
    let exe = installed().ok_or("Couldn’t locate Docker Desktop after installation.")?;
    skip_wsl_welcome();
    progress(app, "Starting Docker Desktop…");
    start(&exe)
}
