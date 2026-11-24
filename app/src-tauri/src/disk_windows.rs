//! Windows: cards through PowerShell's disk listing, and the raw card through this same program started with
//! administrator rights (the usual "Do you want to allow this app to make changes" prompt).
//!
//! A disk is offered only when Windows reports removable media (a card in a reader), it is not the disk
//! Windows starts from, and its size is one SD cards come in. External drives report fixed or external
//! hard disk media and are never listed.
use std::{
    fs::{File, OpenOptions},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;
use sha2::{Digest, Sha256};
use windows_sys::Win32::System::{IO::DeviceIoControl, Ioctl::{FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, IOCTL_DISK_UPDATE_PROPERTIES, IOCTL_STORAGE_CHECK_VERIFY2}};

use crate::card::{self, Card};
use crate::image::{self, Say};

const NO_WINDOW: u32 = 0x0800_0000;
const LIST: &str = r#"$ErrorActionPreference='SilentlyContinue';$d=Get-CimInstance Win32_DiskDrive|ForEach-Object{$n=$_.Index;$disk=Get-Disk -Number $n;[pscustomobject]@{n=[string]$n;instance=[string]$_.PNPDeviceID;arrival=[string]((Get-PnpDeviceProperty -InstanceId $_.PNPDeviceID -KeyName 'DEVPKEY_Device_LastArrivalDate').Data.ToFileTimeUtc());model=[string]$_.Model;size=[string]$disk.Size;media=[string]$_.MediaType;system=[bool](!$disk -or $disk.IsBoot -or $disk.IsSystem);locked=[bool]$disk.IsReadOnly;vols=@(Get-Partition -DiskNumber $n|Get-Volume|ForEach-Object{[pscustomobject]@{path=[string]$_.Path;letter=[string]$_.DriveLetter;label=[string]$_.FileSystemLabel}})}};ConvertTo-Json -InputObject @($d) -Depth 4 -Compress"#;

fn powershell(script: &str) -> Command {
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script]).creation_flags(NO_WINDOW);
    command
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("").trim()
}

fn name_ok(device: &str) -> bool {
    !device.is_empty() && device.len() <= 3 && device.bytes().all(|b| b.is_ascii_digit())
}

fn disks() -> Result<Vec<Value>, String> {
    let out = powershell(LIST).output().map_err(|e| format!("PowerShell could not be run: {e}"))?;
    let parsed: Value = serde_json::from_slice(&out.stdout).map_err(|_| "Windows gave an unreadable list of disks".to_string())?;
    Ok(parsed.as_array().cloned().unwrap_or_default())
}

/// (label, mount point) for each volume with a drive letter.
fn partitions(disk: &Value) -> Vec<(String, String)> {
    disk.get("vols")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|volume| {
            let letter = text(volume, "letter").trim_matches('\0');
            letter.chars().next().filter(char::is_ascii_alphabetic).map(|letter| (text(volume, "label").to_string(), format!("{letter}:\\")))
        })
        .collect()
}

/// The rule, apart from the disk listing so it can be tested: Ok(description, size) or the reason for refusing.
pub fn judge(disk: &Value) -> Result<(String, u64), String> {
    let size = text(disk, "size").parse::<u64>().unwrap_or(0);
    let refuse = if !text(disk, "media").to_ascii_lowercase().starts_with("removable") {
        Some("it is a drive, not a memory card")
    } else if disk.get("system").and_then(Value::as_bool).unwrap_or(true) {
        Some("this computer is running from it")
    } else if let Some(problem) = card::size_problem(size) {
        Some(problem)
    } else if disk.get("locked").and_then(Value::as_bool).unwrap_or(false) {
        Some("it is locked. Slide the switch on the side of the card")
    } else {
        None
    };
    if let Some(why) = refuse {
        return Err(format!("Disk {} will not be erased: {why}.", text(disk, "n")));
    }
    let model = text(disk, "model").replace(" USB Device", "");
    Ok((if model.is_empty() { "SD card".to_string() } else { model }, size))
}

fn find(device: &str) -> Result<Value, String> {
    if !name_ok(device) {
        return Err("Not a disk number.".into());
    }
    disks()?.into_iter().find(|disk| text(disk, "n") == device).ok_or_else(|| "The card was removed.".to_string())
}

pub fn check(device: &str) -> Result<(String, u64), String> {
    judge(&find(device)?)
}

fn ioctl(file: &File, code: u32, output: &mut [u8]) -> std::io::Result<u32> {
    let mut count = 0;
    let result = unsafe {
        DeviceIoControl(file.as_raw_handle(), code, std::ptr::null(), 0,
            output.as_mut_ptr().cast(), output.len() as u32, &mut count, std::ptr::null_mut())
    };
    if result == 0 { Err(std::io::Error::last_os_error()) } else { Ok(count) }
}

// The driver increments the counter for media changes within the reader.
// The PnP arrival distinguishes unplugging the whole reader (which resets its counter).
fn insertion(disk: &Value, handle: &File) -> Result<String, String> {
    let mut counter = [0u8; 4];
    if text(disk, "instance").is_empty() || text(disk, "arrival").parse::<u64>().unwrap_or(0) == 0
        || ioctl(handle, IOCTL_STORAGE_CHECK_VERIFY2, &mut counter).ok() != Some(4) {
        return Err(card::MEDIA_CHANGED.into());
    }
    let mut hash = Sha256::new();
    hash.update(text(disk, "instance"));
    hash.update([0]);
    hash.update(text(disk, "arrival"));
    hash.update(counter);
    Ok(format!("{:x}", hash.finalize()))
}

pub fn media_id(device: &str) -> Result<String, String> {
    let disk = find(device)?;
    let handle = OpenOptions::new().access_mode(0x80) // FILE_READ_ATTRIBUTES
        .open(format!(r"\\.\PhysicalDrive{device}")).map_err(|_| card::MEDIA_CHANGED)?;
    insertion(&disk, &handle)
}

pub fn writer_identity(device: &str) -> Result<impl Fn(&File) -> Result<String, String>, String> {
    let disk = find(device)?;
    Ok(move |handle: &File| insertion(&disk, handle))
}

pub fn list() -> Result<Vec<Card>, String> {
    let mut cards = Vec::new();
    for disk in disks()? {
        let (device, size) = (text(&disk, "n").to_string(), text(&disk, "size").parse::<u64>().unwrap_or(0));
        if size == 0 {
            continue; // a reader with no card in it
        }
        match judge(&disk) {
            Ok(_) => {
                if let Ok(row) = card::identified(&device, || {
                    let disk = find(&device)?;
                    let (name, size) = judge(&disk)?;
                    Ok(card::usable(&device, name, size, partitions(&disk)))
                }) { cards.push(row); }
            }
            Err(why) => {
                if text(&disk, "media").to_ascii_lowercase().starts_with("removable") {
                    cards.push(card::refused(&device, text(&disk, "model").to_string(), size, why));
                }
            }
        }
    }
    Ok(cards)
}

pub fn ratspeak_boot(device: &str) -> Result<Option<PathBuf>, String> {
    let disk = find(device)?;
    judge(&disk)?;
    Ok(card::boot_of(&partitions(&disk)))
}

/// A PowerShell single-quoted string. PowerShell also ends one at the curly single quotes, so each is doubled.
fn literal(text: &str) -> String {
    let mut out = String::from("'");
    for c in text.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out + "'"
}

fn quoted(text: &str) -> String {
    literal(&format!("\"{text}\""))
}

pub fn write(op: image::CardWrite<'_>, device: &str, selected_media: &str, cache: &Path, say: Say) -> Result<(), String> {
    check(device)?;
    // Start-Process -Verb RunAs is what raises the Windows permission prompt; this process only waits for it.
    let (exe, args) = image::helper_arguments(op, device, selected_media, cache)?;
    let script = format!(
        "try {{ $p = Start-Process -FilePath {} -ArgumentList {} -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode }} catch {{ exit 126 }}",
        literal(&exe),
        args.iter().map(|arg| quoted(arg)).collect::<Vec<_>>().join(",")
    );
    let mut command = powershell(&script);
    image::wait_for_helper(&mut command, cache, say).map_err(|(code, why)| if code == Some(126) { image::CANCELLED.to_string() } else { why })
}

/// Keep mounted filesystems locked for the entire raw write. Never clear a disk by number.
pub fn open_raw(device: &str) -> Result<(File, Vec<File>), String> {
    let disk = find(device)?;
    judge(&disk)?;
    let target = OpenOptions::new().read(true).write(true)
        .open(format!(r"\\.\PhysicalDrive{device}")).map_err(|e| format!("Could not open the card: {e}"))?;
    let mut locks = Vec::new();
    for volume in disk["vols"].as_array().into_iter().flatten() {
        let path = text(volume, "path").trim_end_matches('\\');
        if !path.starts_with(r"\\?\Volume{") || !path.ends_with('}') {
            return Err("Could not identify a volume on the selected card. Close any files and retry.".into());
        }
        let handle = OpenOptions::new().read(true).write(true).open(path)
            .map_err(|e| format!("Could not open the card’s volume: {e}"))?;
        ioctl(&handle, FSCTL_LOCK_VOLUME, &mut []).and_then(|_| ioctl(&handle, FSCTL_DISMOUNT_VOLUME, &mut []))
            .map_err(|_| "The card is in use. Close its files and try again.")?;
        locks.push(handle);
    }
    Ok((target, locks))
}

pub fn before_verify(_card: &File) {}
pub fn after_write(card: &File) {
    let _ = ioctl(card, IOCTL_DISK_UPDATE_PROPERTIES, &mut []);
}

/// Windows writes removable cards through at once ("quick removal"), so a card whose files are closed can be
/// pulled out; there is nothing to eject, and this reports that it did not.
pub fn eject(_device: &str) -> bool {
    false
}

pub fn wifi_names() -> Vec<String> {
    let Ok(out) = Command::new("netsh.exe").args(["wlan", "show", "profiles"]).creation_flags(NO_WINDOW).output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.contains("All User Profile") || line.contains("Current User Profile"))
        .filter_map(|line| line.split_once(':').map(|(_, name)| name.trim().to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_cards_are_offered() {
        let reader = json!({"n": "2", "model": "Generic STORAGE DEVICE USB Device", "size": "31914983424", "media": "Removable Media",
            "system": false, "locked": false, "vols": [{"letter": "E", "label": "bootfs"}, {"letter": "", "label": ""}]});
        assert_eq!(judge(&reader), Ok(("Generic STORAGE DEVICE".to_string(), 31914983424)));
        assert_eq!(partitions(&reader), vec![("bootfs".to_string(), "E:\\".to_string())]);
        let ssd = json!({"n": "1", "model": "Samsung T7", "size": "500107862016", "media": "External hard disk media", "system": false});
        assert!(judge(&ssd).unwrap_err().contains("drive, not a memory card"));
        let system = json!({"n": "0", "model": "NVMe", "size": "256060514304", "media": "Fixed hard disk media", "system": true});
        assert!(judge(&system).is_err());
        let odd = json!({"n": "3", "size": "31914983424", "media": "Removable Media"}); // no answer about the system disk
        assert!(judge(&odd).unwrap_err().contains("running from it"));
        assert_eq!(quoted(r"C:\Users\O'Neil\x y.img"), r#"'"C:\Users\O''Neil\x y.img"'"#);
        assert_eq!(quoted("C:\\Users\\O\u{2019}Neil\\x.img"), "'\"C:\\Users\\O\u{2019}\u{2019}Neil\\x.img\"'");
        assert_eq!(literal("a\u{2018}b\u{201A}c\u{201B}"), "'a\u{2018}\u{2018}b\u{201A}\u{201A}c\u{201B}\u{201B}'");
    }
}
