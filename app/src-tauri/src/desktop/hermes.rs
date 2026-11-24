//! The managed receiver reaches Hermes through a private Docker exec pipe.
//! Only this host process connects to the user's Hermes API, using the gateway Setup recorded.
use super::{hermes_address, AgentKind, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

#[derive(Clone, Deserialize, Serialize)]
pub struct Connection {
    /// None is this computer; otherwise a private-network or VPN address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub port: u16,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_name: Option<String>,
}

impl Connection {
    fn request(&self, path: &str, chat: Option<&Turn>) -> Result<Value, String> {
        if self.port == 0 || !crate::venice::valid_key(&self.key) {
            return Err("Enter the Hermes gateway port and its API server key.".into());
        }
        let target = hermes_address::target(self.host.as_deref(), self.port)?;
        let client = target.client(Duration::from_secs(3), Duration::from_secs(if chat.is_some() { 180 } else { 6 }))?;
        let url = target.url(self.profile_name.as_deref(), path)?;
        let request = if let Some(turn) = chat {
            client.post(url)
                .header("X-Hermes-Session-Id", &turn.session)
                .header("Idempotency-Key", &turn.turn)
                .json(&json!({"model":turn.model, "stream":false, "messages":[{"role":"user","content":turn.text}]}))
        } else {
            client.get(url)
        };
        let response = request.bearer_auth(&self.key).send().map_err(|_| target.unreachable())?;
        match response.status().as_u16() {
            200 => (),
            401 | 403 => return Err("Hermes declined this API server key.".into()),
            _ => {
                return Err(
                    "Hermes could not complete the request. Check the API server and try again."
                        .into(),
                )
            }
        }
        let mut bytes = Vec::new();
        response
            .take(1_048_577)
            .read_to_end(&mut bytes)
            .map_err(|_| "Incomplete Hermes response.")?;
        if bytes.len() > 1_048_576 {
            return Err("Hermes returned too much data.".into());
        }
        serde_json::from_slice(&bytes).map_err(|_| "Hermes returned an unreadable response.".into())
    }

    pub fn verify(&self) -> Result<String, String> {
        let target = hermes_address::target(self.host.as_deref(), self.port)?;
        let state = super::hermes_discovery::probe(&target, self.profile_name.as_deref())?;
        match state {
            "ready" | "protected" => (),
            "unsecured" => {
                return Err(
                    "Set API_SERVER_KEY in Hermes and restart its gateway before connecting."
                        .into(),
                )
            }
            "offline" => return Err(target.unreachable()),
            _ => return Err("This port needs a Hermes gateway with the chat API enabled.".into()),
        }
        let caps = self.request("v1/capabilities", None)?;
        if caps["object"] != "hermes.api_server.capabilities"
            || caps["features"]["chat_completions"] != true
        {
            return Err("This port needs a Hermes gateway with the chat API enabled.".into());
        }
        if caps["features"]["session_continuity_header"] != "X-Hermes-Session-Id" {
            return Err(
                "Update Hermes to a version that supports conversation history through its API."
                    .into(),
            );
        }
        if caps["auth"]["required"] != true {
            return Err(
                "Set API_SERVER_KEY in Hermes and restart its gateway before connecting.".into(),
            );
        }
        // Older gateways expose capabilities publicly. Prove the key on a read-only
        // authenticated endpoint when capabilities did not issue a challenge.
        if state == "ready" {
            self.request("v1/toolsets", None)?;
        }
        caps["model"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .map(str::to_owned)
            .ok_or_else(|| "Hermes did not identify its configured agent profile.".into())
    }
}

#[tauri::command]
pub async fn verify_hermes(connection: Connection) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || connection.verify())
        .await
        .map_err(|e| e.to_string())?
}

/// The gateway one Hermes receiver may use. Setup writes it at install; the container never supplies it.
#[derive(Deserialize, Serialize)]
pub(super) struct Gateway {
    #[serde(flatten)]
    pub connection: Connection,
    pub model: String,
}

fn record(dir: &Path, volume: &str) -> std::path::PathBuf {
    dir.join("hermes").join(format!("{volume}.json"))
}

pub(super) fn remember(dir: &Path, volume: &str, gateway: &Gateway) -> Result<(), String> {
    let path = record(dir, volume);
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|_| "Could not save the Hermes connection.")?;
    crate::write_atomic(&path, &serde_json::to_vec(gateway).map_err(|e| e.to_string())?)
}

pub(super) fn forget(dir: &Path, volume: &str) {
    let _ = std::fs::remove_file(record(dir, volume));
}

fn recall(dir: &Path, volume: &str) -> Option<Gateway> {
    serde_json::from_slice(&std::fs::read(record(dir, volume)).ok()?).ok()
}

#[derive(Deserialize)]
struct Turn {
    #[serde(skip)]
    model: String,
    session: String,
    turn: String,
    text: String,
}

fn respond(gateway: &Gateway, line: &[u8]) -> Result<Value, String> {
    let mut turn: Turn = serde_json::from_slice(line).map_err(|_| "Invalid Hermes turn.")?;
    turn.model = gateway.model.clone();
    let answer = gateway
        .connection
        .request("v1/chat/completions", Some(&turn))?;
    let reply = answer["choices"][0]["message"]["content"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or("Hermes returned an empty reply.")?;
    // Keep the reply inside the 16 KiB LXMF limit even in a JSON message envelope.
    let mut text = String::new();
    let mut encoded = 0;
    for ch in reply.chars() {
        let cost = match ch {
            '\x00'..='\x1f' => 6,
            '"' | '\\' => 2,
            _ => ch.len_utf8(),
        };
        if encoded + cost > 15 * 1024 {
            text.push('…');
            break;
        }
        text.push(ch);
        encoded += cost;
    }
    Ok(json!({"reply": text, "usage":answer["usage"]}))
}

fn serve(engine: &Engine, id: &str, gateway: &Gateway) -> std::io::Result<()> {
    let mut child = engine
        .cmd()
        .args(["exec", "-i", id, "python3", "-m", "ratspeak_pi.hermes"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let result = forward(gateway, &mut output, &mut input);
    drop(input);
    let _ = child.wait();
    result
}

fn forward(gateway: &Gateway, output: &mut impl BufRead, input: &mut impl Write) -> std::io::Result<()> {
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = (&mut *output)
            .take(256 * 1024)
            .read_until(b'\n', &mut line)?;
        if n == 0 || line.last() != Some(&b'\n') {
            break;
        }
        let answer = respond(gateway, &line).unwrap_or_else(|_| json!({"error":"Hermes unavailable"}));
        serde_json::to_writer(&mut *input, &answer)?;
        input.write_all(b"\n")?;
        input.flush()?;
    }
    Ok(())
}

pub(super) fn supervise(dir: &Path) {
    // This worker is independent of LAN discovery. A manual TCP or public hub still works.
    let mut workers = std::collections::HashMap::<String,std::thread::JoinHandle<()>>::new();
    while std::fs::read(dir.join("enabled")).as_deref().ok() != Some(b"no") {
        workers.retain(|_, task| !task.is_finished());
        if let Some(local) = std::fs::read_to_string(dir.join("engine")).ok().and_then(|e| Engine::from_endpoint(e).ok()) {
            for slot in local.installed_slots().unwrap_or_default() {
                let target = local.in_slot(slot);
                if let Ok(Some(obj)) = target.object("container", &slot.container()) {
                    if obj["State"]["Running"] == true && AgentKind::from_labels(&obj["Config"]["Labels"]) == AgentKind::Hermes {
                        if let (Some(id), Some(gateway)) = (obj["Id"].as_str(), recall(dir, &slot.volume())) {
                            workers.entry(id.to_owned()).or_insert_with(|| {
                                let id = id.to_owned();
                                std::thread::spawn(move || { let _ = serve(&target, &id, &gateway); })
                            });
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_profile_is_used_for_discovery_verification_and_chat() {
        use std::{net::TcpListener, thread};
        let server = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = server.local_addr().unwrap().port();
        let home = tempfile::tempdir().unwrap();
        let profile = home.path().join("profiles/work");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("config.yaml"), "{}").unwrap();
        std::fs::write(
            home.path().join("config.yaml"),
            format!("platforms:\n  api_server:\n    port: {port}"),
        )
        .unwrap();
        let worker = thread::spawn(move || {
            let caps = r#"{"object":"hermes.api_server.capabilities","model":"hermes-agent","features":{"chat_completions":true,"session_continuity_header":"X-Hermes-Session-Id"},"auth":{"required":true}}"#;
            let health = r#"{"status":"ok","platform":"hermes-agent"}"#;
            for (verb, path, status, body, authenticated) in [
                ("GET", "v1/capabilities", "401 Unauthorized", "{}", false),
                ("GET", "health", "200 OK", health, false),
                ("GET", "v1/capabilities", "401 Unauthorized", "{}", false),
                ("GET", "health", "200 OK", health, false),
                ("GET", "v1/capabilities", "200 OK", caps, true),
                (
                    "POST",
                    "v1/chat/completions",
                    "200 OK",
                    r#"{"choices":[{"message":{"content":"Hello"}}]}"#,
                    true,
                ),
            ] {
                let (mut stream, _) = server.accept().unwrap();
                let mut reader = BufReader::new(&stream);
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                }
                assert!(headers.starts_with(&format!("{verb} /p/work/{path} HTTP/1.1\r\n")));
                assert_eq!(
                    headers
                        .to_lowercase()
                        .contains("authorization: bearer fixture"),
                    authenticated
                );
                if verb == "POST" {
                    let len: usize = headers
                        .lines()
                        .find_map(|s| {
                            s.to_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse().ok())
                        })
                        .unwrap();
                    reader.read_exact(&mut vec![0; len]).unwrap();
                }
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let discovered = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(super::super::hermes_discovery::discover_hermes(
                Some(profile),
                None,
                None,
                None,
            ))
            .unwrap();
        assert_eq!(discovered["state"], "protected");
        assert_eq!(discovered["port"], port);
        assert_eq!(discovered["profile_name"], "work");
        let connection = Connection {
            host: None,
            port,
            key: "fixture".into(),
            profile_name: Some("work".into()),
        };
        let gateway = Gateway { model: connection.verify().unwrap(), connection };
        // A port or key sent by the container is ignored; the recorded gateway answers.
        let turn = json!({"port": 1, "key": "container-key", "model": "other",
            "session": "test-session", "turn": "test-turn", "text": "Hi"});
        assert_eq!(
            respond(&gateway, &serde_json::to_vec(&turn).unwrap()).unwrap()["reply"],
            "Hello"
        );
        worker.join().unwrap();
    }

    #[test]
    fn each_receiver_gets_only_the_gateway_setup_recorded_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = Gateway {
            connection: Connection { host: None, port: 8642, key: "local-key".into(), profile_name: None },
            model: "my-hermes".into(),
        };
        remember(dir.path(), "ratspeak-companion-agent-data", &gateway).unwrap();
        assert_eq!(recall(dir.path(), "ratspeak-companion-agent-data").unwrap().connection.port, 8642);
        assert!(recall(dir.path(), "ratspeak-agent-4245-data").is_none());
        forget(dir.path(), "ratspeak-companion-agent-data");
        assert!(recall(dir.path(), "ratspeak-companion-agent-data").is_none());
    }
}
