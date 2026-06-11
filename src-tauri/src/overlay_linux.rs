// overlay_linux.rs
//
// Architecture: ALL overlays use an XWayland subprocess (GDK_BACKEND=x11).
// A Wayland-native Tauri window is NOT used for the overlay because:
//   - Wayland has no standard protocol for input-transparent (click-through) windows
//   - XWayland + EWMH hints is more predictable across DE versions
//
// After the window is mapped, we apply two layers of "stay on top":
//   1. EWMH X11 hints  (universal — via xprop)
//   2. Compositor-specific IPC:
//        KDE      → one-shot KWin D-Bus script (keepAbove=true)
//        Sway     → swaymsg float + sticky
//        Hyprland → hyprctl windowrulev2 float + pin
//        X11 WMs  → EWMH hints alone are sufficient
//
// The optional KWin window rule (kwinrulesrc) is still installable from the UI
// as an extra safety net, but it is no longer the primary KDE mechanism.

use std::process::Command;
use std::time::Duration;
use std::thread;

// ─── Env var keys shared with lib.rs ─────────────────────────────────────────

pub const ENV_OVERLAY_FLAG:       &str = "FRAMEFORGE_OVERLAY";
pub const ENV_OVERLAY_PAYLOAD:    &str = "FRAMEFORGE_OVERLAY_PAYLOAD";
/// Plain-text compositor name passed from parent → subprocess.
pub const ENV_OVERLAY_COMPOSITOR: &str = "FRAMEFORGE_COMPOSITOR";

// ─── Compositor ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Compositor {
    Kde,
    Sway,
    Hyprland,
    Wlroots, // generic wlroots (labwc, wayfire, river, …)
    X11,
    Other,
}

/// Detect the current compositor from environment variables.
pub fn detect_compositor() -> Compositor {
    // Check most-specific first
    if std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        return Compositor::Hyprland;
    }
    if std::env::var("SWAYSOCK").is_ok() {
        return Compositor::Sway;
    }

    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_lowercase();
    let session = std::env::var("XDG_SESSION_TYPE")
        .unwrap_or_default()
        .to_lowercase();

    if desktop.contains("kde") || desktop.contains("plasma") {
        return Compositor::Kde;
    }
    if desktop.contains("sway") {
        return Compositor::Sway;
    }

    // Pure X11 session or display without Wayland
    if session == "x11"
        || (std::env::var("DISPLAY").is_ok()
            && std::env::var("WAYLAND_DISPLAY").is_err())
    {
        return Compositor::X11;
    }

    if session == "wayland" {
        return Compositor::Wlroots;
    }

    Compositor::Other
}

/// Parse the compositor name written by the parent process into ENV_OVERLAY_COMPOSITOR.
pub fn compositor_from_env() -> Compositor {
    match std::env::var(ENV_OVERLAY_COMPOSITOR)
        .unwrap_or_default()
        .as_str()
    {
        "Kde"      => Compositor::Kde,
        "Sway"     => Compositor::Sway,
        "Hyprland" => Compositor::Hyprland,
        "Wlroots"  => Compositor::Wlroots,
        "X11"      => Compositor::X11,
        _          => Compositor::Other,
    }
}

// ─── Overlay payload ─────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct OverlayPayload {
    pub items:           Vec<String>,
    pub positions:       Vec<f32>,
    pub win_w:           u32,
    pub win_h:           u32,
    pub priority:        String,
    pub dismiss_path:    String,
    /// Kept for API / frontend compatibility. No longer used for spawn routing.
    pub kwin_script:     bool,
    pub scanner_enabled: bool,
    /// Fully-resolved reward items (name, plat, ducats, components, ownership),
    /// enriched in the MAIN process where the catalog/prices/quantities live.
    /// The overlay subprocess has no AppState, so it renders these directly
    /// instead of trying (and failing) to resolve the raw paths itself.
    /// Opaque JSON: the shape is defined by the TS RewardItem type. Optional —
    /// defaults to null when absent (e.g. legacy callers).
    #[serde(default)]
    pub rewards:         serde_json::Value,
    /// In-game Warframe UI scale as a fraction (1.0 = 100%). Lets the overlay
    /// match smaller card layouts. Defaults to 1.0 for legacy callers.
    #[serde(default = "default_ui_scale")]
    pub ui_scale:        f32,
}

fn default_ui_scale() -> f32 { 1.0 }

// ─── Return type ─────────────────────────────────────────────────────────────

#[derive(serde::Serialize, Clone)]
pub enum OverlayMethod {
    Subprocess,
    LayerShell, // reserved for future gtk-layer-shell path
}

// ─── Subprocess spawn ─────────────────────────────────────────────────────────

/// Spawn the overlay as an XWayland subprocess.
///
/// GDK_BACKEND=x11 forces GTK / WebKitGTK to use the X11 backend,
/// producing an XWayland surface regardless of whether the session is
/// X11 or Wayland. The compositor type is propagated via an env var so
/// the child can run the right IPC hooks once its window is mapped.
pub fn spawn_overlay_subprocess(payload: &OverlayPayload) -> Result<u32, String> {
    let json = serde_json::to_string(payload)
        .map_err(|e| format!("serialize overlay payload: {e}"))?;

    let payload_path = std::env::temp_dir()
        .join("frameforge_overlay_payload.json");
    std::fs::write(&payload_path, &json)
        .map_err(|e| format!("write overlay payload: {e}"))?;

    // Clear any stale dismiss signal so the new overlay doesn't exit instantly
    let _ = std::fs::remove_file(
        std::env::temp_dir().join("frameforge_overlay_dismiss"),
    );
    // Clear any stale price-enrichment file from a previous reward screen so the
    // overlay doesn't briefly show the last screen's prices before its own arrive.
    let _ = std::fs::remove_file(
        std::env::temp_dir().join("frameforge_overlay_enriched.json"),
    );

    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot find own executable: {e}"))?;

    let compositor = detect_compositor();
    // Format::Debug gives us "Kde", "Sway", etc. — no serde quoting needed
    let compositor_str = format!("{compositor:?}");

    let mut cmd = Command::new(&exe);
    cmd.env(ENV_OVERLAY_FLAG, "1")
       .env(ENV_OVERLAY_PAYLOAD, &payload_path)
       .env(ENV_OVERLAY_COMPOSITOR, &compositor_str)
       // Force X11 backend → XWayland window (works on both X11 and Wayland sessions)
       .env("GDK_BACKEND", "x11")
       .stdout(std::process::Stdio::null());

    // Redirect stderr to a rotating log so subprocess panics / GTK errors are visible.
    let stderr_path = std::env::temp_dir().join("frameforge_overlay_stderr.log");
    let stderr_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stderr_path)
        .ok();
    if let Some(f) = stderr_file {
        cmd.stderr(std::process::Stdio::from(f));
    } else {
        cmd.stderr(std::process::Stdio::null());
    }

    // Make sure DISPLAY is set for XWayland; most DEs auto-start Xwayland and
    // set DISPLAY, but if we're in a bare Wayland session it may be absent.
    if std::env::var("DISPLAY").is_err() {
        cmd.env("DISPLAY", ":0");
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn overlay subprocess: {e}"))?;

    eprintln!("[FF overlay] spawned subprocess pid={}", child.id());

    Ok(child.id())
}

// ─── X11 / EWMH hints ────────────────────────────────────────────────────────

/// Apply EWMH window type + state hints so the XWayland window floats above
/// all other windows, including fullscreen ones.
///
/// **KDE layer ordering** (ascending):
///   NormalLayer < AboveLayer (keepAbove/Dock) < FullscreenLayer
///       < CriticalNotificationLayer < OverlayLayer
///
/// Using `_NET_WM_WINDOW_TYPE_DOCK` only reaches AboveLayer — below
/// FullscreenLayer — so the overlay disappears under a fullscreen Warframe.
/// Using `_KDE_NET_WM_WINDOW_TYPE_CRITICAL_NOTIFICATION` reaches
/// CriticalNotificationLayer, which is above FullscreenLayer.
///
/// On all other compositors the standard DOCK type is sufficient since
/// those either float XWayland overlays freely or handle it via swaymsg/hyprctl.
pub fn apply_x11_hints(xid: Option<u64>, title: &str, compositor: Compositor) {
    let id_str: String;
    let target: Vec<&str> = if let Some(id) = xid {
        id_str = format!("0x{id:x}");
        vec!["-id", &id_str]
    } else if let Some(found) = xdotool_find(title) {
        id_str = format!("0x{found:x}");
        vec!["-id", &id_str]
    } else {
        vec!["-name", title]
    };

    // On KDE use the critical-notification type which maps to
    // CriticalNotificationLayer — above fullscreen windows.
    // On other compositors use DOCK which is above normal windows.
    let window_type = match compositor {
        Compositor::Kde => "_KDE_NET_WM_WINDOW_TYPE_CRITICAL_NOTIFICATION",
        _               => "_NET_WM_WINDOW_TYPE_DOCK",
    };
    xprop_set(&target, "_NET_WM_WINDOW_TYPE", "32a", window_type);

    // ABOVE + STAYS_ON_TOP as belt-and-suspenders; skip taskbar and pager
    xprop_set(&target, "_NET_WM_STATE", "32a",
              "_NET_WM_STATE_ABOVE,_NET_WM_STATE_STAYS_ON_TOP,\
               _NET_WM_STATE_SKIP_TASKBAR,_NET_WM_STATE_SKIP_PAGER");

    // Remove decorations at the WM level (in addition to Tauri's decorations=false)
    xprop_set(&target, "_MOTIF_WM_HINTS", "32c", "2, 0, 0, 0, 0");

    // Hint to skip compositor's own pass (reduces latency/stutter on some setups)
    xprop_set(&target, "_NET_WM_BYPASS_COMPOSITOR", "32c", "2");

    // ── Focus-steal prevention ───────────────────────────────────────────────
    // Without this the WM gives the freshly-mapped XWayland window keyboard focus,
    // pulling it away from the game until the user clicks back. _NET_WM_USER_TIME=0
    // marks the map as not user-initiated "now", so EWMH WMs (KWin/Mutter) decline
    // to focus it on map.
    // (We deliberately do NOT clobber WM_HINTS here: xprop can only write it as type
    // CARDINAL, but ICCCM requires type WM_HINTS — XGetWMHints would reject it and
    // the WM would fall back to the default input=True, the opposite of what we want.
    // GTK already publishes a valid WM_HINTS; we leave it intact and rely on the
    // user-time hint plus refocus_warframe() below.)
    xprop_set(&target, "_NET_WM_USER_TIME", "32c", "0");
}

/// Return input focus to the Warframe window. The overlay is always-on-top, so it
/// stays visually above even after Warframe regains focus — this just stops the
/// freshly-mapped overlay from holding keyboard focus, so the player never has to
/// click back on the game.
pub fn refocus_warframe() {
    // Non-blocking search: do NOT use `--sync` (it would block forever if the game
    // window isn't found, hanging this thread).
    let out = Command::new("xdotool")
        .args(["search", "--name", "Warframe"])
        .output();
    let Ok(out) = out else { return };
    if let Some(id) = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        let _ = Command::new("xdotool").args(["windowactivate", &id]).output();
    }
}

fn xprop_set(target: &[&str], prop: &str, fmt: &str, value: &str) {
    let mut args: Vec<&str> = target.to_vec();
    args.extend(["-f", prop, fmt, "-set", prop, value]);
    let _ = Command::new("xprop").args(&args).output();
}

/// Find a window by title using xdotool (returns X11 window ID).
fn xdotool_find(title: &str) -> Option<u64> {
    let out = Command::new("xdotool")
        .args(["search", "--sync", "--name", title])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .lines()
        .next()
        .and_then(|s| s.trim().parse::<u64>().ok())
}

// ─── Compositor-specific IPC ──────────────────────────────────────────────────

/// Run compositor IPC so the window is kept above all others.
/// Call this from a background thread after the window is mapped.
/// `title` is the window title used to target the right window (so this works
/// for both the relic overlay "FrameForge Overlay" and the riven overlay
/// "FrameForge Riven").
pub fn apply_compositor_hooks(compositor: Compositor, _xid: Option<u64>, title: &str) {
    match compositor {
        Compositor::Kde      => kde_keepabove(title),
        Compositor::Sway     => sway_float_sticky(title),
        Compositor::Hyprland => hyprland_pin(title),
        // X11 WMs, generic wlroots, unknown: EWMH hints are sufficient
        _ => {}
    }
}

// ── KDE: one-shot KWin D-Bus scripting ───────────────────────────────────────

/// Run a temporary KWin script that sets keepAbove=true on our overlay window.
///
/// Supports both:
///   Plasma 5 → qdbus  org.kde.KWin /Scripting loadScript / Script.run
///   Plasma 6 → gdbus  call --method org.kde.kwin.Scripting.loadScript
fn kde_keepabove(title: &str) {
    // The script handles both the "window already exists" and "window appears
    // after the script runs" cases, and works with both Plasma 5 and 6 APIs.
    let script_tmpl = r#"
(function() {
    var TITLE = "__FF_TITLE__";

    function applyToWindow(w) {
        if (!w) return false;
        var cap = w.caption || w.title || "";
        if (cap.indexOf(TITLE) === -1) return false;
        try { w.keepAbove     = true;  } catch(e) {}
        try { w.keepBelow     = false; } catch(e) {}
        try { w.noBorder      = true;  } catch(e) {}
        try { w.skipTaskbar   = true;  } catch(e) {}
        try { w.skipPager     = true;  } catch(e) {}
        try { w.skipSwitcher  = true;  } catch(e) {}
        try { w.onAllDesktops = true;  } catch(e) {}
        return true;
    }

    function applyToAll() {
        var wins = null;
        // Plasma 6
        if (typeof workspace.windowList === 'function') wins = workspace.windowList();
        else if (typeof workspace.windows !== 'undefined') wins = workspace.windows;
        // Plasma 5
        if (!wins && typeof workspace.clientList === 'function') wins = workspace.clientList();
        if (!wins) return;
        for (var i = 0; i < (wins.length || 0); i++) applyToWindow(wins[i]);
    }

    applyToAll();

    // Hook future windows in case ours isn't mapped yet
    var hook = function(w) { applyToWindow(w); };
    if (workspace.windowAdded) workspace.windowAdded.connect(hook);
    if (workspace.clientAdded) workspace.clientAdded.connect(hook);
})();
"#;

    // Derive a per-title temp file + script id so a relic and riven keep-above
    // never clash on the same KWin script name.
    let slug: String = title.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
    let script = script_tmpl.replace("__FF_TITLE__", title);
    let script_path = std::env::temp_dir().join(format!("ff_kwin_{slug}.js"));
    if std::fs::write(&script_path, &script).is_err() {
        return;
    }
    let sp = script_path.to_string_lossy().to_string();
    let script_id = format!("ff-{slug}-once");

    // ── Plasma 5: qdbus ──────────────────────────────────────────────────────
    let id_p5: Option<i32> = Command::new("qdbus")
        .args([
            "org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting.loadScript",
            &sp,
            &script_id,
        ])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok());

    if let Some(id) = id_p5.filter(|&id| id >= 0) {
        let path = format!("/{id}");
        let _ = Command::new("qdbus")
            .args(["org.kde.KWin", &path, "org.kde.kwin.Script.run"])
            .output();
        thread::sleep(Duration::from_millis(250));
        let _ = Command::new("qdbus")
            .args(["org.kde.KWin", &path, "org.kde.kwin.Script.stop"])
            .output();
        let _ = Command::new("qdbus")
            .args([
                "org.kde.KWin",
                "/Scripting",
                "org.kde.kwin.Scripting.unloadScript",
                &script_id,
            ])
            .output();
        return;
    }

    // ── Plasma 6: gdbus ──────────────────────────────────────────────────────
    let gdbus_out = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.kde.KWin",
            "--object-path",
            "/Scripting",
            "--method",
            "org.kde.kwin.Scripting.loadScript",
            &sp,
            &script_id,
        ])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok());

    // gdbus prints "(int32 N,)" — pull out the number
    let id_p6: Option<i32> = gdbus_out
        .as_deref()
        .map(|s| s.trim().trim_start_matches('(').trim_end_matches(')').trim())
        .and_then(|s| s.trim_end_matches(',').trim().parse().ok());

    if let Some(id) = id_p6.filter(|&id| id >= 0) {
        let path = format!("/{id}");
        let _ = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.kde.KWin",
                "--object-path",
                &path,
                "--method",
                "org.kde.kwin.Script.run",
            ])
            .output();
        thread::sleep(Duration::from_millis(250));
        let _ = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.kde.KWin",
                "--object-path",
                &path,
                "--method",
                "org.kde.kwin.Script.stop",
            ])
            .output();
    } else {
        // Older Plasma 6 builds: try the "start" method
        let _ = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.kde.KWin",
                "--object-path",
                "/Scripting",
                "--method",
                "org.kde.kwin.Scripting.start",
            ])
            .output();
    }
}

// ── Sway: float + sticky ─────────────────────────────────────────────────────

fn sway_float_sticky(title: &str) {
    // Try multiple match criteria in case title or app_id differs at runtime.
    // Tauri on Linux sets app_id to the last segment of `identifier`
    // (tauri.conf.json: "com.jochem.frameforge" → app_id "frameforge").
    let title_criteria = format!(r#"[title="{title}"]"#);
    let criteria_list: [&str; 3] = [
        title_criteria.as_str(),
        r#"[app_id="frameforge"]"#,
        r#"[app_id="warframe-companion"]"#,
    ];

    for criteria in &criteria_list {
        let cmd = format!(
            "{criteria} floating enable; {criteria} sticky enable; {criteria} border none"
        );
        let ok = Command::new("swaymsg")
            .arg(&cmd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            break;
        }
    }
}

// ── Hyprland: windowrulev2 float + pin ───────────────────────────────────────

fn hyprland_pin(title: &str) {
    // Pre-register window rules so the window gets them on first appearance.
    let title_sel = format!("title:{title}");
    let ts = title_sel.as_str();
    let rules: [(&str, &str); 9] = [
        ("float",    ts),
        ("pin",      ts),
        ("noborder", ts),
        ("nofocus",  ts),
        ("noshadow", ts),
        // Class-based fallbacks
        ("float",    "class:frameforge"),
        ("pin",      "class:frameforge"),
        ("float",    "class:warframe-companion"),
        ("pin",      "class:warframe-companion"),
    ];

    for (rule, selector) in &rules {
        let _ = Command::new("hyprctl")
            .args(["keyword", "windowrulev2", &format!("{rule},{selector}")])
            .output();
    }

    // Also send a dispatch pin once the window is likely mapped
    thread::sleep(Duration::from_millis(300));
    let _ = Command::new("hyprctl")
        .args(["dispatch", "pin", ts])
        .output();
}

// ─── Dismiss signal ───────────────────────────────────────────────────────────

/// Write a dismiss signal file. The overlay subprocess polls for this
/// and exits when it appears.
pub fn signal_overlay_dismiss() {
    let path = std::env::temp_dir().join("frameforge_overlay_dismiss");
    let _ = std::fs::write(&path, "");
}

// ─── (KWin window rule integration removed) ──────────────────────────────────
// The kwinrulesrc approach has been removed. KDE overlay behaviour is handled
// entirely by the one-shot KWin D-Bus script in kde_keepabove() above.
//
// Overlay subprocess stub commands are implemented as a plain closure in
// run_overlay() (lib.rs) to avoid the #[tauri::command] macro name conflict
// that would arise from defining identically-named commands in two places
// within the same crate.
