use super::{lan, Engine};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;
use std::{
    env, fs, io,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};

pub(super) fn enable(app: &AppHandle, engine: &Engine) -> io::Result<String> {
    let dir = app
        .path()
        .app_local_data_dir()
        .map_err(io::Error::other)?;
    // Hand LAN ownership to the multi-agent helper on existing installations.
    let old = dir.join("desktop-lan/enabled");
    if old.is_file() { lan::write_changed(&old, b"no")?; }
    let dir = dir.join("agent-lan");
    fs::create_dir_all(&dir)?;
    lan::write_changed(&dir.join("enabled"), b"yes")?;
    let changed = fs::read_to_string(dir.join("engine")).ok().as_ref() != Some(&engine.endpoint);
    lan::write_changed(&dir.join("engine"), engine.endpoint.as_bytes())?;
    let exe = env::current_exe()?;
    // The executable inside an AppImage's temporary mount disappears on logout.
    #[cfg(target_os = "linux")]
    let exe = env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_file())
        .unwrap_or(exe);
    let starting = match lan::locked(&dir) {
        Ok(_lock) => {
            lan::write_changed(&dir.join("state"), b"starting")?;
            true
        }
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => false,
        Err(e) => return Err(e),
    };
    register(&exe, &dir, starting)?;
    let deadline = Instant::now() + Duration::from_secs(if changed { 7 } else { 3 });
    loop {
        let status = fs::read_to_string(dir.join("state")).unwrap_or_default();
        if !matches!(status.as_str(), "" | "starting" | "waiting") || Instant::now() >= deadline {
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(target_os = "macos")]
fn register(exe: &Path, dir: &Path, starting: bool) -> io::Result<()> {
    use plist::{Dictionary, Value};
    const JOB: &str = "org.ratspeak.desktop-agent.lan";
    let home =
        PathBuf::from(env::var_os("HOME").ok_or_else(|| io::Error::other("Missing home folder"))?);
    let folder = home.join("Library/LaunchAgents");
    fs::create_dir_all(&folder)?;
    let file = folder.join(format!("{JOB}.plist"));
    let mut job = Dictionary::new();
    job.insert("Label".into(), JOB.into());
    job.insert(
        "ProgramArguments".into(),
        Value::Array(vec![
            exe.to_string_lossy().into_owned().into(),
            "--desktop-agent-lan".into(),
            dir.to_string_lossy().into_owned().into(),
        ]),
    );
    let mut keep = Dictionary::new();
    keep.insert("SuccessfulExit".into(), false.into());
    job.insert("KeepAlive".into(), keep.into());
    job.insert("RunAtLoad".into(), true.into());
    job.insert("ThrottleInterval".into(), 10.into());
    let mut bytes = Vec::new();
    Value::Dictionary(job)
        .to_writer_xml(&mut bytes)
        .map_err(io::Error::other)?;
    let changed = fs::read(&file).ok().as_deref() != Some(&bytes);
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let target = format!("{domain}/{JOB}");
    let ctl = |args: &[&str]| Command::new("/bin/launchctl").args(args).output();
    let loaded = ctl(&["print", &target])?.status.success();
    if changed {
        lan::write_changed(&file, &bytes)?;
        if loaded && !ctl(&["bootout", &target])?.status.success() {
            return Err(io::Error::other("Could not update the LAN login item"));
        }
    }
    if (!loaded || changed)
        && !ctl(&[
            "bootstrap",
            &domain,
            file.to_str().ok_or(io::ErrorKind::InvalidInput)?,
        ])?
        .status
        .success()
        && !ctl(&["print", &target])?.status.success()
    {
        return Err(io::Error::other("Could not start the LAN login item"));
    }
    if starting && loaded && !changed { let _ = ctl(&["kickstart", &target])?; }
    Ok(())
}

#[cfg(target_os = "linux")]
fn register(exe: &Path, dir: &Path, starting: bool) -> io::Result<()> {
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(|| io::Error::other("Missing configuration folder"))?;
    let folder = config.join("autostart");
    fs::create_dir_all(&folder)?;
    // Exec quoting is parsed after desktop-file string escapes and field-code expansion.
    let quote = |p: &Path| -> io::Result<String> {
        let s = p.to_str().ok_or(io::ErrorKind::InvalidInput)?;
        if s.chars().any(char::is_control) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut arg = String::from("\"");
        for c in s.chars() {
            if matches!(c, '\\' | '"' | '$' | '`') {
                arg.push('\\');
            }
            if c == '%' {
                arg.push('%');
            }
            arg.push(c);
        }
        arg.push('"');
        Ok(arg.replace('\\', "\\\\"))
    };
    let prefix = if env::var_os("APPIMAGE").as_deref().map(Path::new) == Some(exe)
        && env::var_os("APPIMAGE_EXTRACT_AND_RUN").is_some()
    {
        "env APPIMAGE_EXTRACT_AND_RUN=1 "
    } else {
        ""
    };
    let entry = format!("[Desktop Entry]\nType=Application\nName=Ratspeak Desktop Agent LAN\nExec={prefix}{} --desktop-agent-lan {}\nTerminal=false\n", quote(exe)?, quote(dir)?);
    lan::write_changed(
        &folder.join("org.ratspeak.desktop-agent-lan.desktop"),
        entry.as_bytes(),
    )?;
    if starting {
        spawn(exe, dir)?;
    }
    Ok(())
}

#[cfg(windows)]
fn register(exe: &Path, dir: &Path, starting: bool) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    let command = format!(
        "\"{}\" --desktop-agent-lan \"{}\"",
        exe.display(),
        dir.display()
    );
    let out = Command::new("reg.exe")
        .creation_flags(0x0800_0000)
        .args([
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            "RatspeakDesktopAgentLAN",
            "/t",
            "REG_SZ",
            "/d",
            &command,
            "/f",
        ])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("Could not register LAN at sign-in"));
    }
    if starting {
        spawn(exe, dir)?;
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn spawn(exe: &Path, dir: &Path) -> io::Result<()> {
    use std::process::Stdio;
    let mut cmd = Command::new(exe);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd
        .arg("--desktop-agent-lan")
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
