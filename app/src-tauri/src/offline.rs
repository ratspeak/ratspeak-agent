use crate::{
    image::Say,
    installer::CATALOG,
    network::curl,
    Settings,
};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    thread, time::Duration,
};

fn verified(path: &Path, expected: &str) -> bool {
    let Ok(mut f) = fs::File::open(path) else {
        return false;
    };
    let mut hash = Sha256::new();
    io::copy(&mut f, &mut hash).is_ok() && hex::encode(hash.finalize()) == expected
}

#[derive(serde::Deserialize, serde::Serialize, Clone)]
pub struct Library {
    pub label: String,
    pub command: String,
    pub example: String,
    pub file: String,
    pub url: String,
    pub sha256: String,
    pub size_mb: u64,
    pub download_bytes: u64,
}

/// The offline libraries, in catalog order.
pub fn libraries() -> Vec<(String, Library)> {
    const RAW: &str = include_str!("../../../ratspeak_pi/resources.json");
    let catalog: serde_json::Map<String, serde_json::Value> = serde_json::from_str(RAW).expect("bundled resources");
    let mut found: Vec<_> = catalog.into_iter().map(|(id, v)| (id, serde_json::from_value(v).expect("bundled library"))).collect();
    // The map is sorted by name; the file's own order is the order people see.
    found.sort_by_key(|(id, _): &(String, Library)| RAW.find(&format!("\"{id}\": {{")));
    found
}

#[derive(serde::Deserialize)]
pub struct Pack {
    pub file: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(serde::Deserialize)]
pub struct Language {
    pub label: String,
    pub size_mb: u64,
    pub packs: Vec<Pack>,
}

/// Offline translation languages in catalog order, and the Pi's (aarch64) engine wheels.
pub fn translations() -> (Vec<(String, Language)>, Vec<Pack>) {
    const RAW: &str = include_str!("../../../ratspeak_pi/translations.json");
    #[derive(serde::Deserialize)]
    struct Catalog {
        runtime: std::collections::BTreeMap<String, Vec<Pack>>,
        languages: serde_json::Map<String, serde_json::Value>,
    }
    let mut catalog: Catalog = serde_json::from_str(RAW).expect("bundled translations");
    let mut found: Vec<_> = catalog.languages.into_iter().map(|(id, v)| (id, serde_json::from_value(v).expect("bundled language"))).collect();
    found.sort_by_key(|(id, _): &(String, Language)| RAW.find(&format!("\"{id}\": {{")));
    (found, catalog.runtime.remove("aarch64").expect("aarch64 engine"))
}

#[derive(serde::Deserialize)]
pub struct VoiceEngine {
    pub size_mb: u64,
    pub files: Vec<Pack>,
    #[serde(default)]
    pub runtime: std::collections::BTreeMap<String, Pack>,
}

/// Speech-to-text engines for voice messages, by name (`ratspeak_pi/voice.py` picks one per computer).
pub fn voice_engines() -> std::collections::BTreeMap<String, VoiceEngine> {
    #[derive(serde::Deserialize)]
    struct Catalog {
        engines: std::collections::BTreeMap<String, VoiceEngine>,
    }
    serde_json::from_str::<Catalog>(include_str!("../../../ratspeak_pi/voice.json")).expect("bundled voice engines").engines
}

/// Vosk's ARM library links libatomic, which Raspberry Pi OS Lite may lack.
const LIBATOMIC: (&str, &str, u64) = ("libatomic1_14.2.0-19_arm64.deb",
    "7ccff90234484bf21ae5ecb79d25b6a84b2f4038f65dd139085b2e3a937cd8dc", 10_128);

/// The size of the CTranslate2 engine wheels for one processor.
fn engine_bytes(arch: &str) -> u64 {
    let raw: serde_json::Value = serde_json::from_str(include_str!("../../../ratspeak_pi/translations.json")).expect("bundled translations");
    raw["runtime"][arch].as_array().expect("engine wheels").iter().filter_map(|w| w["bytes"].as_u64()).sum()
}

/// The extras a settings feature map turns on, in catalog order.
pub fn chosen(features: &std::collections::BTreeMap<String, bool>) -> Vec<String> {
    let ids = std::iter::once("voice".to_string()).chain(libraries().into_iter().map(|(id, _)| id))
        .chain(translations().0.into_iter().map(|(id, _)| id));
    ids.filter(|id| features.get(id) == Some(&true)).collect()
}

#[tauri::command]
pub fn library_catalog() -> Vec<serde_json::Value> {
    let (languages, engine) = translations();
    let engine_mb = engine.iter().map(|w| w.bytes).sum::<u64>().div_ceil(1_000_000);
    // A desktop agent's container runs on this computer's processor, so its engine wheels match it.
    let desktop_mb = if cfg!(target_arch = "x86_64") { engine_bytes("x86_64") } else { engine_bytes("aarch64") }.div_ceil(1_000_000);
    let voice = voice_engines();
    let sizes = serde_json::json!({"desktop":voice["whisper-base"].size_mb,"pi":voice["whisper-tiny"].size_mb,
        "pi_small":voice["vosk"].size_mb});
    std::iter::once(serde_json::json!({"id":"voice","label":"Voice messages","command":"","kind":"voice",
        "size_mb":voice["whisper-tiny"].size_mb,"sizes":sizes,"engine_mb":engine_mb,"engine_mb_desktop":desktop_mb}))
        .chain(libraries().into_iter().map(|(id, l)| serde_json::json!({"id":id,"label":l.label,"command":l.command,
        "example":l.example,"size_mb":l.size_mb})))
        .chain(languages.into_iter().map(|(id, l)| serde_json::json!({"id":id,"label":l.label,"command":"/translate",
            "example":format!("/translate {} Where is the station?", l.label),"size_mb":l.size_mb,"kind":"translation",
            "engine_mb":engine_mb,"engine_mb_desktop":desktop_mb})))
        .collect()
}

/// Kiwix sends each location to one mirror, which can crawl. Its metalink lists the others: time a short
/// sample from each and try the fastest first. The pinned checksum still decides what is kept.
fn sources(url: &str) -> Vec<String> {
    let mut found = Vec::new();
    if url.starts_with("https://download.kiwix.org/") {
        let meta = curl().args(["--disable", "--fail", "--silent", "--location", "--proto", "=https", "--max-time", "20",
            "--max-filesize", "4000000"]).arg(format!("{url}.meta4")).output();
        let text = meta.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        for piece in text.split("<url ").skip(1).filter(|p| p.contains("priority=")) {
            if let Some(link) = piece.split_once('>').and_then(|(_, rest)| rest.split_once("</url>")).map(|(link, _)| link.trim()) {
                if link.starts_with("https://") && !link.contains(['"', '<', ' ']) && found.len() < 8 {
                    found.push(link.to_string());
                }
            }
        }
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let speeds: Vec<f64> = thread::scope(|scope| {
            let probes: Vec<_> = found.iter().map(|mirror| scope.spawn(move || {
                curl().args(["--disable", "--fail", "--silent", "--proto", "=https", "--range", "0-4194303", "--max-time", "6",
                    "--output", null, "--write-out", "%{speed_download}"]).arg(mirror).output()
                    .ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok()).unwrap_or(0.0)
            })).collect();
            probes.into_iter().map(|p| p.join().unwrap_or(0.0)).collect()
        });
        let mut ranked: Vec<_> = found.into_iter().zip(speeds).collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        found = ranked.into_iter().map(|(link, _)| link).collect();
    }
    found.push(url.to_string());
    found
}

fn cached(dir: &Path, file: &str, url: &str, sha: &str, report: impl Fn(u64)) -> Result<PathBuf, String> {
    let path = dir.join(file);
    if verified(&path, sha) {
        return Ok(path);
    }
    let temporary = tempfile::NamedTempFile::new_in(dir)
        .map_err(|e| e.to_string())?
        .into_temp_path();
    let candidates = sources(url);
    let mut status = None;
    for (number, source) in candidates.iter().enumerate() {
        // A source under 50 KB/s for a minute hands over and the next resumes; the last has no floor.
        let floor = if number + 1 < candidates.len() { "51200" } else { "1024" };
        let mut child = curl()
            .args(["--disable", "--fail", "--location", "--proto", "=https", "--proto-redir", "=https", "--silent",
                "--retry", "3", "--connect-timeout", "12", "--continue-at", "-", "--speed-limit", floor,
                "--speed-time", if number + 1 < candidates.len() { "60" } else { "90" }, "--output"])
            .arg(&temporary)
            .arg(source)
            .spawn()
            .map_err(|_| "Could not start the offline download.")?;
        let done = loop {
            if let Some(done) = child.try_wait().map_err(|e| e.to_string())? { break done; }
            report(fs::metadata(&temporary).map(|m| m.len()).unwrap_or(0));
            thread::sleep(Duration::from_millis(250));
        };
        status = Some(done);
        if done.success() {
            break;
        }
    }
    if !status.is_some_and(|s| s.success()) {
        return Err(format!(
            "Couldn’t download {file}. Check this computer’s internet connection."
        ));
    }
    if !verified(&temporary, sha) {
        return Err(format!(
            "The download of {file} failed its checksum. Try again."
        ));
    }
    temporary.persist(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

type Asset = (String, String, String, u64);

struct Plan {
    assets: Vec<Asset>,
    download_bytes: u64,
    card_bytes: u64,
}

fn plan(model: Option<&str>, wanted_libraries: &[String]) -> Result<Plan, String> {
    let rungs = &CATALOG.rungs;
    let chosen = match model {
        Some(id) => rungs.iter().position(|m| m.id == id).ok_or("Choose a model from the list.")?,
        None => rungs.len() - 1, // The Pi chooses from the full ladder on first boot.
    };
    let engine = &CATALOG.llama_cpp;
    let mut wanted = vec![(
        format!("llama-{}.tar.gz", engine.tag),
        engine.url.clone(),
        engine.sha256.clone(),
        engine.size,
    )];
    // A named model is final. Only automatic setup needs all candidates available at boot.
    let candidates = if model.is_some() { &rungs[chosen..=chosen] } else { &rungs[..] };
    for m in candidates {
        wanted.push((m.file.clone(), m.url.clone(), m.sha256.clone(), m.download_bytes));
    }
    for (file, sha, bytes) in [
        (
            "libqrencode4_4.1.1-2_arm64.deb",
            "8a52834e626c6f9c662005e4db5ee45043e1ed50f6a7833ff7e18c2f3a8b4edb",
            38_736,
        ),
        (
            "qrencode_4.1.1-2_arm64.deb",
            "5d2832d42f85bb915ee6c5038c1dfa3e94035cbb54816e5173bab8362dfb4291",
            41_048,
        ),
    ] {
        wanted.push((
            file.into(),
            format!("https://deb.debian.org/debian/pool/main/q/qrencode/{file}"),
            sha.into(),
            bytes,
        ));
    }
    // /sun and /moon. Debian's package is architecture independent.
    wanted.push((
        "python3-astral_3.2-2_all.deb".into(),
        "https://deb.debian.org/debian/pool/main/a/astral/python3-astral_3.2-2_all.deb".into(),
        "552ee2e7fa100b647df799d00f41c38232b4eba050ab5426df681edeefbf8c63".into(),
        30_508,
    ));
    let mut installed_bytes = rungs[chosen].download_bytes;
    let chosen: Vec<_> = libraries().into_iter().filter(|(id, _)| wanted_libraries.contains(id)).collect();
    let (languages, engine) = translations();
    let languages: Vec<_> = languages.into_iter().filter(|(id, _)| wanted_libraries.contains(id)).collect();
    let voice = wanted_libraries.iter().any(|id| id == "voice");
    if chosen.len() + languages.len() + usize::from(voice) != wanted_libraries.len() {
        return Err("Choose libraries from the list.".into());
    }
    if voice {
        // The Pi picks Vosk or Whisper tiny from its memory at first boot, so the card carries both.
        let engines = voice_engines();
        let vosk = &engines["vosk"];
        for file in engines["whisper-tiny"].files.iter().chain(&vosk.files).chain(vosk.runtime.get("aarch64")) {
            installed_bytes += file.bytes;
            wanted.push((file.file.clone(), file.url.clone(), file.sha256.clone(), file.bytes));
        }
        let (file, sha, bytes) = LIBATOMIC;
        wanted.push((file.into(), format!("https://deb.debian.org/debian/pool/main/g/gcc-14/{file}"), sha.into(), bytes));
    }
    if !languages.is_empty() || voice {
        for wheel in &engine {
            installed_bytes += 4 * wheel.bytes; // unpacked
            wanted.push((wheel.file.clone(), wheel.url.clone(), wheel.sha256.clone(), wheel.bytes));
        }
    }
    for pack in languages.iter().flat_map(|(_, l)| &l.packs) {
        installed_bytes += pack.bytes;
        wanted.push((pack.file.clone(), pack.url.clone(), pack.sha256.clone(), pack.bytes));
    }
    for (_, library) in &chosen {
        installed_bytes += library.download_bytes;
        wanted.push((library.file.clone(), library.url.clone(), library.sha256.clone(), library.download_bytes));
    }
    if !chosen.is_empty() {
        wanted.push(("wiki-reader.whl".into(),
            "https://files.pythonhosted.org/packages/cb/2b/4a36963684f181b023cfd9be3db930ae817a437a9bac788d8b2e89bc1c31/libzim-4.1.0-cp311-abi3-manylinux_2_27_aarch64.manylinux_2_28_aarch64.whl".into(),
            "791fccae1b3364c310320326dfea0277bc4677cb72719472ab9591750dd4a5e8".into(), 19_550_557));
    }
    let total: u64 = wanted.iter().map(|(_, _, _, bytes)| bytes).sum();
    // First boot copies the active GGUF and optional ZIM from FAT to Linux.
    // Leave space for FAT alignment, the unpacked engine and the running Pi.
    let card_bytes = crate::image::RAW_BYTES + total + installed_bytes + (512 << 20);
    Ok(Plan { assets: wanted, download_bytes: total, card_bytes })
}

#[derive(serde::Serialize)]
pub struct Requirements {
    download_bytes: u64,
    pub card_bytes: u64,
}

#[tauri::command]
pub fn offline_requirements(model: Option<String>, libraries: Vec<String>) -> Result<Requirements, String> {
    let plan = plan(model.as_deref(), &libraries)?;
    Ok(Requirements { download_bytes: plan.download_bytes, card_bytes: plan.card_bytes })
}

pub fn prepare(cache: &Path, s: &Settings, say: Say) -> Result<Vec<PathBuf>, String> {
    let plan = plan(s.model.as_deref(), &chosen(&s.features))?;
    let base = cache.join("offline");
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let total = plan.download_bytes;
    let mut files = Vec::new();
    let mut ready = 0;
    say("offline", 0, total);
    for (file, url, sha, bytes) in plan.assets {
        files.push(cached(&base, &file, &url, &sha, |done| say("offline", ready + done.min(bytes), total))?);
        ready += bytes;
        say("offline", ready, total);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Network: cargo test -- --ignored kiwix_mirrors
    #[test]
    #[ignore]
    fn kiwix_mirrors_are_ranked_and_the_original_link_comes_last() {
        let url = "https://download.kiwix.org/zim/wiktionary/wiktionary_en-simple_all_nopic_2026-10.zim";
        let found = sources(url);
        println!("{found:#?}");
        assert!(found.len() > 2);
        assert_eq!(found.last().map(String::as_str), Some(url));
        assert!(found.iter().all(|link| link.starts_with("https://")));
    }

    #[test]
    fn offline_stages_the_explicit_model_or_all_automatic_candidates() {
        let all = &CATALOG.rungs;
        for chosen in all {
            let plan = plan(Some(&chosen.id), &[]).unwrap();
            let files: Vec<_> = plan.assets.iter().filter(|a| a.0.ends_with(".gguf")).collect();
            assert_eq!(files.len(), 1);
            assert_eq!((&files[0].0, files[0].3), (&chosen.file, chosen.download_bytes));
            assert!(plan.card_bytes > crate::image::RAW_BYTES + plan.download_bytes + chosen.download_bytes);
        }
        assert!(plan(Some("unknown"), &[]).is_err());
        let auto = plan(None, &[]).unwrap();
        assert!(all.iter().all(|m| auto.assets.iter().any(|a| a.0 == m.file)));
        assert!(auto.card_bytes > 8_000_000_000);
        let wiki = plan(None, &["wikipedia".into()]).unwrap();
        assert!(wiki.card_bytes > auto.card_bytes + 2_000_000_000);
        let more = plan(None, &["wikipedia".into(), "medicine".into(), "dictionary".into()]).unwrap();
        assert_eq!(more.assets.iter().filter(|a| a.0.ends_with(".zim")).count(), 3);
        assert_eq!(more.assets.iter().filter(|a| a.0 == "wiki-reader.whl").count(), 1);
        assert!(plan(None, &["encyclopedia-galactica".into()]).is_err());
        assert_eq!(chosen(&[("medicine".to_string(), true), ("wikipedia".to_string(), true), ("translate".to_string(), true),
                            ("translate-es".to_string(), true)].into()),
                   ["wikipedia", "medicine", "translate-es"]);
        let spoken = plan(None, &["translate-es".into(), "translate-fr".into()]).unwrap();
        assert_eq!(spoken.assets.iter().filter(|a| a.0.ends_with(".argosmodel")).count(), 4);
        assert_eq!(spoken.assets.iter().filter(|a| a.0.ends_with("aarch64.whl")).count(), 4);
        assert_eq!(spoken.assets.iter().filter(|a| a.0 == "wiki-reader.whl").count(), 0);
        assert!(spoken.card_bytes > auto.card_bytes + 600_000_000);
        let heard = plan(None, &["voice".into(), "translate-es".into()]).unwrap();
        assert_eq!(heard.assets.iter().filter(|a| a.0.ends_with("aarch64.whl")).count(), 5); // engine and Vosk
        assert!(heard.assets.iter().any(|a| a.0 == "vosk-model-small-en-us-0.15.zip"));
        assert!(heard.assets.iter().any(|a| a.0 == "whisper-tiny.en-model.bin"));
        assert!(heard.assets.iter().any(|a| a.0 == LIBATOMIC.0));
        assert!(heard.assets.iter().all(|a| a.0 != "whisper-base.en-model.bin"));
        assert_eq!(chosen(&[("voice".to_string(), true)].into()), ["voice"]);
        assert_eq!(library_catalog()[0]["sizes"]["desktop"], voice_engines()["whisper-base"].size_mb);
    }
}
