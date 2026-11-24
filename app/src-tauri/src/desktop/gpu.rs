//! NVIDIA graphics cards a local model can run on through Docker.
use serde_json::{json, Value};
use std::{path::PathBuf, process::Command, sync::OnceLock};

/// Room left on the card for the desktop and llama.cpp's own fit margin.
const RESERVED_MB: u64 = 1536;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Card {
    pub name: String,
    pub memory_mb: u64,
}

impl Card {
    /// Graphics memory a model and its cache may use.
    pub fn budget_mb(&self) -> u64 {
        self.memory_mb.saturating_sub(RESERVED_MB)
    }
}

/// The host's largest NVIDIA card, read once from its driver's nvidia-smi.
pub(super) fn host() -> Option<Card> {
    static CARD: OnceLock<Option<Card>> = OnceLock::new();
    CARD.get_or_init(|| {
        let mut command = Command::new(smi()?);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let output = command
            .args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| largest(&String::from_utf8_lossy(&output.stdout)))?
    })
    .clone()
}

fn largest(listing: &str) -> Option<Card> {
    listing
        .lines()
        .filter_map(|line| {
            let (name, memory) = line.rsplit_once(',')?;
            Some(Card {
                name: name.trim().to_owned(),
                memory_mb: memory.trim().parse().ok()?,
            })
        })
        .filter(|card| !card.name.is_empty() && card.memory_mb > 0)
        .max_by_key(|card| card.memory_mb)
}

#[cfg(windows)]
fn smi() -> Option<PathBuf> {
    let system = PathBuf::from(std::env::var_os("SystemRoot")?).join("System32/nvidia-smi.exe");
    let legacy = std::env::var_os("ProgramW6432")
        .map(|p| PathBuf::from(p).join("NVIDIA Corporation/NVSMI/nvidia-smi.exe"));
    [Some(system), legacy].into_iter().flatten().find(|p| p.is_file())
}

#[cfg(target_os = "linux")]
fn smi() -> Option<PathBuf> {
    ["/usr/bin/nvidia-smi", "/usr/local/bin/nvidia-smi"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

#[cfg(target_os = "macos")]
fn smi() -> Option<PathBuf> {
    None
}

/// Why Docker can't give this engine's containers the card, or None when it can.
/// `info` is `docker info --format '{{.Architecture}}|{{.OperatingSystem}}|{{.KernelVersion}}'`.
pub(super) fn blocked(info: &str, endpoint: &str) -> Option<&'static str> {
    let mut fields = info.trim().splitn(3, '|');
    let (arch, system, kernel) = (fields.next()?, fields.next()?, fields.next()?);
    if !matches!(arch, "x86_64" | "amd64") {
        return Some("Docker can’t use the graphics card on this engine.");
    }
    if cfg!(windows) {
        // Docker Desktop passes NVIDIA cards through only with its WSL 2 backend, and bundles the toolkit.
        return (!kernel.contains("WSL2")).then_some("Turn on “Use the WSL 2 based engine” in Docker Desktop’s settings to use the graphics card.");
    }
    if cfg!(target_os = "linux") {
        if system.contains("Docker Desktop") || !endpoint.starts_with("unix://") {
            return Some("Docker can’t use the graphics card on this engine.");
        }
        // Docker Engine offers GPUs whenever NVIDIA's hook is on its path, as moby checks.
        let hooked = ["nvidia-container-runtime-hook", "nvidia-cdi-hook"]
            .iter()
            .any(|hook| ["/usr/bin", "/usr/local/bin"].iter().any(|dir| PathBuf::from(dir).join(hook).is_file()));
        return (!hooked).then_some("Install the NVIDIA Container Toolkit to use the graphics card.");
    }
    Some("Docker can’t use the graphics card on this engine.")
}

/// What the app shows about the card: its name and memory, and whether agents can use it.
pub(super) fn describe(card: &Card, problem: Option<&str>) -> Value {
    json!({"name": card.name, "memory_mb": card.memory_mb, "usable": problem.is_none(), "note": problem})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_largest_card_is_chosen() {
        let listing = "NVIDIA GeForce RTX 3060, 12288\nNVIDIA GeForce RTX 3090, 24576\n";
        assert_eq!(largest(listing), Some(Card { name: "NVIDIA GeForce RTX 3090".into(), memory_mb: 24576 }));
        assert_eq!(largest("No devices were found\n"), None);
        assert_eq!(largest(""), None);
    }

    #[test]
    fn engines_without_passthrough_are_explained() {
        assert!(blocked("aarch64|Docker Desktop|6.6.87-linuxkit", "unix:///x").is_some());
        if cfg!(windows) {
            assert_eq!(blocked("x86_64|Docker Desktop|6.6.87.2-microsoft-standard-WSL2", "npipe:////./pipe/docker_engine"), None);
            assert!(blocked("x86_64|Docker Desktop|6.6.87-linuxkit", "npipe:////./pipe/docker_engine").is_some());
        }
        if cfg!(target_os = "macos") {
            assert!(blocked("x86_64|Docker Desktop|6.6.87-linuxkit", "unix:///x").is_some());
        }
    }
}
