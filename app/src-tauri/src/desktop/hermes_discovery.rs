//! Locate a local Hermes profile and probe its gateway without sending credentials.
use super::hermes_address::{self, Target};
use reqwest::blocking::Response;
use serde_json::{json, Value};
use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

const DEFAULT_PORT: u16 = 8642;

fn default_profile() -> Option<PathBuf> {
    if let Some(path) = env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return Some(path);
    }
    #[cfg(windows)]
    {
        env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("hermes"))
    }
    #[cfg(not(windows))]
    {
        env::var_os("HOME").map(|p| PathBuf::from(p).join(".hermes"))
    }
}

fn has_launcher() -> bool {
    let bin = if cfg!(windows) {
        "hermes.exe"
    } else {
        "hermes"
    };
    let on_path =
        env::split_paths(&env::var_os("PATH").unwrap_or_default()).any(|p| p.join(bin).is_file());
    let user_bin = env::var_os("HOME")
        .map(PathBuf::from)
        .is_some_and(|p| p.join(".local/bin").join(bin).is_file());
    on_path
        || user_bin
        || (cfg!(target_os = "macos") && Path::new("/Applications/Hermes.app").is_dir())
}

fn profile_exists(dir: &Path) -> bool {
    [
        "config.yaml",
        ".env",
        "SOUL.md",
        "profile.yaml",
        "auth.json",
        "state.db",
        "hermes-agent/hermes_cli/main.py",
    ]
    .iter()
    .any(|marker| dir.join(marker).is_file())
}

fn text_file(path: &Path) -> Option<String> {
    if !fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(262_145).read_to_string(&mut text).ok()?;
    (text.len() <= 262_144).then_some(text)
}

fn profile_port(dir: &Path, environment_port: Option<u16>) -> Option<u16> {
    let dotenv = text_file(&dir.join(".env")).unwrap_or_default();
    for line in dotenv.lines().rev() {
        let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "API_SERVER_PORT" {
                let value = value.split('#').next()?.trim().trim_matches(['"', '\'']);
                if let Ok(n @ 1..) = value.parse::<u16>() {
                    return Some(n);
                }
            }
        }
    }
    // The gateway loads its profile .env over inherited variables, then applies
    // API_SERVER_PORT to the YAML platform settings.
    if environment_port.is_some() {
        return environment_port;
    }
    let cfg: serde_yaml::Value =
        serde_yaml::from_str(&text_file(&dir.join("config.yaml"))?).ok()?;
    let sections = [
        &cfg["gateway"]["api_server"],
        &cfg["platforms"]["api_server"],
        &cfg["gateway"]["platforms"]["api_server"],
    ];
    // Hermes merges platform sections and explicit extra settings take precedence.
    for extra in [true, false] {
        for section in sections {
            let value = if extra {
                &section["extra"]["port"]
            } else {
                &section["port"]
            };
            let n = value
                .as_u64()
                .and_then(|v| u16::try_from(v).ok())
                .or_else(|| value.as_str()?.parse().ok());
            if n.is_some_and(|v| v > 0) {
                return n;
            }
        }
    }
    None
}

fn small_json(response: Response) -> Option<Value> {
    let mut bytes = Vec::new();
    response.take(262_145).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 262_144 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

pub(super) fn probe(target: &Target, profile: Option<&str>) -> Result<&'static str, String> {
    let capabilities = target.url(profile, "v1/capabilities")?;
    let wait = Duration::from_secs(if target.local { 2 } else { 5 });
    let client = target.client(wait, wait)?;
    let Ok(response) = client.get(capabilities).send() else {
        return Ok("offline");
    };
    if matches!(response.status().as_u16(), 401 | 403) {
        // A published Docker or WSL gateway can have no profile files on this OS.
        // Hermes identifies itself at /health before we ask the user for a key.
        let health = client
            .get(target.url(profile, "health")?)
            .send()
            .ok()
            .filter(|r| r.status().is_success())
            .and_then(small_json);
        return Ok(
            if health.is_some_and(|v| v["platform"] == "hermes-agent" && v["status"] == "ok") {
                "protected"
            } else {
                "other"
            },
        );
    }
    if !response.status().is_success() {
        return Ok("other");
    }
    let Some(caps) = small_json(response) else {
        return Ok("other");
    };
    if caps["object"] != "hermes.api_server.capabilities"
        || caps["features"]["chat_completions"] != true
    {
        return Ok("other");
    }
    Ok(if caps["auth"]["required"] == true {
        "ready"
    } else {
        "unsecured"
    })
}

#[tauri::command]
pub async fn discover_hermes(profile: Option<PathBuf>, port: Option<u16>, profile_name: Option<String>, host: Option<String>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || discover(profile, port, profile_name.as_deref(), host.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

fn discover(profile: Option<PathBuf>, port: Option<u16>, profile_name: Option<&str>, host: Option<&str>) -> Result<Value, String> {
    if port == Some(0) {
        return Err("Enter a port between 1 and 65535.".into());
    }
    if let Some(host) = hermes_address::normalize(host)? {
        // Profile files on this computer say nothing about another computer's gateway.
        let port = port.unwrap_or(DEFAULT_PORT);
        let target = hermes_address::target(Some(&host), port)?;
        let named = profile_name.filter(|n| !n.is_empty());
        let state = probe(&target, named)?;
        return Ok(json!({"state":state, "port":port, "profile_name":named, "profile":null, "host":host,
            "detail":(state == "offline").then(|| target.unreachable())}));
    }
    if profile
        .as_ref()
        .is_some_and(|p| !p.is_absolute() || !profile_exists(p))
    {
        return Err("Choose a Hermes profile folder containing config.yaml or .env.".into());
    }
    let custom = profile.is_some();
    let dir = profile.or_else(default_profile);
    let found = dir.as_deref().is_some_and(profile_exists);
    let env_port = if custom {
        None
    } else {
        env::var("API_SERVER_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|p| *p > 0)
    };
    let mut named = None;
    let mut shared_port = None;
    if let Some(dir) = dir
        .as_deref()
        .filter(|d| found && d.parent().and_then(Path::file_name) == Some("profiles".as_ref()))
    {
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid Hermes profile name.")?;
        named = Some(name);
        shared_port = dir
            .parent()
            .and_then(Path::parent)
            .and_then(|root| profile_port(root, None));
    }
    if let Some(requested) = profile_name {
        named = (!requested.is_empty()).then_some(requested);
    }
    let selected_port = dir
        .as_deref()
        .and_then(|d| profile_port(d, env_port))
        .or(env_port);
    let mut port_to_check = port
        .or(selected_port)
        .or(shared_port)
        .unwrap_or(DEFAULT_PORT);
    let mut state = probe(&hermes_address::target(None, port_to_check)?, named)?;
    // A secondary profile's old standalone port may remain in its configuration.
    // Only retry the selected profile's prefix on the shared listener.
    if named.is_some() && port.is_none() && matches!(state, "offline" | "other") {
        let shared = shared_port.unwrap_or(DEFAULT_PORT);
        if shared != port_to_check {
            let shared_state = probe(&hermes_address::target(None, shared)?, named)?;
            if !matches!(shared_state, "offline" | "other") {
                state = shared_state;
                port_to_check = shared;
            }
        }
    }
    if state == "offline" && !found && !has_launcher() {
        state = "missing";
    }
    Ok(
        json!({"state":state, "port":port_to_check, "profile_name":named, "profile":dir.as_deref().filter(|_| found), "host":null}),
    )
}

#[tauri::command]
pub async fn choose_hermes_profile() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title("Choose your Hermes profile folder")
        .pick_folder()
        .await
        .map(|f| f.path().to_owned())
}

#[tauri::command]
pub fn open_hermes_guide(install: bool) -> Result<(), String> {
    crate::updates::open_in_browser(if install {
        "https://hermes-agent.nousresearch.com/docs/getting-started/installation"
    } else {
        "https://hermes-agent.nousresearch.com/docs/user-guide/features/api-server"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protected_service_needs_the_hermes_health_marker() {
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
            thread,
        };
        for (health, expected) in [
            (r#"{"status":"ok","platform":"hermes-agent"}"#, "protected"),
            (r#"{"status":"ok"}"#, "other"),
        ] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let worker = thread::spawn(move || {
                for (path, status, body) in [
                    ("v1/capabilities", "401 Unauthorized", "{}"),
                    ("health", "200 OK", health),
                ] {
                    let (mut client, _) = listener.accept().unwrap();
                    let headers: Vec<String> = BufReader::new(&client)
                        .lines()
                        .map(Result::unwrap)
                        .take_while(|s| !s.is_empty())
                        .collect();
                    assert!(headers[0].starts_with(&format!("GET /{path} ")));
                    assert!(!headers
                        .iter()
                        .any(|s| s.to_lowercase().starts_with("authorization:")));
                    write!(client, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            });
            assert_eq!(probe(&hermes_address::target(None, port).unwrap(), None).unwrap(), expected);
            worker.join().unwrap();
        }
    }
    #[test]
    fn recognizes_profiles_and_reads_only_the_gateway_port() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert!(!profile_exists(dir));
        fs::write(
            dir.join("config.yaml"),
            "gateway:\n  api_server:\n    port: 8655\n",
        )
        .unwrap();
        assert!(profile_exists(dir));
        assert_eq!(profile_port(dir, None), Some(8655));
        assert_eq!(profile_port(dir, Some(8657)), Some(8657));
        fs::write(
            dir.join(".env"),
            "export API_SERVER_PORT='8656' # personal profile\nAPI_SERVER_KEY=fixture",
        )
        .unwrap();
        assert_eq!(profile_port(dir, Some(8657)), Some(8656));
    }

    #[test]
    fn another_computer_skips_local_profiles_and_stays_off_the_internet() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".env"), "API_SERVER_PORT=1").unwrap();
        assert!(discover(Some(tmp.path().to_owned()), None, None, Some("8.8.8.8")).unwrap_err().contains("isn’t on your local network"));
        assert!(discover(None, None, None, Some("https://hermes.local")).is_err());
        assert!(discover(None, Some(0), None, Some("10.1.1.50")).unwrap_err().contains("between 1 and 65535"));
        // This computer, typed out, is the local lookup and reads the profile's port.
        let local = discover(Some(tmp.path().to_owned()), None, None, Some("localhost")).unwrap();
        assert_eq!((local["port"].as_u64(), local["host"].is_null()), (Some(1), true));
    }

    #[tokio::test]
    async fn checks_the_selected_profile_gateway_without_credentials() {
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
            thread,
        };
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join(".env"),
            format!("API_SERVER_KEY=fixture\nAPI_SERVER_PORT={port}"),
        )
        .unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let lines: Vec<_> = BufReader::new(&stream)
                .lines()
                .map(Result::unwrap)
                .take_while(|s| !s.is_empty())
                .collect();
            assert!(lines[0].starts_with("GET /v1/capabilities "));
            assert!(!lines
                .iter()
                .any(|s| s.to_lowercase().starts_with("authorization:")));
            let body = r#"{"object":"hermes.api_server.capabilities","auth":{"required":true},"features":{"chat_completions":true}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let result = discover_hermes(Some(tmp.path().to_owned()), None, None, None)
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(result["state"], "ready");
        assert_eq!(result["port"], port);
        assert!(!result.to_string().contains("fixture"));
        assert_eq!(
            discover_hermes(Some(tmp.path().to_owned()), None, None, None)
                .await
                .unwrap()["state"],
            "offline"
        );
    }
}
