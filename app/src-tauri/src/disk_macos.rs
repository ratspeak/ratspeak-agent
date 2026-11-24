//! macOS: cards through `diskutil`, and the raw card through `authopen`.
//!
//! A disk is offered only when macOS reports it as a whole, physical disk holding removable media (a card in
//! a reader, or the built-in card slot), of a size SD cards come in. External drives report fixed media and
//! are never listed.
use std::{
    fs::File,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use plist::{Dictionary, Value};

use crate::card::{self, Card};
use crate::image::{self, Say};

fn diskutil(args: &[&str]) -> Result<Dictionary, String> {
    let out = Command::new("/usr/sbin/diskutil").args(args).output().map_err(|e| format!("diskutil: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Value::from_reader(Cursor::new(out.stdout))
        .ok()
        .and_then(Value::into_dictionary)
        .ok_or_else(|| "diskutil gave an unreadable answer".to_string())
}

fn name_ok(device: &str) -> bool {
    device.strip_prefix("disk").map(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).unwrap_or(false)
}

// An IOMedia registry entry lasts for one attachment. A replacement gets a new
// registry ID even when its BSD name, reader and filesystem are identical.
pub fn media_id(device: &str) -> Result<String, String> {
    use std::ffi::{c_char, c_void, CString};
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOBSDNameMatching(port: u32, options: u32, name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(port: u32, matching: *mut c_void) -> u32;
        fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
        fn IOObjectRelease(object: u32) -> i32;
    }
    if !name_ok(device) { return Err(card::MEDIA_CHANGED.into()); }
    let name = CString::new(device).unwrap();
    let mut id = 0;
    unsafe {
        let matching = IOBSDNameMatching(0, 0, name.as_ptr());
        if matching.is_null() { return Err(card::MEDIA_CHANGED.into()); }
        let entry = IOServiceGetMatchingService(0, matching);
        if entry == 0 { return Err(card::MEDIA_CHANGED.into()); }
        let status = IORegistryEntryGetRegistryEntryID(entry, &mut id);
        IOObjectRelease(entry);
        if status != 0 || id == 0 { return Err(card::MEDIA_CHANGED.into()); }
    }
    Ok(format!("{id:x}"))
}

fn text<'a>(d: &'a Dictionary, key: &str) -> &'a str {
    d.get(key).and_then(Value::as_string).unwrap_or("")
}

fn flag(d: &Dictionary, key: &str) -> bool {
    d.get(key).and_then(Value::as_boolean).unwrap_or(false)
}

/// (volume name, mount point) for each partition of a whole disk.
fn partitions(device: &str) -> Result<Vec<(String, String)>, String> {
    let listing = diskutil(&["list", "-plist", device])?;
    let mut out = Vec::new();
    for disk in listing.get("AllDisksAndPartitions").and_then(Value::as_array).into_iter().flatten() {
        let Some(disk) = disk.as_dictionary() else { continue };
        for part in disk.get("Partitions").and_then(Value::as_array).into_iter().flatten() {
            if let Some(part) = part.as_dictionary() {
                out.push((text(part, "VolumeName").to_string(), text(part, "MountPoint").to_string()));
            }
        }
        if !text(disk, "MountPoint").is_empty() {
            out.push((text(disk, "VolumeName").to_string(), text(disk, "MountPoint").to_string()));
        }
    }
    Ok(out)
}

/// Ok only for a disk that is safe to erase as an SD card. Returns its description and size.
pub fn check(device: &str) -> Result<(String, u64), String> {
    if !name_ok(device) {
        return Err("Not a disk name.".into());
    }
    let info = diskutil(&["info", "-plist", device])?;
    let size = info.get("TotalSize").and_then(Value::as_unsigned_integer).unwrap_or(0);
    let sd_slot = text(&info, "BusProtocol") == "Secure Digital";
    let refuse = if !flag(&info, "WholeDisk") || text(&info, "VirtualOrPhysical") == "Virtual" {
        Some("it is not a whole physical disk")
    } else if !(flag(&info, "RemovableMedia") || sd_slot) {
        Some("it is a drive, not a memory card")
    } else if flag(&info, "Internal") && !sd_slot {
        Some("it is built into this computer")
    } else if let Some(problem) = card::size_problem(size) {
        Some(problem)
    } else if info.contains_key("WritableMedia") && !flag(&info, "WritableMedia") {
        Some("it is locked. Slide the switch on the side of the card")
    } else {
        None
    };
    if let Some(why) = refuse {
        return Err(format!("{device} will not be erased: {why}."));
    }
    for (_, mount) in partitions(device)? {
        if mount == "/" || mount.starts_with("/System") {
            return Err(format!("{device} will not be erased: this computer is running from it."));
        }
    }
    let name = [text(&info, "IORegistryEntryName"), text(&info, "MediaName")]
        .into_iter()
        .find(|n| !n.is_empty())
        .unwrap_or("SD card")
        .replace(" Media", "");
    Ok((name, size))
}

pub fn ratspeak_boot(device: &str) -> Result<Option<PathBuf>, String> {
    check(device)?;
    Ok(card::boot_of(&partitions(device)?))
}

pub fn list() -> Result<Vec<Card>, String> {
    let listing = diskutil(&["list", "-plist", "physical"])?;
    let mut cards = Vec::new();
    for disk in listing.get("WholeDisks").and_then(Value::as_array).into_iter().flatten() {
        let Some(device) = disk.as_string() else { continue };
        match check(device) {
            Ok(_) => {
                if let Ok(row) = card::identified(device, || {
                    let (name, size) = check(device)?;
                    Ok(card::usable(device, name, size, partitions(device)?))
                }) { cards.push(row); }
            }
            Err(why) => {
                if let Ok(info) = diskutil(&["info", "-plist", device]) {
                    if !flag(&info, "Internal") && (flag(&info, "RemovableMedia") || text(&info, "BusProtocol") == "USB") {
                        let size = info.get("TotalSize").and_then(Value::as_unsigned_integer).unwrap_or(0);
                        cards.push(card::refused(device, text(&info, "MediaName").to_string(), size, why));
                    }
                }
            }
        }
    }
    Ok(cards)
}

/// Open the raw card for reading and writing through macOS's own `authopen`, which asks for an administrator
/// password and hands back an open file. Opening a raw disk any other way is refused ("Operation not
/// permitted") even for root, so nothing in this app runs with administrator rights on macOS.
pub(crate) fn authopen(path: &str) -> Result<File, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let mut pair = [0i32; 2];
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, pair.as_mut_ptr()) } != 0 {
        return Err("Could not ask macOS for access to the card.".into());
    }
    let ours = unsafe { File::from_raw_fd(pair[0]) }; // closed when this returns
    let mut child = Command::new("/usr/libexec/authopen")
        .args(["-stdoutpipe", "-o", "2", path]) // 2 = O_RDWR
        .stdout(unsafe { Stdio::from_raw_fd(pair[1]) })
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("authopen: {e}"))?;
    let (mut byte, mut control) = ([0u8; 16], [0u64; 8]);
    let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: byte.len() };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control) as _;
    let got = unsafe { libc::recvmsg(ours.as_raw_fd(), &mut message, 0) };
    let mut card = None;
    if got >= 0 {
        let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
        if !header.is_null() && unsafe { (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS } {
            let fd = unsafe { std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<i32>()) };
            card = Some(unsafe { File::from_raw_fd(fd) });
        }
    }
    let mut why = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut why);
    }
    let _ = child.wait();
    card.ok_or_else(|| {
        if why.trim().is_empty() || why.contains("cancel") {
            image::CANCELLED.to_string()
        } else {
            format!("macOS did not allow the card to be written. {}", why.trim())
        }
    })
}

/// Erase the card and write the image to it, then read it back.
pub fn write(op: image::CardWrite<'_>, device: &str, selected_media: &str, _cache: &Path, say: Say) -> Result<(), String> {
    card::require_media(selected_media, media_id(device)?)?;
    let (_, capacity) = check(device)?;
    if let image::CardWrite::Reset { expected, .. } = op { image::check_erase_target(device, expected)?; }
    let out = Command::new("/usr/sbin/diskutil").args(["unmountDisk", device]).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("The card is in use and could not be unmounted. Close anything using it and try again. ({})",
                           String::from_utf8_lossy(&out.stderr).trim()));
    }
    // FAT inspection needs byte-addressable reads; the buffered disk device also supports whole-card writes.
    let path = match op { image::CardWrite::Reset { .. } => format!("/dev/{device}"), _ => format!("/dev/r{device}") };
    let result = authopen(&path).and_then(|mut target| {
        if let image::CardWrite::Reset { expected, .. } = op {
            image::verify_raw_identity(&mut target, expected)?;
        }
        op.run_selected(target, capacity, selected_media, &|_| media_id(device), say)
    });
    if result.is_err() && matches!(op, image::CardWrite::Reset { .. }) {
        let _ = Command::new("/usr/sbin/diskutil").args(["mountDisk", device]).output();
    }
    result
}

/// True when the card was ejected and can be pulled out.
pub fn eject(device: &str) -> bool {
    name_ok(device) && Command::new("/usr/sbin/diskutil").args(["eject", device]).output().map(|o| o.status.success()).unwrap_or(false)
}

/// Wi-Fi networks this computer has joined, to offer as suggestions. No password is read.
pub fn wifi_names() -> Vec<String> {
    let mut names = Vec::new();
    for device in ["en0", "en1"] {
        if let Ok(out) = Command::new("/usr/sbin/networksetup").args(["-listpreferredwirelessnetworks", device]).output() {
            if out.status.success() {
                names.extend(String::from_utf8_lossy(&out.stdout).lines().skip(1).map(|l| l.trim().to_string()));
            }
        }
    }
    names
}
