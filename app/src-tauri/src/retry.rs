use std::{fs, io::Write, path::Path};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

const RESELECT: &str = "The card changed. Select it again.";
const SEED_ERROR: &str = "This card’s first-boot configuration could not be used for a retry.";

#[derive(Serialize)]
pub struct NetworkRetry {
    attempt: String,
    queued: bool,
}

fn attempt_id(boot: &Path, status: &Value) -> Option<String> {
    let detail = status.get("detail")?.as_str()?;
    match status.get("state")?.as_str()? {
        "installing" if detail == "Waiting for the network" => (),
        "failed" if detail.starts_with("No internet after ") => (),
        _ => return None,
    }
    // Pi clocks can repeat after a power cycle. Each attempt also appends to the log.
    let mut hash = Sha256::new();
    hash.update(status.to_string());
    let log_size = fs::metadata(boot.join("ratspeak-pi/install.log"))
        .map(|m| m.len())
        .unwrap_or(0);
    hash.update(log_size.to_le_bytes());
    Some(format!("ratspeak-retry-{:x}", hash.finalize()))
}

impl NetworkRetry {
    pub fn attempt(&self) -> &str { &self.attempt }
    pub fn is_queued(&self) -> bool { self.queued }
}

pub fn network_retry(boot: &Path, status: &Value) -> Option<NetworkRetry> {
    let attempt = attempt_id(boot, status)?;
    let seed = fs::read_to_string(boot.join("meta-data")).unwrap_or_default();
    let expected = format!("instance-id: {attempt}");
    Some(NetworkRetry {
        queued: seed.lines().any(|line| line == expected),
        attempt,
    })
}

pub fn from_card(boot: &Path) -> Option<NetworkRetry> {
    let status = serde_json::from_slice(&crate::card::small(&boot.join("ratspeak-pi/status.json"), 256 << 10)?).ok()?;
    network_retry(boot, &status)
}

/// A new NoCloud instance ID makes the existing network-config and first-boot runcmd run again.
/// Only change the simple seed shipped with Raspberry Pi OS or one already updated by this app.
fn retry_seed(seed: &str, id: &str) -> Result<String, String> {
    let mut found = false;
    let mut local = false;
    let mut lines = Vec::new();
    for line in seed.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            lines.push(line.to_owned());
        } else if line == "dsmode: local" && !local {
            local = true;
            lines.push(line.to_owned());
        } else if let Some(value) = line
            .strip_prefix("instance_id: ")
            .or_else(|| line.strip_prefix("instance-id: "))
        {
            if found || !(value == "rpios-image" || value.starts_with("ratspeak-retry-")) {
                return Err(SEED_ERROR.into());
            }
            found = true;
            lines.push(format!("instance-id: {id}"));
        } else {
            return Err(SEED_ERROR.into());
        }
    }
    if !found || !local {
        return Err(SEED_ERROR.into());
    }
    let newline = if seed.contains("\r\n") { "\r\n" } else { "\n" };
    Ok(lines.join(newline) + newline)
}

pub(crate) fn queue_retry(boot: &Path, expected_attempt: &str) -> Result<(), String> {
    let status = fs::read(boot.join("ratspeak-pi/status.json")).map_err(|_| RESELECT)?;
    let status: Value = serde_json::from_slice(&status).map_err(|_| RESELECT)?;
    let attempt = attempt_id(boot, &status).ok_or(RESELECT)?;
    if expected_attempt != attempt {
        return Err(RESELECT.into());
    }
    let user_data = fs::read_to_string(boot.join("user-data")).map_err(|_| SEED_ERROR)?;
    let launcher = "  - [ systemd-run, --unit=ratspeak-pi-firstboot, --no-block, sh, /boot/firmware/ratspeak-pi/firstboot.sh ]";
    if user_data.lines().next() != Some("#cloud-config")
        || !user_data.lines().any(|line| line == launcher)
        || !["firstboot.sh", "settings.json", "release.tar.gz"]
            .iter()
            .all(|name| boot.join("ratspeak-pi").join(name).is_file())
    {
        return Err(SEED_ERROR.into());
    }
    let path = boot.join("meta-data");
    let seed = fs::read_to_string(&path).map_err(|_| SEED_ERROR)?;
    let next = retry_seed(&seed, &attempt)?;
    if next == seed {
        return Ok(());
    }
    let write_error =
        |_| "Couldn’t save the retry. Check that the SD card is writable.".to_string();
    let mut temp = tempfile::NamedTempFile::new_in(boot).map_err(write_error)?;
    temp.write_all(next.as_bytes()).map_err(write_error)?;
    temp.as_file().sync_all().map_err(write_error)?;
    temp.persist(&path).map_err(|e| write_error(e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retry_rearms_an_existing_card_only_for_the_selected_network_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let boot = dir.path();
        let app = boot.join("ratspeak-pi");
        fs::create_dir(&app).unwrap();
        for name in ["firstboot.sh", "settings.json", "release.tar.gz"] {
            fs::write(app.join(name), b"fixture").unwrap();
        }
        fs::write(boot.join("user-data"), "#cloud-config\nruncmd:\n  - [ systemd-run, --unit=ratspeak-pi-firstboot, --no-block, sh, /boot/firmware/ratspeak-pi/firstboot.sh ]\n").unwrap();
        let meta = boot.join("meta-data");
        let stock = "# SD seed\r\ndsmode: local\r\ninstance_id: rpios-image\r\n";
        fs::write(&meta, stock).unwrap();
        let status = json!({"state":"installing","detail":"Waiting for the network","updated":42});
        fs::write(app.join("status.json"), status.to_string()).unwrap();
        let first = network_retry(boot, &status).unwrap();
        assert!(!first.queued);
        assert!(queue_retry(boot, "an older selection").is_err());
        assert_eq!(fs::read_to_string(&meta).unwrap(), stock);
        queue_retry(boot, &first.attempt).unwrap();
        queue_retry(boot, &first.attempt).unwrap();
        assert!(network_retry(boot, &status).unwrap().queued);
        assert_eq!(
            fs::read_to_string(&meta).unwrap(),
            format!(
                "# SD seed\r\ndsmode: local\r\ninstance-id: {}\r\n",
                first.attempt
            )
        );
        fs::write(app.join("install.log"), "another boot").unwrap();
        let second = network_retry(boot, &status).unwrap();
        assert!(!second.queued);
        assert_ne!(first.attempt, second.attempt);
        assert!(queue_retry(boot, &first.attempt).is_err());
        fs::write(app.join("status.json"), r#"{"state":"ready"}"#).unwrap();
        assert!(queue_retry(boot, &second.attempt).is_err());
    }

    #[test]
    fn leaves_unrecognized_seeds_alone() {
        assert!(retry_seed("dsmode: local\ninstance-id: custom\n", "test").is_err());
        assert!(retry_seed(
            "dsmode: local\ninstance_id: rpios-image\ninstance-id: another\n",
            "test"
        )
        .is_err());
    }
}
