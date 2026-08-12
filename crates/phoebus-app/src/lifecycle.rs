//! Native application lifecycle behavior that eframe does not provide itself.

/// Teach AppKit how to reopen the hidden eframe window when Phoebus's Dock icon is clicked.
///
/// winit owns the application delegate, so replacing it would break its event loop. Instead,
/// install the optional `NSApplicationDelegate` reopen method on that delegate's existing
/// Objective-C class. The callback only queues an egui viewport command; eframe still owns
/// the window and applies the command on its next event-loop pass.
#[cfg(target_os = "macos")]
pub fn install_reopen_handler(ctx: &egui::Context) {
    use objc2::runtime::{AnyObject, AnyProtocol, Bool, Imp, Sel};
    use objc2::{MainThreadMarker, sel};
    use objc2_app_kit::NSApplication;

    static REOPEN_CONTEXT: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();

    extern "C-unwind" fn reopen(
        _delegate: &AnyObject,
        _selector: Sel,
        _app: &NSApplication,
        has_visible_windows: Bool,
    ) -> Bool {
        log::debug!(
            "lifecycle: AppKit requested Dock reopen (visible windows: {})",
            has_visible_windows.as_bool()
        );
        if !has_visible_windows.as_bool()
            && let Some(ctx) = REOPEN_CONTEXT.get()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }
        Bool::YES
    }

    let Some(main_thread) = MainThreadMarker::new() else {
        log::warn!("lifecycle: cannot install the Dock reopen handler off the main thread");
        return;
    };
    let app = NSApplication::sharedApplication(main_thread);
    let Some(delegate) = app.delegate() else {
        log::warn!("lifecycle: AppKit has no application delegate to extend");
        return;
    };
    let selector = sel!(applicationShouldHandleReopen:hasVisibleWindows:);
    let delegate: &AnyObject = (*delegate).as_ref();
    let class = delegate.class();
    if class.instance_method(selector).is_some() {
        log::debug!("lifecycle: the application delegate already handles Dock reopen");
        return;
    }

    let Some(protocol) = AnyProtocol::get(c"NSApplicationDelegate") else {
        log::warn!("lifecycle: AppKit did not register NSApplicationDelegate");
        return;
    };

    let _ = REOPEN_CONTEXT.set(ctx.clone());
    type ReopenHandler = extern "C-unwind" fn(&AnyObject, Sel, &NSApplication, Bool) -> Bool;
    let handler: ReopenHandler = reopen;

    // SAFETY: `class` is the live winit application delegate's Objective-C class. The
    // selector is an optional method from NSApplicationDelegate, and its exact runtime type
    // encoding comes from that protocol rather than being reconstructed in Rust. `handler`
    // has the corresponding Objective-C ABI: receiver, selector, NSApplication*, BOOL -> BOOL.
    #[allow(unsafe_code)]
    let added = unsafe {
        let description =
            objc2::ffi::protocol_getMethodDescription(protocol, selector, Bool::NO, Bool::YES);
        if description.types.is_null() {
            log::warn!("lifecycle: AppKit supplied no type encoding for its reopen method");
            return;
        }
        objc2::ffi::class_addMethod(
            class as *const _ as *mut _,
            selector,
            std::mem::transmute::<ReopenHandler, Imp>(handler),
            description.types,
        )
    };
    if added.as_bool() {
        log::debug!("lifecycle: installed the Dock reopen handler");
    } else {
        log::warn!("lifecycle: AppKit refused the Dock reopen handler");
    }
}

/// Other platforms keep eframe's normal close-to-quit lifecycle.
#[cfg(not(target_os = "macos"))]
pub fn install_reopen_handler(_ctx: &egui::Context) {}
