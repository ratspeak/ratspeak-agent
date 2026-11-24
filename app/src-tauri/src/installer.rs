use rand::{rngs::OsRng, Rng};
use rns_identity::{destination::Destination, identity::Identity};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Deserialize, Serialize, Clone)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub file: String,
    pub size_mb: u64,
    pub download_bytes: u64,
    pub overhead_mb: u64,
    #[serde(default)]
    pub min_generation: Option<u8>,
    pub url: String,
    pub sha256: String,
}
#[derive(Deserialize)]
pub struct Engine {
    pub tag: String,
    pub size: u64,
    pub sha256: String,
    pub url: String,
}
#[derive(Deserialize, Serialize)]
pub struct Menu {
    pub min_generation: Option<u8>,
    pub models: Vec<String>,
}
#[derive(Deserialize, Serialize)]
pub struct Catalog {
    pub rungs: Vec<Model>,
    pub menus: Vec<Menu>,
    pub os_mb: u64,
    #[serde(skip_serializing)]
    pub llama_cpp: Engine,
}
pub static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../ratspeak_pi/models.json"))
        .expect("bundled model catalog")
});
#[tauri::command]
pub fn model_catalog() -> &'static Catalog {
    &CATALOG
}
pub fn validate_preferences(s: &crate::Settings) -> Result<(), String> {
    if s.model
        .as_ref()
        .is_some_and(|id| !CATALOG.rungs.iter().any(|m| &m.id == id))
    {
        return Err("Choose a model from the list.".into());
    }
    if !s.model_options.is_empty() && !CATALOG.menus.iter().any(|menu|
        menu.models == s.model_options && s.model.as_ref().is_some_and(|id| menu.models.contains(id))) {
        return Err("Choose a model offered for this Pi.".into());
    }
    if s.offline && s.wifi.is_some() {
        return Err("Choose either Wi-Fi or offline setup.".into());
    }
    Ok(())
}
pub struct Pairing {
    identity: Identity,
    pub link: String,
    pub claim_secret: Option<String>,
    pub monitor_key: String,
}
impl Pairing {
    pub fn new(later: bool) -> Self {
        let identity = Identity::new();
        let address =
            Destination::hash_from_name_and_identity("lxmf.delivery", Some(&identity.hash));
        let link = format!(
            "lxma://{}:{}",
            hex::encode(address),
            hex::encode(identity.get_public_key())
        );
        let claim_secret = later.then(|| hex::encode(OsRng.gen::<[u8; 32]>()));
        Self {
            identity,
            monitor_key: hex::encode(OsRng.gen::<[u8; 32]>()),
            link,
            claim_secret,
        }
    }
    pub fn private_key(&self) -> Vec<u8> {
        self.identity
            .get_private_key()
            .expect("fresh identity")
            .to_vec()
    }
}
