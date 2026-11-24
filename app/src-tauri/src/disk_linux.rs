//! Linux: cards through `lsblk`, and the raw card through this same program started as root by `pkexec`.
//!
//! A disk is offered only when the kernel marks it removable (a card in a USB reader) or it is an SD card in a
//! built-in slot, of a size SD cards come in. USB drives and built-in eMMC storage are never listed.
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

use crate::card::{self, Card};
use crate::image::{self, Say};

fn truth(v: &Value) -> bool {
    v.as_bool().unwrap_or_else(|| v.as_str().map(|s| s == "1" || s == "true").unwrap_or(false))
}

fn number(v: &Value) -> u64 {
    v.as_u64().unwrap_or_else(|| v.as_str().and_then(|s| s.parse().ok()).unwrap_or(0))
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("").trim()
}

fn name_ok(device: &str) -> bool {
    !device.is_empty() && device.len() <= 32 && device.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn lsblk() -> Result<Vec<Value>, String> {
    let out = Command::new("lsblk")
        .args(["--json", "--bytes", "--output", "NAME,PATH,SIZE,RM,RO,TYPE,TRAN,VENDOR,MODEL,LABEL,MOUNTPOINT"])
        .output()
        .map_err(|e| format!("lsblk could not be run: {e}"))?;
    let parsed: Value = serde_json::from_slice(&out.stdout).map_err(|_| "lsblk gave an unreadable answer".to_string())?;
    Ok(parsed.get("blockdevices").and_then(Value::as_array).cloned().unwrap_or_default())
}

/// (label, mount point, device path) for each partition; for a disk formatted without partitions, the disk.
fn partitions(disk: &Value) -> Vec<(String, String, String)> {
    let row = |v: &Value| (text(v, "label").to_string(), text(v, "mountpoint").to_string(), text(v, "path").to_string());
    match disk.get("children").and_then(Value::as_array) {
        Some(children) if !children.is_empty() => children.iter().map(row).collect(),
        _ => vec![row(disk)],
    }
}

/// True for an SD card in a built-in slot; false for soldered eMMC, which shares the mmcblk name.
fn is_sd_slot(name: &str) -> bool {
    name.starts_with("mmcblk") && fs::read_to_string(format!("/sys/block/{name}/device/type")).map(|t| t.trim() == "SD").unwrap_or(false)
}

/// The rule, apart from the disk listing so it can be tested: Ok(description, size) or the reason for refusing.
pub fn judge(disk: &Value, sd_slot: bool) -> Result<(String, u64), String> {
    let name = text(disk, "name");
    let size = number(disk.get("size").unwrap_or(&Value::Null));
    let refuse = if text(disk, "type") != "disk" {
        Some("it is not a whole physical disk")
    } else if !(truth(disk.get("rm").unwrap_or(&Value::Null)) || sd_slot) {
        Some("it is a drive, not a memory card")
    } else if let Some(problem) = card::size_problem(size) {
        Some(problem)
    } else if truth(disk.get("ro").unwrap_or(&Value::Null)) {
        Some("it is locked. Slide the switch on the side of the card")
    } else if partitions(disk).iter().any(|(_, mount, _)| {
        mount == "/" || mount == "[SWAP]" || ["/boot", "/usr", "/var", "/home", "/etc"].iter().any(|system| mount == system || mount.starts_with(&format!("{system}/")))
    }) {
        Some("this computer is running from it")
    } else {
        None
    };
    if let Some(why) = refuse {
        return Err(format!("{name} will not be erased: {why}."));
    }
    let label = [text(disk, "vendor"), text(disk, "model")].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ");
    Ok((if label.is_empty() { "SD card".to_string() } else { label }, size))
}

fn find(device: &str) -> Result<Value, String> {
    if !name_ok(device) {
        return Err("Not a disk name.".into());
    }
    lsblk()?.into_iter().find(|disk| text(disk, "name") == device).ok_or_else(|| "The card was removed.".to_string())
}

pub fn check(device: &str) -> Result<(String, u64), String> {
    judge(&find(device)?, is_sd_slot(device))
}

pub fn media_id(device: &str) -> Result<String, String> {
    if !name_ok(device) { return Err(card::MEDIA_CHANGED.into()); }
    fs::read_to_string(format!("/sys/class/block/{device}/diskseq")).ok()
        .and_then(|s| s.trim().parse::<u64>().ok()).filter(|n| *n != 0)
        .map(|n| format!("{n:x}")).ok_or_else(|| card::MEDIA_CHANGED.into())
}

pub fn writer_identity(_device: &str) -> Result<fn(&File) -> Result<String, String>, String> {
    Ok(opened_id)
}

fn opened_id(target: &File) -> Result<String, String> {
    use std::os::fd::AsRawFd;
    let mut sequence: u64 = 0;
    // _IOR(0x12, 128, __u64), the kernel's sequence for this open block device.
    if unsafe { libc::ioctl(target.as_raw_fd(), 0x8008_1280, &mut sequence) } < 0 {
        return Err(card::MEDIA_CHANGED.into());
    }
    Ok(format!("{sequence:x}"))
}

pub fn list() -> Result<Vec<Card>, String> {
    let mut cards = Vec::new();
    for disk in lsblk()? {
        let name = text(&disk, "name").to_string();
        if text(&disk, "type") != "disk" || number(disk.get("size").unwrap_or(&Value::Null)) == 0 {
            continue; // loop devices, RAM disks, and readers with no card in them
        }
        let sd_slot = is_sd_slot(&name);
        match judge(&disk, sd_slot) {
            Ok(_) => {
                if let Ok(row) = card::identified(&name, || {
                    let disk = find(&name)?;
                    let (label, size) = judge(&disk, is_sd_slot(&name))?;
                    let parts = partitions(&disk).into_iter().map(|(label, mount, _)| (label, mount)).collect();
                    Ok(card::usable(&name, label, size, parts))
                }) { cards.push(row); }
            }
            Err(why) => {
                if truth(disk.get("rm").unwrap_or(&Value::Null)) || sd_slot {
                    let label = text(&disk, "model").to_string();
                    cards.push(card::refused(&name, label, number(disk.get("size").unwrap_or(&Value::Null)), why));
                }
            }
        }
    }
    Ok(cards)
}

/// The mounted boot partition of a card this app set up. Desktops mount cards as they are put in; where one
/// does not, ask udisks to (no password is needed for removable media).
pub fn ratspeak_boot(device: &str) -> Result<Option<PathBuf>, String> {
    check(device)?;
    for (label, mount, path) in partitions(&find(device)?) {
        if label == "bootfs" && mount.is_empty() {
            let _ = Command::new("udisksctl").args(["mount", "--no-user-interaction", "-b", &path]).output();
        }
    }
    let parts: Vec<_> = partitions(&find(device)?).into_iter().map(|(label, mount, _)| (label, mount)).collect();
    Ok(card::boot_of(&parts))
}

pub fn write(op: image::CardWrite<'_>, device: &str, selected_media: &str, cache: &Path, say: Say) -> Result<(), String> {
    check(device)?;
    let mut pkexec = Command::new("pkexec");
    image::elevated(&mut pkexec, op, device, selected_media, cache, say).map_err(|(code, why)| match code {
        Some(126) => image::CANCELLED.to_string(),
        Some(127) if why.is_empty() => "This computer has no way to ask for an administrator password (pkexec is missing). Install polkit and try again.".to_string(),
        _ => why,
    })
}

/// Runs as root: unmount the card's partitions and open the whole disk, refusing if anything still holds it.
pub fn open_raw(device: &str) -> Result<(File, ()), String> {
    for (_, mount, path) in partitions(&find(device)?) {
        if !mount.is_empty() && !Command::new("umount").arg(&path).status().map(|s| s.success()).unwrap_or(false) {
            return Err("The card is in use and could not be unmounted. Close anything using it and try again.".into());
        }
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_EXCL)
        .open(format!("/dev/{device}"))
        .map(|file| (file, ()))
        .map_err(|e| format!("The card could not be opened for writing: {e}"))
}

/// Runs as root, between writing and reading back: drop the kernel's copy so the check reads the card itself.
pub fn before_verify(card: &File) {
    use std::os::fd::AsRawFd;
    const BLKFLSBUF: libc::c_ulong = 0x1261;
    unsafe { libc::ioctl(card.as_raw_fd(), BLKFLSBUF as _, 0) };
}

/// Runs as root, after the card is written: have the kernel read the new partition table.
pub fn after_write(card: &File) {
    use std::os::fd::AsRawFd;
    const BLKRRPART: libc::c_ulong = 0x125F;
    unsafe { libc::ioctl(card.as_raw_fd(), BLKRRPART as _, 0) };
}

pub fn eject(device: &str) -> bool {
    let Ok(disk) = find(device) else { return false };
    let _ = Command::new("sync").status();
    for (_, mount, path) in partitions(&disk) {
        if !mount.is_empty() && !Command::new("udisksctl").args(["unmount", "--no-user-interaction", "-b", &path]).output().map(|o| o.status.success()).unwrap_or(false) {
            return false;
        }
    }
    let _ = Command::new("udisksctl").args(["power-off", "--no-user-interaction", "-b", &format!("/dev/{device}")]).output();
    true
}

pub fn wifi_names() -> Vec<String> {
    let Ok(out) = Command::new("nmcli").args(["--terse", "--fields", "NAME,TYPE", "connection", "show"]).output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.strip_suffix(":802-11-wireless"))
        .map(|name| name.replace("\\:", ":"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_cards_are_offered() {
        let reader = json!({"name": "sdb", "path": "/dev/sdb", "size": 31914983424u64, "rm": true, "ro": false, "type": "disk",
            "tran": "usb", "vendor": "Generic ", "model": "STORAGE DEVICE", "children": [
            {"name": "sdb1", "path": "/dev/sdb1", "label": "bootfs", "mountpoint": "/media/me/bootfs"}]});
        assert_eq!(judge(&reader, false), Ok(("Generic STORAGE DEVICE".to_string(), 31914983424)));
        assert_eq!(partitions(&reader)[0], ("bootfs".to_string(), "/media/me/bootfs".to_string(), "/dev/sdb1".to_string()));
        // Older lsblk prints numbers and flags as strings.
        let old = json!({"name": "sdc", "size": "63864569856", "rm": "1", "ro": "0", "type": "disk"});
        assert_eq!(judge(&old, false), Ok(("SD card".to_string(), 63864569856)));
        let ssd = json!({"name": "sda", "size": 500107862016u64, "rm": false, "ro": false, "type": "disk", "tran": "usb"});
        assert!(judge(&ssd, false).unwrap_err().contains("drive, not a memory card"));
        let system = json!({"name": "nvme0n1", "size": 256060514304u64, "rm": false, "ro": false, "type": "disk",
            "children": [{"name": "nvme0n1p2", "mountpoint": "/"}]});
        assert!(judge(&system, false).is_err());
        let slot = json!({"name": "mmcblk0", "size": 63864569856u64, "rm": false, "ro": false, "type": "disk"});
        assert!(judge(&slot, true).is_ok() && judge(&slot, false).is_err()); // an SD slot, and soldered eMMC
        let booted = json!({"name": "mmcblk0", "size": 63864569856u64, "rm": false, "ro": false, "type": "disk",
            "children": [{"name": "mmcblk0p2", "mountpoint": "/"}, {"name": "mmcblk0p1", "mountpoint": "/boot/firmware"}]});
        assert!(judge(&booted, true).unwrap_err().contains("running from it")); // this app run on a Pi itself
        let locked = json!({"name": "sdb", "size": 31914983424u64, "rm": true, "ro": true, "type": "disk"});
        assert!(judge(&locked, false).unwrap_err().contains("locked"));
        let stick = json!({"name": "sdd", "size": 2004877312u64, "rm": true, "ro": false, "type": "disk"});
        assert!(judge(&stick, false).unwrap_err().contains("too small"));
    }
}
