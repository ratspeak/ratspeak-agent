//! Move managed receivers onto the runtime and container options this Setup embeds. Agent data stays in each volume.
use super::{removal::previous_name, AgentKind, Engine, AMD64, DOCKERFILE, DOCKER_HELP, IMAGE, LABEL, RECEIVER_OPTIONS};
use crate::RELEASE;
use sha2::{Digest, Sha256};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader},
    process::Stdio,
    sync::{mpsc, OnceLock},
    thread,
};
use tempfile::tempdir;

const RUNTIME: &str = "org.ratspeak.runtime";
const FAILED: &str = "Could not update the agent. Its data was preserved; reopen Ratspeak Agent to try again.";

fn digest() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();
    DIGEST.get_or_init(|| {
        let mut hash = Sha256::new();
        let options = RECEIVER_OPTIONS.join(" ");
        for part in [RELEASE, DOCKERFILE.as_bytes(), AMD64, options.as_bytes()] {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part);
        }
        hex::encode(hash.finalize())
    })
}

/// "#7 [3/5] RUN …" from BuildKit or "Step 3/8 : RUN …" from the older builder.
fn build_step(line: &str) -> Option<(u32, u32)> {
    let text = line.strip_prefix("Step ").or_else(|| line.split_once('[').map(|(_, rest)| rest))?;
    let (step, rest) = text.split_once('/')?;
    let steps: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let (step, steps) = (step.trim().parse().ok()?, steps.parse().ok()?);
    (step >= 1 && step <= steps).then_some((step, steps))
}

impl Engine {
    /// The receiver image for this release, built only when missing or from another release; reports build steps.
    pub(super) fn current_image_reporting(&self, progress: &dyn Fn(Value)) -> Result<String, String> {
        if let Some(image) = self.object("image", IMAGE)? {
            if image["Config"]["Labels"][RUNTIME] == digest() {
                return image["Id"].as_str().map(str::to_owned).ok_or_else(|| "Missing agent image identity.".into());
            }
        }
        let context = tempdir().map_err(|e| e.to_string())?;
        for (name, bytes) in [("release.tar.gz", RELEASE), ("Dockerfile", DOCKERFILE.as_bytes()), ("desktop-native.gz", AMD64)] {
            fs::write(context.path().join(name), bytes).map_err(|e| e.to_string())?;
        }
        let owner = format!("{LABEL}=1");
        let runtime = format!("{RUNTIME}={}", digest());
        let path = context.path().to_str().ok_or("Invalid temporary folder.")?;
        let mut child = self.cmd().args(["build", "--label", &owner, "--label", &runtime, "--tag", IMAGE, path])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|_| DOCKER_HELP)?;
        // Without a terminal BuildKit reports plainly on stderr, and the older builder (Docker CLIs
        // without buildx, which reject --progress) on stdout; read both.
        let (lines, received) = mpsc::channel();
        for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
                     child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)].into_iter().flatten() {
            let lines = lines.clone();
            thread::spawn(move || BufReader::new(pipe).lines().map_while(Result::ok).for_each(|line| { let _ = lines.send(line); }));
        }
        drop(lines);
        let mut reached = 0;
        for line in received {
            if let Some((step, steps)) = build_step(&line).filter(|(step, _)| *step > reached) {
                reached = step;
                progress(json!({"text":"Building the agent container…","step":step,"steps":steps}));
            }
        }
        if !child.wait().map_err(|_| DOCKER_HELP)?.success() {
            return Err("Could not build the agent container. Check Docker’s internet connection and free space.".into());
        }
        self.object("image", IMAGE)?
            .and_then(|image| image["Id"].as_str().map(str::to_owned))
            .ok_or_else(|| "Missing agent image identity.".into())
    }

    /// Recreates this slot's receiver from the current image when it runs an older one.
    pub(super) fn update_receiver(&self) -> Result<bool, String> {
        self.update_receiver_reporting(&|_| {})
    }

    /// Like `update_receiver`, reporting any image build's steps.
    pub(super) fn update_receiver_reporting(&self, progress: &dyn Fn(Value)) -> Result<bool, String> {
        let name = self.slot.container();
        let Some(old) = self.object("container", &name)? else { return Ok(false) };
        let image = self.current_image_reporting(progress)?;
        if old["Image"] == image.as_str() {
            return Ok(false);
        }
        let id = old["Id"].as_str().ok_or("Missing container identity.")?;
        let parked = previous_name(self.slot, id).ok_or("Missing container identity.")?;
        let running = old["State"]["Running"] == true;
        let kind = AgentKind::from_labels(&old["Config"]["Labels"]);
        let gpu = super::uses_gpu(&old);
        if running {
            self.require(&["stop", id], FAILED)?;
        }
        self.require(&["rename", id, &parked], FAILED)?;
        let fresh = self.create_receiver(IMAGE, kind, gpu)
            .and_then(|_| if running { self.require(&["start", &name], FAILED).map(drop) } else { Ok(()) });
        if let Err(error) = fresh {
            // Put the previous receiver back exactly as it was.
            let _ = self.call(&["rm", "--force", &name]);
            let _ = self.call(&["rename", id, &name]);
            if running {
                let _ = self.call(&["start", id]);
            }
            return Err(error);
        }
        // A parked copy left by a failed removal is still recognised and removed with the agent.
        let _ = self.call(&["rm", id]);
        // Docker keeps an image another container still uses.
        if let Some(previous) = old["Image"].as_str() {
            if matches!(self.object("image", previous), Ok(Some(_))) {
                let _ = self.call(&["rmi", previous]);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_digest_names_the_embedded_release() {
        let digest = super::digest();
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(digest, super::digest());
    }

    #[test]
    fn build_steps_are_read_from_either_builder() {
        use super::build_step;
        assert_eq!(build_step("#7 [3/5] RUN tar xzf /tmp/release.tar.gz"), Some((3, 5)));
        assert_eq!(build_step("#5 [internal] load metadata for docker.io/library/debian:trixie-slim"), None);
        assert_eq!(build_step("Step 2/8 : RUN apt-get update"), Some((2, 8)));
        assert_eq!(build_step("#9 DONE 0.1s"), None);
    }
}
