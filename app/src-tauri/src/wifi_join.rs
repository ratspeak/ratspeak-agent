const JOIN_FAILED: &str = "Couldn't connect. Check the password and try again.";

#[cfg(target_os = "macos")]
pub fn join(ssid: &str, password: &str) -> Result<(), String> {
    use objc2_core_wlan::CWWiFiClient;
    use objc2_foundation::NSString;
    unsafe {
        let interface = CWWiFiClient::sharedWiFiClient()
            .interface()
            .ok_or("Wi-Fi is unavailable on this computer.")?;
        let name = NSString::from_str(ssid);
        let networks = interface
            .scanForNetworksWithName_includeHidden_error(Some(&name), true)
            .map_err(|_| "Couldn’t scan Wi-Fi. Check Wi-Fi and Location Services permissions.")?;
        let network = networks
            .iter()
            .filter(|n| n.ssid().is_some_and(|s| s.to_string() == ssid))
            .max_by_key(|n| n.rssiValue())
            .ok_or("This network is out of range.")?;
        let key = (!password.is_empty()).then(|| NSString::from_str(password));
        interface
            .associateToNetwork_password_error(&network, key.as_deref())
            .map_err(|_| JOIN_FAILED)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn country() -> Option<String> {
    unsafe {
        objc2_core_wlan::CWWiFiClient::sharedWiFiClient()
            .interface()?
            .countryCode()
            .map(|s| s.to_string())
    }
}
#[cfg(not(target_os = "macos"))]
pub fn country() -> Option<String> {
    None
} // Frontend uses the explicit region in the system locale.

#[cfg(target_os = "linux")]
pub fn join(ssid: &str, password: &str) -> Result<(), String> {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let devices = Command::new("nmcli")
        .args(["-t", "-f", "DEVICE,TYPE", "device"])
        .output()
        .map_err(|_| "NetworkManager is unavailable.")?;
    let list = String::from_utf8_lossy(&devices.stdout);
    let iface = list
        .lines()
        .find_map(|s| s.strip_suffix(":wifi"))
        .ok_or("Wi-Fi is unavailable on this computer.")?;
    let mut secret =
        tempfile::NamedTempFile::new().map_err(|_| "Could not prepare Wi-Fi authentication.")?;
    if !password.is_empty() {
        writeln!(secret, "802-11-wireless-security.psk:{password}").map_err(|_| JOIN_FAILED)?;
    }
    let name = format!(
        "Ratspeak-Agent-{}",
        secret.path().file_name().unwrap().to_string_lossy()
    );
    let mut add = Command::new("nmcli");
    add.args([
        "--terse",
        "connection",
        "add",
        "save",
        "no",
        "type",
        "wifi",
        "con-name",
        &name,
        "ifname",
        iface,
        "ssid",
        ssid,
        "connection.autoconnect",
        "no",
        "wifi.hidden",
        "yes",
    ]);
    if !password.is_empty() {
        add.args(["wifi-sec.key-mgmt", "wpa-psk", "wifi-sec.psk-flags", "2"]);
    }
    if !add.output().is_ok_and(|r| r.status.success()) {
        return Err("Could not prepare a Wi-Fi connection.".into());
    }
    let joined = Command::new("nmcli")
        .args([
            "--wait",
            "30",
            "connection",
            "up",
            "id",
            &name,
            "ifname",
            iface,
            "passwd-file",
        ])
        .arg(secret.path())
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|r| r.status.success());
    if !joined {
        let _ = Command::new("nmcli")
            .args(["connection", "delete", "id", &name])
            .output();
        return Err(JOIN_FAILED.into());
    }
    Ok(())
}

#[cfg(windows)]
pub fn join(ssid: &str, password: &str) -> Result<(), String> {
    use std::{
        ptr::{null, null_mut},
        time::Duration,
    };
    use windows_sys::Win32::{Foundation::HANDLE, NetworkManagement::WiFi::*};
    struct Client(HANDLE);
    impl Drop for Client {
        fn drop(&mut self) {
            unsafe {
                WlanCloseHandle(self.0, null());
            }
        }
    }
    struct Association {
        ssid: Vec<u8>,
        guid: windows_sys::core::GUID,
        result: std::sync::mpsc::Sender<bool>,
    }
    unsafe extern "system" fn completed(
        note: *mut L2_NOTIFICATION_DATA,
        context: *mut std::ffi::c_void,
    ) {
        let Some(note) = note.as_ref() else { return };
        if note.NotificationSource != WLAN_NOTIFICATION_SOURCE_ACM
            || note.NotificationCode != wlan_notification_acm_connection_complete as u32
            || (note.dwDataSize as usize)
                < std::mem::offset_of!(WLAN_CONNECTION_NOTIFICATION_DATA, strProfileXml)
            || note.pData.is_null()
        {
            return;
        }
        let request = &*context.cast::<Association>();
        let g = note.InterfaceGuid;
        if (g.data1, g.data2, g.data3, g.data4)
            != (
                request.guid.data1,
                request.guid.data2,
                request.guid.data3,
                request.guid.data4,
            )
        {
            return;
        }
        let data = note.pData.cast::<WLAN_CONNECTION_NOTIFICATION_DATA>();
        let net = (*data).dot11Ssid;
        if (*data).wlanConnectionMode == wlan_connection_mode_temporary_profile
            && net.ucSSID.get(..net.uSSIDLength as usize) == Some(request.ssid.as_slice())
        {
            let _ = request
                .result
                .send((*data).wlanReasonCode == WLAN_REASON_CODE_SUCCESS);
        }
    }
    let xml = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    };
    let name = format!(
        "Ratspeak-Agent-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let security = if password.is_empty() {
        "<authEncryption><authentication>open</authentication><encryption>none</encryption><useOneX>false</useOneX></authEncryption>".to_owned()
    } else {
        format!("<authEncryption><authentication>WPA2PSK</authentication><encryption>AES</encryption><useOneX>false</useOneX></authEncryption><sharedKey><keyType>passPhrase</keyType><protected>false</protected><keyMaterial>{}</keyMaterial></sharedKey>",xml(password))
    };
    let profile = format!(
        r#"<WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1"><name>{name}</name><SSIDConfig><SSID><name>{}</name></SSID><nonBroadcast>true</nonBroadcast></SSIDConfig><connectionType>ESS</connectionType><connectionMode>manual</connectionMode><MSM><security>{security}</security></MSM></WLANProfile>"#,
        xml(ssid)
    );
    let wide: Vec<u16> = profile.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let (mut version, mut handle) = (0, null_mut());
        if WlanOpenHandle(2, null(), &mut version, &mut handle) != 0 {
            return Err("Wi-Fi is unavailable on this computer.".into());
        }
        let client = Client(handle);
        let mut list = null_mut();
        if WlanEnumInterfaces(client.0, null(), &mut list) != 0 {
            return Err("Could not find a Wi-Fi adapter.".into());
        }
        let guid = ((*list).dwNumberOfItems > 0).then(|| (*list).InterfaceInfo[0].InterfaceGuid);
        WlanFreeMemory(list.cast());
        let guid = guid.ok_or("No Wi-Fi adapter found.")?;
        let params = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_temporary_profile,
            strProfile: wide.as_ptr(),
            dot11BssType: dot11_BSS_type_infrastructure,
            ..Default::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let request = Association {
            ssid: ssid.as_bytes().to_vec(),
            guid,
            result: tx,
        };
        if WlanRegisterNotification(
            client.0,
            WLAN_NOTIFICATION_SOURCE_ACM,
            0,
            Some(completed),
            (&request as *const Association).cast(),
            null(),
            null_mut(),
        ) != 0
        {
            return Err("Windows did not allow Wi-Fi connection events.".into());
        }
        let requested = WlanConnect(client.0, &guid, &params, null()) == 0;
        let joined = requested && rx.recv_timeout(Duration::from_secs(30)).unwrap_or(false);
        // Closing the handle synchronizes callback teardown before the context can leave scope.
        drop(client);
        if joined {
            return Ok(());
        }
    }
    Err(JOIN_FAILED.into())
}
