//! The Raspberry Pi OS image: download, unpack and verify, add the first-boot files, write it to the card, and
//! read the card back.
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

#[cfg(not(target_os = "macos"))]
use std::process::Command;

use crate::network::curl;
use sha2::{Digest, Sha256};

// Raspberry Pi OS Lite (64-bit), Debian 13 "Trixie", as listed by downloads.raspberrypi.com for Raspberry Pi Imager.
const URL: &str = "https://downloads.raspberrypi.com/raspios_lite_arm64/images/raspios_lite_arm64-2026-10-06/2026-10-06-raspios-trixie-arm64-lite.img.xz";
const XZ_BYTES: u64 = 550_466_056;
pub const RAW_BYTES: u64 = 3_078_619_136;
pub const RAW_SHA256: &str = "b8a393dc1c3701450b5fffbfe74368a98fb6eab7415ce456cc0cead886ea8780";
pub const CANCELLED: &str = "Cancelled. Permission was not given, so the card was not changed.";
const BLOCK: usize = 4 << 20;
const SECTOR: u64 = 512;

pub type Say<'a> = &'a dyn Fn(&str, u64, u64);

fn hex(digest: impl AsRef<[u8]>) -> String {
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn download(xz: &Path, say: Say) -> Result<(), String> {
    if size(xz) == XZ_BYTES {
        return Ok(());
    }
    if size(xz) > XZ_BYTES {
        let _ = fs::remove_file(xz);
    }
    let mut curl = curl()
        // A stalled transfer fails instead of hanging; --retry and --continue-at resume it.
        .args(["--fail", "--location", "--silent", "--show-error", "--continue-at", "-", "--retry", "3",
               "--connect-timeout", "30", "--speed-limit", "1024", "--speed-time", "60", "--output"])
        .arg(xz)
        .arg(URL)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Raspberry Pi OS could not be downloaded: curl is not installed on this computer ({e})."))?;
    let status = loop {
        say("download", size(xz), XZ_BYTES);
        if let Some(status) = curl.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        thread::sleep(Duration::from_millis(400));
    };
    if !status.success() || size(xz) != XZ_BYTES {
        let mut why = String::new();
        if let Some(mut err) = curl.stderr.take() {
            let _ = err.read_to_string(&mut why);
        }
        return Err(format!("Raspberry Pi OS could not be downloaded. Check this computer's internet connection and try again. {}", why.trim()));
    }
    Ok(())
}

/// Pull bounded chunks from the decoder so progress is independent of the archive's XZ block size.
fn unpack(input: BufReader<File>, output: File, say: Say) -> std::io::Result<bool> {
    let mut decoder = xz2::bufread::XzDecoder::new(input);
    let mut output = BufWriter::with_capacity(1 << 20, output);
    let mut block = vec![0; 256 << 10];
    let mut hash = Sha256::new();
    let mut done = 0;
    let mut last_update = Instant::now();
    say("unpack", 0, RAW_BYTES);
    loop {
        let n = decoder.read(&mut block)?;
        if n == 0 { break; }
        done += n as u64;
        if done > RAW_BYTES { return Ok(false); }
        output.write_all(&block[..n])?;
        hash.update(&block[..n]);
        if last_update.elapsed() >= Duration::from_millis(150) {
            say("unpack", done, RAW_BYTES);
            last_update = Instant::now();
        }
    }
    output.flush()?;
    Ok(done == RAW_BYTES && hex(hash.finalize()) == RAW_SHA256)
}

/// Download (or reuse) the compressed image, unpack it, and check the result against the published SHA-256.
pub fn prepare(dir: &Path, say: Say) -> Result<PathBuf, String> {
    let xz = dir.join("raspios-trixie-arm64-lite.img.xz");
    let raw = dir.join("raspios-trixie-arm64-lite.img");
    for attempt in 0..2 {
        download(&xz, say)?;
        let io = |e: std::io::Error| format!("Could not unpack Raspberry Pi OS: {e}");
        let input = BufReader::with_capacity(1 << 20, File::open(&xz).map_err(io)?);
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&raw).map_err(io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600)).map_err(io)?;
        }
        let unpacked = unpack(input, file, say);
        if matches!(unpacked, Ok(true)) {
            say("unpack", RAW_BYTES, RAW_BYTES);
            return Ok(raw);
        }
        let _ = fs::remove_file(&raw);
        let _ = fs::remove_file(&xz); // a damaged download: fetch it once more
        if attempt == 1 {
            return Err(match unpacked {
                Err(why) if matches!(why.kind(), std::io::ErrorKind::StorageFull) => {
                    "This computer is out of disk space. 4 GB is needed while the card is written.".into()
                }
                _ => "The Raspberry Pi OS download did not match its published checksum, twice. Nothing was written to the card.".into(),
            });
        }
    }
    unreachable!()
}

type Fat = fatfs::FileSystem<fscommon::BufStream<fscommon::StreamSlice<File>>>;

/// Where the image's first partition, the FAT32 boot partition, starts and how long it is, in bytes.
fn boot_extent(image: &mut File) -> Result<(u64, u64), String> {
    let mut table = [0u8; 512];
    image.seek(SeekFrom::Start(0)).and_then(|_| image.read_exact(&mut table)).map_err(|e| format!("The image could not be read: {e}"))?;
    let entry = &table[446..462];
    let start = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as u64 * SECTOR;
    let length = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as u64 * SECTOR;
    if table[510..512] != [0x55, 0xAA] || ![0x0B, 0x0C].contains(&entry[4]) || length == 0 {
        return Err("The Raspberry Pi OS image does not start with a FAT32 boot partition.".into());
    }
    Ok((start, length))
}

fn boot_partition(mut image: File) -> Result<Fat, String> {
    let io = |e: std::io::Error| format!("The image's boot partition could not be opened: {e}");
    let (start, length) = boot_extent(&mut image)?;
    let partition = fscommon::BufStream::new(fscommon::StreamSlice::new(image, start, start + length).map_err(io)?);
    fatfs::FileSystem::new(partition, fatfs::FsOptions::new()).map_err(io)
}

fn copy_tree<A: fatfs::ReadWriteSeek, B: fatfs::ReadWriteSeek>(
    src: fatfs::Dir<'_, A>, dst: fatfs::Dir<'_, B>,
) -> std::io::Result<()> {
    for entry in src.iter() {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.as_str(), "." | "..") { continue; }
        if entry.is_dir() { copy_tree(entry.to_dir(), dst.create_dir(&name)?)?; }
        else { std::io::copy(&mut entry.to_file(), &mut dst.create_file(&name)?)?; }
    }
    Ok(())
}

/// Enlarge FAT in the private working image, keeping the Linux filesystem last so Pi OS can grow it.
pub fn grow_boot(raw: &Path, assets: &[PathBuf], progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let io = |e: std::io::Error| format!("Could not make room for offline files: {e}");
    let mut original = OpenOptions::new().read(true).write(true).open(raw).map_err(io)?;
    let (start, old) = boot_extent(&mut original)?;
    let mut mbr = [0u8;512];
    original.seek(SeekFrom::Start(0)).and_then(|_| original.read_exact(&mut mbr)).map_err(io)?;
    let second = u32::from_le_bytes(mbr[470..474].try_into().unwrap()) as u64 * SECTOR;
    if mbr[466] != 0x83 || second < start + old || mbr[478..510].iter().any(|b| *b != 0) {
        return Err("The Raspberry Pi OS partition layout is not supported for offline setup.".into());
    }
    let mut bytes = 32u64 << 20;
    for asset in assets { bytes += fs::metadata(asset).map_err(io)?.len(); }
    let extra = bytes.div_ceil(4 << 20) * (4 << 20);
    let new_len = old + extra;
    let original_len = original.metadata().map_err(io)?.len();
    let root_len = original_len.checked_sub(start + old).ok_or("The Raspberry Pi OS image is truncated.")?;
    let work = root_len + new_len;
    progress(0, work);
    let source = boot_partition(File::open(raw).map_err(io)?)?;
    let parent = raw.parent().ok_or("Missing image directory")?;
    let mut fat_image = tempfile::tempfile_in(parent).map_err(io)?;
    fat_image.set_len(new_len).map_err(io)?;
    let mut label = [b' '; 11];
    let source_label = source.volume_label_as_bytes();
    label[..source_label.len()].copy_from_slice(source_label);
    let options = fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32)
        .bytes_per_cluster(8192).volume_id(source.volume_id()).volume_label(label);
    fatfs::format_volume(&mut fat_image, options).map_err(io)?;
    fat_image.seek(SeekFrom::Start(0)).map_err(io)?;
    let dest = fatfs::FileSystem::new(&mut fat_image, fatfs::FsOptions::new()).map_err(io)?;
    copy_tree(source.root_dir(), dest.root_dir()).map_err(io)?;
    dest.unmount().map_err(io)?;
    source.unmount().map_err(io)?;
    // Copy backwards so moving the root partition never overwrites unread blocks.
    let mut end = original_len;
    original.set_len(end + extra).map_err(io)?;
    let mut block = vec![0u8; BLOCK];
    while end > start + old {
        let n = (end - start - old).min(BLOCK as u64) as usize;
        end -= n as u64;
        original.seek(SeekFrom::Start(end)).and_then(|_| original.read_exact(&mut block[..n])).map_err(io)?;
        original.seek(SeekFrom::Start(end + extra)).and_then(|_| original.write_all(&block[..n])).map_err(io)?;
        progress(original_len - end, work);
    }
    fat_image.seek(SeekFrom::Start(0)).map_err(io)?;
    original.seek(SeekFrom::Start(start)).map_err(io)?;
    let mut copied = 0;
    while copied < new_len {
        let n = (new_len - copied).min(BLOCK as u64) as usize;
        fat_image.read_exact(&mut block[..n]).map_err(io)?;
        original.write_all(&block[..n]).map_err(io)?;
        copied += n as u64;
        progress(original_len - end + copied, work);
    }
    mbr[458..462].copy_from_slice(&((new_len / SECTOR) as u32).to_le_bytes());
    mbr[470..474].copy_from_slice(&(((second + extra) / SECTOR) as u32).to_le_bytes());
    original.seek(SeekFrom::Start(0)).and_then(|_| original.write_all(&mbr)).map_err(io)?;
    Ok(())
}

pub fn add_offline_assets(raw: &Path, paths: &[PathBuf], advanced: &mut dyn FnMut(usize)) -> Result<(),String> {
    let io = |e: std::io::Error| format!("Could not add the offline downloads: {e}");
    let fat = boot_partition(OpenOptions::new().read(true).write(true).open(raw).map_err(io)?)?;
    {
        let dir = fat.root_dir().open_dir("ratspeak-pi").map_err(io)?.create_dir("offline").map_err(io)?;
        for path in paths {
            let name = path.file_name().and_then(|s| s.to_str()).ok_or("Missing download filename")?;
            let mut src = File::open(path).map_err(io)?;
            let mut dst = dir.create_file(name).map_err(io)?;
            let mut buf = vec![0; BLOCK];
            loop {
                let n = src.read(&mut buf).map_err(io)?;
                if n == 0 { break; }
                dst.write_all(&buf[..n]).map_err(io)?;
                advanced(n);
            }
        }
    }
    fat.unmount().map_err(io)?;
    fix_new_folders(&mut OpenOptions::new().read(true).write(true).open(raw).map_err(io)?).map_err(io)
}

/// Repair the folders the FAT library created. It writes a long-name entry in front of each of a folder's
/// "." and ".." entries, and records the root folder's real cluster in ".." where FAT wants 0. Systems read
/// such a folder, but disk checkers report it, and Raspberry Pi OS checks this partition as it starts.
/// This rewrites each such folder as: ".", "..", then everything else in order.
fn fix_new_folders(image: &mut File) -> std::io::Result<()> {
    let (start, length) = boot_extent(image).map_err(std::io::Error::other)?;
    let mut boot = [0u8; 512];
    image.seek(SeekFrom::Start(start))?;
    image.read_exact(&mut boot)?;
    let sector = u16::from_le_bytes([boot[11], boot[12]]) as u64;
    let cluster = boot[13] as u64 * sector;
    let table = start + u16::from_le_bytes([boot[14], boot[15]]) as u64 * sector;
    let table_bytes = u32::from_le_bytes(boot[36..40].try_into().unwrap()) as u64 * sector;
    let root = u32::from_le_bytes(boot[44..48].try_into().unwrap());
    let data = table + boot[16] as u64 * table_bytes;
    if cluster == 0 || data >= start + length {
        return Ok(());
    }
    let mut fat = vec![0u8; table_bytes as usize];
    image.seek(SeekFrom::Start(table))?;
    image.read_exact(&mut fat)?;
    let next = |n: u32| fat.get(n as usize * 4..n as usize * 4 + 4).map(|e| u32::from_le_bytes(e.try_into().unwrap()) & 0x0FFF_FFFF);
    let offset = |n: u32| data + (n as u64 - 2) * cluster;
    let mut folders = vec![root];
    let mut visited = std::collections::HashSet::new();
    while let Some(first) = folders.pop() {
        if !visited.insert(first) { continue; }
        let mut chain = vec![first];
        while let Some(n) = next(*chain.last().unwrap()).filter(|n| (2..0x0FFF_FFF8).contains(n) && chain.len() < 4096) {
            chain.push(n);
        }
        let mut entries = Vec::new();
        for n in &chain {
            let mut block = vec![0u8; cluster as usize];
            image.seek(SeekFrom::Start(offset(*n)))?;
            image.read_exact(&mut block)?;
            entries.extend(block.chunks(32).map(<[u8]>::to_vec));
        }
        let total = entries.len();
        for entry in entries.iter().take_while(|e| e[0] != 0) {
            if entry[0] != 0xe5 && entry[0] != b'.' && entry[11] & 0x10 != 0 && entry[11] != 0x0f {
                let child = u32::from_le_bytes([entry[26], entry[27], entry[20], entry[21]]);
                if child >= 2 && offset(child) < start + length { folders.push(child); }
            }
        }
        if entries[0][11] != 0x0f || entries[1][..11] != *b".          " ||
            entries[2][11] != 0x0f || entries[3][..11] != *b"..         " { continue; }
        entries.remove(2);
        entries.remove(0);
        let parent = u32::from_le_bytes([entries[1][26], entries[1][27], entries[1][20], entries[1][21]]);
        if parent == root {
            entries[1][20..22].fill(0);
            entries[1][26..28].fill(0);
        }
        entries.resize(total, vec![0u8; 32]);
        for (n, block) in chain.iter().zip(entries.chunks(cluster as usize / 32)) {
            image.seek(SeekFrom::Start(offset(*n)))?;
            image.write_all(&block.concat())?;
        }
    }
    image.flush()
}

/// Put files on the image's boot partition, creating folders as needed. Doing this to the image, before it is
/// written, means a new card never has to be mounted, which works differently on every system.
pub fn add_files(raw: &Path, files: &[(String, Vec<u8>)], advanced: &mut dyn FnMut(usize)) -> Result<(), String> {
    let io = |e: std::io::Error| format!("The first-boot files could not be added to the image: {e}");
    let fat = boot_partition(OpenOptions::new().read(true).write(true).open(raw).map_err(io)?)?;
    {
        let root = fat.root_dir();
        for (path, data) in files {
            if let Some((folder, _)) = path.rsplit_once('/') {
                root.create_dir(folder).map_err(io)?;
            }
            let mut file = root.create_file(path).map_err(io)?;
            file.truncate().map_err(io)?;
            for chunk in data.chunks(1 << 20) {
                file.write_all(chunk).map_err(io)?;
                advanced(chunk.len());
            }
        }
    }
    fat.unmount().map_err(io)?;
    fix_new_folders(&mut OpenOptions::new().read(true).write(true).open(raw).map_err(io)?).map_err(io)
}

fn fill(input: &mut impl Read, block: &mut [u8]) -> std::io::Result<usize> {
    let mut have = 0;
    while have < block.len() {
        match input.read(&mut block[have..])? {
            0 => break,
            n => have += n,
        }
    }
    Ok(have)
}

/// A raw card takes writes straight through and may answer a flush request with "not supported".
fn flush_unsupported(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    return matches!(error.raw_os_error(), Some(libc::ENOTTY | libc::EINVAL | libc::ENOTSUP));
    #[cfg(windows)]
    return matches!(error.raw_os_error(), Some(1 | 50)); // ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED
}

/// Write the image to an open card (or, in tests, a file), then read it back and compare.
#[cfg(test)]
pub fn write_image(raw: &Path, target: File, say: Say) -> Result<(), String> {
    write_checked_image(raw, target, &|_| Ok(()), say)
}

type MediaCheck<'a> = &'a dyn Fn(&File) -> Result<(), String>;

fn write_checked_image(raw: &Path, mut target: File, check: MediaCheck, say: Say) -> Result<(), String> {
    let io = |what: &'static str| move |e: std::io::Error| format!("{what}: {e}");
    let mut input = File::open(raw).map_err(io("The image could not be opened"))?;
    let total = input.metadata().map_err(io("The image size could not be read"))?.len();
    let (mut block, mut done, mut hash) = (vec![0u8; BLOCK], 0u64, Sha256::new());
    loop {
        let n = fill(&mut input, &mut block).map_err(io("The image could not be read"))?;
        if n == 0 {
            break;
        }
        hash.update(&block[..n]);
        check(&target)?;
        target.write_all(&block[..n]).map_err(io("Writing the card failed. The card may be worn out or locked"))?;
        done += n as u64;
        say("write", done, total);
    }
    check(&target)?;
    if let Err(error) = target.sync_all() {
        if !flush_unsupported(&error) {
            return Err(format!("Writing the card failed: {error}"));
        }
    }
    #[cfg(not(target_os = "macos"))]
    crate::disk::before_verify(&target);
    let expected = hex(hash.finalize());
    // Read it back: worn and counterfeit cards accept writes and return something else.
    target.seek(SeekFrom::Start(0)).map_err(io("The card could not be read back"))?;
    let (mut seen, mut hash) = (0u64, Sha256::new());
    while seen < done {
        let want = BLOCK.min((done - seen) as usize);
        let n = fill(&mut target, &mut block[..want]).map_err(io("The card could not be read back"))?;
        if n == 0 {
            break;
        }
        hash.update(&block[..n]);
        seen += n as u64;
        say("verify", seen, total);
    }
    check(&target)?;
    if seen != done || hex(hash.finalize()) != expected {
        return Err("The card did not keep what was written to it. It is probably worn out or counterfeit. Use another card.".into());
    }
    #[cfg(not(target_os = "macos"))]
    crate::disk::after_write(&target);
    Ok(())
}

#[derive(Clone, Copy)]
pub enum CardWrite<'a> {
    Image(&'a Path),
    InstallAfterReset(&'a Path, &'a str),
    Reset { expected: &'a str, receipt: &'a str },
}

impl CardWrite<'_> {
    pub fn run_selected(self, mut target: File, capacity: u64, selected: &str,
                        identity: &dyn Fn(&File) -> Result<String, String>, say: Say) -> Result<(), String> {
        let check = |file: &File| crate::card::require_media(selected, identity(file)?);
        check(&target)?;
        match self {
            Self::Image(path) => write_checked_image(path, target, &check, say),
            Self::InstallAfterReset(path, receipt) => {
                verify_reset(&mut target, capacity, receipt)?;
                target.rewind().map_err(|e| e.to_string())?;
                write_checked_image(path, target, &check, say)
            }
            Self::Reset { receipt, .. } => reset_layout(target, capacity, receipt, &check),
        }
    }
}

pub fn check_erase_target(device: &str, expected: &str) -> Result<(), String> {
    let boot = crate::disk::ratspeak_boot(device)?.ok_or("Reinsert the selected Pi card.")?;
    if expected.is_empty() || crate::card::selection_key(&crate::card::status(&boot)) != Some(expected) {
        return Err("The card changed before it could be erased.".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn verify_raw_identity(target: &mut File, expected: &str) -> Result<(), String> {
    // Authorization can take time after unmounting. Verify the media through the actual open handle.
    let fat = boot_partition(target.try_clone().map_err(|e| e.to_string())?)?;
    let folder = fat.root_dir().open_dir("ratspeak-pi").map_err(|_| "The card changed.")?;
    let read = |name: &str| {
        let mut bytes = Vec::new();
        folder.open_file(name).ok()?.take(128 * 1024).read_to_end(&mut bytes).ok()?;
        serde_json::from_slice(&bytes).ok()
    };
    let status = crate::card::base_status(read("status.json"), read("settings.json"));
    if crate::card::selection_key(&status) != Some(expected) { return Err("The card changed.".into()); }
    Ok(())
}

// Cover the MBR and both GPT tables on cards with 512-byte or 4 KiB sectors.
const LAYOUT_BYTES: usize = 1 << 20;

pub fn valid_receipt(receipt: &str) -> bool {
    receipt.len() == 32 && receipt.bytes().all(|c| c.is_ascii_hexdigit())
}

fn reset_header(receipt: &str) -> Result<[u8; 4096], String> {
    if !valid_receipt(receipt) { return Err("Invalid card reset receipt.".into()); }
    // Non-bootable, with no partition entries or filesystem signature. Install overwrites it.
    let mut header = [0; 4096];
    header[..16].copy_from_slice(b"Ratspeak reset: ");
    header[16..48].copy_from_slice(receipt.as_bytes());
    Ok(header)
}

fn verify_reset(target: &mut File, capacity: u64, receipt: &str) -> Result<(), String> {
    let expected = reset_header(receipt)?;
    let mut header = [0; 4096];
    let io = |e: std::io::Error| format!("Couldn’t check the card layout: {e}");
    target.rewind().and_then(|_| target.read_exact(&mut header)).map_err(io)?;
    let mut end = [0; 4096];
    let last = capacity.checked_sub(end.len() as u64).ok_or("Invalid card size.")?;
    target.seek(SeekFrom::Start(last)).and_then(|_| target.read_exact(&mut end)).map_err(io)?;
    if header != expected || end != [0; 4096] {
        return Err("The card has changed since reset. Select it again before installing.".into());
    }
    Ok(())
}

fn reset_layout(mut target: File, capacity: u64, receipt: &str, check: MediaCheck) -> Result<(), String> {
    if capacity < 2 * LAYOUT_BYTES as u64 || capacity % SECTOR != 0 { return Err("Unknown SD card capacity.".into()); }
    let header = reset_header(receipt)?;
    let io = |e: std::io::Error| format!("Couldn’t reset the card layout: {e}");
    let mut region = vec![0; LAYOUT_BYTES];
    // Publish the receipt last, after clearing the backup table.
    target.seek(SeekFrom::Start(capacity - LAYOUT_BYTES as u64)).map_err(io)?;
    check(&target)?;
    target.write_all(&region).map_err(io)?;
    region[..header.len()].copy_from_slice(&header);
    target.rewind().map_err(io)?;
    check(&target)?;
    target.write_all(&region).map_err(io)?;
    if let Err(error) = target.sync_all() {
        if !flush_unsupported(&error) { return Err(io(error)); }
    }
    #[cfg(not(target_os = "macos"))]
    crate::disk::before_verify(&target);
    check(&target)?;
    verify_reset(&mut target, capacity, receipt)?;
    #[cfg(not(target_os = "macos"))]
    crate::disk::after_write(&target);
    Ok(())
}

#[cfg(test)]
#[test]
fn reset_only_touches_partition_metadata_and_install_checks_the_same_card() {
    let mut card = tempfile::tempfile().unwrap();
    let receipt = "aa".repeat(16);
    let capacity = 3 * LAYOUT_BYTES;
    card.write_all(&vec![0xa5; capacity]).unwrap();
    reset_layout(card.try_clone().unwrap(), capacity as u64, &receipt, &|_| Ok(())).unwrap();
    card.rewind().unwrap();
    let mut data = Vec::new();
    card.read_to_end(&mut data).unwrap();
    assert!(data[48..LAYOUT_BYTES].iter().all(|&v| v == 0));
    assert!(data[LAYOUT_BYTES..2 * LAYOUT_BYTES].iter().all(|&v| v == 0xa5));
    assert!(data[2 * LAYOUT_BYTES..].iter().all(|&v| v == 0));
    let mut image = tempfile::NamedTempFile::new().unwrap();
    image.write_all(&[0x5a; 8192]).unwrap();
    let install = |key: &str| CardWrite::InstallAfterReset(image.path(), key)
        .run_selected(card.try_clone().unwrap(), capacity as u64, "fixture", &|_| Ok("fixture".into()), &|_, _, _| {});
    assert!(install(&"bb".repeat(16)).is_err());
    install(&receipt).unwrap();
    assert!(install(&receipt).is_err());
    card.rewind().unwrap();
    let mut installed = [0; 8192];
    card.read_exact(&mut installed).unwrap();
    assert_eq!(installed, [0x5a; 8192]);
}

#[test]
fn replacement_at_the_same_path_is_never_written() {
    let source = tempfile::NamedTempFile::new().unwrap();
    fs::write(source.path(), [7u8; 8192]).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reader");
    fs::write(&path, [1u8; 8192]).unwrap();
    // The same reader now contains a second, equal-sized card.
    std::fs::rename(&path, directory.path().join("selected")).unwrap();
    fs::write(&path, [2u8; 8192]).unwrap();
    let open = || fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
    let op = CardWrite::Image(source.path());
    for id in ["replacement", ""] {
        let error = op.run_selected(open(), 8192, "first", &|_| Ok(id.into()), &|_, _, _| panic!("write started")).unwrap_err();
        assert_eq!(error, crate::card::MEDIA_CHANGED);
        assert_eq!(fs::read(&path).unwrap(), [2u8; 8192]);
    }
    op.run_selected(open(), 8192, "replacement", &|_| Ok("replacement".into()), &|_, _, _| {}).unwrap();
    assert_eq!(fs::read(&path).unwrap(), [7u8; 8192]);
}

#[test]
fn stops_writing_if_media_changes_between_blocks() {
    let source = tempfile::NamedTempFile::new().unwrap();
    fs::write(source.path(), vec![7u8; 2 * BLOCK]).unwrap();
    let target = tempfile::NamedTempFile::new().unwrap();
    target.as_file().set_len((2 * BLOCK) as u64).unwrap();
    let replaced = std::cell::Cell::new(false);
    let result = CardWrite::Image(source.path()).run_selected(target.reopen().unwrap(), (2 * BLOCK) as u64, "a",
        &|_| Ok(if replaced.get() { "b" } else { "a" }.to_string()),
        &|phase, _, _| if phase == "write" { replaced.set(true); });
    assert_eq!(result.unwrap_err(), crate::card::MEDIA_CHANGED);
    let bytes = fs::read(target.path()).unwrap();
    assert!(bytes[..BLOCK].iter().all(|b| *b == 7));
    assert!(bytes[BLOCK..].iter().all(|b| *b == 0));
}

// On Linux and Windows a raw disk can only be opened by an administrator, so the app starts a second copy of
// itself with those rights to do the writing, and follows its progress through a small file. macOS hands an
// unprivileged app the open card instead (see disk_macos.rs), so none of this is built there.

#[cfg(not(target_os = "macos"))]
fn progress_file(cache: &Path) -> PathBuf {
    cache.join("write-progress")
}

/// This program's own path and the arguments that make it the card writer.
#[cfg(not(target_os = "macos"))]
pub fn helper_arguments(op: CardWrite<'_>, device: &str, selected_media: &str, cache: &Path) -> Result<(String, Vec<String>), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    fs::write(progress_file(cache), "write 0").map_err(|e| e.to_string())?;
    let text = |path: &Path| path.to_string_lossy().to_string();
    let mut args = match op {
        CardWrite::Image(path) => vec!["--write-image".into(), text(path)],
        CardWrite::InstallAfterReset(path, receipt) => vec!["--install-after-reset".into(), text(path), receipt.into()],
        CardWrite::Reset { expected, receipt } => vec!["--reset-card".into(), expected.into(), receipt.into()],
    };
    args.extend([device.into(), selected_media.into(), text(&progress_file(cache))]);
    Ok((text(&exe), args))
}

/// Run the command that starts the writer, reporting its progress. Err carries the exit code and the reason.
#[cfg(not(target_os = "macos"))]
pub fn wait_for_helper(command: &mut Command, cache: &Path, say: Say) -> Result<(), (Option<i32>, String)> {
    let progress = progress_file(cache);
    let mut child = command.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|_| (Some(127), String::new()))?;
    let status = loop {
        let line = fs::read_to_string(&progress).unwrap_or_default();
        let mut words = line.split_whitespace();
        if let (Some(phase @ ("write" | "verify")), Some(done)) = (words.next(), words.next().and_then(|d| d.parse().ok())) {
            say(phase, done, words.next().and_then(|d| d.parse().ok()).unwrap_or(RAW_BYTES));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(400)),
            Err(e) => return Err((None, e.to_string())),
        }
    };
    let last = fs::read_to_string(&progress).unwrap_or_default();
    let _ = fs::remove_file(&progress);
    if status.success() && last.trim() == "done" {
        return Ok(());
    }
    let mut why = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut why);
    }
    let reason = match last.trim().strip_prefix("error ") {
        Some(message) => message.to_string(),
        None => format!("The card could not be written. {}", why.trim()),
    };
    Err((status.code(), reason))
}

/// `command` is the program that grants administrator rights (pkexec); this adds the writer after it.
#[cfg(target_os = "linux")]
pub fn elevated(command: &mut Command, op: CardWrite<'_>, device: &str, selected_media: &str, cache: &Path, say: Say) -> Result<(), (Option<i32>, String)> {
    let (exe, args) = helper_arguments(op, device, selected_media, cache).map_err(|e| (None, e))?;
    if tauri::utils::platform::bundle_type() == Some(tauri::utils::config::BundleType::AppImage) {
        if let Some(path) = std::env::var_os("APPIMAGE").map(PathBuf::from) {
            if !path.is_absolute() || !path.is_file() {
                return Err((None, "Reopen Ratspeak Agent from its AppImage and try again.".into()));
            }
            // The AppImage's FUSE mount is private to the desktop user.
            let helper = include_str!("../linux/appimage-card-helper.sh");
            command.args(["/bin/sh", "-c", helper, "ratspeak-card-helper"]);
            command.arg(path).arg("--appimage-extract-and-run").args(args);
            return wait_for_helper(command, cache, say);
        }
    }
    wait_for_helper(command.arg(exe).args(args), cache, say)
}

/// `--write-image <image> <device> <progress file>`: the part that runs with administrator rights. It checks
/// again that the device is a card before touching it.
#[cfg(not(target_os = "macos"))]
pub fn helper(args: &[String]) -> i32 {
    let (op, device, selected_media, progress) = match args {
        [verb, path, device, selected_media, progress] if verb == "--write-image" =>
            (CardWrite::Image(Path::new(path)), device, selected_media, progress),
        [verb, path, receipt, device, selected_media, progress] if verb == "--install-after-reset" && valid_receipt(receipt) =>
            (CardWrite::InstallAfterReset(Path::new(path), receipt), device, selected_media, progress),
        [verb, expected, receipt, device, selected_media, progress] if verb == "--reset-card" && valid_receipt(receipt) =>
            (CardWrite::Reset { expected, receipt }, device, selected_media, progress),
        _ => return 2,
    };
    // The progress file sits in the user's cache: never follow a link from there with these rights.
    let Some(file) = progress_handle(Path::new(progress)) else { return 2 };
    let note = |text: String| {
        let mut file = &file;
        let _ = file.seek(SeekFrom::Start(0)).and_then(|_| file.set_len(0)).and_then(|_| file.write_all(text.as_bytes()));
    };
    let written = crate::disk::check(device).and_then(|(_, capacity)| {
        crate::card::require_media(selected_media, crate::disk::media_id(device)?)?;
        if let CardWrite::Reset { expected, .. } = op { check_erase_target(device, expected)?; }
        let (target, _volume_locks) = crate::disk::open_raw(device)?;
        let identity = crate::disk::writer_identity(device)?;
        op.run_selected(target, capacity, selected_media, &identity, &|phase, done, total| note(format!("{phase} {done} {total}")))
    });
    match written {
        Ok(()) => {
            note("done".into());
            0
        }
        Err(message) => {
            note(format!("error {message}"));
            1
        }
    }
}

/// The existing progress file, opened for writing only when it is a regular file that is not a link.
#[cfg(not(target_os = "macos"))]
fn progress_handle(path: &Path) -> Option<File> {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Nonblocking so a FIFO planted there fails instead of hanging.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        let meta = fs::symlink_metadata(path).ok()?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return None;
        }
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return None; // a hard link to another file
        }
    }
    meta.is_file().then_some(file)
}

/// Files read back from the image's boot partition.
#[cfg(test)]
pub fn read_files(raw: &Path, paths: &[&str]) -> Vec<Vec<u8>> {
    let fat = boot_partition(File::open(raw).unwrap()).unwrap();
    let root = fat.root_dir();
    paths
        .iter()
        .map(|path| {
            let mut data = Vec::new();
            root.open_file(path).unwrap().read_to_end(&mut data).unwrap();
            data
        })
        .collect()
}
