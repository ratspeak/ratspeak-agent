use super::Engine;
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Desktop,
    Hermes,
}

impl AgentKind {
    pub(super) fn from_provider(provider: &str) -> Self {
        if provider == "hermes" {
            Self::Hermes
        } else {
            Self::Desktop
        }
    }

    pub(super) fn from_labels(labels: &Value) -> Self {
        if labels["org.ratspeak.hermes"] == "1" {
            Self::Hermes
        } else {
            Self::Desktop
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Desktop => "org.ratspeak.hermes=0",
            Self::Hermes => "org.ratspeak.hermes=1",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Slot {
    pub port: u16,
}

pub(super) const PRIMARY: Slot = Slot { port: 4243 };
pub(super) const COMPANION: Slot = Slot { port: 4244 };
impl Slot {
    pub(super) fn container(self) -> String {
        match self {
            PRIMARY => "ratspeak-desktop-agent".into(),
            COMPANION => "ratspeak-companion-agent".into(),
            _ => format!("ratspeak-agent-{}", self.port),
        }
    }
    pub(super) fn volume(self) -> String { format!("{}-data", self.container()) }
    fn named(name: &str) -> Option<Self> {
        match name {
            "ratspeak-desktop-agent" => Some(PRIMARY),
            "ratspeak-companion-agent" => Some(COMPANION),
            _ => {
                let port = name.strip_prefix("ratspeak-agent-")?.parse().ok()?;
                let slot = Self { port };
                (port > COMPANION.port && slot.container() == name).then_some(slot)
            }
        }
    }
}

fn slot_for(kind: AgentKind, primary: Option<AgentKind>, companion: Option<AgentKind>) -> Slot {
    // Earlier installers could put either provider in the original container.
    // Its volume, identity and published port must follow that provider.
    let primary_is_hermes = primary
        .map(|k| k == AgentKind::Hermes)
        .unwrap_or(companion == Some(AgentKind::Desktop));
    if primary_is_hermes == (kind == AgentKind::Hermes) {
        PRIMARY
    } else {
        COMPANION
    }
}

impl Engine {
    pub(super) fn in_slot(&self, slot: Slot) -> Self {
        Self {
            endpoint: self.endpoint.clone(),
            slot,
        }
    }

    pub(super) fn installed_slots(&self) -> Result<Vec<Slot>, String> {
        let names = self.require(&["ps","-a","--filter",&format!("label={}=1",super::LABEL),"--format","{{.Names}}"], super::DOCKER_HELP)?;
        let mut slots: Vec<_> = names.lines().filter_map(Slot::named).collect();
        slots.sort_by_key(|slot| slot.port);
        Ok(slots)
    }

    pub(super) fn pending_creation(&self, kind: AgentKind, attempt: &str) -> Result<Option<Self>, String> {
        let filter = format!("label=org.ratspeak.setup-attempt={attempt}");
        let owner = format!("label={}=1", super::LABEL);
        let provider = format!("label={}", kind.label());
        let names = self.require(&["volume","ls","--filter",&owner,"--filter",&provider,"--filter",&filter,"--format","{{.Name}}"], super::DOCKER_HELP)?;
        if names.is_empty() { return Ok(None); }
        let slot = names.strip_suffix("-data").and_then(Slot::named).ok_or("Could not identify the pending installation.")?;
        let volume = self.object("volume", &slot.volume())?.ok_or("The pending installation is missing.")?;
        if volume["Labels"]["org.ratspeak.setup-attempt"] != attempt || AgentKind::from_labels(&volume["Labels"]) != kind {
            return Err("The pending installation has changed.".into());
        }
        Ok(Some(self.in_slot(slot)))
    }

    pub(super) fn vacant(&self, kind: AgentKind) -> Result<Self,String> {
        let first = self.for_kind(kind)?.slot;
        for slot in std::iter::once(first).chain((COMPANION.port+1..=u16::MAX).map(|port| Slot{port})) {
            if self.object("container", &slot.container())?.is_none() && self.object("volume", &slot.volume())?.is_none() {
                return Ok(self.in_slot(slot));
            }
        }
        Err("No available agent ports.".into())
    }

    pub(super) fn selected(&self, kind: AgentKind, port: Option<u16>, link: Option<&str>) -> Result<Self,String> {
        let Some(link) = link else {
            return if port.is_none() { self.for_kind(kind) } else { Err("Select an agent again.".into()) };
        };
        let slots = match port {
            Some(port) if port >= PRIMARY.port => vec![Slot{port}],
            Some(_) => vec![],
            None => self.installed_slots()?,
        };
        for slot in slots {
            let engine = self.in_slot(slot);
            if let Ok(Some(row)) = engine.agent() {
                if row["link"] == link && row["provider"].as_str().map(AgentKind::from_provider) == Some(kind) {
                    return Ok(engine);
                }
            }
        }
        Err("The selected agent is no longer available.".into())
    }

    fn resident_kind(&self, slot: Slot) -> Result<Option<AgentKind>, String> {
        if let Some(c) = self.object("container", &slot.container())? {
            return Ok(Some(AgentKind::from_labels(&c["Config"]["Labels"])));
        }
        let Some(volume) = self.object("volume", &slot.volume())? else {
            return Ok(None);
        };
        let labels = &volume["Labels"];
        if labels["org.ratspeak.hermes"].is_string() {
            return Ok(Some(AgentKind::from_labels(labels)));
        }
        // A retained volume from an earlier version has no provider label.
        // Read only the provider, without starting its receiver or mounting it writable.
        let image = self.object("image", super::IMAGE)?.ok_or(
            "The saved agent volume needs its original Docker image to identify the provider.",
        )?;
        let image = image["Id"]
            .as_str()
            .ok_or("Missing agent image identity.")?;
        let provider = self.require(&["run","--rm","--pull=never","--network=none","--read-only","--security-opt","no-new-privileges","--cap-drop","ALL",
            "--mount",&format!("type=volume,source={},target=/saved,readonly,volume-nocopy",slot.volume()),
            "--entrypoint","python3",image,"-c",
            "import json,pathlib; p=pathlib.Path('/saved/config.json'); print(json.loads(p.read_text()).get('provider','local') if p.is_file() else 'empty')"],
            "Could not identify the saved agent volume. Its data was preserved.")?;
        Ok((provider != "empty").then(|| AgentKind::from_provider(&provider)))
    }

    pub(super) fn for_kind(&self, kind: AgentKind) -> Result<Self, String> {
        let primary = self.resident_kind(PRIMARY)?;
        let companion = if primary.is_none() {
            self.resident_kind(COMPANION)?
        } else {
            None
        };
        Ok(self.in_slot(slot_for(kind, primary, companion)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AgentKind::*;

    #[test]
    fn desktop_and_hermes_keep_separate_resources_in_either_install_order() {
        for (primary, companion) in [
            (None, None),
            (Some(Desktop), None),
            (None, Some(Hermes)),
            (Some(Desktop), Some(Hermes)),
            (Some(Hermes), None),
            (Some(Hermes), Some(Desktop)),
            (None, Some(Desktop)),
        ] {
            let desktop = slot_for(Desktop, primary, companion);
            let hermes = slot_for(Hermes, primary, companion);
            assert_ne!(desktop.container(), hermes.container());
            assert_ne!(desktop.volume(), hermes.volume());
            assert_ne!(desktop.port, hermes.port);
            if let Some(first) = primary {
                assert_eq!(slot_for(first, primary, companion), PRIMARY);
            }
            if let Some(second) = companion {
                assert_eq!(slot_for(second, primary, companion), COMPANION);
            }
        }
    }

    #[test]
    #[ignore = "reads the current user's local managed Docker installations"]
    fn read_installed_agents_through_their_own_cards() {
        let engine = Engine::local().unwrap();
        let desktop = engine.for_kind(Desktop).unwrap();
        let hermes = engine.for_kind(Hermes).unwrap();
        assert_ne!(desktop.slot, hermes.slot);
        for (kind, selected) in [(Desktop, desktop), (Hermes, hermes)] {
            match selected.agent().unwrap() {
                Some(agent) => {
                    let provider = agent["provider"].as_str().unwrap();
                    assert_eq!(AgentKind::from_provider(provider), kind);
                    assert_eq!(agent["lan_port"], selected.slot.port);
                    println!("{kind:?}: {provider}, {}", agent["state"]);
                }
                None => println!("{kind:?}: no receiver"),
            }
        }
    }
}
