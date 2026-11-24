//! Remove a confirmed Desktop Agent from its own managed Docker slot.
use super::{instances::AgentKind, lan, lock, Engine, Slot, IMAGE, LABEL, PRIMARY, STATE, UNOWNED};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};

const CHANGED: &str =
    "This agent or its storage changed. Reopen its management screen and try again.";

fn check_target(
    slot: Slot,
    container: &Value,
    volume: &Value,
    agent: &Value,
    link: &str,
) -> Result<(), String> {
    let labels = &container["Config"]["Labels"];
    let owns_state = container["Mounts"].as_array().is_some_and(|mounts| {
        mounts
            .iter()
            .any(|m| m["Type"] == "volume" && m["Name"] == slot.volume() && m["Destination"] == STATE)
    });
    if link.is_empty()
        || agent["link"] != link
        || !matches!(agent["provider"].as_str(), Some("local" | "venice" | "hermes"))
        || labels[LABEL] != "1"
        || Some(AgentKind::from_labels(labels)) != agent["provider"].as_str().map(AgentKind::from_provider)
        || volume["Name"] != slot.volume()
        || volume["Labels"][LABEL] != "1"
        || AgentKind::from_labels(&volume["Labels"]) != AgentKind::from_labels(labels)
        || !owns_state
    {
        return Err(CHANGED.into());
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalPlan {
    container: String,
    previous: Vec<String>,
}

struct CheckedRemoval {
    plan: RemovalPlan,
    container: Value,
    volume: Value,
}

pub(super) fn previous_name(slot: Slot, id: &str) -> Option<String> {
    if id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        Some(format!("{}-previous-{}", slot.container(), &id[..12]))
    } else {
        None
    }
}

fn stopped_previous(slot: Slot, kind: AgentKind, id: &str, container: &Value) -> bool {
    let Some(name) = previous_name(slot, id) else {
        return false;
    };
    let config = &container["Config"];
    let labels = &config["Labels"];
    let Some(mounts) = container["Mounts"].as_array() else {
        return false;
    };
    container["Id"] == id
        && container["Name"] == format!("/{name}")
        && container["State"]["Status"] == "exited"
        && container["State"]["Running"] == false
        && labels[LABEL] == "1"
        && AgentKind::from_labels(labels) == kind
        && config["Image"] == IMAGE
        && config["Entrypoint"]
            == serde_json::json!(["python3", "-m", "ratspeak_pi.desktop", "serve"])
        && mounts.len() == 1
        && mounts[0]["Type"] == "volume"
        && mounts[0]["Name"] == slot.volume()
        && mounts[0]["Destination"] == STATE
}

impl Engine {
    fn checked_removal(&self, link: &str) -> Result<CheckedRemoval, String> {
        let slot = self.slot;
        let container = self.object("container", &slot.container())?.ok_or(CHANGED)?;
        let volume = self.object("volume", &slot.volume())?.ok_or(CHANGED)?;
        let agent = self.agent()?.ok_or(CHANGED)?;
        check_target(slot, &container, &volume, &agent, link)?;
        let id = container["Id"].as_str().ok_or(CHANGED)?;
        let previous = self.previous_copies(id, &container)?;
        Ok(CheckedRemoval {
            plan: RemovalPlan {
                container: id.to_owned(),
                previous,
            },
            container,
            volume,
        })
    }

    /// Stopped earlier copies of the receiver; any other container using the storage blocks removal.
    fn previous_copies(&self, id: &str, container: &Value) -> Result<Vec<String>, String> {
        let slot = self.slot;
        let users = self.require(
            &[
                "ps",
                "-a",
                "--no-trunc",
                "--filter",
                &format!("volume={}", slot.volume()),
                "--format",
                "{{.ID}}\t{{.Names}}",
            ],
            "Could not check which containers use this agent’s storage.",
        )?;
        let mut previous = Vec::new();
        let mut found = false;
        for line in users.lines() {
            let (user, name) = line.split_once('\t').ok_or(CHANGED)?;
            if user == id && name == slot.container() {
                found = true;
                continue;
            }
            let blocked = || {
                format!("The Docker container “{name}” also uses this agent’s storage. Detach it from the volume before retrying.")
            };
            if previous_name(slot, user).as_deref() != Some(name) {
                return Err(blocked());
            }
            let candidate = self.object("container", user)?.ok_or(CHANGED)?;
            if !stopped_previous(slot, AgentKind::from_labels(&container["Config"]["Labels"]), user, &candidate) {
                return Err(blocked());
            }
            previous.push(user.to_owned());
        }
        if !found {
            return Err(CHANGED.into());
        }
        previous.sort_unstable();
        Ok(previous)
    }

    /// Removes a receiver whose settings can't be read, with its storage, so setup can start over.
    fn discard(&self, kind: AgentKind) -> Result<(), String> {
        let slot = self.slot;
        let container = self.object("container", &slot.container())?.ok_or(CHANGED)?;
        let labels = &container["Config"]["Labels"];
        if labels[LABEL] != "1" || AgentKind::from_labels(labels) != kind {
            return Err(CHANGED.into());
        }
        let volume = self.object("volume", &slot.volume())?;
        if volume.as_ref().is_some_and(|v| v["Labels"][LABEL] != "1") {
            return Err(UNOWNED.into());
        }
        let id = container["Id"].as_str().ok_or(CHANGED)?;
        let previous = if volume.is_some() { self.previous_copies(id, &container)? } else { Vec::new() };
        for previous in &previous {
            self.require_explained(&["rm", "--force", previous], "Could not remove the agent’s previous copy")?;
        }
        self.require_explained(&["rm", "--force", id], "Could not remove the agent")?;
        if volume.is_some() {
            self.require_explained(&["volume", "rm", &slot.volume()], "Docker kept the agent’s storage")?;
        }
        Ok(())
    }

    fn remove_desktop(&self, link: &str, confirmed: &RemovalPlan) -> Result<(), String> {
        let CheckedRemoval {
            plan,
            container,
            volume,
        } = self.checked_removal(link)?;
        if &plan != confirmed {
            return Err(CHANGED.into());
        }
        let id = plan.container.as_str();
        let image = container["Image"].as_str().ok_or(CHANGED)?;
        let slot = self.slot;
        // Only immutable IDs from the freshly checked and confirmed plan reach Docker.
        // Non-forced removal also refuses a previous receiver that has since started.
        for previous in &plan.previous {
            self.require(
                &["rm", previous],
                "Could not remove the previous copy. Reopen Remove agent and try again.",
            )?;
        }
        if container["State"]["Running"] == true {
            self.require(&["stop", id], "Could not stop the agent. Try again.")?;
        }
        self.require(
            &["rm", id],
            "Could not remove the stopped agent. Try again.",
        )?;
        if self
            .require(
                &["volume", "rm", &slot.volume()],
                "Could not remove the agent’s storage.",
            )
            .is_err()
        {
            // Reattach only the original retained volume so the user can retry from management.
            match self.object("volume", &slot.volume())? {
                None => return Ok(()),
                Some(retained) if retained == volume => {
                    self.create_receiver(image, AgentKind::from_labels(&container["Config"]["Labels"]), super::uses_gpu(&container)).map_err(|_| "The container was removed, but Docker kept the agent’s storage. Reopen Desktop Agent setup to reconnect it.")?;
                }
                _ => return Err(CHANGED.into()),
            }
            return Err(
                "Docker kept the agent’s storage. The agent is stopped. Try Remove agent again."
                    .into(),
            );
        }
        Ok(())
    }
}

#[tauri::command(async)]
pub fn desktop_removal_plan(app: AppHandle, link: String, kind: AgentKind, port: Option<u16>) -> Result<RemovalPlan, String> {
    let _guard = lock(&app)?;
    Ok(Engine::local()?
        .selected(kind,port,Some(&link))?
        .checked_removal(&link)?
        .plan)
}

#[tauri::command(async)]
pub fn desktop_remove_agent(app: AppHandle, link: String, plan: RemovalPlan, kind: AgentKind, port: Option<u16>) -> Result<(), String> {
    let _guard = lock(&app)?;
    let engine = Engine::local()?.selected(kind,port,Some(&link))?;
    engine.remove_desktop(&link, &plan)?;
    forget(&app, &engine)
}

/// For an agent whose settings can't be read, which Remove agent cannot confirm by its link.
#[tauri::command(async)]
pub fn desktop_discard_agent(app: AppHandle, kind: AgentKind, port: u16) -> Result<(), String> {
    let _guard = lock(&app)?;
    if port < PRIMARY.port {
        return Err(CHANGED.into());
    }
    let engine = Engine::local()?.in_slot(Slot { port });
    if matches!(engine.agent(), Ok(Some(_))) {
        return Err("This agent works again. Manage it from its card.".into());
    }
    engine.discard(kind)?;
    forget(&app, &engine)
}

fn forget(app: &AppHandle, engine: &Engine) -> Result<(), String> {
    if let Ok(base) = app.path().app_local_data_dir() {
        let dir = base.join("agent-lan");
        super::hermes::forget(&dir, &engine.slot.volume());
        // The shared LAN helper also serves Hermes. Leave it active while any receiver exists.
        if engine.installed_slots()?.is_empty()
            && std::fs::read_to_string(dir.join("engine")).ok().as_ref() == Some(&engine.endpoint)
        {
            let _ = lan::write_changed(&dir.join("enabled"), b"no");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::instances::{COMPANION, PRIMARY};
    use serde_json::json;

    #[test]
    fn previous_copy_requires_the_stopped_managed_receiver_of_this_slot() {
        let slot = PRIMARY;
        let id = "abc123".repeat(10) + "7890";
        let name = format!("/{}-previous-{}", slot.container(), &id[..12]);
        let mount = json!({"Type":"volume","Name":slot.volume(),"Destination":STATE});
        let saved = json!({"Id":id,"Name":name,"Config":{
            "Image":IMAGE, "Labels":{(LABEL):"1"},
            "Entrypoint":["python3","-m","ratspeak_pi.desktop","serve"]},
            "State":{"Running":false,"Status":"exited"}, "Mounts":[mount]});
        let desktop = AgentKind::Desktop;
        assert!(stopped_previous(slot, desktop, &id, &saved));
        assert!(!stopped_previous(slot, AgentKind::Hermes, &id, &saved));
        assert!(!stopped_previous(COMPANION, desktop, &id, &saved));
        assert!(!stopped_previous(slot, desktop, &id[..12], &saved));
        let mut hermes = saved.clone();
        hermes["Config"]["Labels"] = json!({(LABEL):"1","org.ratspeak.hermes":"1"});
        assert!(stopped_previous(slot, AgentKind::Hermes, &id, &hermes));
        for (pointer, changed) in [
            ("/State/Status", json!("running")),
            ("/State/Running", json!(true)),
            ("/Id", json!("other")),
            ("/Name", json!("/ratspeak-desktop-agent-previous-user")),
            ("/Config/Labels", json!({"org.ratspeak.hermes":"0"})),
            (
                "/Config/Labels",
                json!({(LABEL):"1","org.ratspeak.hermes":"1"}),
            ),
            ("/Config/Image", json!("other")),
            ("/Config/Entrypoint", json!(["sleep"])),
            ("/Mounts/0/Type", json!("bind")),
            ("/Mounts/0/Destination", json!("/other")),
            ("/Mounts/0/Name", json!(COMPANION.volume())),
            ("/Mounts", json!([mount, mount])),
        ] {
            let mut other = saved.clone();
            *other.pointer_mut(pointer).unwrap() = changed;
            assert!(!stopped_previous(slot, desktop, &id, &other), "{pointer}");
        }
    }

    #[test]
    fn removal_requires_the_selected_identity_and_owned_desktop_volume_in_either_slot() {
        for slot in [PRIMARY, COMPANION] {
            let agent = json!({"provider":"venice","link":"test-identity"});
            let labels = json!({(LABEL):"1"});
            let volume = json!({"Name":slot.volume(),"Labels":labels});
            let container = json!({"Config":{"Labels":labels},"Mounts":[{"Destination":STATE,"Type":"volume","Name":slot.volume()}]});
            assert!(check_target(slot, &container, &volume, &agent, "test-identity").is_ok());
            assert!(check_target(slot, &container, &volume, &agent, "old-identity").is_err());
            for (field, value) in [
                ("Name", json!("another-volume")),
                ("Labels", json!({})),
                ("Labels", json!({(LABEL):"1","org.ratspeak.hermes":"1"})),
            ] {
                let mut other = volume.clone();
                other[field] = value;
                assert!(check_target(slot, &container, &other, &agent, "test-identity").is_err());
            }
            for (field, value) in [
                ("Mounts", json!([])),
                (
                    "Config",
                    json!({"Labels":{(LABEL):"1","org.ratspeak.hermes":"1"}}),
                ),
            ] {
                let mut other = container.clone();
                other[field] = value;
                assert!(check_target(slot, &other, &volume, &agent, "test-identity").is_err());
            }
            let hermes = json!({"provider":"hermes","link":"test-identity"});
            assert!(check_target(slot, &container, &volume, &hermes, "test-identity").is_err());
        }
    }
}
