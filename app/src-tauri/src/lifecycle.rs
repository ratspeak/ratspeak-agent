//! The window, tray and Docker agent have independent lifetimes.
#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
use crate::desktop;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
    thread,
    time::Duration,
};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{TrayIcon, TrayIconBuilder},
    AppHandle, Emitter, Manager, RunEvent, WindowEvent,
};

static EXIT: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
static HIDDEN: AtomicBool = AtomicBool::new(false);
static KNOWN: AtomicBool = AtomicBool::new(false);
// A fresh setup (including the Pi route) never needs to look up or launch Docker.
static DESKTOP_MONITOR: OnceLock<()> = OnceLock::new();

pub fn monitor_desktop() {
    DESKTOP_MONITOR.get_or_init(|| ());
}

fn has_agent_record(app: &AppHandle) -> bool {
    app.path().app_local_data_dir().is_ok_and(|d| {
        d.join("agent-lan/engine").is_file() || d.join("desktop-lan/engine").is_file()
    })
}

pub fn resume_desktop(app: &AppHandle, background: bool) {
    if background || has_agent_record(app) {
        monitor_desktop();
        desktop::attach_existing(app.clone());
    }
}
// Reserve a tray power change before its worker starts. A confirmed quit can then
// wait for startup (including LAN discovery) to finish before stopping the agent.
static TRAY_POWER: tauri::async_runtime::Mutex<()> = tauri::async_runtime::Mutex::const_new(());

pub fn reveal(app: &AppHandle) {
    #[cfg(target_os = "linux")]
    linux::cancel_pending_minimize();
    #[cfg(target_os = "macos")]
    {
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
        let _ = app.show();
    }
    HIDDEN.store(false, Ordering::Relaxed);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn request_close(app: &AppHandle, quitting: bool) {
    reveal(app);
    let _ = app.emit("agent-close-request", quitting);
}

#[tauri::command(async)]
pub fn window_ready(app: AppHandle) -> Option<&'static str> {
    READY.store(true, Ordering::Release);
    if !has_agent_record(&app) && !app.path().app_local_data_dir().is_ok_and(|p|p.join("pending-pi").is_dir()) && !std::env::args().any(|arg| arg == "--agent-tray") {
        return None;
    }
    Some("agents")
}

#[tauri::command]
pub fn tray_available(app: AppHandle) -> bool {
    visible_tray(&app).is_some()
}

fn visible_tray(app: &AppHandle) -> Option<TrayIcon> {
    let tray = app.tray_by_id("agent")?;
    #[cfg(target_os = "linux")]
    if !tray
        .with_inner_tray_icon(|_| linux::has_tray_host())
        .unwrap_or(false)
    {
        return None;
    }
    Some(tray)
}

// Run the visibility changes together on the UI thread. A tray click can otherwise
// interleave a restore with a partially completed hide.
pub fn background_window(app: &AppHandle) -> tauri::Result<()> {
    let window = app
        .get_webview_window("main")
        .ok_or(tauri::Error::WindowNotFound)?;
    let Some(tray) = visible_tray(app) else {
        if !window.is_visible()? {
            #[cfg(target_os = "linux")]
            return linux::show_minimized(&window);
            #[cfg(not(target_os = "linux"))]
            window.show()?;
        }
        return window.minimize();
    };
    tray.set_visible(true)?;
    window.hide()?;
    #[cfg(target_os = "macos")]
    if let Err(error) = app.set_activation_policy(tauri::ActivationPolicy::Accessory) {
        reveal(app);
        return Err(error);
    }
    HIDDEN.store(true, Ordering::Relaxed);
    Ok(())
}

#[tauri::command(async)]
pub fn close_state(app: AppHandle) -> &'static str {
    if DESKTOP_MONITOR.get().is_none() {
        return "idle";
    }
    if TRAY_POWER.try_lock().is_err() {
        return "running";
    }
    match desktop::running_states() {
        Ok(states) if states.iter().filter(|(_, running)| *running).count() > 1 => "multiple",
        Ok(states) if states.iter().any(|(_, running)| *running) => "running",
        Ok(_) => "idle",
        Err(_) if KNOWN.load(Ordering::Relaxed) || has_agent_record(&app) => "unknown",
        Err(_) => "idle",
    }
}

#[tauri::command]
pub async fn close_choice(app: AppHandle, choice: String) -> Result<(), String> {
    match choice.as_str() {
        "background" => {
            let (send, result) = std::sync::mpsc::sync_channel(1);
            let handle = app.clone();
            app.run_on_main_thread(move || {
                let _ = send.send(background_window(&handle).map_err(|e| e.to_string()));
            })
            .map_err(|e| e.to_string())?;
            result.recv().map_err(|e| e.to_string())?
        }
        "stop" => {
            let _power = TRAY_POWER.lock().await;
            let handle = app.clone();
            tauri::async_runtime::spawn_blocking(move || desktop::stop_for_exit(&handle))
                .await
                .map_err(|e| e.to_string())??;
            exit(&app);
            Ok(())
        }
        "quit" => {
            exit(&app);
            Ok(())
        }
        _ => Err("Unknown window action.".into()),
    }
}

fn exit(app: &AppHandle) {
    EXIT.store(true, Ordering::Release);
    app.exit(0);
}

pub fn event(app: &AppHandle, event: RunEvent) {
    match event {
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } if label == "main" && READY.load(Ordering::Acquire) && !EXIT.load(Ordering::Acquire) => {
            api.prevent_close();
            request_close(app, false);
        }
        RunEvent::ExitRequested { api, code, .. }
            if code != Some(tauri::RESTART_EXIT_CODE)
                && READY.load(Ordering::Acquire) && !EXIT.load(Ordering::Acquire) =>
        {
            api.prevent_exit();
            request_close(app, true);
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } => reveal(app),
        _ => (),
    }
}

pub fn tray(app: &AppHandle, background: bool) -> tauri::Result<()> {
    let status = MenuItem::with_id(
        app,
        "agent-state",
        "Checking desktop agent…",
        false,
        None::<&str>,
    )?;
    let open = MenuItem::with_id(app, "agent-open", "Show window", true, None::<&str>)?;
    let desktop_start = MenuItem::with_id(
        app,
        "desktop-start",
        "Start desktop agent",
        true,
        None::<&str>,
    )?;
    let hermes_start = MenuItem::with_id(
        app,
        "hermes-start",
        "Start Hermes receiver",
        true,
        None::<&str>,
    )?;
    let desktop_stop = MenuItem::with_id(
        app,
        "desktop-stop",
        "Stop desktop agent",
        true,
        None::<&str>,
    )?;
    let hermes_stop = MenuItem::with_id(
        app,
        "hermes-stop",
        "Stop Hermes receiver",
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "agent-quit", "Quit…", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&status, &open, &sep, &quit])?;
    let builder = TrayIconBuilder::with_id("agent")
        .icon(tauri::image::Image::from_bytes(include_bytes!(
            "../icons/agent-tray.png"
        ))?)
        .icon_as_template(cfg!(target_os = "macos"))
        .menu(&menu)
        .tooltip("Ratspeak Agent")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "agent-open" => reveal(app),
            "agent-quit" => request_close(app, true),
            id @ ("desktop-start" | "desktop-stop" | "hermes-start" | "hermes-stop") => {
                let Ok(power) = TRAY_POWER.try_lock() else {
                    return;
                };
                if EXIT.load(Ordering::Acquire) {
                    return;
                }
                let app = app.clone();
                let running = id.ends_with("-start");
                let kind = if id.starts_with("hermes-") {
                    desktop::AgentKind::Hermes
                } else {
                    desktop::AgentKind::Desktop
                };
                thread::spawn(move || {
                    let result = desktop::desktop_set_running(app.clone(), kind, running,None,None);
                    drop(power);
                    match result {
                        Ok(_) => {
                            let _ = app.emit("agent-state-changed", ());
                        }
                        Err(e) => {
                            reveal(&app);
                            let _ = app.emit("agent-power-error", e);
                        }
                    }
                });
            }
            _ => (),
        });
    #[cfg(target_os = "windows")]
    let builder = builder
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                reveal(tray.app_handle());
            }
        });
    let tray = builder.build(app)?;
    if !background {
        tray.set_visible(false)?;
    }
    let handle = app.clone();
    thread::spawn(move || {
        DESKTOP_MONITOR.wait();
        let mut previous = None;
        loop {
            let state = desktop::running_states().ok();
            let exists = state.as_ref().map(|s| !s.is_empty()).unwrap_or_else(|| KNOWN.load(Ordering::Relaxed) || background);
            KNOWN.store(exists, Ordering::Relaxed);
            if previous.as_ref() != Some(&state) {
                let label = match state.as_deref() {
                    None => "Agents unavailable".to_owned(),
                    Some([]) => "No desktop agents".to_owned(),
                    Some(rows) => format!("{} of {} agents running",rows.iter().filter(|(_,on)| *on).count(),rows.len()),
                };
                let _ = status.set_text(&label);
                let _ = tray.set_tooltip(Some(&label));
                let unique = |rows: &[(desktop::AgentKind,bool)], kind| {
                    let mut matching=rows.iter().filter(|(k,_)| *k==kind);
                    let (_,running)=matching.next()?;
                    matching.next().is_none().then_some(*running)
                };
                let controls = |rows: &[(desktop::AgentKind,bool)]| [unique(rows,desktop::AgentKind::Desktop),unique(rows,desktop::AgentKind::Hermes)];
                let [desktop,hermes] = controls(state.as_deref().unwrap_or_default());
                let old = controls(previous.as_ref().and_then(|v: &Option<Vec<_>>| v.as_deref()).unwrap_or_default());
                for (index, (running, start, stop)) in [
                    (desktop, &desktop_start, &desktop_stop),
                    (hermes, &hermes_start, &hermes_stop),
                ]
                .into_iter()
                .enumerate()
                {
                    if old[index] == running {
                        continue;
                    }
                    if let Some(was_running) = old[index] {
                        let _ = menu.remove(if was_running { stop } else { start });
                    }
                    if let Some(is_running) = running {
                        let position = 2 + usize::from(index == 1 && desktop.is_some());
                        let _ = menu.insert(if is_running { stop } else { start }, position);
                    }
                }
                let icon = tray.clone();
                let _ = handle.run_on_main_thread(move || {
                    let _ =
                        icon.set_visible(exists || background || HIDDEN.load(Ordering::Relaxed));
                });
                previous = Some(state);
            }
            thread::sleep(Duration::from_secs(if exists { 5 } else { 20 }));
        }
    });
    Ok(())
}
