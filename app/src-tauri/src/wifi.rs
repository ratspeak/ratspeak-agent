//! Nearby Wi-Fi networks, so the person picks theirs from a list instead of typing its name.
//!
//!   macOS    CoreWLAN. macOS only tells an app network names once the person allows Location Services for
//!            it, so the first scan raises that system prompt. Declined, the list falls back to the networks
//!            this Mac has joined before, which need no permission.
//!   Linux    NetworkManager (`nmcli`).
//!   Windows  `netsh wlan show networks`.
//! Anything that fails returns fewer networks, never an error: the window always offers "Other network".
use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Network {
    pub ssid: String,
    /// 0 to 100, or None when only the name is known.
    pub signal: Option<u8>,
    pub secure: bool,
    /// This computer has joined it before.
    pub known: bool,
}

#[derive(Serialize)]
pub struct Scan {
    pub networks: Vec<Network>,
    /// False when names could not be scanned (permission declined, or no Wi-Fi hardware) and the list holds
    /// only networks this computer has joined before.
    pub scanned: bool,
}

/// One entry per name, strongest signal first, then names without a signal in alphabetical order.
pub fn tidy(found: Vec<Network>, known: &[String]) -> Vec<Network> {
    let mut best: Vec<Network> = Vec::new();
    for mut network in found {
        if network.ssid.is_empty() || network.ssid.chars().any(char::is_control) {
            continue;
        }
        network.known = known.contains(&network.ssid);
        match best.iter_mut().find(|other| other.ssid == network.ssid) {
            Some(other) if other.signal < network.signal => *other = network,
            Some(_) => {}
            None => best.push(network),
        }
    }
    best.sort_by(|a, b| b.signal.cmp(&a.signal).then_with(|| a.ssid.to_lowercase().cmp(&b.ssid.to_lowercase())));
    best.truncate(30);
    best
}

#[cfg(target_os = "macos")]
pub fn scan(app: &tauri::AppHandle, known: Vec<String>) -> Scan {
    listed(platform::scan(app), known)
}

#[cfg(not(target_os = "macos"))]
pub fn scan(known: Vec<String>) -> Scan {
    listed(platform::scan(), known)
}

fn listed(found: Vec<Network>, known: Vec<String>) -> Scan {
    let scanned = !found.is_empty();
    let found = if scanned { found } else { known.iter().map(|ssid| Network { ssid: ssid.clone(), signal: None, secure: true, known: true }).collect() };
    Scan { networks: tidy(found, &known), scanned }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{cell::RefCell, sync::mpsc, thread, time::Duration};

    use objc2::rc::Retained;
    use objc2_core_location::{CLAuthorizationStatus, CLLocationManager};
    use objc2_core_wlan::{CWSecurity, CWWiFiClient};

    use super::Network;

    thread_local! {
        // Lives on the main thread for the life of the app: the permission prompt belongs to it.
        static MANAGER: RefCell<Option<Retained<CLLocationManager>>> = const { RefCell::new(None) };
    }

    /// Location permission as macOS reports it, asking for it first when `ask` is set and it was never decided.
    fn permission(app: &tauri::AppHandle, ask: bool) -> Option<CLAuthorizationStatus> {
        let (send, receive) = mpsc::channel();
        app.run_on_main_thread(move || {
            let status = MANAGER.with(|slot| {
                let mut slot = slot.borrow_mut();
                let manager = slot.get_or_insert_with(|| unsafe { CLLocationManager::new() });
                let status = unsafe { manager.authorizationStatus() };
                if ask && status == CLAuthorizationStatus::NotDetermined {
                    unsafe { manager.requestWhenInUseAuthorization() };
                }
                status
            });
            let _ = send.send(status);
        })
        .ok()?;
        receive.recv_timeout(Duration::from_secs(5)).ok()
    }

    pub fn scan(app: &tauri::AppHandle) -> Vec<Network> {
        if permission(app, true) == Some(CLAuthorizationStatus::NotDetermined) {
            for _ in 0..240 {
                // The system prompt is up: wait for the answer, up to two minutes.
                thread::sleep(Duration::from_millis(500));
                if permission(app, false) != Some(CLAuthorizationStatus::NotDetermined) {
                    break;
                }
            }
        }
        let mut found = Vec::new();
        unsafe {
            let Some(interface) = CWWiFiClient::sharedWiFiClient().interface() else { return found };
            let Ok(networks) = interface.scanForNetworksWithName_error(None) else { return found };
            for network in networks.iter() {
                let Some(ssid) = network.ssid() else { continue }; // nil without the permission
                let signal = ((network.rssiValue() + 90) * 100 / 60).clamp(0, 100) as u8;
                found.push(Network { ssid: ssid.to_string(), signal: Some(signal), secure: !network.supportsSecurity(CWSecurity::None), known: false });
            }
        }
        found
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::process::Command;

    use super::Network;

    /// One line of `nmcli --terse` output, "SSID:SIGNAL:SECURITY", where a colon inside the name is "\:".
    pub fn parse(line: &str) -> Option<Network> {
        let (rest, security) = line.rsplit_once(':')?;
        let (ssid, signal) = rest.rsplit_once(':')?;
        Some(Network { ssid: ssid.replace("\\:", ":").replace("\\\\", "\\"), signal: signal.parse().ok(), secure: !security.is_empty() && security != "--", known: false })
    }

    pub fn scan() -> Vec<Network> {
        let Ok(out) = Command::new("nmcli").args(["--terse", "--fields", "SSID,SIGNAL,SECURITY", "device", "wifi", "list", "--rescan", "auto"]).output() else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout).lines().filter_map(parse).collect()
    }
}

#[cfg(windows)]
mod platform {
    use std::{os::windows::process::CommandExt, process::Command};

    use super::Network;

    /// `netsh wlan show networks mode=bssid`. Each network starts with "SSID n : name"; the second line after
    /// it is the authentication ("Open" when there is no password), and each access point has a "Signal : 86%"
    /// line. Read by position and by the percent sign, because the labels are translated on non-English Windows.
    pub fn parse(text: &str) -> Vec<Network> {
        let mut found: Vec<Network> = Vec::new();
        let mut since = usize::MAX;
        for line in text.lines() {
            let trimmed = line.trim();
            let value = trimmed.split_once(':').map(|(_, value)| value.trim()).unwrap_or("");
            if trimmed.starts_with("SSID ") && !line.starts_with(' ') {
                found.push(Network { ssid: value.to_string(), signal: None, secure: true, known: false });
                since = 0;
                continue;
            }
            since = since.saturating_add(1);
            let Some(network) = found.last_mut() else { continue };
            if since == 2 {
                network.secure = !value.eq_ignore_ascii_case("open");
            } else if let Some(percent) = value.strip_suffix('%').and_then(|number| number.trim().parse::<u8>().ok()) {
                network.signal = network.signal.max(Some(percent.min(100)));
            }
        }
        found
    }

    pub fn scan() -> Vec<Network> {
        let Ok(out) = Command::new("netsh.exe").args(["wlan", "show", "networks", "mode=bssid"]).creation_flags(0x0800_0000).output() else {
            return Vec::new();
        };
        parse(&String::from_utf8_lossy(&out.stdout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(ssid: &str, signal: Option<u8>) -> Network {
        Network { ssid: ssid.into(), signal, secure: true, known: false }
    }

    #[test]
    fn one_row_per_name_strongest_first() {
        let found = vec![network("Home", Some(40)), network("", Some(90)), network("Cafe", Some(70)), network("Home", Some(82)), network("Attic", None)];
        let tidy = tidy(found, &["Home".to_string()]);
        assert_eq!(tidy.iter().map(|n| (n.ssid.as_str(), n.signal, n.known)).collect::<Vec<_>>(),
                   [("Home", Some(82), true), ("Cafe", Some(70), false), ("Attic", None, false)]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn nmcli_lines() {
        assert_eq!(platform::parse("Home:82:WPA2"), Some(network("Home", Some(82))));
        assert_eq!(platform::parse("Caf\\:e 5G:40:").map(|n| (n.ssid, n.secure)), Some(("Caf:e 5G".to_string(), false)));
        assert_eq!(platform::parse("Open Net:55:--").map(|n| n.secure), Some(false));
    }

    #[cfg(windows)]
    #[test]
    fn netsh_output() {
        let text = "\r\nInterface name : Wi-Fi\r\nThere are 2 networks currently visible.\r\n\r\nSSID 1 : Home\r\n    Network type            : Infrastructure\r\n    Authentication          : WPA2-Personal\r\n    Encryption              : CCMP\r\n    BSSID 1                 : aa:bb:cc:dd:ee:ff\r\n         Signal             : 86%\r\n    BSSID 2                 : aa:bb:cc:dd:ee:00\r\n         Signal             : 40%\r\n\r\nSSID 2 : Cafe\r\n    Network type            : Infrastructure\r\n    Authentication          : Open\r\n    Encryption              : None\r\n    BSSID 1                 : 11:22:33:44:55:66\r\n         Signal             : 55%\r\n";
        let found = platform::parse(text);
        assert_eq!(found.iter().map(|n| (n.ssid.as_str(), n.signal, n.secure)).collect::<Vec<_>>(), [("Home", Some(86), true), ("Cafe", Some(55), false)]);
    }
}
