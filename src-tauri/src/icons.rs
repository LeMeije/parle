//! App-icon switching. Three surfaces, honestly handled:
//! 1. Runtime Dock icon (NSApp.applicationIconImage) — applies instantly, but
//!    an Accessory (menu-bar) app has no Dock tile, so it mostly matters in
//!    onboarding/regular mode.
//! 2. The bundle's icon.icns (Finder, Cmd+Tab) — swapped on disk when the
//!    bundle is writable; needs a restart plus a Finder cache nudge.
//! 3. The in-app brand image — the webview reads the setting directly.

use tauri::{AppHandle, Manager};

/// Returns Ok(true) when a restart is needed to fully apply.
pub fn apply_app_icon(app: &AppHandle, icon_id: &str) -> Result<bool, String> {
    let variant = variant_icns(app, icon_id)?;

    #[cfg(target_os = "macos")]
    {
        // Runtime Dock icon (instant, no restart).
        let path = variant.clone();
        let _ = app.run_on_main_thread(move || unsafe {
            use objc2::msg_send;
            use objc2::runtime::AnyClass;
            use objc2_foundation::NSString;
            let (Some(img_cls), Some(app_cls)) = (AnyClass::get(c"NSImage"), AnyClass::get(c"NSApplication")) else {
                return;
            };
            let ns_path = NSString::from_str(&path.to_string_lossy());
            let img: *mut objc2::runtime::AnyObject = msg_send![img_cls, alloc];
            let img: *mut objc2::runtime::AnyObject = msg_send![img, initWithContentsOfFile: &*ns_path];
            if img.is_null() {
                return;
            }
            let shared: *mut objc2::runtime::AnyObject = msg_send![app_cls, sharedApplication];
            let _: () = msg_send![shared, setApplicationIconImage: img];
        });

        // Bundle icon swap (Finder), when the bundle is writable (dev builds,
        // user-writable installs). Signed installs in /Applications refuse
        // gracefully.
        if let Some(bundle_icns) = bundle_icns_path() {
            match std::fs::copy(&variant, &bundle_icns) {
                Ok(_) => {
                    // Nudge Finder's icon cache by touching the bundle root.
                    if let Some(bundle_root) = bundle_icns.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
                        let _ = std::process::Command::new("touch").arg(bundle_root).status();
                    }
                    return Ok(true);
                }
                Err(e) => {
                    tracing::info!("bundle icon not writable ({e}); runtime icon only");
                    return Ok(false);
                }
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = variant;
        let _ = app;
    }
    Ok(false)
}

fn variant_icns(app: &AppHandle, icon_id: &str) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .resource_dir()
        .map_err(|e| e.to_string())?
        .join("icons/variants");
    let p = dir.join(format!("{icon_id}.icns"));
    if p.exists() {
        Ok(p)
    } else {
        Err(format!("icon variant missing: {}", p.display()))
    }
}

fn bundle_icns_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let contents = exe.parent()?.parent()?;
    let p = contents.join("Resources/icon.icns");
    p.exists().then_some(p)
}
