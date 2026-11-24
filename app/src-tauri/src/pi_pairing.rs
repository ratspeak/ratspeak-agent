//! Private pairing tickets let the installer reopen a Pi's rotating code after eject.
use serde_json::{json, Value};
use std::{fs, io::Write, path::PathBuf};
use tauri::{AppHandle, Manager};

fn folder(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("pending-pi"))
}
fn ticket(app: &AppHandle, address: &str) -> Result<PathBuf, String> {
    if !crate::is_address(address) {
        return Err("Invalid Pi address.".into());
    }
    Ok(folder(app)?.join(format!("{address}.json")))
}
pub fn remember(app: &AppHandle, payload: &Value) -> Result<(), String> {
    let address = payload["link"]
        .as_str()
        .and_then(|s| s.strip_prefix("lxma://"))
        .and_then(|s| s.split_once(':'))
        .ok_or("Invalid pairing link.")?
        .0;
    let path = ticket(app, address)?;
    let dir = path.parent().unwrap();
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut file, payload).map_err(|e| e.to_string())?;
    file.flush()
        .and_then(|_| file.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    file.persist(&path).map_err(|e| e.to_string())?;
    Ok(())
}
#[tauri::command(async)]
pub fn remember_sd_agent(app: AppHandle, device: String, selected_media: String, link: String) -> Result<Value,String> {
    crate::card::require_media(&selected_media,crate::disk::media_id(&device)?)?;
    let (boot,status) = crate::selected_boot(&device,&link)?;
    let address=link.strip_prefix("lxma://").and_then(|v|v.split_once(':')).filter(|(a,k)|crate::is_address(a) && k.len()==128 && k.bytes().all(|b|b.is_ascii_hexdigit())).ok_or("Invalid agent address.")?.0;
    let mut record=read_pi_pairing(app.clone(),address.to_owned()).unwrap_or_else(|_|json!({}));
    record["link"]=json!(link);
    for field in ["name","provider","hostname"] {
        if status[field].is_string() { record[field]=status[field].clone(); }
    }
    if !record["hostname"].is_string() {
        if let Some(name) = record["name"].as_str() { record["hostname"] = json!(crate::hostname(name)); }
    }
    summarize(&mut record, &status);
    if let Some(model) = status["model"].as_str().or_else(||status["venice_model"].as_str()) {
        record["model"]=json!(model);
    }
    record["claimed"]=json!(record["claimed"]==true || status["access"]["mode"].as_str().is_some_and(|v|v!="later"));
    if let Ok(raw)=fs::read(boot.join("ratspeak-pi/settings.json")) {
        if let Ok(seed)=serde_json::from_slice::<Value>(&raw) {
            if seed["pairing_link"]==link {
                for key in ["claim_secret","monitor_key"] {
                    if seed[key].as_str().is_some_and(|s|s.len()==64 && s.bytes().all(|b|b.is_ascii_hexdigit())) { record[key]=seed[key].clone(); }
                }
            }
        }
    }
    if record["claimed"]==true { record.as_object_mut().unwrap().remove("claim_secret"); }
    crate::card::require_media(&selected_media,crate::disk::media_id(&device)?)?;
    remember(&app,&record)?;
    Ok(record)
}

/// What the card last said, for Settings while the card is out. Secrets never leave the card read.
fn summarize(record: &mut Value, status: &Value) {
    let mut card = json!({"read_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)});
    for key in ["network","pending","apply_failed","hubs","model","model_id","model_options","provider","venice_model","features","access","offline","ram_mb",
                "state","detail","installed","log_end","chat_erasure","next_link","can_clear_chats","can_reset_identity","identity_retry","stats","sampled_at"] {
        if !status[key].is_null() { card[key] = status[key].clone(); }
    }
    record["card"] = card;
    match status["next_link"].as_str() { Some(next) => record["next_link"] = json!(next), None => { record.as_object_mut().unwrap().remove("next_link"); } }
}

/// After Setup writes to a Pi's card, keep its record in step, so Settings shows what waits for the next boot.
pub fn refresh_card(app: &AppHandle, boot: &std::path::Path, link: &str) {
    let Some(address) = link.strip_prefix("lxma://").and_then(|s| s.split_once(':')).map(|(a, _)| a) else { return };
    if let Ok(mut record) = read_pi_pairing(app.clone(), address.to_owned()) {
        summarize(&mut record, &crate::card::status(boot));
        let _ = remember(app, &record);
    }
}

#[tauri::command]
pub fn pending_pi_pairings(app: AppHandle) -> Result<Vec<Value>, String> {
    Ok(directory(&app)?.into_iter().filter(|r| r["claimed"] != true).collect())
}

/// Every saved Pi record, by address.
pub fn records(app: &AppHandle) -> Result<Vec<(String, Value)>, String> {
    let dir = folder(app)?;
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut found = Vec::new();
    for item in fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let path = item.path();
        let Some(address) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| crate::is_address(s))
        else {
            continue;
        };
        if let Ok(record) = read_pi_pairing(app.clone(), address.to_string()) {
            found.push((address.to_string(), record));
        }
    }
    Ok(found)
}

pub fn directory(app: &AppHandle) -> Result<Vec<Value>,String> {
    Ok(records(app)?.into_iter().map(|(address, record)| {
        let (state, percent) = crate::pi_watch::row_state(&address, &record);
        json!({"kind":"pi","name":record["name"],"link":record["link"],"state":state,"percent":percent,"claimed":record["claimed"],"provider":record["provider"],"model":record["model"],"address":address})
    }).collect())
}
#[tauri::command]
pub fn read_pi_pairing(app: AppHandle, address: String) -> Result<Value, String> {
    let bytes =
        fs::read(ticket(&app, &address)?).map_err(|_| "Couldn’t open the saved pairing.")?;
    serde_json::from_slice(&bytes).map_err(|_| "Invalid saved pairing.".into())
}
#[tauri::command]
pub fn complete_pi_pairing(app: AppHandle, address: String) -> Result<(), String> {
    let mut record = read_pi_pairing(app.clone(), address)?;
    record["claimed"] = true.into();
    if let Some(obj) = record.as_object_mut() { obj.remove("claim_secret"); }
    remember(&app, &record)
}

#[tauri::command]
pub fn forget_pi(app: AppHandle, address: String) -> Result<(),String> {
    let path = ticket(&app, &address)?;
    if path.exists() {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}
