//! A deliberately lightweight, device-local beta opt-in for A2A and Connect Hermes.
use std::{fs, io::ErrorKind, path::Path};

// Keep in sync with ratspeak_pi.contacts.BETA_PASSWORD. This is not a secret.
pub const PASSWORD: &str = "blackout";

pub fn enabled(dir: &Path) -> Result<bool, String> {
    match fs::read(dir.join("a2a-beta")) {
        Ok(value) => Ok(value == b"1"),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(_) => Err("Could not read the saved beta unlock.".into()),
    }
}

pub fn remember(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|_| "Could not save the beta unlock.")?;
    crate::write_atomic(&dir.join("a2a-beta"), b"1")
}

// Device-local, so locked and stopped agents never gate the unlock; receivers adopt it on first use.
pub fn unlock(dir: &Path, password: &str) -> Result<(), String> {
    if password != PASSWORD {
        return Err("That password isn’t right. Try again.".into());
    }
    remember(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlock_survives_reopening_without_storing_the_password() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("app-data");
        assert!(!enabled(&dir).unwrap());
        remember(&dir).unwrap();
        assert!(enabled(&dir.to_path_buf()).unwrap());
        assert_eq!(fs::read(dir.join("a2a-beta")).unwrap(), b"1");
        assert!(!enabled(&temp.path().join("another-device")).unwrap());
        fs::write(dir.join("a2a-beta"), b"0").unwrap();
        assert!(!enabled(&dir).unwrap());
    }

    #[test]
    fn unlock_needs_only_the_password() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("app-data");
        assert!(unlock(&dir, "wrong").is_err());
        assert!(!enabled(&dir).unwrap());
        unlock(&dir, PASSWORD).unwrap();
        assert!(enabled(&dir).unwrap());
    }

    #[test]
    fn desktop_and_receiver_use_the_same_beta_password() {
        let receiver = include_str!("../../../../ratspeak_pi/contacts.py");
        assert!(receiver.contains(&format!("BETA_PASSWORD = \"{PASSWORD}\"")));
    }
}
