//! Watches the Pis set up here, also while the window is hidden: install progress, readiness, and claims.
use aes_gcm::{aead::{Aead, KeyInit, Payload}, Aes256Gcm, Nonce};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager};

const PERIOD: Duration = Duration::from_secs(6);
/// A claimed Pi is only checked this often while the window is hidden.
const HIDDEN_PERIOD: u64 = 60;
/// Missed checks before a Pi that answered counts as unreachable; first boot hands its port over once.
const MISSES: u32 = 3;

#[derive(Default, Clone)]
pub struct Watch {
    status: Option<Value>,
    seen_at: Option<u64>,
    checked_at: u64,
    misses: u32,
    state: Option<&'static str>,
}

static WATCHES: Mutex<BTreeMap<String, Watch>> = Mutex::new(BTreeMap::new());
static STARTED: OnceLock<()> = OnceLock::new();

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// How long a Pi may stay silent after its card was written before Setup says so.
pub fn patience(record: &Value) -> u64 {
    record["estimate"].as_u64().map_or(1200, |e| e + (e / 2).max(300))
}

/// One state per Pi, shared by Overview, Pairing, and Agents.
pub fn classify(record: &Value, watch: &Watch, at: u64) -> &'static str {
    let phase = watch.status.as_ref().map(|s| s["phase"].as_str().unwrap_or("ready"));
    match phase {
        Some("failed") => "failed",
        Some(_) if watch.misses >= MISSES => "unreachable",
        Some("installing") => "installing",
        Some(_) if record["claimed"] == true || watch.status.as_ref().is_some_and(|s| s["claimed"] == true) => "online",
        Some(_) => "ready",
        None => match record["written_at"].as_u64() {
            Some(written) if at.saturating_sub(written) < patience(record) => "booting",
            Some(_) => "unseen",
            None => "unknown",
        },
    }
}

fn read(record: &Value) -> Option<Value> {
    let host = record["hostname"].as_str()?;
    let key = hex::decode(record["monitor_key"].as_str()?).ok().filter(|k| k.len() == 32)?;
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    let (iv, challenge): ([u8; 12], [u8; 16]) = (rand::random(), rand::random());
    let sealed = cipher.encrypt(Nonce::from_slice(&iv), Payload { msg: &challenge, aad: b"ratspeak-status-request-v1" }).ok()?;
    let raw = crate::pi_status(host, &(hex::encode(iv) + &hex::encode(sealed))).ok()?;
    let packet = hex::decode(raw.trim()).ok().filter(|p| p.len() > 28)?;
    let plain = cipher.decrypt(Nonce::from_slice(&packet[..12]), Payload { msg: &packet[12..], aad: b"ratspeak-status-response-v1" }).ok()?;
    let status: Value = serde_json::from_slice(&plain).ok()?;
    (status["challenge"] == hex::encode(challenge) && status["link"] == record["link"]).then_some(status)
}

fn reading(address: &str, record: &Value, watch: &Watch) -> Value {
    let state = watch.state.unwrap_or_else(|| classify(record, watch, now()));
    json!({"address": address, "name": record["name"], "state": state, "status": watch.status, "seen_at": watch.seen_at, "misses": watch.misses,
        "written_at": record["written_at"], "estimate": record["estimate"], "patience": patience(record),
        "percent": watch.status.as_ref().filter(|_| state == "installing").map(|s| s["percent"].clone())})
}

/// The current state and install percent of a Pi, for the agent list.
pub fn row_state(address: &str, record: &Value) -> (&'static str, Value) {
    let watches = WATCHES.lock().unwrap_or_else(|e| e.into_inner());
    let watch = watches.get(address).cloned().unwrap_or_default();
    let state = watch.state.unwrap_or_else(|| classify(record, &watch, now()));
    (state, watch.status.filter(|_| state == "installing").map_or(Value::Null, |s| s["percent"].clone()))
}

fn round(app: &AppHandle) {
    let records = crate::pi_pairing::records(app).unwrap_or_default();
    let hidden = !app.get_webview_window("main").and_then(|w| w.is_visible().ok()).unwrap_or(false);
    WATCHES.lock().unwrap_or_else(|e| e.into_inner()).retain(|address, _| records.iter().any(|(a, _)| a == address));
    for (address, mut record) in records {
        let at = now();
        let previous = WATCHES.lock().unwrap_or_else(|e| e.into_inner()).get(&address).cloned().unwrap_or_default();
        if hidden && record["claimed"] == true && previous.state == Some("online") && at < previous.checked_at + HIDDEN_PERIOD {
            continue;
        }
        let status = read(&record);
        let mut watch = previous.clone();
        watch.checked_at = at;
        match status {
            Some(status) => { watch.status = Some(status); watch.seen_at = Some(at); watch.misses = 0; }
            None => watch.misses = watch.misses.saturating_add(1),
        }
        if record["claimed"] != true && watch.status.as_ref().is_some_and(|s| s["claimed"] == true)
            && crate::pi_pairing::complete_pi_pairing(app.clone(), address.clone()).is_ok() {
            record["claimed"] = true.into();
        }
        watch.state = Some(classify(&record, &watch, at));
        let payload = reading(&address, &record, &watch);
        WATCHES.lock().unwrap_or_else(|e| e.into_inner()).insert(address, watch);
        let _ = app.emit("pi-status", payload);
    }
}

pub fn start(app: &AppHandle) {
    if STARTED.set(()).is_err() {
        return;
    }
    let app = app.clone();
    thread::spawn(move || loop {
        round(&app);
        thread::sleep(PERIOD);
    });
}

/// Every watched Pi's latest reading.
#[tauri::command(async)]
pub fn pi_readings(app: AppHandle) -> Value {
    start(&app);
    let records = crate::pi_pairing::records(&app).unwrap_or_default();
    let watches = WATCHES.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let readings: Vec<Value> = records.iter()
        .map(|(address, record)| reading(address, record, &watches.get(address).cloned().unwrap_or_default())).collect();
    json!({"readings": readings, "now": now()})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(status: Value, misses: u32) -> Watch {
        Watch { status: Some(status), seen_at: Some(1), misses, ..Default::default() }
    }

    #[test]
    fn states_follow_the_pi_and_the_clock() {
        let record = json!({"written_at": 1000, "estimate": 240, "claimed": false});
        assert_eq!(patience(&record), 540);
        assert_eq!(patience(&json!({})), 1200);
        assert_eq!(patience(&json!({"estimate": 1200})), 1800);
        let unseen = Watch::default();
        assert_eq!(classify(&record, &unseen, 1000 + 539), "booting");
        assert_eq!(classify(&record, &unseen, 1000 + 540), "unseen");
        assert_eq!(classify(&json!({}), &unseen, 5000), "unknown");
        assert_eq!(classify(&record, &seen(json!({"phase": "installing", "percent": 40}), 0), 9999), "installing");
        assert_eq!(classify(&record, &seen(json!({"phase": "installing"}), 3), 9999), "unreachable");
        assert_eq!(classify(&record, &seen(json!({"phase": "failed"}), 9), 9999), "failed");
        // A Pi from before install progress reports no phase once it answers.
        assert_eq!(classify(&record, &seen(json!({"claimed": false}), 0), 9999), "ready");
        assert_eq!(classify(&record, &seen(json!({"phase": "ready", "claimed": true}), 0), 9999), "online");
        assert_eq!(classify(&json!({"claimed": true}), &seen(json!({}), 2), 9999), "online");
        assert_eq!(classify(&json!({"claimed": true}), &seen(json!({}), 3), 9999), "unreachable");
    }
}
