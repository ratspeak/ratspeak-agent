//! User-requested release checks. Cargo's repository field supplies the project and update links.
use std::process::Command;
use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::{Error as UpdaterError, UpdaterExt};
use tokio::sync::Mutex;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const CHECK_FAILED: &str = "Could not check for updates. Check your connection and try again.";
const INSTALL_FAILED: &str = "Could not install the update. Try again or view the release.";
const MANIFEST: &str = "latest.json";

#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<tauri_plugin_updater::Update>>);

#[derive(Serialize)]
pub struct AppInfo {
    version: &'static str,
    repository: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Update {
    Unconfigured,
    Unpublished,
    Current,
    Available { version: String, installable: bool },
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
}

fn repository_slug(url: &str) -> Option<String> {
    let path = url
        .strip_prefix("https://github.com/")?
        .trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    let owner_ok = |b: u8| b.is_ascii_alphanumeric() || b == b'-';
    let repo_ok = |b: u8| b.is_ascii_alphanumeric() || b"._-".contains(&b);
    if owner.is_empty()
        || repo.is_empty()
        || [".", ".."].contains(&repo)
        || !owner.bytes().all(owner_ok)
        || !repo.bytes().all(repo_ok)
    {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

#[tauri::command]
pub fn app_info() -> AppInfo {
    AppInfo {
        version: VERSION,
        repository: repository_slug(REPOSITORY).map(|slug| format!("https://github.com/{slug}")),
    }
}

fn latest_release(status: u16, body: &str) -> Result<Option<(Version, Release)>, String> {
    match status {
        404 => return Ok(None),
        403 | 429 => return Err("Too many update checks. Try again later.".into()),
        200 => (),
        _ => return Err(CHECK_FAILED.into()),
    }
    let release: Release =
        serde_json::from_str(body).map_err(|_| "The update response could not be read.")?;
    if release.draft || release.prerelease || release.tag_name.starts_with("test-") {
        return Ok(None);
    }
    let version = Version::parse(
        release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name),
    )
    .map_err(|_| "The release version could not be read.")?;
    Ok(version.pre.is_empty().then_some((version, release)))
}

fn fetch_release(slug: &str) -> Result<Option<(Version, Release)>, String> {
    let response = crate::network::curl()
        .args([
            "--disable",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--location",
            "--proto-redir",
            "=https",
            "--max-redirs",
            "3",
            "--connect-timeout",
            "5",
            "--max-time",
            "15",
            "--max-filesize",
            "1048576",
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "X-GitHub-Api-Version: 2026-03-10",
            "--user-agent",
            &format!("ratspeak-agent/{VERSION}"),
            "--write-out",
            "\n%{http_code}",
        ])
        .arg(format!(
            "https://api.github.com/repos/{slug}/releases/latest"
        ))
        .output()
        .map_err(|_| CHECK_FAILED)?;
    if !response.status.success() {
        return Err(CHECK_FAILED.into());
    }
    let response = String::from_utf8(response.stdout).map_err(|_| CHECK_FAILED)?;
    let (body, status) = response.rsplit_once('\n').ok_or(CHECK_FAILED)?;
    latest_release(status.parse().map_err(|_| CHECK_FAILED)?, body)
}

#[tauri::command]
pub async fn check_for_updates(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<Update, String> {
    let mut pending = pending
        .0
        .try_lock()
        .map_err(|_| "An update is already in progress.")?;
    *pending = None;
    let Some(slug) = repository_slug(REPOSITORY) else {
        return Ok(Update::Unconfigured);
    };
    let repo = slug.clone();
    let Some((version, release)) =
        tauri::async_runtime::spawn_blocking(move || fetch_release(&repo))
            .await
            .map_err(|_| CHECK_FAILED)??
    else {
        return Ok(Update::Unpublished);
    };
    let installed = Version::parse(VERSION).map_err(|_| "The app version could not be read.")?;
    if !version.cmp_precedence(&installed).is_gt() {
        return Ok(Update::Current);
    }
    let version = version.to_string();
    if release.assets.iter().any(|asset| asset.name == MANIFEST) {
        let base = format!(
            "https://github.com/{slug}/releases/download/{}/",
            release.tag_name
        );
        let url = format!("{base}{MANIFEST}")
            .parse()
            .map_err(|_| CHECK_FAILED)?;
        let updater = app
            .updater_builder()
            .endpoints(vec![url])
            .map_err(|_| CHECK_FAILED)?
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| CHECK_FAILED)?;
        match updater.check().await {
            Ok(Some(update))
                if update.version == version && update.download_url.as_str().starts_with(&base) =>
            {
                *pending = Some(update);
            }
            Err(UpdaterError::TargetNotFound(_) | UpdaterError::TargetsNotFound(_)) => (),
            _ => return Err(CHECK_FAILED.into()),
        }
    }
    Ok(Update::Available {
        version,
        installable: pending.is_some(),
    })
}

#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<(), String> {
    let mut pending = pending
        .0
        .try_lock()
        .map_err(|_| "An update is already in progress.")?;
    let update = pending.as_mut().ok_or("Check for updates again.")?;
    let _card_lock = crate::card_writer_lock(&crate::cache(&app)?)?;
    update.timeout = Some(Duration::from_secs(300));
    let bytes = update.download(|_, _| {}, || {}).await.map_err(|error| {
        eprintln!("Update download failed: {error}");
        "Could not download or verify the update. Try again or view the release."
    })?;
    let _ = app.emit("app-update-installing", ());
    let update = update.clone();
    tauri::async_runtime::spawn_blocking(move || update.install(bytes))
        .await
        .map_err(|_| INSTALL_FAILED)?
        .map_err(|error| {
            eprintln!("Update install failed: {error}");
            INSTALL_FAILED
        })?;
    *pending = None;
    app.request_restart();
    Ok(())
}

#[tauri::command(async)]
pub fn open_project(releases: bool) -> Result<(), String> {
    let slug = repository_slug(REPOSITORY).ok_or("The project link is not available yet.")?;
    let url = format!(
        "https://github.com/{slug}{}",
        if releases { "/releases" } else { "" }
    );
    open_in_browser(&url)
}

pub(crate) fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("rundll32.exe");
        command
            .args(["url.dll,FileProtocolHandler"])
            .creation_flags(0x0800_0000);
        command
    };
    let opened = command
        .arg(url)
        .status()
        .map_err(|_| "The browser could not be opened.")?;
    if opened.success() {
        Ok(())
    } else {
        Err("The browser could not be opened.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> String {
        serde_json::json!({"tag_name": tag, "draft": false, "prerelease": false, "assets": []})
            .to_string()
    }

    #[test]
    fn only_github_repository_roots_are_accepted() {
        assert_eq!(
            repository_slug("https://github.com/example/project.git/"),
            Some("example/project".into())
        );
        for url in [
            "",
            "http://github.com/example/project",
            "https://github.com.evil.test/example/project",
            "https://github.com/example/project/releases",
            "https://github.com/example/project?token=x",
            "https://github.com/example/..",
            "https://github.com/example/project#fragment",
        ] {
            assert!(repository_slug(url).is_none(), "{url}");
        }
    }

    #[test]
    fn official_latest_release_supplies_version_and_assets() {
        let (version, release) = latest_release(
            200,
            r#"{"tag_name":"v0.2.0","draft":false,"prerelease":false,"assets":[{"name":"latest.json"}]}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(version, Version::new(0, 2, 0));
        assert_eq!(release.assets[0].name, MANIFEST);
    }

    #[test]
    fn unpublished_and_failed_checks_never_claim_current() {
        assert!(latest_release(404, "").unwrap().is_none());
        assert!(latest_release(200, &release("99.0.0-beta.1"))
            .unwrap()
            .is_none());
        assert!(latest_release(200, &release("test-build"))
            .unwrap()
            .is_none());
        for (draft, prerelease) in [(true, false), (false, true)] {
            let body = serde_json::json!({
                "tag_name": "v99.0.0", "draft": draft, "prerelease": prerelease, "assets": []
            });
            assert!(latest_release(200, &body.to_string()).unwrap().is_none());
        }
        for (status, body) in [
            (403, ""),
            (429, ""),
            (500, ""),
            (200, "not JSON"),
            (200, "{}"),
            (200, &release("latest")),
        ] {
            assert!(latest_release(status, body).is_err());
        }
    }
}
