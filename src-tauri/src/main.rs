// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // On a Wayland session, run the GUI through XWayland so the compositor draws
    // NATIVE server-side decorations (e.g. KDE/Plasma Breeze titlebars). tao —
    // Tauri's windowing layer — otherwise forces its own GNOME-style client-side
    // titlebar on Wayland, ignoring the desktop's window decorations. The overlay
    // subprocess already forces GDK_BACKEND=x11; setting it here gives the main
    // window native titlebars too. X11 sessions are already x11 (no-op), and an
    // explicit user-set GDK_BACKEND is always respected.
    #[cfg(not(target_os = "windows"))]
    if std::env::var_os("GDK_BACKEND").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_some() {
        std::env::set_var("GDK_BACKEND", "x11");
    }

    #[cfg(not(target_os = "windows"))]
    {
        if std::env::var("FRAMEFORGE_OVERLAY").as_deref() == Ok("1") {
            return warframe_companion_lib::run_overlay();
        }

        if args.contains(&"--test-overlay".to_string()) {
            // Write a test payload and spawn the overlay subprocess.
            // Using spawn_overlay_subprocess() ensures FRAMEFORGE_COMPOSITOR
            // is set correctly so compositor IPC hooks run in the child.
            let payload = warframe_companion_lib::overlay_linux::OverlayPayload {
                items: vec![
                    "/Lotus/Types/Recipes/WarframeRecipes/GaussPrimeBlueprint".to_string(),
                    "/Lotus/Types/Recipes/Weapons/WeaponParts/AcceltraPrimeBarrel".to_string(),
                    "/Lotus/Types/Recipes/Weapons/WeaponParts/AkariusPrimeReceiver".to_string(),
                    "/Lotus/Types/Recipes/WarframeRecipes/NezhaPrimeSystemsComponent".to_string(),
                ],
                positions: vec![0.31, 0.44, 0.56, 0.69],
                win_w: 1920,
                win_h: 1080,
                priority: "completion".to_string(),
                dismiss_path: std::env::temp_dir()
                    .join("frameforge_overlay_dismiss")
                    .to_string_lossy()
                    .to_string(),
                kwin_script: false,
                scanner_enabled: true,
                rewards: serde_json::Value::Null,
                ui_scale: 1.0,
            };
            let _ = warframe_companion_lib::overlay_linux::spawn_overlay_subprocess(&payload);

            // Continue to run the main app normally
        }
    }

    warframe_companion_lib::run()
}
