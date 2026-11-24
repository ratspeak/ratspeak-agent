//! Ratspeak Agent: runs desktop agents, prepares an SD card so a Raspberry Pi sets itself up on first boot, and edits the
//! settings of a card that is already set up. The Pi itself is never connected to this computer.
//! Runs on macOS, Linux, and Windows; what differs between them is in the three disk_*.rs files.
#![cfg_attr(all(not(debug_assertions), not(test)), windows_subsystem = "windows")]

mod card;
mod pi_pairing;
mod pi_watch;
mod desktop;
mod lifecycle;
mod venice;
#[cfg_attr(target_os = "macos", path = "disk_macos.rs")]
#[cfg_attr(target_os = "linux", path = "disk_linux.rs")]
#[cfg_attr(windows, path = "disk_windows.rs")]
mod disk;
mod image;
mod network;
mod installer;
mod offline;
mod wifi_join;
mod retry;
mod updates;
mod wifi;

use std::{fs, path::PathBuf, thread};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

// static, not const: each use of a const embeds its own copy of the bytes.
static RELEASE: &[u8] =
    include_bytes!(concat!("../../../build/dist/ratspeak-pi-", env!("CARGO_PKG_VERSION"), "-aarch64.tar.gz"));
const FIRSTBOOT: &str = include_str!("../../../firstboot.sh");

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct Wifi {
    pub ssid: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub country: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Access {
    pub mode: String,
    #[serde(default)]
    pub owners: Vec<String>,
}

#[derive(Deserialize, Clone)]
pub struct Settings {
    pub name: String,
    #[serde(default = "local_provider")]
    pub provider: String,
    pub venice_model: Option<String>,
    pub venice_api_key: Option<String>,
    #[serde(default)]
    pub model_options: Vec<String>,
    /// Optional libraries the Pi downloads, by name: {"wikipedia": true}.
    #[serde(default)]
    pub features: std::collections::BTreeMap<String, bool>,
    pub access: Access,
    #[serde(default)]
    pub hubs: Vec<String>,
    pub wifi: Option<Wifi>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub offline: bool,
    /// Download the model and tools here and carry them on the card, so the Pi only unpacks them.
    #[serde(default)]
    pub carry_files: bool,
    /// The Wi-Fi is for a place this computer can't reach, so it was not tested and Setup won't see the Pi.
    #[serde(default)]
    pub elsewhere: bool,
    #[serde(default)]
    pub ssh_key: String,
}

fn is_address(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_hub(s: &str) -> bool {
    match s.rsplit_once(':') {
        Some((host, port)) => {
            !host.is_empty()
                && host.len() <= 253
                && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
                && port.parse::<u16>().map(|p| p > 0).unwrap_or(false)
        }
        None => false,
    }
}

fn printable(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

fn check_wifi(wifi: &Wifi) -> Result<(), String> {
    if wifi.ssid.is_empty() || wifi.ssid.len() > 32 || !printable(&wifi.ssid) {
        return Err("The Wi-Fi name must be 1 to 32 characters.".into());
    }
    if !wifi.password.is_empty() && !(8..=63).contains(&wifi.password.len()) || !printable(&wifi.password) {
        return Err("A Wi-Fi password is 8 to 63 characters. Leave it empty for an open network.".into());
    }
    if !wifi.country.is_empty() && (wifi.country.len() != 2 || !wifi.country.bytes().all(|b| b.is_ascii_uppercase())) {
        return Err("The Wi-Fi country is a two-letter code, such as US.".into());
    }
    Ok(())
}

fn check_access(access: &Access) -> Result<(), String> {
    if !["owners", "anyone", "later"].contains(&access.mode.as_str()) {
        return Err("Choose who may message it.".into());
    }
    if let Some(bad) = access.owners.iter().find(|o| !is_address(o)) {
        return Err(format!("{bad} is not a Ratspeak address (32 characters, 0-9 and a-f)."));
    }
    if access.mode == "owners" && access.owners.is_empty() {
        return Err("Enter your Ratspeak address, or choose another option.".into());
    }
    Ok(())
}

fn local_provider() -> String { "local".into() }

fn check_name(name: &str) -> Result<(),String> {
    let ok = |b: u8| b.is_ascii_alphanumeric() || b" _.-".contains(&b);
    if name.is_empty() || name.len() > 32 || !name.as_bytes()[0].is_ascii_alphanumeric() || !name.bytes().all(ok) {
        return Err("The name can use letters, digits, spaces, and . _ - (32 characters at most).".into());
    }
    Ok(())
}

fn check_common(settings: &Settings) -> Result<(), String> {
    match settings.provider.as_str() {
        "venice" if settings.offline || settings.model.is_some() || !settings.model_options.is_empty() =>
            return Err("Venice requires internet and a cloud model.".into()),
        "venice" if !settings.venice_model.as_deref().is_some_and(venice::valid_id)
            || !settings.venice_api_key.as_deref().is_some_and(venice::valid_key) =>
            return Err("Verify a Venice key and choose a model.".into()),
        "local" if settings.venice_model.is_some() || settings.venice_api_key.is_some() =>
            return Err("Unexpected Venice settings for a local model.".into()),
        "local" | "venice" => (),
        _ => return Err("Choose a model provider.".into()),
    }
    if settings.carry_files && (settings.provider != "local" || settings.model.is_none() && !settings.offline) {
        return Err("Only a chosen local model can be carried on the card.".into());
    }
    check_name(&settings.name)?;
    check_access(&settings.access)?;

    if let Some(bad) = settings.hubs.iter().find(|h| !is_hub(h)) {
        return Err(format!("{bad} is not a hub address. Use host:port, such as hub.example.org:4242."));
    }
    if let Some(wifi) = &settings.wifi {
        check_wifi(wifi)?;
    } else if settings.elsewhere {
        return Err("Enter the Wi-Fi network your Pi will use.".into());
    }
    let key = settings.ssh_key.trim();
    if !key.is_empty() && (!printable(key) || !(key.starts_with("ssh-") || key.starts_with("ecdsa-"))) {
        return Err("That does not look like an SSH public key.".into());
    }
    Ok(())
}

fn check(settings: &Settings) -> Result<(), String> {
    check_common(settings)?;
    installer::validate_preferences(settings)
}

fn yaml(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The Pi's network name: the chosen name in lower case with dashes, so "Kitchen Pi" is kitchen-pi.local.
pub fn hostname(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "ratspeak-pi".into() } else { out }
}

/// Seconds from power-on until the Pi answers, roughly; None when the Pi picks its own model.
/// From a 1 GB Pi 5 downloading LFM2.5 1.2B over home internet (about 9 minutes).
pub fn first_start_estimate(settings: &Settings) -> Option<u64> {
    const BOOT: u64 = 75;
    const FINISH: u64 = 40; // identity and services
    if settings.provider == "venice" {
        return Some(BOOT + 20 + 90 + 15 + FINISH);
    }
    let model = installer::CATALOG.rungs.iter().find(|m| settings.model.as_ref() == Some(&m.id))?;
    let bytes = model.download_bytes + installer::CATALOG.llama_cpp.size;
    let speed_test = 60;
    Some(if settings.offline || settings.carry_files {
        BOOT + 30 + bytes / 25_000_000 + speed_test + FINISH // packages and copies from the card
    } else {
        BOOT + 20 + 90 + bytes / 2_500_000 + speed_test + FINISH // network wait, apt, downloads
    })
}

/// cloud-init user-data, in the form Raspberry Pi Imager writes for Raspberry Pi OS Trixie.
pub fn user_data(settings: &Settings) -> String {
    let key = settings.ssh_key.trim();
    let mut out = format!(
        "#cloud-config\nhostname: {}\nmanage_etc_hosts: true\n\nuser:\n  name: \"pi\"\n  shell: /bin/bash\n  lock_passwd: true\n",
        yaml(&hostname(&settings.name))
    );
    if !key.is_empty() {
        out += &format!("  ssh_authorized_keys:\n    - {}\n", yaml(key));
    }
    out += "ssh_pwauth: false\n\nruncmd:\n";
    if !key.is_empty() {
        out += "  - [ systemctl, enable, --now, ssh ]\n";
    }
    // Its own unit, so the long install (model downloads) does not hold up the rest of first boot.
    out += "  - [ systemd-run, --unit=ratspeak-pi-firstboot, --no-block, sh, /boot/firmware/ratspeak-pi/firstboot.sh ]\n";
    out
}

pub fn network_config(wifi: &Wifi) -> String {
    let auth = if wifi.password.is_empty() {
        "          auth:\n            key-management: none\n".to_string()
    } else {
        format!("          password: {}\n", yaml(&wifi.password))
    };
    let domain = if wifi.country.is_empty() { String::new() } else { format!("      regulatory-domain: {}\n", yaml(&wifi.country)) };
    format!(
        "network:\n  version: 2\n  ethernets:\n    eth0:\n      dhcp4: true\n      dhcp6: true\n      optional: true\n  wifis:\n    wlan0:\n      dhcp4: true\n{domain}      access-points:\n        {}:\n{}      optional: true\n",
        yaml(&wifi.ssid),
        auth
    )
}

/// Everything the Pi needs on the card's boot partition, as (path, contents). The Wi-Fi password goes only
/// into network-config, where Raspberry Pi OS reads it. The image's own meta-data file is left as it is.
fn card_files(settings: &Settings, pairing: &installer::Pairing) -> Vec<(String, Vec<u8>)> {
    let pretty = |value: Value| (serde_json::to_string_pretty(&value).unwrap() + "\n").into_bytes();
    let mut files = vec![("user-data".to_string(), user_data(settings).into_bytes())];
    if let Some(wifi) = &settings.wifi {
        files.push(("network-config".into(), network_config(wifi).into_bytes()));
    }
    files.push(("ratspeak-pi/release.tar.gz".into(), RELEASE.to_vec()));
    files.push(("ratspeak-pi/firstboot.sh".into(), FIRSTBOOT.as_bytes().to_vec()));
    files.push(("ratspeak-pi/settings.json".into(), pretty(json!({"name": settings.name, "features": settings.features, "access": settings.access, "hubs": settings.hubs,
        "provider": settings.provider, "venice_model": settings.venice_model, "model": settings.model, "offline": settings.offline, "pairing_link": pairing.link, "claim_secret": pairing.claim_secret, "monitor_key": pairing.monitor_key}))));
    files.push(("ratspeak-pi/status.json".into(), pretty(json!({"state": "new", "name": settings.name, "can_reset_identity":true,
        "model_options": if settings.offline { settings.model.iter().collect::<Vec<_>>() } else { settings.model_options.iter().collect() },
        "detail": "Not started yet. Put the card in the Pi and plug in power.", "hostname": hostname(&settings.name)}))));
    if let Some(key) = &settings.venice_api_key {
        files.push(("ratspeak-pi/venice.key".into(), key.as_bytes().to_vec()));
    }
    files.push(("ratspeak-pi/identity.seed".into(), pairing.private_key()));
    files
}

#[tauri::command(async)]
fn list_cards() -> Result<Vec<card::Card>, String> {
    disk::list()
}

#[tauri::command]
fn quit_setup(app: AppHandle) {
    lifecycle::request_close(&app, true);
}

#[tauri::command(async)]
fn retry_setup(device: String, selected_media: String, attempt: String) -> Result<(), String> {
    let boot = disk::ratspeak_boot(&device)?.ok_or("The card changed. Select it again.")?;
    card::require_media(&selected_media, disk::media_id(&device)?)?;
    retry::queue_retry(&boot, &attempt)
}

/// Wi-Fi networks this computer has joined, to offer as suggestions. No password is read.
#[tauri::command(async)]
fn wifi_names() -> Vec<String> {
    let mut names = disk::wifi_names();
    names.retain(|name| !name.is_empty());
    names.sort();
    names.dedup();
    names
}

#[tauri::command(async)]
fn connect_wifi(wifi: Wifi) -> Result<bool, String> {
    check_wifi(&wifi)?;
    wifi_join::join(&wifi.ssid, &wifi.password)?;
    Ok(true)
}

#[tauri::command]
fn wifi_country() -> Option<String> { wifi_join::country() }

/// Nearby Wi-Fi networks, strongest first. On macOS the first call raises the system's location prompt.
#[tauri::command(async)]
fn wifi_scan(app: AppHandle) -> wifi::Scan {
    #[cfg(target_os = "macos")]
    return wifi::scan(&app, wifi_names());
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        wifi::scan(wifi_names())
    }
}

fn cache(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_cache_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn card_writer_lock(dir: &std::path::Path) -> Result<fs::File, String> {
    let cache_lock = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(dir.join("setup.lock")).map_err(|e| e.to_string())?;
    cache_lock.try_lock().map_err(|_| "Another card is being prepared.")?;
    Ok(cache_lock)
}

/// Returns whether the card was ejected.
fn flash(app: &AppHandle, device: &str, selected_media: &str, settings: &Settings, expected_card: Option<&str>, reset_receipt: Option<&str>) -> Result<(bool, installer::Pairing), String> {
    let say = |phase: &str, done: u64, total: u64| {
        let _ = app.emit("progress", json!({"phase": phase, "done": done, "total": total}));
    };
    let dir = cache(app)?;
    let _lock = card_writer_lock(&dir)?;
    if settings.provider == "venice" {
        venice::verify_choice(settings.venice_api_key.as_deref().unwrap_or_default(), settings.venice_model.as_deref().unwrap_or_default())?;
    }
    let assets = if settings.offline || settings.carry_files { offline::prepare(&dir, settings, &say)? } else { Vec::new() };
    let raw = image::prepare(&dir, &say)?;
    let pairing = installer::Pairing::new(settings.access.mode == "later");
    let result = (|| {
        let files = card_files(settings, &pairing);
        let mut total = files.iter().map(|(_, data)| data.len() as u64).sum::<u64>();
        for path in &assets {
            total += fs::metadata(path).map_err(|e| e.to_string())?.len();
        }
        let mut done = 0;
        if !assets.is_empty() {
            let payload_bytes = total;
            image::grow_boot(&raw, &assets, &mut |moved, move_bytes| {
                done = moved;
                total = payload_bytes + move_bytes;
                say("files", done, total);
            })?;
        } else {
            say("files", 0, total);
        }
        let mut advanced = |bytes: usize| {
            done += bytes as u64;
            say("files", done, total);
        };
        image::add_files(&raw, &files, &mut advanced)?;
        if !assets.is_empty() { image::add_offline_assets(&raw, &assets, &mut advanced)?; }
        let (_, capacity) = disk::check(device)?;
        let length = fs::metadata(&raw).map_err(|e| e.to_string())?.len();
        if length > capacity { return Err("The prepared image is too large for this card.".into()); }
        if let Some(expected) = expected_card {
            selected_boot(device, expected)?;
        }
        say("write", 0, length);
        let op = match reset_receipt {
            Some(receipt) => image::CardWrite::InstallAfterReset(&raw, receipt),
            None => image::CardWrite::Image(&raw),
        };
        disk::write(op, device, selected_media, &dir, &say)
    })();
    let _ = fs::remove_file(&raw); // 3 GB; the compressed download stays for the next card
    result.map(|()| (disk::media_id(device).as_deref() == Ok(selected_media) && disk::eject(device), pairing))
}

#[tauri::command]
fn start_flash(app: AppHandle, device: String, selected_media: String, settings: Settings, expected_card: Option<String>, reset_receipt: Option<String>) -> Result<(), String> {
    if reset_receipt.as_deref().is_some_and(|id| !image::valid_receipt(id) || expected_card.is_some()) {
        return Err("Select the SD card again.".into());
    }
    check(&settings)?;
    card::require_media(&selected_media, disk::media_id(&device)?)?;
    let (_, capacity) = disk::check(&device)?;
    if settings.offline || settings.carry_files {
        let required = offline::offline_requirements(settings.model.clone(), offline::chosen(&settings.features))?;
        if capacity < required.card_bytes {
            return Err(if settings.offline { "This offline setup needs a larger SD card. Choose a smaller model or use a network." }
                else { "This card is too small to carry the downloads. Use a larger card, or choose Download on Pi during first boot." }.into());
        }
    }
    thread::spawn(move || {
        let payload = match flash(&app, &device, &selected_media, &settings, expected_card.as_deref(), reset_receipt.as_deref()) {
            Ok((ejected, pairing)) => {
                let mut result = json!({"ejected":ejected,"name":settings.name,"hostname":hostname(&settings.name),
                    "link":pairing.link,"claim_secret":pairing.claim_secret,"monitor_key":pairing.monitor_key,
                    "provider":settings.provider,"model":settings.venice_model.as_ref().or(settings.model.as_ref()),"claimed":settings.access.mode != "later",
                    "written_at":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d|d.as_secs()).unwrap_or(0),
                    "estimate":first_start_estimate(&settings),"offline":settings.offline,"elsewhere":settings.elsewhere,
                    // What Settings shows until the card is back in this computer. No secrets.
                    "card":{"state":"new","network":match &settings.wifi { Some(wifi) => json!({"kind":"wifi","ssid":wifi.ssid}),
                        None if settings.offline => json!({"kind":"none"}), None => json!({"kind":"ethernet"}) },
                        "hubs":settings.hubs,"model":settings.model,"provider":settings.provider,"venice_model":settings.venice_model,
                        "features":settings.features,"access":settings.access,"offline":settings.offline}});
                if pi_pairing::remember(&app,&result).is_err() {
                    result["pairing_save_failed"] = true.into();
                }
                result
            }
            Err(error) => json!({"error":error,"ejected":false}),
        };
        let _ = app.emit("flash-done", payload);
    });
    Ok(())
}

fn selected_boot(device: &str, expected: &str) -> Result<(PathBuf, Value), String> {
    let boot = disk::ratspeak_boot(device)?.ok_or("Reinsert this Pi’s SD card and try again.")?;
    let status = card::status(&boot);
    if expected.is_empty() || card::selection_key(&status) != Some(expected) {
        return Err("The card changed. Reinsert the SD card for this Pi.".into());
    }
    Ok((boot, status))
}

#[tauri::command(async)]
fn reset_card(app: AppHandle, device: String, selected_media: String, expected_card: String) -> Result<String, String> {
    let valid_link = expected_card.strip_prefix("lxma://").and_then(|v| v.split_once(':'))
        .is_some_and(|(address, key)| is_address(address) && key.len() == 128 && key.bytes().all(|c| c.is_ascii_hexdigit()));
    let valid_snapshot = expected_card.strip_prefix("status-")
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()));
    if !(valid_link || valid_snapshot) { return Err("Select the card again.".into()); }
    let cache = cache(&app)?;
    let _lock = card_writer_lock(&cache)?;
    card::require_media(&selected_media, disk::media_id(&device)?)?;
    let (_, status) = selected_boot(&device, &expected_card)?;
    let receipt = hex::encode(rand::random::<[u8; 16]>());
    let op = image::CardWrite::Reset { expected: &expected_card, receipt: &receipt };
    disk::write(op, &device, &selected_media, &cache, &|_, _, _| {})?;
    for link in [Some(expected_card.as_str()),status["next_link"].as_str()].into_iter().flatten() {
        if let Some((address,_)) = link.strip_prefix("lxma://").and_then(|s|s.split_once(':')) {
            // The reset already succeeded. A stale local entry can also be forgotten from Agents.
            let _ = pi_pairing::forget_pi(app.clone(),address.to_owned());
        }
    }
    Ok(receipt)
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum EraseData { Chats, Identity, SetupLog }

#[tauri::command(async)]
fn erase_pi_data(app: AppHandle, device: String, selected_media: String, expected_card: String, action: EraseData) -> Result<Option<String>, String> {
    let _lock = card_writer_lock(&cache(&app)?)?;
    let (boot, status) = selected_boot(&device, &expected_card)?;
    card::require_media(&selected_media, disk::media_id(&device)?)?;
    let result = erase_from_boot(&boot, &status, &expected_card, action)?;
    pi_pairing::refresh_card(&app, &boot, &expected_card);
    Ok(result)
}

fn erase_from_boot(boot: &std::path::Path, status: &Value, expected_card: &str, action: EraseData) -> Result<Option<String>, String> {
    let dir = boot.join("ratspeak-pi");
    match action {
        EraseData::Chats => {
            if status["can_clear_chats"] != true || status["link"] != expected_card || status["state"] == "new" {
                return Err("This installation does not support clearing chats separately.".into());
            }
            let request = json!({"link": expected_card, "id": hex::encode(rand::random::<[u8; 16]>())});
            let path = dir.join("erase-chats.json");
            if !path.exists() { write_atomic(&path, request.to_string().as_bytes())?; }
            Ok(None)
        }
        EraseData::Identity => {
            if status["can_reset_identity"] != true || status["link"] != expected_card {
                return Err("This installation does not support replacing its identity.".into());
            }
            let path = dir.join("new-identity.json");
            if path.exists() {
                return status["next_link"].as_str().map(|s| Some(s.to_string()))
                    .ok_or_else(|| "The existing identity request could not be read.".into());
            }
            let next = installer::Pairing::new(false);
            let request = json!({"old":expected_card,"new":next.link,"seed":hex::encode(next.private_key())});
            write_atomic(&path, request.to_string().as_bytes())?;
            Ok(Some(next.link))
        }
        EraseData::SetupLog => {
            if !status["link"].is_string() || !dir.join("install.log").is_file() {
                return Err("There is no completed setup log to clear on this card.".into());
            }
            write_atomic(&dir.join("install.log"), b"")?;
            Ok(None)
        }
    }
}

/// Leave changes on a card that is already set up. The Pi applies them the next time it starts.
/// Returns whether the card was ejected afterwards.
#[tauri::command(async)]
fn save_changes(app: AppHandle, device: String, selected_media: String, changes: Value, expected_link: String) -> Result<bool, String> {
    let _lock = card_writer_lock(&cache(&app)?)?;
    let (_, status) = selected_boot(&device, &expected_link)?;
    card::require_media(&selected_media, disk::media_id(&device)?)?;
    if changes.get("venice_model").is_some() || changes.get("venice_api_key").is_some() {
        let key = changes["venice_api_key"].as_str().ok_or("Enter a Venice key.")?;
        let model = changes["venice_model"].as_str().ok_or("Choose a Venice model.")?;
        if status["provider"] != "venice" { return Err("This agent uses a local model.".into()); }
        venice::verify_choice(key, model)?;
        if status["state"] != "new" { venice::test_card_change(key, model)?; }
    }
    // A network request can outlive this insertion. Resolve the mount again before saving.
    let (boot, _) = selected_boot(&device, &expected_link)?;
    card::require_media(&selected_media, disk::media_id(&device)?)?;
    let new_name=changes.get("name").and_then(Value::as_str).map(str::to_owned);
    save_to_boot(&boot, changes, &expected_link)?;
    pi_pairing::refresh_card(&app, &boot, &expected_link);
    if let Some(name)=new_name {
        if let Some(address)=expected_link.strip_prefix("lxma://").and_then(|s|s.split_once(':')).map(|(a,_)|a) {
            if let Ok(mut record)=pi_pairing::read_pi_pairing(app.clone(),address.to_owned()) {
                record["name"]=name.into();
                pi_pairing::remember(&app,&record)?;
            }
        }
    }
    Ok(disk::media_id(&device).as_deref() == Ok(selected_media.as_str()) && disk::eject(&device))
}

fn save_to_boot(boot: &std::path::Path, changes: Value, expected_link: &str) -> Result<(), String> {
    let status = card::status(&boot);
    if expected_link.is_empty() || status["link"].as_str() != Some(expected_link) {
        return Err("The card changed. Reinsert the SD card for this Pi.".into());
    }
    let mut out = serde_json::Map::new();
    if let Some(name) = changes.get("name") {
        check_name(name.as_str().ok_or("Invalid name.")?)?;
        if status["state"] != "new" && status["can_rename"] != true { return Err("This Pi installation needs an updated image to change its announced name.".into()); }
        out.insert("name".into(),name.clone());
    }
    if changes.get("venice_model").is_some() || changes.get("venice_api_key").is_some() {
        let key = changes["venice_api_key"].as_str().ok_or("Enter a Venice key.")?;
        let model = changes["venice_model"].as_str().ok_or("Choose a Venice model.")?;
        if status["provider"] != "venice" { return Err("This agent uses a local model.".into()); }
        out.insert("venice_api_key".into(), json!(key));
        out.insert("venice_model".into(), json!(model));
    }
    if let Some(id) = changes.get("model").and_then(Value::as_str) {
        let offered = status["model_options"].as_array().is_some_and(|ids| ids.iter().any(|v| v == id));
        if !offered || !installer::CATALOG.rungs.iter().any(|m| m.id == id) {
            return Err("Choose a model offered for this Pi.".into());
        }
        out.insert("model".into(), json!(id));
    }
    if changes.get("model").is_some_and(|v| !v.is_null()) && status["provider"] == "venice" {
        return Err("This agent uses a Venice model.".into());
    }
    if let Some(features) = changes.get("features") {
        let features: std::collections::BTreeMap<String, bool> = serde_json::from_value(features.clone()).map_err(|e| e.to_string())?;
        out.insert("features".into(), json!(features));
    }
    if let Some(access) = changes.get("access") {
        let access: Access = serde_json::from_value(access.clone()).map_err(|e| e.to_string())?;
        check_access(&access)?;
        out.insert("access".into(), json!(access));
    }
    if let Some(hubs) = changes.get("hubs") {
        let hubs: Vec<String> = serde_json::from_value(hubs.clone()).map_err(|e| e.to_string())?;
        if let Some(bad) = hubs.iter().find(|h| !is_hub(h)) {
            return Err(format!("{bad} is not a hub address. Use host:port, such as hub.example.org:4242."));
        }
        out.insert("hubs".into(), json!(hubs));
    }
    if let Some(wifi) = changes.get("wifi").filter(|w| !w.is_null()) {
        let wifi: Wifi = serde_json::from_value(wifi.clone()).map_err(|e| e.to_string())?;
        check_wifi(&wifi)?;
        out.insert("wifi".into(), json!(wifi));
    }
    if out.is_empty() {
        return Err("Nothing was changed.".into());
    }
    let dir = boot.join("ratspeak-pi");
    // An install that stopped (no network, say) only takes a new network, which reruns first boot.
    if status["state"] != "new" && status["installed"] == false {
        let wifi = out.remove("wifi").ok_or("Until it finishes setting up, only its Wi-Fi can change.")?;
        if !out.is_empty() { return Err("Until it finishes setting up, only its Wi-Fi can change.".into()); }
        let wifi: Wifi = serde_json::from_value(wifi).map_err(|e| e.to_string())?;
        write_atomic(&boot.join("network-config"), network_config(&wifi).as_bytes())?;
        if let Some(retry) = retry::from_card(boot).filter(|r| !r.is_queued()) {
            retry::queue_retry(boot, retry.attempt())?;
        }
        return Ok(());
    }
    if status["state"] == "new" {
        let path = dir.join("settings.json");
        let mut seed: Value = serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if seed["pairing_link"].as_str() != Some(expected_link) { return Err("The card changed.".into()); }
        if seed["offline"] == true {
            for (id, library) in offline::libraries() {
                if out.get("features").is_some_and(|f| f[&id] == true) && !dir.join("offline").join(&library.file).is_file() {
                    return Err(format!("This offline card does not contain {}. Prepare it again with it selected.", library.label));
                }
            }
            let voice = offline::voice_engines();
            let heard = ["whisper-tiny", "vosk"].iter().flat_map(|e| &voice[*e].files);
            if out.get("features").is_some_and(|f| f["voice"] == true) && heard.into_iter().any(|p| !dir.join("offline").join(&p.file).is_file()) {
                return Err("This offline card does not contain voice messages. Prepare it again with it selected.".into());
            }
            for (id, language) in offline::translations().0 {
                if out.get("features").is_some_and(|f| f[&id] == true) && language.packs.iter().any(|p| !dir.join("offline").join(&p.file).is_file()) {
                    return Err(format!("This offline card does not contain {} translation. Prepare it again with it selected.", language.label));
                }
            }
        }
        if let Some(wifi) = out.remove("wifi") {
            let wifi: Wifi = serde_json::from_value(wifi).map_err(|e| e.to_string())?;
            write_atomic(&boot.join("network-config"), network_config(&wifi).as_bytes())?;
        }
        // Keep credentials and the selected model together for an atomic pre-boot rotation.
        if out.contains_key("venice_api_key") {
            let credentials = json!({"key":out.remove("venice_api_key"),"model":out.get("venice_model")});
            write_atomic(&dir.join("venice-update.json"), &serde_json::to_vec(&credentials).unwrap())?;
        }
        for (key, value) in out { seed[key] = value; }
        write_atomic(&path, &serde_json::to_vec_pretty(&seed).map_err(|e| e.to_string())?)?;
        return Ok(());
    }
    // Multiple edits before the next boot belong to the same pending change.
    let pending = dir.join("apply.json");
    if pending.exists() {
        let mut previous: serde_json::Map<String, Value> =
            serde_json::from_slice(&fs::read(&pending).map_err(|e| e.to_string())?)
            .map_err(|_| "Could not read pending settings on this card.")?;
        previous.extend(out);
        out = previous;
    }
    for old in ["applied.json", "apply-failed.json"] {
        let _ = fs::remove_file(dir.join(old));
    }
    write_atomic(&dir.join("apply.json"), &serde_json::to_vec(&Value::Object(out)).unwrap())
        .map_err(|e| format!("Could not write to the card: {e}"))?;
    let _ = fs::remove_file(dir.join("._apply.json")); // macOS leaves a companion file on FAT volumes
    Ok(())
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let dir = path.parent().ok_or("Missing card folder.")?;
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    temp.write_all(bytes).and_then(|_| temp.as_file().sync_all()).map_err(|e| e.to_string())?;
    temp.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

/// "macos", "linux", or "windows", for the few words that differ.
#[tauri::command]
fn platform() -> &'static str {
    std::env::consts::OS
}

/// One sealed status reading from a Pi on this network (see pi_watch).
pub fn pi_status(host: &str, query: &str) -> Result<String, String> {
    if host.is_empty() || host.len() > 63 || hostname(host) != host || query.len() != 88
        || !query.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid Pi status request.".into());
    }
    use std::{io::Read, sync::LazyLock, time::Duration};
    // In this process, so the local network permission is Setup's own.
    static CLIENT: LazyLock<Option<reqwest::blocking::Client>> = LazyLock::new(|| reqwest::blocking::Client::builder()
        .no_proxy().connect_timeout(Duration::from_secs(2)).timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none()).build().ok());
    let client = CLIENT.as_ref().ok_or("Pi unavailable.")?;
    let response = client.get(format!("http://{host}.local:8080/agent-status/{query}")).send()
        .ok().filter(|r| r.status().is_success()).ok_or("Pi unavailable.")?;
    let mut body = String::new();
    response.take(16385).read_to_string(&mut body).map_err(|_| "Invalid Pi status.")?;
    if body.len() > 16384 { return Err("Invalid Pi status.".into()); }
    Ok(body)
}

fn contact_card_payload(link: &str, name: &str) -> Result<String, String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use rns_identity::{destination::Destination, identity::Identity};

    let (address, key_hex) = link.strip_prefix("lxma://").and_then(|s| s.split_once(':'))
        .ok_or("Missing agent identity.")?;
    let mut key = [0u8; 64];
    hex::decode_to_slice(key_hex, &mut key).map_err(|_| "Invalid agent public key.")?;
    let identity = Identity::from_public_key(&key).map_err(|_| "Invalid agent public key.")?;
    let destination = hex::encode(Destination::hash_from_name_and_identity("lxmf.delivery", Some(&identity.hash)));
    if !destination.eq_ignore_ascii_case(address) {
        return Err("The agent address does not match its public key.".into());
    }
    // Match Ratspeak's messaging contact format and its 40-byte UTF-8 name budget.
    let name = name.trim();
    let mut end = name.len().min(40);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    Ok(format!("RSCP2:person:{}:{destination}:{}:{}:", URL_SAFE_NO_PAD.encode(name[..end].trim_end()),
        hex::encode(identity.hash), URL_SAFE_NO_PAD.encode(key)))
}

/// What the pairing QR encodes; the window draws it as Ratspeak does (ui/vendor/ratspeak-qr.js).
#[tauri::command]
fn contact_payload(link: String, name: String) -> Result<String, String> {
    contact_card_payload(&link, &name)
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new("--desktop-agent-lan")) {
        let result = args.next().filter(|_| args.next().is_none()).ok_or(std::io::ErrorKind::InvalidInput.into())
            .and_then(|dir| desktop::lan::supervise(std::path::Path::new(&dir)));
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let args: Vec<String> = std::env::args().collect();
        if matches!(args.get(1).map(String::as_str), Some("--write-image" | "--install-after-reset" | "--reset-card")) {
            // Started with administrator rights by this same app, to write the raw card.
            std::process::exit(image::helper(&args[1..]));
        }
    }
    let background = std::env::args().any(|a| a == "--agent-tray");
    let context = tauri::generate_context!();
    #[cfg(target_os = "linux")]
    if !background && lifecycle::linux::forward_launcher_activation(&context.config().identifier) {
        return;
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if args.iter().any(|a| a == "--agent-tray") {
                lifecycle::monitor_desktop();
                return;
            }
            #[cfg(target_os = "linux")]
            if lifecycle::linux::activate_from_launcher(app, &args) { return; }
            lifecycle::reveal(app);
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updates::PendingUpdate::default())
        .setup(move |app| {
            app.set_theme(Some(tauri::Theme::Dark));
            #[cfg(target_os = "macos")]
            lifecycle::macos::install_quit_handler(app.handle());
            if let Err(error) = lifecycle::tray(app.handle(), background) {
                eprintln!("System tray unavailable: {error}");
            }
            if !background || lifecycle::background_window(app.handle()).is_err() { lifecycle::reveal(app.handle()); }
            lifecycle::resume_desktop(app.handle(), background);
            pi_watch::start(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![desktop::desktop_contacts, desktop::desktop_contact_attach, desktop::desktop_contact_save, desktop::beta_unlocked, desktop::beta_unlock, list_cards, quit_setup, wifi_names, wifi_scan, start_flash, save_changes, reset_card, erase_pi_data, platform, contact_payload, desktop::runtime::prepare_desktop_runtime, desktop::runtime::reconnect_desktop_agent,
            desktop::desktop_rename, desktop::desktop_extras, desktop::managed_agents, pi_pairing::forget_pi, pi_pairing::remember_sd_agent, pi_pairing::pending_pi_pairings, pi_pairing::read_pi_pairing, pi_pairing::complete_pi_pairing, pi_watch::pi_readings, lifecycle::window_ready, lifecycle::tray_available, lifecycle::close_state, lifecycle::close_choice, desktop::hermes_discovery::discover_hermes, desktop::hermes_discovery::choose_hermes_profile, desktop::hermes_discovery::open_hermes_guide, desktop::hermes::verify_hermes, desktop::desktop_snapshot, desktop::desktop_access, desktop::desktop_status, desktop::desktop_install, desktop::desktop_set_running, desktop::removal::desktop_removal_plan, desktop::removal::desktop_remove_agent, desktop::removal::desktop_discard_agent, desktop::desktop_configure, venice::open_venice_keys, desktop::runtime::open_docker_terms, venice::venice_models, retry_setup, installer::model_catalog, offline::offline_requirements, offline::library_catalog, connect_wifi, wifi_country, updates::app_info, updates::check_for_updates, updates::install_update, updates::open_project])
        .build(context)
        .expect("the window could not be opened").run(lifecycle::event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_contact_card_matches_ratspeak_format_and_binds_address() {
        let link = "lxma://3b7fa8f155489eed1b6ddac070fea623:a3c4214383b39348eb95dc12ee98f673efafc825d5422cb4ec4b4f9b84a8a234ad9130b9e22319f3bf7d560f413ca2661584a621070270c367cf4fb123ca5fad";
        let name = "Napping Raccoon";
        let expected = "RSCP2:person:TmFwcGluZyBSYWNjb29u:3b7fa8f155489eed1b6ddac070fea623:ad5e4a1388d168895379e5e2cb58a4ca:o8QhQ4Ozk0jrldwS7pj2c--vyCXVQiy07EtPm4SoojStkTC54iMZ8799Vg9BPKJmFYSmIQcCcMNnz0-xI8pfrQ:";
        assert_eq!(contact_card_payload(link, name).unwrap(), expected);
        assert!(contact_card_payload(&link.replacen("3b7f", "0000", 1), name).is_err());
        assert!(contact_payload(link.into(), name.into()).unwrap().starts_with("RSCP2:person:"));
    }

    #[test]
    fn carried_files_need_a_chosen_local_model() {
        let model = installer::CATALOG.rungs[0].id.clone();
        assert!(check_common(&Settings { carry_files: true, model: Some(model.clone()), ..settings() }).is_ok());
        assert!(check_common(&Settings { carry_files: true, ..settings() }).is_err());
        assert!(check_common(&Settings { carry_files: true, offline: true, wifi: None, ..settings() }).is_ok());
        let venice = Settings { provider: "venice".into(), venice_model: Some("private-text".into()), carry_files: true, ..settings() };
        assert!(check_common(&venice).is_err());
    }

    #[test]
    fn first_start_estimates_follow_where_the_files_come_from() {
        let model = installer::CATALOG.rungs.iter().find(|m| m.id == "lfm2.5-1.2b").map(|m| m.id.clone());
        let downloads = first_start_estimate(&Settings { model: model.clone(), ..settings() }).unwrap();
        let carried = first_start_estimate(&Settings { model, carry_files: true, ..settings() }).unwrap();
        assert!((480..=660).contains(&downloads), "{downloads}");
        assert!(carried < downloads / 2, "{carried}");
        assert_eq!(first_start_estimate(&settings()), None);
    }

    fn settings() -> Settings {
        Settings {
            name: "Kitchen Pi".into(), provider: local_provider(), venice_model: None, venice_api_key: None,
            model_options: vec![],
            model: None, offline: false, carry_files: false, elsewhere: false,
            features: [("wikipedia".to_string(), true)].into(),
            access: Access { mode: "later".into(), owners: vec![] },
            hubs: vec![],
            wifi: Some(Wifi { ssid: "Home \"5G\"".into(), password: "pass\\word1".into(), country: "US".into() }),
            ssh_key: String::new(),
        }
    }

    #[test]
    fn cloud_card_keeps_key_out_of_settings_and_status() {
        let mut cloud = settings();
        cloud.provider = "venice".into();
        cloud.venice_api_key = Some("test-value-only".into());
        cloud.venice_model = Some("test-model".into());
        assert!(check(&cloud).is_ok());
        let files = card_files(&cloud, &installer::Pairing::new(true));
        for (name, bytes) in &files {
            if name.ends_with("venice.key") {
                assert_eq!(bytes, b"test-value-only");
            } else {
                assert!(!bytes.windows(15).any(|s| s == b"test-value-only"));
            }
        }
        assert!(files.iter().any(|(name, _)| name.ends_with("/venice.key")));
        cloud.offline = true;
        assert!(check(&cloud).is_err());
    }

    #[test]
    fn names_and_addresses() {
        assert_eq!(hostname("Kitchen Pi"), "kitchen-pi");
        assert_eq!(hostname("--"), "ratspeak-pi");
        assert!(check(&settings()).is_ok());
        let mut bad = settings();
        bad.access = Access { mode: "owners".into(), owners: vec![] };
        assert!(check(&bad).is_err());
        bad.access.owners = vec!["AB".repeat(16)]; // the window lower-cases before sending
        assert!(check(&bad).is_err());
        bad.access.owners = vec!["ab".repeat(16)];
        assert!(check(&bad).is_ok());
        bad.hubs = vec!["hub.example.org".into()];
        assert!(check(&bad).is_err());
        bad.hubs = vec!["hub.example.org:4242".into()];
        assert!(check(&bad).is_ok());
        bad.name = "x\nruncmd:".into();
        assert!(check(&bad).is_err());
    }

    #[test]
    fn files_for_the_card() {
        let s = settings();
        let net = network_config(s.wifi.as_ref().unwrap());
        assert!(net.contains("        \"Home \\\"5G\\\"\":\n          password: \"pass\\\\word1\"\n"));
        assert!(net.contains("regulatory-domain: \"US\""));
        let user = user_data(&s);
        assert!(user.starts_with("#cloud-config\nhostname: \"kitchen-pi\"\n"));
        assert!(user.contains("firstboot.sh") && !user.contains("enable, --now, ssh"));
        let pairing = installer::Pairing::new(true);
        let files = card_files(&s, &pairing);
        let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["user-data", "network-config", "ratspeak-pi/release.tar.gz", "ratspeak-pi/firstboot.sh",
                           "ratspeak-pi/settings.json", "ratspeak-pi/status.json", "ratspeak-pi/identity.seed"]);
        let on_card = String::from_utf8(files[4].1.clone()).unwrap();
        assert!(!on_card.contains("pass") && on_card.contains("\"later\"") && on_card.contains("\"wikipedia\": true"));
        let mut cable = settings();
        cable.wifi = None;
        assert!(card_files(&cable, &pairing).iter().all(|(name, _)| name != "network-config"));
    }

    #[test]
    fn erase_actions_affect_only_the_requested_data() {
        let temp = tempfile::tempdir().unwrap();
        let boot = temp.path();
        let dir = boot.join("ratspeak-pi");
        fs::create_dir(&dir).unwrap();
        let link = installer::Pairing::new(false).link;
        let mut status = json!({"state":"ready","link":link});
        fs::write(dir.join("apply.json"), b"pending settings").unwrap();
        fs::write(dir.join("install.log"), b"installation log").unwrap();
        let queue = || fs::read_to_string(dir.join("erase-chats.json")).unwrap();
        assert!(erase_from_boot(boot, &status, &link, EraseData::Chats).is_err());
        status["can_clear_chats"] = true.into();
        assert!(erase_from_boot(boot, &status, "other", EraseData::Chats).is_err());
        erase_from_boot(boot, &status, &link, EraseData::Chats).unwrap();
        let first = queue();
        let request: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(request["link"], link);
        assert_eq!(request["id"].as_str().unwrap().len(), 32);
        erase_from_boot(boot, &status, &link, EraseData::Chats).unwrap();
        assert_eq!(queue(), first);
        assert_eq!(fs::read(dir.join("install.log")).unwrap(), b"installation log");
        erase_from_boot(boot, &status, &link, EraseData::SetupLog).unwrap();
        assert!(fs::read(dir.join("install.log")).unwrap().is_empty());
        assert_eq!(fs::read(dir.join("apply.json")).unwrap(), b"pending settings");
        assert_eq!(queue(), first);
    }

    #[test]
    fn modify_before_boot_binds_to_the_identity_and_preserves_the_claim() {
        let temp = tempfile::tempdir().unwrap();
        let boot = temp.path();
        let pairing = installer::Pairing::new(true);
        let mut original = settings();
        original.model = Some("lfm2.5-1.2b".into());
        original.model_options = vec!["lfm2.5-1.2b".into(), "lfm2.5-350m".into(), "lfm2.5-230m".into()];
        for (path, data) in card_files(&original, &pairing) {
            let path = boot.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, data).unwrap();
        }
        let status = card::status(boot);
        assert_eq!(status["link"], pairing.link);
        assert!(status.get("claim_secret").is_none());
        assert!(status.get("monitor_key").is_none());
        let changes = json!({"model":"lfm2.5-350m","hubs":[],"features":{"translate":false,"wikipedia":false}});
        let path = boot.join("ratspeak-pi/settings.json");
        save_to_boot(boot, json!({"hubs":[]}), &pairing.link).unwrap();
        let before = fs::read(&path).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&before).unwrap()["model"], "lfm2.5-1.2b");
        assert!(save_to_boot(boot, json!({"model":"qwen3.5-4b"}), &pairing.link).is_err());
        assert!(save_to_boot(boot, changes.clone(), "another-card").is_err());
        assert_eq!(before, fs::read(&path).unwrap());
        save_to_boot(boot, changes, &pairing.link).unwrap();
        let seed: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(seed["model"], "lfm2.5-350m");
        assert_eq!(seed["hubs"], json!([]));
        assert_eq!(seed["claim_secret"].as_str(), pairing.claim_secret.as_deref());
        assert_eq!(seed["monitor_key"], pairing.monitor_key);
        assert_eq!(hex::decode(&pairing.monitor_key).unwrap().len(), 32);
        assert_eq!(card::status(boot)["features"]["translate"], false);
        assert!(!boot.join("ratspeak-pi/apply.json").exists());
    }

    /// What this computer offers right now, and why each disk is or is not a card. Prints only; erases nothing.
    #[test]
    #[ignore]
    fn this_computer() {
        for card in disk::list().unwrap() {
            println!("{} {} {} GB: {}", card.device, card.name, card.size / 1_000_000_000, card.refused.unwrap_or("offered".into()));
        }
    }

    /// The real thing short of a card: download and verify Raspberry Pi OS, add the first-boot files to its
    /// boot partition, write it to a file with the card writer, and read the files back out of that file.
    /// RATSPEAK_PI_IMAGE_TEST_DIR=<empty folder> cargo test -- --ignored --nocapture real_image
    #[test]
    #[ignore]
    fn real_image() {
        let dir = PathBuf::from(std::env::var("RATSPEAK_PI_IMAGE_TEST_DIR").unwrap());
        let started = std::time::Instant::now();
        let updates = std::cell::RefCell::new(Vec::new());
        let raw = image::prepare(&dir, &|phase, done, total| {
            if phase == "unpack" { updates.borrow_mut().push((started.elapsed(), done, total)); }
        }).unwrap();
        let updates = updates.into_inner();
        assert!(updates.len() > 2);
        assert_eq!(updates.first().unwrap().1, 0);
        assert_eq!(updates.last().unwrap().1, image::RAW_BYTES);
        let max_gap = updates.windows(2).map(|pair| pair[1].0 - pair[0].0).max().unwrap();
        println!("Unpack: {} byte updates, longest gap {:.2}s", updates.len(), max_gap.as_secs_f32());
        let files = card_files(&settings(), &installer::Pairing::new(true));
        image::add_files(&raw, &files, &mut |_| {}).unwrap();
        let target = dir.join("card.img");
        let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&target).unwrap();
        image::write_image(&raw, file, &|_, _, _| {}).unwrap();
        let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        let back = image::read_files(&target, &names);
        assert!(files.iter().zip(&back).all(|((_, sent), got)| sent == got));
        let shipped = image::read_files(&target, &["meta-data", "cmdline.txt", "config.txt"]);
        assert!(String::from_utf8_lossy(&shipped[0]).contains("instance_id") && shipped.iter().all(|file| !file.is_empty()));
    }

    /// File-only integration: verify both filesystems after moving the root image for an offline card.
    #[test]
    #[ignore]
    fn offline_image() {
        use std::io::{Read, Seek, SeekFrom};
        use sha2::{Digest,Sha256};
        fn root_hash(raw: &std::path::Path) -> (u32, u32, Vec<u8>) {
            let mut f = fs::File::open(raw).unwrap();
            let mut mbr = [0;512];
            f.read_exact(&mut mbr).unwrap();
            let start = u32::from_le_bytes(mbr[470..474].try_into().unwrap());
            let sectors = u32::from_le_bytes(mbr[474..478].try_into().unwrap());
            f.seek(SeekFrom::Start(u64::from(start)*512)).unwrap();
            let mut digest = Sha256::new();
            assert_eq!(std::io::copy(&mut f.take(u64::from(sectors)*512), &mut digest).unwrap(),u64::from(sectors)*512);
            (start, sectors, digest.finalize().to_vec())
        }
        let dir = PathBuf::from(std::env::var("RATSPEAK_PI_IMAGE_TEST_DIR").unwrap());
        let wiki = std::env::var_os("RATSPEAK_PI_TEST_WIKIPEDIA").is_some();
        let settings = Settings {model:Some("smollm2-360m".into()),offline:true,wifi:None,features:[("wikipedia".into(),wiki)].into(),..settings()};
        let assets = offline::prepare(&dir,&settings,&|_,_,_|{}).unwrap();
        let raw = image::prepare(&dir,&|_,_,_|{}).unwrap();
        let before = root_hash(&raw);
        let system = ["config.txt","cmdline.txt","overlays/README","kernel8.img"];
        let originals = image::read_files(&raw,&system);
        image::grow_boot(&raw,&assets,&mut |_,_|{}).unwrap();
        let pairing = installer::Pairing::new(true);
        let files = card_files(&settings,&pairing);
        image::add_files(&raw,&files, &mut |_| {}).unwrap();
        image::add_offline_assets(&raw,&assets, &mut |_| {}).unwrap();
        let after = root_hash(&raw);
        assert!(before.0 < after.0);
        assert_eq!((before.1,before.2),(after.1,after.2));
        assert_eq!(originals,image::read_files(&raw,&system));
        for path in assets {
            let name = format!("ratspeak-pi/offline/{}",path.file_name().unwrap().to_str().unwrap());
            let read = image::read_files(&raw,&[&name]);
            assert_eq!(Sha256::digest(&read[0]),Sha256::digest(fs::read(path).unwrap()));
        }
        assert_eq!(image::read_files(&raw,&["ratspeak-pi/identity.seed"])[0],pairing.private_key());
    }
}
