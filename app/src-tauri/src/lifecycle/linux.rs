use gdkx11::{
    gdk, gio,
    glib::{self, prelude::*, translate::ToGlibPtr},
    X11Display,
};
use gtk::prelude::{GtkWindowExt, WidgetExtManual};
use std::{
    cell::Cell,
    ffi::CString,
    sync::atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Manager};
use x11::xlib;

static MINIMIZE_AFTER_MAP: AtomicBool = AtomicBool::new(false);
const LAUNCHER_ACTIVATION: &str = "--launcher-activation";

// The single-instance plugin forwards argv and cwd, but drops the graphical
// launcher's environment. Pass its one-use token to the existing window over
// the same session bus so the compositor can authorize the user's activation.
pub(crate) fn forward_launcher_activation(identifier: &str) -> bool {
    let Ok(token) = std::env::var("XDG_ACTIVATION_TOKEN") else {
        return false;
    };
    if token.is_empty() {
        return false;
    }
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return false;
    };
    let service = format!("{identifier}.SingleInstance");
    let path = format!("/{}", service.replace('.', "/").replace('-', "_"));
    let message = (vec![LAUNCHER_ACTIVATION.to_owned(), token], "").to_variant();
    bus.call_sync(
        Some(&service),
        &path,
        "org.SingleInstance.DBus",
        "ExecuteCallback",
        Some(&message),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        1000,
        gio::Cancellable::NONE,
    )
    .is_ok()
}

pub(crate) fn activate_from_launcher(app: &AppHandle, args: &[String]) -> bool {
    let [flag, token] = args else { return false };
    if flag != LAUNCHER_ACTIVATION {
        return false;
    }
    let handle = app.clone();
    let token = token.clone();
    app.run_on_main_thread(move || {
        super::reveal(&handle);
        if let Some(window) = handle.get_webview_window("main") {
            if let Ok(native) = window.gtk_window() {
                native.set_startup_id(&token);
            }
        }
    })
    .is_ok()
}

pub(super) fn cancel_pending_minimize() {
    MINIMIZE_AFTER_MAP.store(false, Ordering::Relaxed);
}

pub(super) fn show_minimized(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    // The first GTK tick precedes the first surface commit. A Wayland compositor
    // can ignore a minimize request until that commit maps the window.
    let native = window.gtk_window()?;
    let first_frame = Cell::new(true);
    MINIMIZE_AFTER_MAP.store(true, Ordering::Relaxed);
    native.add_tick_callback(move |widget, _| {
        if first_frame.replace(false) {
            return glib::ControlFlow::Continue;
        }
        if MINIMIZE_AFTER_MAP.swap(false, Ordering::Relaxed) {
            widget.iconify();
        }
        glib::ControlFlow::Break
    });
    window.show()
}

// AppIndicator's set_visible succeeds even if there is no panel to show it.
// Check both the StatusNotifier host and AppIndicator's legacy XEmbed fallback.
pub(super) fn has_tray_host() -> bool {
    const WATCHER: &str = "org.kde.StatusNotifierWatcher";
    if let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        let property = (WATCHER, "IsStatusNotifierHostRegistered").to_variant();
        if bus
            .call_sync(
                Some(WATCHER),
                "/StatusNotifierWatcher",
                "org.freedesktop.DBus.Properties",
                "Get",
                Some(&property),
                None,
                gio::DBusCallFlags::NO_AUTO_START,
                350,
                gio::Cancellable::NONE,
            )
            .ok()
            .and_then(|reply| reply.get::<(glib::Variant,)>())
            .and_then(|(value,)| value.get::<bool>())
            == Some(true)
        {
            return true;
        }
    }
    let Some(display) = gdk::Display::default().and_then(|d| d.downcast::<X11Display>().ok())
    else {
        return false;
    };
    // On the GTK thread, use its existing X connection and screen. A Wayland
    // window cannot embed a legacy tray icon in an unrelated XWayland panel.
    unsafe {
        let x = gdkx11::ffi::gdk_x11_display_get_xdisplay(display.to_glib_none().0);
        let name = CString::new(format!("_NET_SYSTEM_TRAY_S{}", xlib::XDefaultScreen(x))).unwrap();
        let selection = xlib::XInternAtom(x, name.as_ptr(), xlib::True);
        selection != 0 && xlib::XGetSelectionOwner(x, selection) != 0
    }
}
