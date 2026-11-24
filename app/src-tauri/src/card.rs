//! What the window is told about a card, the same on every system.
use std::path::PathBuf;

use serde::Serialize;

pub const MIN_BYTES: u64 = 7_000_000_000; // an "8 GB" card
pub const MAX_BYTES: u64 = 600_000_000_000;

#[derive(Serialize)]
pub struct Card {
    pub device: String,
    pub media_id: Option<String>,
    pub name: String,
    pub size: u64,
    pub volumes: Vec<String>,
    /// status.json from a card this app set up before, when its boot partition is mounted.
    pub ratspeak: Option<serde_json::Value>,
    pub network_retry: Option<crate::retry::NetworkRetry>,
    pub log: String,
    /// Set for a connected removable disk that will not be offered, with the reason, so the person can see
    /// that their reader was noticed.
    pub refused: Option<String>,
}

pub const MEDIA_CHANGED: &str = "The selected SD card was removed or changed. Select the card again.";

/// Bind the row's name, size and volumes to one insertion, even if the reader changes during discovery.
pub fn identified(device: &str, describe: impl FnOnce() -> Result<Card, String>) -> Result<Card, String> {
    let before = crate::disk::media_id(device);
    let mut card = describe()?;
    if let Ok(id) = before {
        require_media(&id, crate::disk::media_id(device)?)?;
        card.media_id = Some(id);
    }
    Ok(card)
}

pub fn require_media(selected: &str, current: String) -> Result<(), String> {
    if selected.is_empty() || selected != current {
        return Err(MEDIA_CHANGED.into());
    }
    Ok(())
}

pub fn size_problem(size: u64) -> Option<&'static str> {
    if size < MIN_BYTES {
        Some("it is too small. A Pi needs a card of 8 GB or more")
    } else if size > MAX_BYTES {
        Some("it is larger than any SD card this app sets up")
    } else {
        None
    }
}

/// The mounted partition holding a `ratspeak-pi` folder, given (label, mount point) pairs.
pub fn boot_of(parts: &[(String, String)]) -> Option<PathBuf> {
    parts.iter().map(|(_, mount)| PathBuf::from(mount)).find(|mount| !mount.as_os_str().is_empty()
        && std::fs::symlink_metadata(mount.join("ratspeak-pi")).is_ok_and(|m| m.is_dir()))
}

pub fn selection_key(status: &serde_json::Value) -> Option<&str> {
    status["link"].as_str().or(status["selection_key"].as_str())
}

/// Up to `limit` bytes from the end of a regular file. A card can carry any filesystem, so symlinks are refused.
fn tail(path: &std::path::Path, limit: u64) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(limit))).ok()?;
    let mut data = Vec::new();
    file.take(limit).read_to_end(&mut data).ok()?;
    Some(data)
}

/// A whole regular file of at most `limit` bytes.
pub(crate) fn small(path: &std::path::Path, limit: u64) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > limit {
        return None;
    }
    tail(path, limit)
}

pub fn status(boot: &std::path::Path) -> serde_json::Value {
    let dir = boot.join("ratspeak-pi");
    let read = |name: &str| -> Option<serde_json::Value> {
        serde_json::from_slice(&small(&dir.join(name), 256 << 10)?).ok()
    };
    let mut status = base_status(read("status.json"), read("settings.json"));
    if let Some(activity) = read("activity.json") {
        if status["link"].is_string() && activity["link"] == status["link"]
            && activity["sampled_at"].as_u64() > status["sampled_at"].as_u64() && activity["stats"].is_object() {
            status["stats"] = activity["stats"].clone();
            status["sampled_at"] = activity["sampled_at"].clone();
        }
    }
    if dir.join("erase-chats.json").is_file() {
        status["chat_erasure"] = "queued".into();
    } else if dir.join("erase-chats-failed.json").is_file() {
        status["chat_erasure"] = "failed".into();
    }
    if let Some(request) = read("new-identity.json") {
        if request["old"] == status["link"] && status["link"].is_string() {
            status["next_link"] = request["new"].clone();
        }
    }
    status["identity_retry"] = dir.join("identity-error").is_file().into();
    // What Settings shows for its network: the last applied change, else what Setup wrote. Never a password.
    let applied = read("applied.json").and_then(|a| a["wifi"]["ssid"].as_str().map(str::to_owned));
    let written = small(&boot.join("network-config"), 64 << 10).and_then(|b| wifi_name(&String::from_utf8_lossy(&b)));
    status["network"] = match applied.or(written) {
        Some(ssid) => serde_json::json!({"kind": "wifi", "ssid": ssid}),
        None if status["offline"] == true => serde_json::json!({"kind": "none"}),
        None => serde_json::json!({"kind": "ethernet"}),
    };
    if let Some(mut pending) = read("apply.json").filter(|p| p.is_object()) {
        pending.as_object_mut().unwrap().remove("venice_api_key");
        if let Some(wifi) = pending["wifi"].as_object_mut() { wifi.remove("password"); }
        status["pending"] = pending;
    }
    status["apply_failed"] = dir.join("apply-failed.json").is_file().into();
    if let Some(log) = tail(&dir.join("install.log"), 16 << 10) {
        let log = String::from_utf8_lossy(&log);
        if let Some(last) = log.lines().rev().find(|l| l.starts_with("== ")) {
            let done = last.starts_with("== done ");
            let at = last.trim_start_matches("== done ").trim_start_matches("== first boot ").to_owned();
            status["log_end"] = serde_json::json!({"done": done, "at": at});
        }
    }
    status
}

/// The Wi-Fi name in a network-config Setup wrote (see `network_config`), unquoted.
pub(crate) fn wifi_name(config: &str) -> Option<String> {
    let entry = config.lines().skip_while(|l| l.trim() != "access-points:").nth(1)?.trim();
    let quoted = entry.strip_suffix(':')?.strip_prefix('"')?.strip_suffix('"')?;
    let (mut out, mut chars) = (String::new(), quoted.chars());
    while let Some(c) = chars.next() {
        out.push(if c == '\\' { chars.next()? } else { c });
    }
    Some(out)
}

pub(crate) fn base_status(status: Option<serde_json::Value>, seed: Option<serde_json::Value>) -> serde_json::Value {
    let mut status = status.filter(|v| v.is_object()).unwrap_or_else(|| serde_json::json!({"state": "unknown"}));
    // Until an install finishes, its settings stay on the card: a failed card still names its Pi and its choices.
    if let Some(seed) = &seed {
        if let Some((address, _)) = seed["pairing_link"].as_str().and_then(|s| s.strip_prefix("lxma://")).and_then(|s| s.split_once(':')) {
            let new = status["state"] == "new";
            for key in ["name", "access", "provider", "venice_model", "model", "features", "hubs", "offline"] {
                if new || status[key].is_null() { status[key] = seed[key].clone(); }
            }
            if new || status["link"].is_null() {
                status["address"] = address.into();
                status["link"] = seed["pairing_link"].clone();
            }
            status["installed"] = false.into();
        }
    }
    if status["link"].as_str().is_none() {
        use sha2::{Digest, Sha256};
        let mut fingerprint = Sha256::new();
        fingerprint.update(status.to_string());
        if let Some(seed) = &seed {
            if let Some(link) = seed["pairing_link"].as_str() { fingerprint.update(link); }
        }
        status["selection_key"] = format!("status-{:x}", fingerprint.finalize()).into();
    }
    status
}

pub fn usable(device: &str, name: String, size: u64, parts: Vec<(String, String)>) -> Card {
    let boot = boot_of(&parts);
    let ratspeak = boot.as_deref().map(status);
    let network_retry = boot.as_deref().and_then(crate::retry::from_card);
    let log = boot
        .and_then(|boot| tail(&boot.join("ratspeak-pi/install.log"), 64 << 10))
        .map(|log| {
            let log = String::from_utf8_lossy(&log);
            let lines: Vec<&str> = log.lines().collect();
            lines[lines.len().saturating_sub(40)..].join("\n")
        })
        .unwrap_or_default();
    let volumes = parts.into_iter().map(|(label, _)| label).filter(|label| !label.is_empty()).collect();
    Card { media_id: None, device: device.to_string(), name, size, volumes, ratspeak, network_retry, log, refused: None }
}

pub fn refused(device: &str, name: String, size: u64, why: String) -> Card {
    let reason = why.split_once(": ").map(|(_, reason)| reason.to_string()).unwrap_or(why);
    Card { media_id: None, device: device.to_string(), name, size, volumes: Vec::new(), ratspeak: None, network_retry: None, log: String::new(), refused: Some(reason) }
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_the_wifi_name_setup_wrote_and_never_its_password() {
        let wifi = crate::Wifi { ssid: "Home \"5G\" \\ net".into(), password: "secret-pass".into(), country: "US".into() };
        let name = wifi_name(&crate::network_config(&wifi));
        assert_eq!(name.as_deref(), Some("Home \"5G\" \\ net"));
        assert_eq!(wifi_name("network:\n  version: 2\n  ethernets:\n    eth0:\n      dhcp4: true\n"), None);
    }

    #[test]
    fn a_stopped_install_keeps_its_link_and_choices() {
        let link = format!("lxma://{}:{}", "ab".repeat(16), "cd".repeat(64));
        let seed = json!({"pairing_link": link, "name": "Kitchen Pi", "hubs": ["3.ratspeak.org:4343"], "offline": false});
        let failed = base_status(Some(json!({"state": "failed", "detail": "No internet after 10 minutes."})), Some(seed.clone()));
        assert_eq!(failed["link"], json!(link));
        assert_eq!((failed["name"].as_str(), failed["installed"].as_bool()), (Some("Kitchen Pi"), Some(false)));
        assert_eq!(failed["state"], "failed");
        // A finished install has no settings on the card and keeps what it reported.
        let ready = base_status(Some(json!({"state": "ready", "link": "lxma://x", "name": "Pi"})), None);
        assert_eq!((ready["link"].as_str(), ready["installed"].is_null()), (Some("lxma://x"), true));
    }
}
