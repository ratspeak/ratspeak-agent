use super::{request_close, EXIT, READY};
use objc2::{
    class, ffi, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Imp, Sel},
    sel,
};
use std::sync::{atomic::Ordering, OnceLock};
use tauri::AppHandle;

static APP: OnceLock<AppHandle> = OnceLock::new();

// NSApplication's Quit menu item and the Dock both call terminate: directly.
// Tao delivers applicationWillTerminate: after AppKit has committed to exit.
// Use the earlier delegate decision so the WebView can finish the user's choice.
extern "C-unwind" fn should_terminate(_: &AnyObject, _: Sel, _: &AnyObject) -> usize {
    if READY.load(Ordering::Acquire) && !EXIT.load(Ordering::Acquire) {
        if let Some(app) = APP.get() {
            request_close(app, true);
            return 0; // NSTerminateCancel. Confirmation exits via close_choice.
        }
    }
    1 // NSTerminateNow
}

pub fn install_quit_handler(app: &AppHandle) {
    // Called on the main thread during Tauri setup. Add only the optional
    // selector on Tao's own delegate class, preserving all of its other methods.
    unsafe {
        let ns_app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
        let Some(delegate): Option<Retained<AnyObject>> = msg_send![&*ns_app, delegate] else {
            return;
        };
        let class = delegate.class();
        let selector = sel!(applicationShouldTerminate:);
        if class.name() != c"TaoAppDelegateParent" || class.instance_method(selector).is_some() {
            return;
        }
        let _ = APP.set(app.clone());
        // NSApplicationTerminateReply is NSUInteger on both supported Mac ABIs.
        let method: extern "C-unwind" fn(&AnyObject, Sel, &AnyObject) -> usize = should_terminate;
        if ffi::class_addMethod(
            (class as *const AnyClass).cast_mut(),
            selector,
            std::mem::transmute::<_, Imp>(method),
            c"Q@:@".as_ptr(),
        )
        .as_bool()
        {
            // AppKit caches the optional selectors when installing the delegate.
            let _: () = msg_send![&*ns_app, setDelegate: Option::<&AnyObject>::None];
            let _: () = msg_send![&*ns_app, setDelegate: &*delegate];
        }
    }
}
