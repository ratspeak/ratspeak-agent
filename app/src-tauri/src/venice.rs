//! Only the native process contacts Venice. Pass the bearer header over stdin.
use serde::Serialize;
use serde_json::Value;
use std::{collections::HashSet, io::Write, process::Stdio};

#[derive(Serialize)]
pub struct Model {
    pub id: String,
    name: String,
}

#[derive(Serialize)]
pub struct VerifiedCatalog {
    models: Vec<Model>,
    inference_available: bool,
    has_model_limits: bool,
}

pub fn valid_id(id: &str) -> bool {
    (1..=160).contains(&id.len())
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:/-".contains(&c))
}

pub fn valid_key(key: &str) -> bool {
    (1..=1024).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_graphic())
}

fn request(path: &str, key: &str, body: Option<&Value>) -> Result<Value, String> {
    let mut curl = crate::network::curl();
    curl.args([
        "--disable",
        "--silent",
        "--proto",
        "=https",
        "--connect-timeout",
        "8",
        "--max-time",
        "25",
        "--max-filesize",
        "4000000",
        "--write-out",
        "\n%{http_code}",
        &format!("https://api.venice.ai/api/v1/{path}"),
    ]);
    if let Some(body) = body {
        curl.args(["--header", "Content-Type: application/json", "--data-binary", &body.to_string(), "--max-time", "75"]);
    }
    let mut child = curl
        .args(["--header", "@-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Could not start the secure connection to Venice.")?;
    let sent = writeln!(child.stdin.take().unwrap(), "Authorization: Bearer {key}");
    let out = child
        .wait_with_output()
        .map_err(|_| "The connection to Venice stopped.")?;
    if sent.is_err() || !out.status.success() {
        return Err("Couldn’t reach Venice. Check your internet connection and try again.".into());
    }
    let data = std::str::from_utf8(&out.stdout).map_err(|_| "Invalid response from Venice.")?;
    let (body, status) = data
        .rsplit_once('\n')
        .ok_or("Incomplete response from Venice.")?;
    match status {
        "200" => serde_json::from_str(body).map_err(|_| "Invalid response from Venice.".into()),
        "401" => Err("Venice rejected the key. Enter an active inference key.".into()),
        "403" => {
            Err("Venice denied access. Check the key’s permissions and IP restrictions.".into())
        }
        "402" => Err("Check your API key and available credit in Venice.".into()),
        "429" => Err("Venice is limiting requests. Try again shortly.".into()),
        _ => Err("Venice could not complete this request. Try again.".into()),
    }
}
fn selectable(data: &Value) -> Vec<Model> {
    let mut models: Vec<Model> = data["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let spec = &m["model_spec"];
            let id = m["id"].as_str()?;
            (m["type"] == "text"
                && spec["privacy"] == "private"
                && spec["offline"] != true
                && valid_id(id))
            .then(|| Model {
                id: id.to_owned(),
                name: spec["name"].as_str().unwrap_or(id).to_owned(),
            })
        })
        .collect();
    models.sort_by_cached_key(|model| {
        let name = model.name.to_lowercase();
        let id = model.id.to_ascii_lowercase();
        let uncensored = name.contains("uncensored") || id.contains("uncensored");
        let venice = name.starts_with("venice ") || id.starts_with("venice-");
        (!uncensored, uncensored && !venice, name, id)
    });
    models
}
fn verified_catalog(access: &Value, catalog: &Value) -> Result<VerifiedCatalog, String> {
    let data = &access["data"];
    let inference_available = data["accessPermitted"]
        .as_bool()
        .ok_or("Venice did not return the key’s inference status. Try again.")?;
    let rows = data["rateLimits"]
        .as_array()
        .ok_or("Venice did not return the key’s model limits. Try again.")?;
    let mut has_model_limits = false;
    let mut permitted = HashSet::new();
    for row in rows {
        let Some(id) = row["apiModelId"].as_str().filter(|id| valid_id(id)) else {
            continue;
        };
        has_model_limits = true;
        if row["rateLimits"].as_array().is_some_and(|limits| {
            limits
                .iter()
                .all(|limit| limit["amount"].as_f64().is_some_and(|amount| amount > 0.0))
        }) {
            permitted.insert(id);
        }
    }
    if !catalog["data"].is_array() {
        return Err("Couldn’t read the Venice model list. Try again.".into());
    }
    let mut models = selectable(catalog);
    if has_model_limits {
        models.retain(|model| permitted.contains(model.id.as_str()));
    }
    Ok(VerifiedCatalog {
        models,
        inference_available,
        has_model_limits,
    })
}

#[tauri::command(async)]
pub fn open_venice_keys() -> Result<(), String> {
    crate::updates::open_in_browser("https://venice.ai/settings/api")
}

#[tauri::command(async)]
pub fn venice_models(api_key: String) -> Result<VerifiedCatalog, String> {
    if !valid_key(&api_key) {
        return Err("Enter a Venice API key.".into());
    }
    let access = request("api_keys/rate_limits", &api_key, None)?;
    let catalog = request("models?type=text", &api_key, None)?;
    verified_catalog(&access, &catalog)
}
pub fn verify_choice(key: &str, model: &str) -> Result<(), String> {
    check_model(&venice_models(key.to_string())?, model)
}

fn check_model(catalog: &VerifiedCatalog, model: &str) -> Result<(), String> {
    if !catalog.inference_available {
        return Err("Venice recognizes this key but has paused inference. Check your Venice account and reverify.".into());
    }
    if !catalog.models.iter().any(|m| m.id == model) {
        return Err("This model is unavailable with this key. Choose another private model.".into());
    }
    Ok(())
}

/// An installed Pi applies SD edits before networking starts. Check the replacement
/// here, while the old working credential is still on the Pi.
pub fn test_card_change(key: &str, model: &str) -> Result<(), String> {
    let body = serde_json::json!({"model":model,"messages":[{"role":"user","content":"Reply with OK."}],
        "max_completion_tokens":64,"venice_parameters":{"disable_thinking":true},"stream":false});
    let result = request("chat/completions", key, Some(&body))?;
    if result["choices"][0]["message"]["content"].as_str().is_none_or(|s| s.trim().is_empty()) {
        return Err("The model did not return a test reply. Choose another model or try again.".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verified_key_can_browse_when_inference_is_unavailable() {
        let rows = ["allowed", "blocked", "unlisted"].map(
            |id| serde_json::json!({"id":id,"type":"text","model_spec":{"privacy":"private"}}),
        );
        let catalog = serde_json::json!({"data":rows});
        let mut access = serde_json::json!({"data":{"accessPermitted":false,"rateLimits":[
            {"apiModelId":"allowed","rateLimits":[{"type":"RPM","amount":100}]},
            {"apiModelId":"blocked","rateLimits":[{"type":"RPM","amount":100},{"type":"RPD","amount":0}]}
        ]}});
        let result = verified_catalog(&access, &catalog).unwrap();
        assert!(!result.inference_available && result.has_model_limits);
        assert!(check_model(&result, "allowed").is_err());
        let available = VerifiedCatalog { inference_available: true, ..result };
        assert!(check_model(&available, "allowed").is_ok());
        assert!(check_model(&available, "blocked").is_err());
        let result = available;
        assert_eq!(result.models.len(), 1);
        assert_eq!(result.models[0].id, "allowed");
        access["data"]["rateLimits"] = serde_json::json!([]);
        let result = verified_catalog(&access, &catalog).unwrap();
        assert!(!result.has_model_limits && !result.inference_available);
        assert_eq!(result.models.len(), 3);
        access["data"]["rateLimits"] =
            serde_json::json!([{"apiModelId":"blocked","rateLimits":[{"amount":0}]}]);
        assert!(verified_catalog(&access, &catalog)
            .unwrap()
            .models
            .is_empty());
        assert!(verified_catalog(&serde_json::json!({"data":{}}), &catalog).is_err());
    }

    #[test]
    fn venice_uncensored_models_lead_the_picker() {
        let entries = [
            ("alpha", "Alpha", "private"),
            ("gemma", "Gemma Uncensored", "private"),
            ("venice-uncensored-1-2", "Venice Uncensored 1.2", "private"),
            ("venice-chat", "Venice Chat", "private"),
            ("role-play", "Venice Role Play UNCENSORED", "private"),
            ("venice-uncensored-old", "Earlier model", "anonymized"),
        ];
        let rows = entries.map(|(id, name, privacy)| {
            serde_json::json!({"type":"text","id":id,"model_spec":{"name":name,"privacy":privacy}})
        });
        let models = selectable(&serde_json::json!({"data": rows}));
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "role-play",
                "venice-uncensored-1-2",
                "gemma",
                "alpha",
                "venice-chat"
            ]
        );
    }

    #[test]
    fn only_online_private_text_models() {
        let data = ["private", "anonymized", "e2ee", "tee"].map(|p| {
            serde_json::json!({
                "type":"text","id":p,"model_spec":{"privacy":p}
            })
        });
        let models = serde_json::json!({"data":data});
        assert_eq!(selectable(&models).len(), 1);
        assert!(!valid_key("token\nX-Other: h"));
    }
}
