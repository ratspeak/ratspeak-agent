//! Host-wide physical memory for locally hosted models. Docker's VM has a separate memory budget.
use serde_json::{json, Value};

pub(super) fn reading() -> Option<Value> {
    let (used, total) = physical()?;
    (total > 0 && used <= total).then(|| json!({"used":used, "total":total}))
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn counter(input: &str, name: &str) -> Option<u64> {
    input
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(label, _)| *label == name)?
        .1
        .split_whitespace()
        .next()?
        .trim_end_matches('.')
        .parse()
        .ok()
}

#[cfg(any(target_os = "macos", test))]
fn mac_used(input: &str) -> Option<u64> {
    let size: u64 = input
        .lines()
        .next()?
        .split_once("page size of ")?
        .1
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    // Application pages excluding purgeable memory, plus wired and physically compressed pages.
    let app = counter(input, "Anonymous pages")?.checked_sub(counter(input, "Pages purgeable")?)?;
    app.checked_add(counter(input, "Pages wired down")?)?
        .checked_add(counter(input, "Pages occupied by compressor")?)?
        .checked_mul(size)
}

#[cfg(target_os = "macos")]
fn physical() -> Option<(u64, u64)> {
    let output = std::process::Command::new("/usr/bin/vm_stat")
        .output()
        .ok()?;
    let used = mac_used(std::str::from_utf8(&output.stdout).ok()?)?;
    Some((used, super::host_ram() << 20))
}

#[cfg(target_os = "linux")]
fn physical() -> Option<(u64, u64)> {
    let input = std::fs::read_to_string("/proc/meminfo").ok()?;
    let total = counter(&input, "MemTotal")?;
    Some((
        total.checked_sub(counter(&input, "MemAvailable")?)? * 1024,
        total * 1024,
    ))
}

#[cfg(windows)]
fn physical() -> Option<(u64, u64)> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut info = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // The length and address identify a writable MEMORYSTATUSEX for this synchronous call.
    if unsafe { GlobalMemoryStatusEx(&mut info) } == 0 {
        return None;
    }
    Some((
        info.ullTotalPhys.checked_sub(info.ullAvailPhys)?,
        info.ullTotalPhys,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mac_sample_excludes_purgeable_and_counts_compressed_physical_pages() {
        let input = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nAnonymous pages: 300.\nPages purgeable: 10.\nPages wired down: 20.\nPages occupied by compressor: 90.\nPages stored in compressor: 400.";
        assert_eq!(mac_used(input), Some(400 * 16384));
        assert_eq!(mac_used("unavailable"), None);
    }
}
