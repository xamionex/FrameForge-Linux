/// Screen capture + Windows OCR for Warframe relic reward detection.
///
/// Capture strategy (automatic, works for all display modes):
///   1. PrintWindow (GDI) — fast, window-specific, works for Windowed and Borderless Windowed.
///      Quick brightness check: if the result is dark (avg < 30) the game is almost certainly
///      in Fullscreen Exclusive mode and GDI can't reach the DX framebuffer.
///   2. DXGI Desktop Duplication — captures the display output at hardware level, bypasses DWM.
///      Works for Fullscreen Exclusive, Borderless Windowed, and Windowed.
///      The correct monitor is chosen dynamically: whichever monitor the Warframe window is on.

// ─── Screenshot ───────────────────────────────────────────────────────────────

/// Compute average pixel brightness from a BGRA buffer (sampled every 64 pixels).
fn avg_brightness(pixels: &[u8]) -> u32 {
    let sum: u32 = pixels.chunks_exact(4).step_by(64)
        .map(|p| (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3)
        .sum();
    sum / (pixels.len() / 4 / 64).max(1) as u32
}

/// Strip this fraction of the full window height from the top before OCR.
/// Removes HUD overlays (FPS counters, ping displays, Nvidia/AMD overlays).
const OCR_SKIP_TOP_FRAC: f32 = 0.10;

/// Strip this fraction of the full window height from the bottom before OCR.
/// Removes objective trackers, chat, ability bars, and other bottom-HUD elements.
const OCR_SKIP_BOTTOM_FRAC: f32 = 0.10;

/// Crop the top N rows off a pixel buffer. Returns (cropped_pixels, new_cap_h).
fn crop_top_strip(pixels: Vec<u8>, w: u32, cap_h: u32, full_h: u32, skip_frac: f32) -> (Vec<u8>, u32) {
    let skip_rows = ((full_h as f32 * skip_frac) as u32).min(cap_h.saturating_sub(1));
    let skip_bytes = (skip_rows * w * 4) as usize;
    if skip_bytes == 0 || skip_bytes >= pixels.len() {
        return (pixels, cap_h);
    }
    (pixels[skip_bytes..].to_vec(), cap_h - skip_rows)
}

/// Crop the bottom N rows off a pixel buffer. Returns (cropped_pixels, new_cap_h).
fn crop_bottom_strip(pixels: Vec<u8>, w: u32, cap_h: u32, full_h: u32, skip_frac: f32) -> (Vec<u8>, u32) {
    let drop_rows = ((full_h as f32 * skip_frac) as u32).min(cap_h.saturating_sub(1));
    let keep_rows = cap_h.saturating_sub(drop_rows);
    if keep_rows == 0 || keep_rows == cap_h {
        return (pixels, cap_h);
    }
    let keep_bytes = (keep_rows * w * 4) as usize;
    (pixels[..keep_bytes.min(pixels.len())].to_vec(), keep_rows)
}

/// Main entry point. Tries PrintWindow first, falls back to DXGI if the frame is dark.
/// Returns (BGRA pixels, width, captured_height, full_height, capture_info).
/// captured_height covers y=[10%, 38%] of the full window — top and bottom 10% are
/// stripped to exclude HUD overlays (FPS/ping counters, ability bars, chat) from OCR.
/// capture_info describes which path was used and the pixel brightness, for session logging.
#[cfg(target_os = "windows")]
pub fn capture_warframe_reward_area() -> Option<(Vec<u8>, u32, u32, u32, String)> {
    // ── Path A: PrintWindow (Windowed / Borderless Windowed) ──────────────────
    if let Some((pixels, w, cap_h, full_h)) = capture_printwindow() {
        let (pixels, cap_h) = crop_top_strip(pixels, w, cap_h, full_h, OCR_SKIP_TOP_FRAC);
        let (pixels, cap_h) = crop_bottom_strip(pixels, w, cap_h, full_h, OCR_SKIP_BOTTOM_FRAC);
        let avg = avg_brightness(&pixels);
        if avg >= 20 {
            let info = format!("PrintWindow  {}×{}px (skip 10%/10%, cap {}px)  avg_brightness={}", w, full_h, cap_h, avg);
            return Some((pixels, w, cap_h, full_h, info));
        }
        // Dark frame — Fullscreen Exclusive likely. Fall through to DXGI.
        let _ = avg;
        if let Some((px2, w2, cap_h2, full_h2)) = capture_dxgi(0.48) {
            let (px2, cap_h2) = crop_top_strip(px2, w2, cap_h2, full_h2, OCR_SKIP_TOP_FRAC);
            let (px2, cap_h2) = crop_bottom_strip(px2, w2, cap_h2, full_h2, OCR_SKIP_BOTTOM_FRAC);
            let avg2 = avg_brightness(&px2);
            let info = format!(
                "DXGI  {}×{}px (skip 10%/10%, cap {}px)  avg_brightness={} \
                 (PrintWindow was dark: avg={})",
                w2, full_h2, cap_h2, avg2, avg
            );
            return Some((px2, w2, cap_h2, full_h2, info));
        }
        // Both paths failed — return the dark PrintWindow result so the caller
        // can classify it as dark-frame and log it properly.
        let info = format!(
            "PrintWindow  {}×{}px (skip 10%/10%, cap {}px)  avg_brightness={} [DARK — DXGI also failed]",
            w, full_h, cap_h, avg
        );
        return Some((pixels, w, cap_h, full_h, info));
    }

    // PrintWindow found no window (Warframe not running?) — try DXGI anyway
    if let Some((pixels, w, cap_h, full_h)) = capture_dxgi(0.48) {
        let (pixels, cap_h) = crop_top_strip(pixels, w, cap_h, full_h, OCR_SKIP_TOP_FRAC);
        let (pixels, cap_h) = crop_bottom_strip(pixels, w, cap_h, full_h, OCR_SKIP_BOTTOM_FRAC);
        let avg = avg_brightness(&pixels);
        let info = format!(
            "DXGI  {}×{}px (skip 10%/10%, cap {}px)  avg_brightness={} (no Warframe window found)",
            w, full_h, cap_h, avg
        );
        return Some((pixels, w, cap_h, full_h, info));
    }

    None
}

/// GDI PrintWindow capture — works for Windowed and Borderless Windowed.
#[cfg(target_os = "windows")]
fn capture_printwindow() -> Option<(Vec<u8>, u32, u32, u32)> {
    use std::mem;
    use windows_sys::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
            GetDIBits, GetDC, ReleaseDC, SelectObject,
            BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, RGBQUAD,
        },
        UI::WindowsAndMessaging::{FindWindowW, GetClientRect},
    };
    #[link(name = "user32")]
    extern "system" { fn PrintWindow(hwnd: isize, hdcblt: isize, nflags: u32) -> i32; }
    const PW_RENDERFULLCONTENT: u32 = 2;

    unsafe {
        let title: Vec<u16> = "Warframe\0".encode_utf16().collect();
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if hwnd == 0 { return None; }

        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetClientRect(hwnd, &mut rect);
        let full_w = (rect.right - rect.left) as u32;
        let full_h = (rect.bottom - rect.top) as u32;
        if full_w < 100 || full_h < 100 { return None; }

        let cap_h = (full_h as f32 * 0.48) as u32;

        let hdc_win = GetDC(hwnd);
        let hdc_mem = CreateCompatibleDC(hdc_win);
        let hbm     = CreateCompatibleBitmap(hdc_win, full_w as i32, full_h as i32);
        let hbm_old = SelectObject(hdc_mem, hbm);

        PrintWindow(hwnd, hdc_mem, PW_RENDERFULLCONTENT);

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize:          mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth:         full_w as i32,
                biHeight:        -(cap_h as i32),
                biPlanes:        1,
                biBitCount:      32,
                biCompression:   BI_RGB,
                biSizeImage:     0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed:       0,
                biClrImportant:  0,
            },
            bmiColors: [RGBQUAD { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbReserved: 0 }],
        };
        let mut pixels = vec![0u8; (full_w * cap_h * 4) as usize];
        GetDIBits(hdc_mem, hbm, 0, cap_h, pixels.as_mut_ptr() as *mut _, &mut bmi, DIB_RGB_COLORS);

        SelectObject(hdc_mem, hbm_old);
        DeleteObject(hbm);
        DeleteDC(hdc_mem);
        ReleaseDC(hwnd, hdc_win);

        Some((pixels, full_w, cap_h, full_h))
    }
}

/// Capture a vertical slice of the Warframe window and run OCR on it.
/// y_start / y_end are fractions of the full window height (0.0–1.0).
/// Returns the raw OCR text.
/// Capture the Warframe window using PrintWindow and return raw BGRA pixels + dimensions.
/// Single capture can be reused for multiple OCR regions via `ocr_pixels_rect`.
#[cfg(target_os = "windows")]
pub fn capture_warframe_pixels() -> Result<(Vec<u8>, u32, u32), String> {
    use std::mem;
    use windows_sys::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
            GetDIBits, GetDC, ReleaseDC, SelectObject,
            BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, RGBQUAD,
        },
        UI::WindowsAndMessaging::{FindWindowW, GetClientRect},
    };
    #[link(name = "user32")]
    extern "system" { fn PrintWindow(hwnd: isize, hdcblt: isize, nflags: u32) -> i32; }
    const PW_RENDERFULLCONTENT: u32 = 2;

    unsafe {
        let title: Vec<u16> = "Warframe\0".encode_utf16().collect();
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if hwnd == 0 { return Err("Warframe window not found".into()); }

        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetClientRect(hwnd, &mut rect);
        let full_w = (rect.right  - rect.left) as u32;
        let full_h = (rect.bottom - rect.top)  as u32;
        if full_w < 100 || full_h < 100 { return Err("Window too small".into()); }

        let hdc_win = GetDC(hwnd);
        let hdc_mem = CreateCompatibleDC(hdc_win);
        let hbm     = CreateCompatibleBitmap(hdc_win, full_w as i32, full_h as i32);
        let hbm_old = SelectObject(hdc_mem, hbm);
        PrintWindow(hwnd, hdc_mem, PW_RENDERFULLCONTENT);

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: full_w as i32,
                biHeight: -(full_h as i32),
                biPlanes: 1, biBitCount: 32, biCompression: BI_RGB,
                biSizeImage: 0, biXPelsPerMeter: 0, biYPelsPerMeter: 0,
                biClrUsed: 0, biClrImportant: 0,
            },
            bmiColors: [RGBQUAD { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbReserved: 0 }],
        };
        let mut pixels = vec![0u8; (full_w * full_h * 4) as usize];
        GetDIBits(hdc_mem, hbm, 0, full_h,
                  pixels.as_mut_ptr() as *mut _, &mut bmi, DIB_RGB_COLORS);
        SelectObject(hdc_mem, hbm_old);
        DeleteObject(hbm);
        DeleteDC(hdc_mem);
        ReleaseDC(hwnd, hdc_win);
        Ok((pixels, full_w, full_h))
    }
}

/// 2× nearest-neighbour upscale + contrast stretch on BGRA pixels.
/// WinRT OCR accuracy improves significantly on larger, high-contrast images.
/// Grayscale + contrast stretch on BGRA pixels.
/// Converting to grayscale is the key step: element icons (❄ Cold, 🔥 Heat, ☠ Toxin)
/// are colored glyphs — in the original BGRA image WinRT OCR rejects these lines as
/// graphics. After grayscale they become neutral-brightness shapes, so OCR reads the
/// white text on either side of the icon instead of dropping the whole line.
fn preprocess_for_ocr(pixels: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, u32) {
    let mut out = pixels.to_vec();
    for px in out.chunks_mut(4) {
        // Standard luminance: 0.299 R + 0.587 G + 0.114 B (BGRA order)
        let gray = ((px[2] as u32 * 299 + px[1] as u32 * 587 + px[0] as u32 * 114) / 1000)
            .min(255) as u8;
        // Mild contrast stretch [20, 235] → [0, 255]
        let v = ((gray as i32 - 20) * 255 / 215).clamp(0, 255) as u8;
        px[0] = v;
        px[1] = v;
        px[2] = v;
    }
    (out, width, height)
}

/// OCR a rectangle from a pre-captured pixel buffer. All coordinates are 0.0–1.0 fractions.
/// Applies a mild contrast stretch before OCR (no upscaling — upscaling distorts numerals).
#[cfg(target_os = "windows")]
pub fn ocr_pixels_rect(
    pixels: &[u8], full_w: u32, full_h: u32,
    x_start: f32, x_end: f32, y_start: f32, y_end: f32,
) -> Result<String, String> {
    let col_s = (full_w as f32 * x_start.clamp(0.0, 1.0)) as usize;
    let col_e = ((full_w as f32 * x_end.clamp(0.0, 1.0)) as usize).min(full_w as usize);
    let row_s = (full_h as f32 * y_start.clamp(0.0, 1.0)) as usize;
    let row_e = ((full_h as f32 * y_end.clamp(0.0, 1.0)) as usize).min(full_h as usize);
    let rect_w = (col_e - col_s) as u32;
    let rect_h = (row_e - row_s) as u32;
    if rect_w < 4 || rect_h < 4 { return Err("Region too small".into()); }

    let src_stride  = full_w as usize * 4;
    let dst_stride  = rect_w as usize * 4;
    let mut cropped = vec![0u8; dst_stride * rect_h as usize];
    for row in 0..rect_h as usize {
        let src = (row_s + row) * src_stride + col_s * 4;
        let dst = row * dst_stride;
        cropped[dst..dst + dst_stride].copy_from_slice(&pixels[src..src + dst_stride]);
    }

    let (enhanced, ew, eh) = preprocess_for_ocr(&cropped, rect_w, rect_h);
    let bmp = to_bmp(&enhanced, ew, eh);
    let _ = eh;
    run_ocr(bmp, ew).map(|(text, _)| text)
}

/// OCR a rectangle WITHOUT preprocessing — for white-on-dark text that OCRs fine raw.
#[cfg(target_os = "windows")]
pub fn ocr_pixels_rect_raw(
    pixels: &[u8], full_w: u32, full_h: u32,
    x_start: f32, x_end: f32, y_start: f32, y_end: f32,
) -> Result<String, String> {
    let col_s = (full_w as f32 * x_start.clamp(0.0, 1.0)) as usize;
    let col_e = ((full_w as f32 * x_end.clamp(0.0, 1.0)) as usize).min(full_w as usize);
    let row_s = (full_h as f32 * y_start.clamp(0.0, 1.0)) as usize;
    let row_e = ((full_h as f32 * y_end.clamp(0.0, 1.0)) as usize).min(full_h as usize);
    let rect_w = (col_e - col_s) as u32;
    let rect_h = (row_e - row_s) as u32;
    if rect_w < 4 || rect_h < 4 { return Err("Region too small".into()); }
    let src_stride = full_w as usize * 4;
    let dst_stride = rect_w as usize * 4;
    let mut cropped = vec![0u8; dst_stride * rect_h as usize];
    for row in 0..rect_h as usize {
        let src = (row_s + row) * src_stride + col_s * 4;
        let dst = row * dst_stride;
        cropped[dst..dst + dst_stride].copy_from_slice(&pixels[src..src + dst_stride]);
    }
    let bmp = to_bmp(&cropped, rect_w, rect_h);
    run_ocr(bmp, rect_w).map(|(text, _)| text)
}

/// Convenience: capture + OCR a vertical strip of the window (full width).
#[allow(dead_code)]
pub fn capture_and_ocr_region(y_start: f32, y_end: f32) -> Result<String, String> {
    let (pixels, w, h) = capture_warframe_pixels()?;
    ocr_pixels_rect(&pixels, w, h, 0.0, 1.0, y_start, y_end)
}

/// Convenience: capture + OCR a specific rectangle.
#[allow(dead_code)]
pub fn capture_rect_and_ocr(x_start: f32, x_end: f32, y_start: f32, y_end: f32) -> Result<String, String> {
    let (pixels, w, h) = capture_warframe_pixels()?;
    ocr_pixels_rect(&pixels, w, h, x_start, x_end, y_start, y_end)
}

/// Captures the full Warframe window region from the desktop using GDI BitBlt.
/// Because this reads the composited desktop surface (not the window in isolation),
/// any Tauri overlay window sitting on top is included in the result.
/// Falls back to full-monitor DXGI if the frame is dark (fullscreen exclusive mode).
/// Returns BGRA pixels, width, height.
#[cfg(target_os = "windows")]
pub fn capture_screen_for_diagnostics() -> Result<(Vec<u8>, u32, u32), String> {
    if let Some((pixels, w, h)) = capture_screen_gdi() {
        if avg_brightness(&pixels) >= 10 {
            return Ok((pixels, w, h));
        }
    }
    // Dark — fullscreen exclusive. Fall back to full-height DXGI.
    match capture_dxgi(1.0) {
        Some((pixels, w, _cap_h, full_h)) => Ok((pixels, w, full_h)),
        None => Err("Warframe window not found or capture failed".into()),
    }
}

/// GDI BitBlt from the desktop DC covering the Warframe window's screen rectangle.
/// DWM composites all windows before BitBlt reads them, so overlay windows appear.
#[cfg(target_os = "windows")]
fn capture_screen_gdi() -> Option<(Vec<u8>, u32, u32)> {
    use std::mem;
    use windows_sys::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{
            BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
            GetDC, GetDIBits, ReleaseDC, SelectObject,
            BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, RGBQUAD,
            SRCCOPY,
        },
        UI::WindowsAndMessaging::{FindWindowW, GetWindowRect},
    };
    unsafe {
        let title: Vec<u16> = "Warframe\0".encode_utf16().collect();
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if hwnd == 0 { return None; }

        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetWindowRect(hwnd, &mut rect);
        let w = (rect.right  - rect.left) as u32;
        let h = (rect.bottom - rect.top)  as u32;
        if w < 100 || h < 100 { return None; }

        let hdc_screen = GetDC(0);
        let hdc_mem    = CreateCompatibleDC(hdc_screen);
        let hbm        = CreateCompatibleBitmap(hdc_screen, w as i32, h as i32);
        let hbm_old    = SelectObject(hdc_mem, hbm);

        BitBlt(hdc_mem, 0, 0, w as i32, h as i32,
               hdc_screen, rect.left, rect.top, SRCCOPY);

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize:          mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth:         w as i32,
                biHeight:        -(h as i32), // negative = top-down row order
                biPlanes:        1,
                biBitCount:      32,
                biCompression:   BI_RGB,
                biSizeImage:     0, biXPelsPerMeter: 0, biYPelsPerMeter: 0,
                biClrUsed:       0, biClrImportant:  0,
            },
            bmiColors: [RGBQUAD { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbReserved: 0 }],
        };
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        GetDIBits(hdc_mem, hbm, 0, h,
                  pixels.as_mut_ptr() as *mut _, &mut bmi, DIB_RGB_COLORS);

        SelectObject(hdc_mem, hbm_old);
        DeleteObject(hbm);
        DeleteDC(hdc_mem);
        ReleaseDC(0, hdc_screen);
        Some((pixels, w, h))
    }
}

/// DXGI Desktop Duplication capture — works for Fullscreen Exclusive (and all other modes).
///
/// Dynamically determines which monitor the Warframe window is on so this works correctly
/// for any number of monitors, any primary/secondary arrangement, and any resolution.
/// Falls back to the primary monitor if the Warframe window can't be found.
#[cfg(target_os = "windows")]
fn capture_dxgi(cap_frac: f32) -> Option<(Vec<u8>, u32, u32, u32)> {
    use windows::core::Interface; // required for .cast() on COM types
    use windows::Win32::Graphics::{
        Direct3D::D3D_DRIVER_TYPE_HARDWARE,
        Direct3D11::{
            D3D11CreateDevice, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ,
            D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
            ID3D11Resource, ID3D11Texture2D, D3D11_MAPPED_SUBRESOURCE,
        },
        Dxgi::{
            CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
            IDXGIResource, DXGI_OUTDUPL_FRAME_INFO,
        },
        Dxgi::Common::DXGI_SAMPLE_DESC,
    };

    // In fullscreen exclusive mode, DuplicateOutput only succeeds for the output
    // that the game has exclusive ownership of. We use this to find the correct
    // monitor automatically — no GetDesc() or HMONITOR matching needed.
    //
    // For borderless/windowed games, PrintWindow already handled capture above;
    // we only reach this code when PrintWindow returned a dark frame.
    unsafe {
        // D3D11 device — required by DuplicateOutput
        let mut device = None;
        let mut ctx    = None;
        D3D11CreateDevice(
            None, D3D_DRIVER_TYPE_HARDWARE, None,
            Default::default(), None,
            7, // D3D11_SDK_VERSION
            Some(&mut device), None, Some(&mut ctx),
        ).ok()?;
        let device = device?;
        let ctx    = ctx?;
        let unk: windows::core::IUnknown = device.cast().ok()?;

        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;

        // Walk every adapter → every output. In fullscreen exclusive mode, only the
        // output the game owns accepts DuplicateOutput; all others return an error.
        // This lets us find the right monitor for any adapter/display configuration.
        let mut result: Option<(Vec<u8>, u32, u32, u32)> = None;

        'outer: for ai in 0u32.. {
            let adapter = match factory.EnumAdapters(ai) { Ok(a) => a, Err(_) => break };
            for oi in 0u32.. {
                let output: IDXGIOutput = match adapter.EnumOutputs(oi) { Ok(o) => o, Err(_) => break };
                let out1: IDXGIOutput1  = match output.cast() { Ok(o) => o, Err(_) => continue };

                // This fails for all outputs except the one the game is running on
                let dupl = match out1.DuplicateOutput(&unk) { Ok(d) => d, Err(_) => continue };

                // Acquire current frame (500 ms timeout)
                let mut fi  = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut res: Option<IDXGIResource> = None;
                if dupl.AcquireNextFrame(500, &mut fi, &mut res).is_err() { continue; }
                let res = match res { Some(r) => r, None => { let _ = dupl.ReleaseFrame(); continue } };

                // Get the desktop texture and read its dimensions
                let src: ID3D11Texture2D = match res.cast() {
                    Ok(t) => t,
                    Err(_) => { let _ = dupl.ReleaseFrame(); continue }
                };
                let mut src_desc = D3D11_TEXTURE2D_DESC::default();
                src.GetDesc(&mut src_desc);
                let full_w = src_desc.Width;
                let full_h = src_desc.Height;
                if full_w < 100 || full_h < 100 { let _ = dupl.ReleaseFrame(); continue; }

                // Create CPU-readable staging texture (full monitor size)
                let staging_desc = D3D11_TEXTURE2D_DESC {
                    Width:          full_w,
                    Height:         full_h,
                    MipLevels:      1,
                    ArraySize:      1,
                    Format:         src_desc.Format,
                    SampleDesc:     DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage:          D3D11_USAGE_STAGING,
                    BindFlags:      Default::default(),
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags:      Default::default(),
                };
                let mut staging: Option<ID3D11Texture2D> = None;
                if device.CreateTexture2D(&staging_desc, None, Some(&mut staging)).is_err() {
                    let _ = dupl.ReleaseFrame(); continue;
                }
                let staging = match staging { Some(s) => s, None => { let _ = dupl.ReleaseFrame(); continue } };

                // GPU blit → staging → map to CPU
                ctx.CopyResource(&staging.cast::<ID3D11Resource>().ok()?,
                                 &src.cast::<ID3D11Resource>().ok()?);

                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                if ctx.Map(&staging.cast::<ID3D11Resource>().ok()?, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).is_err() {
                    let _ = dupl.ReleaseFrame(); continue;
                }

                let cap_h     = ((full_h as f32 * cap_frac) as u32).max(1);
                let row_pitch = mapped.RowPitch as usize;
                let src_ptr   = mapped.pData as *const u8;

                // DXGI is typically BGRA. Swap R↔B if RGBA so OCR pipeline always gets BGRA.
                use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM;
                let swap_rb = src_desc.Format == DXGI_FORMAT_R8G8B8A8_UNORM;

                let mut pixels = Vec::with_capacity((full_w * cap_h * 4) as usize);
                for row in 0..(cap_h as usize) {
                    let slice = std::slice::from_raw_parts(
                        src_ptr.add(row * row_pitch), full_w as usize * 4);
                    if swap_rb {
                        for px in slice.chunks_exact(4) {
                            pixels.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                        }
                    } else {
                        pixels.extend_from_slice(slice);
                    }
                }

                ctx.Unmap(&staging.cast::<ID3D11Resource>().ok()?, 0);
                let _ = dupl.ReleaseFrame();

                result = Some((pixels, full_w, cap_h, full_h));
                break 'outer;
            }
        }

        result
    }
}

// ─── BMP encoding ─────────────────────────────────────────────────────────────

/// Encode BGRA pixels as a 24-bit BGR BMP (no alpha — BitmapDecoder handles it fine).
pub fn to_bmp(pixels_bgra: &[u8], width: u32, height: u32) -> Vec<u8> {
    let row_bytes = width * 3;
    let padding   = (4 - row_bytes % 4) % 4;
    let row_stride = row_bytes + padding;
    let image_size = row_stride * height;
    let file_size  = 54 + image_size;

    let mut bmp = Vec::with_capacity(file_size as usize);
    // File header
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&54u32.to_le_bytes());
    // Info header
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&(width as i32).to_le_bytes());
    bmp.extend_from_slice(&(-(height as i32)).to_le_bytes()); // top-down
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&24u16.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    bmp.extend_from_slice(&image_size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    // Pixel rows (BGRA → BGR + padding)
    for row in 0..height {
        for col in 0..width {
            let i = ((row * width + col) * 4) as usize;
            bmp.push(pixels_bgra[i]);
            bmp.push(pixels_bgra[i + 1]);
            bmp.push(pixels_bgra[i + 2]);
        }
        for _ in 0..padding { bmp.push(0); }
    }
    bmp
}

// ─── Windows OCR ──────────────────────────────────────────────────────────────

/// Run Windows.Media.Ocr on a BMP. Returns (full_text, line_positions).
/// line_positions: Vec<(line_text, x_frac)> — X centre per line from word bounding rects.
#[cfg(target_os = "windows")]
pub fn run_ocr(bmp: Vec<u8>, img_w: u32) -> Result<(String, Vec<(String, f32)>), String> {
    // Ensure COM is initialized for this thread. Tokio spawn_blocking threads
    // start without a COM apartment; WinRT calls fail or return empty silently.
    // CoInitializeEx returns S_OK (first init), S_FALSE (already MTA), or
    // RPC_E_CHANGED_MODE (already STA) — all safe to ignore.
    unsafe {
        windows_sys::Win32::System::Com::CoInitializeEx(
            std::ptr::null(),
            windows_sys::Win32::System::Com::COINIT_MULTITHREADED.try_into().unwrap_or(0),
        );
    }

    use windows::{
        Foundation::Collections::IVectorView,
        Globalization::Language,
        Graphics::Imaging::BitmapDecoder,
        Media::Ocr::{OcrEngine, OcrLine},
        Storage::Streams::{DataWriter, InMemoryRandomAccessStream},
    };

    (|| -> windows::core::Result<(String, Vec<(String, f32)>)> {
        let stream = InMemoryRandomAccessStream::new()?;
        let writer = DataWriter::CreateDataWriter(&stream)?;
        writer.WriteBytes(&bmp)?;
        writer.StoreAsync()?.get()?;
        writer.FlushAsync()?.get()?;
        writer.DetachStream()?;
        stream.Seek(0)?;

        let decoder = BitmapDecoder::CreateAsync(&stream)?.get()?;
        let bitmap  = decoder.GetSoftwareBitmapAsync()?.get()?;

        // Warframe text is always English. Try "en-US" first so the engine
        // works correctly on non-English Windows installations (Dutch, etc.).
        // Fall back to user profile language if English pack isn't installed.
        let engine = Language::CreateLanguage(&windows::core::HSTRING::from("en-US"))
            .and_then(|lang| OcrEngine::TryCreateFromLanguage(&lang))
            .or_else(|_| OcrEngine::TryCreateFromUserProfileLanguages())?;
        let result = engine.RecognizeAsync(&bitmap)?.get()?;

        let mut full = String::new();
        let mut lines_out: Vec<(String, f32)> = Vec::new();
        let lines: IVectorView<OcrLine> = result.Lines()?;
        let count = lines.Size()?;
        for i in 0..count {
            let line = lines.GetAt(i)?;
            let text = line.Text()?.to_string();
            // Try word bounding rects for X position; fall back to 0.5 if unavailable
            let x_frac = (|| -> windows::core::Result<f32> {
                let words = line.Words()?;
                let wc = words.Size()?;
                if wc == 0 || img_w == 0 { return Ok(0.5); }
                let mut sum = 0.0f32;
                for j in 0..wc {
                    let w = words.GetAt(j)?;
                    let r = w.BoundingRect()?;
                    sum += r.X + r.Width / 2.0;
                }
                Ok((sum / wc as f32) / img_w as f32)
            })().unwrap_or(0.5);
            full.push_str(&text);
            full.push('\n');
            lines_out.push((text, x_frac));
        }
        Ok((full, lines_out))
    })().map_err(|e| e.to_string())
}

// ─── Word matching helpers ────────────────────────────────────────────────────

fn lev_dist(a: &str, b: &str) -> usize {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let (m, n) = (a.len(), b.len());
    if m.abs_diff(n) > 3 { return 99; }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            curr[j] = if a[i-1] == b[j-1] { prev[j-1] }
                      else { 1 + prev[j].min(curr[j-1]).min(prev[j-1]) };
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Check whether `catalog_word` appears in `ocr_words` via:
///   1. Exact match
///   2. Prefix match: OCR truncated ("prime"→"pri", "voruna"→"vor")
///   3. Suffix substring: "neuroptics" → OCR gives "rüroptics"/"tearoptics" which
///      both contain "optics" — the distinctive suffix is preserved even when the
///      prefix is garbled. Check last 5+ chars as a substring in any OCR word.
///   4. Levenshtein ≤ 1 (or ≤ 2 for ≥8-char words) for single-char typos
///   5. Sliding-window inside longer merged tokens ("Sevagotfirime")
fn word_found_in_set(
    catalog_word: &str,
    ocr_words: &std::collections::HashSet<String>,
) -> bool {
    if ocr_words.contains(catalog_word) { return true; }
    if catalog_word.len() < 4 { return false; }

    // Prefix: OCR word is the leading portion of the catalog word
    for ocr_w in ocr_words {
        if ocr_w.len() >= 3 && catalog_word.starts_with(ocr_w.as_str()) { return true; }
    }

    // Suffix substring: check if last N chars of catalog word appear inside any OCR word
    // Handles "neuroptics" → "rüroptics" because both contain "optics"
    // Guard: reject when the suffix appears at exactly position 1 — that means an OCR
    // word is a prefix-stripped version of the catalog word (e.g. "bronco" contains
    // suffix "ronco" of "akbronco" at position 1, which is a false positive).
    if catalog_word.len() >= 6 {
        let suffix_len = (catalog_word.len() / 2).max(5); // half the word, min 5 chars
        let suffix = &catalog_word[catalog_word.len() - suffix_len..];
        if ocr_words.iter().any(|w| w.find(suffix).map_or(false, |p| p != 1)) { return true; }
    }

    let max_dist = if catalog_word.len() >= 8 { 2 } else { 1 };
    let wb = catalog_word.as_bytes();
    for ocr_w in ocr_words {
        // Full-word Levenshtein — reject pure prefix/suffix insertions (len_diff == dist && >= 2)
        // e.g. dist("akbronco","bronco")=2 with len_diff=2 is just "ak" prepended, not a typo.
        // Also require OCR word ≥4 chars: 3-char HUD noise ("RAM","FPS","GPU") must not
        // fuzzy-match 4-char catalog words ("gram","fang"…) regardless of screen position.
        if ocr_w.len() >= 4 {
            let dist = lev_dist(catalog_word, ocr_w);
            let len_diff = (catalog_word.len() as isize - ocr_w.len() as isize).unsigned_abs();
            if dist <= max_dist && !(len_diff == dist && len_diff >= 2) { return true; }
        }
        // Sliding window (merged tokens — e.g. OCR reads "SevagothPrime" as one word).
        // The suffix guard below (win_start>=3 exact-suffix) rejects a short base name
        // matching the tail of a longer different word — e.g. "gara" inside "akjagara".
        let ob = ocr_w.as_bytes();
        if ob.len() >= wb.len() {
            for (win_start, win) in ob.windows(wb.len()).enumerate() {
                let errs = wb.iter().zip(win.iter()).filter(|(a, b)| a != b).count();
                // Guard: reject exact suffix matches where the catalog word cleanly
                // terminates a longer OCR word (e.g. "fang" ending "sarofang").
                // "Sarofang" is a single correctly-read word; "fang" appearing at its
                // tail is a lexical coincidence, not an OCR merge artifact.
                // Only guard exact matches (errs == 0) — fuzzy matches are always valid.
                // win_start >= 3 avoids blocking short prefixes like "ak" in "akbronco".
                if errs == 0 && win_start + wb.len() == ob.len() && win_start >= 3 { continue; }
                if errs <= max_dist { return true; }
            }
        }
    }
    false
}

// ─── Catalog matching ─────────────────────────────────────────────────────────

/// Normalise OCR text for catalog matching.
/// ASCII letters are lowercased. Common diacritics are mapped to their ASCII
/// base (é→e, ü→u, …) so fuzzy matching still works when Windows OCR returns
/// accented surrogates instead of plain letters. Everything else → space.
fn normalise(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii() { return c.to_ascii_lowercase(); }
            match c {
                'À'|'Á'|'Â'|'Ã'|'Ä'|'Å'|'à'|'á'|'â'|'ã'|'ä'|'å' => 'a',
                'È'|'É'|'Ê'|'Ë'|'è'|'é'|'ê'|'ë' => 'e',
                'Ì'|'Í'|'Î'|'Ï'|'ì'|'í'|'î'|'ï' => 'i',
                'Ò'|'Ó'|'Ô'|'Õ'|'Ö'|'ò'|'ó'|'ô'|'õ'|'ö' => 'o',
                'Ù'|'Ú'|'Û'|'Ü'|'ù'|'ú'|'û'|'ü' => 'u',
                'Ñ'|'ñ' => 'n',
                'Ç'|'ç' => 'c',
                'Ý'|'ý'|'ÿ' => 'y',
                _ => ' ',
            }
        })
        .collect()
}

// ─── Rarity bar detection ─────────────────────────────────────────────────────

/// Scan the captured image for the coloured rarity bars below each reward card.
/// Returns (card_x_centers, bar_y_frac) where centers are fractions of image width.
///
/// Uses column aggregation: for each X column, count how many rows in the search
/// band have bar-coloured pixels. Columns that are consistently orange or teal
/// across many rows score high. This is far more robust than row-by-row detection
/// because it tolerates thin bars, color gradients, and single-row noise.
/// Returns `(Some((centers, bar_y_frac)), diagnostic_string)`.
/// `centers` are fractions of image width — the diamond icon X per card.
/// The diagnostic string is always populated for session log inclusion.
fn find_rarity_bars(pixels: &[u8], pix_w: u32, pix_h: u32, ui_scale: f32) -> (Option<(Vec<f32>, f32)>, String) {
    let x_lo = (pix_w as f32 * 0.05) as u32;
    let x_hi = (pix_w as f32 * 0.95) as u32;
    // Bars are at ~89% of captured height (bottom edge of the card area).
    // Starting at 70% skips the card artwork (helmets, weapons) which contains
    // bright orange/gold pixels that create false bar columns.
    let y_lo = (pix_h as f32 * 0.70) as u32;
    let y_hi = (pix_h as f32 * 0.97) as u32;

    let scan_w = (x_hi - x_lo) as usize;

    // Rarity colours (BGRA from PrintWindow/DXGI). Permissive — Warframe's UI
    // background is very dark (avg_brightness often 30–40), so bar pixels can
    // be quite dim. The diamond/arrow icon at each card's centre is near-white.
    //   Orange/bronze : R dominant over B
    //   Silver/teal   : B/G dominant, cool cast
    //   Gold/rare     : warm, R > G > B
    //   Diamond icon  : near-white, brightest point in the bar
    #[inline]
    fn is_bar_pixel(b: u32, g: u32, r: u32) -> bool {
        let lum = (r + g + b) / 3;
        if lum < 25 { return false; }
        let is_orange = r > 80  && r > b + 20;
        let is_teal   = b > 65  && g > 50  && b > r + 8;
        let is_gold   = r > 100 && g > 80  && b < r.saturating_sub(10);
        let is_bright = lum > 100 && r > 70 && g > 70 && b > 70;
        is_orange || is_teal || is_gold || is_bright
    }

    // ── Step 1: Column projection ────────────────────────────────────────────
    //
    // For each X column sum how many rows in the search band contain a
    // bar-coloured pixel.  Accumulating vertically makes this robust to:
    //   • Thin bars    — even a 1-px-tall bar contributes to every column it covers
    //   • Small icons  — the rarity diamond is only ~20-30 px wide but several
    //                    rows tall; rows accumulate into a clear column peak
    //   • Colour noise — one mis-classified pixel doesn't ruin a whole column
    //
    // The previous per-row scan required ≥25 % of scan width (~430 px) lit in a
    // SINGLE row.  With only the small diamond icons present (~4 × 25 px = 100 px)
    // NO row ever reached that threshold → "0 coloured rows" in the log.
    let mut col_score = vec![0u32; scan_w];
    for y in y_lo..y_hi {
        for (xi, x) in (x_lo..x_hi).enumerate() {
            let i = ((y * pix_w + x) * 4) as usize;
            if i + 2 < pixels.len()
                && is_bar_pixel(pixels[i] as u32, pixels[i+1] as u32, pixels[i+2] as u32)
            {
                col_score[xi] += 1;
            }
        }
    }

    let max_col = col_score.iter().max().copied().unwrap_or(0);
    if max_col < 2 {
        return (None, format!(
            "no bars — column projection: max_col={} (need ≥2; y={:.0}–{:.0}%)",
            max_col,
            y_lo as f32 / pix_h as f32 * 100.0,
            y_hi as f32 / pix_h as f32 * 100.0,
        ));
    }

    // ── Step 2: Threshold + gap bridging + segment counting ──────────────────
    //
    // A column is "lit" when its score ≥ max_col/4.
    // Relative threshold handles both full-width bars (many columns, lower peak)
    // and icon-only bars (few columns but a taller, sharper peak).
    let col_threshold = (max_col / 4).max(2);
    let mut lit: Vec<bool> = col_score.iter().map(|&s| s >= col_threshold).collect();

    // Bridge tiny dark notches within one arrow (≤1 % of scan width at 100% UI).
    // The arrow narrows with UI scale, so the bridge width does too.
    // Inter-card gaps are ~10 % of scan width and will NOT be bridged.
    let bridge = (((scan_w as f32 / 100.0) * ui_scale) as usize).max((3.0 * ui_scale) as usize).max(1);
    {
        let mut xi = 0;
        while xi < scan_w {
            if !lit[xi] {
                let gap_start = xi;
                while xi < scan_w && !lit[xi] { xi += 1; }
                let gap_len = xi - gap_start;
                if gap_len <= bridge && gap_start > 0 && xi < scan_w {
                    for gxi in gap_start..xi { lit[gxi] = true; }
                }
            } else {
                xi += 1;
            }
        }
    }

    // Each continuous lit segment = one rarity bar = one reward card.
    // The rarity indicator is a small downward-pointing arrow (~30 px wide at 1080p,
    // 100% UI). Both the arrow width and the minimum band shrink with UI scale.
    // min_band ≈ 0.7% of scan width — passes arrows of ~10 px and above.
    let min_band = (((scan_w as f32 / 150.0) * ui_scale) as usize).max((6.0 * ui_scale) as usize).max(1);
    let mut bands: Vec<(usize, usize)> = Vec::new();
    let mut in_band = false;
    let mut band_start = 0usize;
    for xi in 0..scan_w {
        match (lit[xi], in_band) {
            (true,  false) => { band_start = xi; in_band = true; }
            (false, true)  => {
                if xi - band_start >= min_band { bands.push((band_start, xi)); }
                in_band = false;
            }
            _ => {}
        }
    }
    if in_band && scan_w - band_start >= min_band { bands.push((band_start, scan_w)); }

    let lit_count = lit.iter().filter(|&&b| b).count();
    if bands.is_empty() {
        return (None, format!(
            "no bars — {} lit columns (threshold={}/{}), no segment ≥{}px (bridge={}px)",
            lit_count, col_threshold, max_col, min_band, bridge
        ));
    }
    if bands.len() > 4 {
        // Salvage instead of discarding: on dark frames the diorama / weapon art
        // produces spurious lit segments alongside the real rarity bars. Real bars
        // are sharp, strongly-lit columns; rank bands by peak column score, drop the
        // ones far weaker than the strongest, then cap at 4. Discarding all geometry
        // (the old behaviour) collapsed a 2-card screen to a single hardcoded centre.
        let peak = |b: &(usize, usize)| (b.0..b.1).map(|xi| col_score[xi]).max().unwrap_or(0);
        let top = bands.iter().map(|b| peak(b)).max().unwrap_or(0);
        let cutoff = (top as f32 * 0.55) as u32;
        bands.retain(|b| peak(b) >= cutoff);
        if bands.len() > 4 {
            bands.sort_by_key(|b| std::cmp::Reverse(peak(b)));
            bands.truncate(4);
            bands.sort_by_key(|b| b.0); // restore left→right order
        }
        if bands.is_empty() {
            return (None, format!(
                "no bars — salvage emptied (max_col={}, threshold={})", max_col, col_threshold
            ));
        }
    }

    // ── Step 3: Bar Y position (for icon classifier) ─────────────────────────
    //
    // Restrict the row scan to lit X columns only, then find the row with the
    // most bar pixels.  classify_card_icon uses bar_y to locate the icon region
    // above the rarity bar for each card.
    let lit_xs: Vec<u32> = (0..scan_w as u32)
        .filter(|&xi| lit[xi as usize])
        .map(|xi| x_lo + xi)
        .collect();

    let mut best_row_y = (y_lo + y_hi) / 2; // fallback: geometric centre
    let mut best_row_cnt = 0u32;
    for y in y_lo..y_hi {
        let mut cnt = 0u32;
        for &x in &lit_xs {
            let i = ((y * pix_w + x) * 4) as usize;
            if i + 2 < pixels.len()
                && is_bar_pixel(pixels[i] as u32, pixels[i+1] as u32, pixels[i+2] as u32)
            {
                cnt += 1;
            }
        }
        if cnt > best_row_cnt { best_row_cnt = cnt; best_row_y = y; }
    }

    // ── Step 4: Card X center — peak column within each band ─────────────────
    //
    // The diamond/arrow icon sits at the exact centre of each card.
    // The column with the highest accumulated score within each band is the
    // most reliably lit X → use it as the card center.
    let centers: Vec<f32> = bands.iter().map(|(s, e)| {
        let best_xi = (*s..*e)
            .max_by_key(|&xi| col_score[xi])
            .unwrap_or((s + e) / 2);
        (x_lo as f32 + best_xi as f32) / pix_w as f32
    }).collect();

    let bar_y = best_row_y as f32 / pix_h as f32;
    let diag = format!(
        "{} bars — centers x=[{}], bar_y={:.2} ({:.0}%), max_col={}px, threshold={}px, lit={}px",
        bands.len(),
        centers.iter().map(|x| format!("{:.3}", x)).collect::<Vec<_>>().join(", "),
        bar_y, bar_y * 100.0, max_col, col_threshold, lit_count,
    );
    (Some((centers, bar_y)), diag)
}

// ─── Icon component classifier ────────────────────────────────────────────────

/// What the card icon looks like, used to constrain catalog matching.
#[derive(Debug, Clone, PartialEq)]
pub enum IconType {
    /// Generic REUSED component shape — same icon appears across many primes.
    /// e.g. all neuroptics share the same helmet silhouette, all barrels look alike.
    /// The TEXT below identifies WHICH prime it belongs to.
    Component(&'static str), // "neuroptics" | "systems" | "chassis" |
                              // "barrel" | "stock" | "receiver" | "handle" |
                              // "blade" | "grip" | "upper limb" | "lower limb"
    /// Full 3D model of a unique warframe or weapon.
    /// Every prime has its own unique render → card always shows "[Name] Prime Blueprint".
    /// The TEXT (or partial text) gives us the [Name].
    FullModel,
    /// Forma spiral (distinctively blue)
    Forma,
    /// Could not classify
    Unknown,
}

/// Classify the reward card icon using an 8×8 spatial brightness grid.
///
/// Features extracted:
///   fill_ratio — fraction of grid cells above threshold (dense = full model)
///   aspect     — bounding-box width / height (> 1 wide, < 1 tall)
///   cm_y       — vertical centre-of-mass (0 = top, 1 = bottom)
///   symmetry   — left / right balance (1 = symmetric)
///   blue_dom   — blue channel dominance (Forma indicator)
///
/// Rule set (in priority order):
///   ① Forma        — blue channel dominates → blue spiral icon
///   ② FullModel    — high fill + even spread → complete warframe/weapon render;
///                    text gives "[Name] Prime Blueprint"
///   ③ neuroptics   — bright top half, symmetric, roughly square (helmet shape)
///   ④ systems      — bright central region, compact, somewhat circular (gear)
///   ⑤ chassis      — large central region, wider, lower CoM (torso)
///   ⑥ barrel       — wide aspect ratio (elongated horizontal part)
///   ⑦ handle       — tall aspect ratio (elongated vertical / melee handle)
///   ⑧ blade        — low symmetry, moderate aspect (flat asymmetric part)
///   ⑨ upper/lower limb — low fill, arc-shaped (bow components)
///   Unknown        — ambiguous; fall back to text-only matching
fn classify_card_icon(
    pixels: &[u8], pix_w: u32, pix_h: u32,
    x_left: f32, x_right: f32, bar_y: f32,
) -> IconType {
    // Card icon sits between the card top and the rarity bar.
    // In the capture buffer the icon occupies roughly bar_y-0.28 → bar_y-0.04.
    let iy_top = ((bar_y - 0.28).max(0.0) * pix_h as f32) as u32;
    let iy_bot = ((bar_y - 0.04).min(1.0) * pix_h as f32) as u32;
    let ix_lo  = (x_left  * pix_w as f32) as u32;
    let ix_hi  = (x_right * pix_w as f32).min(pix_w as f32) as u32;
    if ix_hi <= ix_lo || iy_bot <= iy_top { return IconType::Unknown; }

    const G: usize = 8;
    let mut lum  = [[0.0f32; G]; G];
    let mut blue = [[0.0f32; G]; G];
    let mut cnt  = [[0u32;  G]; G];

    for y in iy_top..iy_bot {
        let gy = (((y - iy_top) as f32 / (iy_bot - iy_top) as f32) * G as f32)
                     .min(G as f32 - 1.0) as usize;
        for x in ix_lo..ix_hi {
            let gx = (((x - ix_lo) as f32 / (ix_hi - ix_lo) as f32) * G as f32)
                         .min(G as f32 - 1.0) as usize;
            let i = ((y * pix_w + x) * 4) as usize;
            if i + 2 >= pixels.len() { continue; }
            let b = pixels[i]     as f32;
            let g = pixels[i + 1] as f32;
            let r = pixels[i + 2] as f32;
            lum [gy][gx] += (r + g + b) / 3.0;
            blue[gy][gx] += b;
            cnt [gy][gx] += 1;
        }
    }
    for gy in 0..G { for gx in 0..G {
        let c = cnt[gy][gx];
        if c > 0 { lum[gy][gx] /= c as f32; blue[gy][gx] /= c as f32; }
    }}

    let avg_lum  = lum.iter().flatten().sum::<f32>()  / (G*G) as f32;
    let avg_blue = blue.iter().flatten().sum::<f32>() / (G*G) as f32;

    // ① Forma: blue channel clearly stronger than average luminance
    if avg_blue > 75.0 && avg_blue > avg_lum * 1.35 { return IconType::Forma; }

    // Threshold: cells are "bright" if > 40 % of the peak cell
    let peak = lum.iter().flatten().cloned().fold(0.0f32, f32::max);
    let thr  = peak * 0.40;

    let mut bright_rows = [false; G];
    let mut bright_cols = [false; G];
    let mut n_bright = 0usize;
    let mut cx_sum   = 0.0f32;
    let mut cy_sum   = 0.0f32;

    for gy in 0..G { for gx in 0..G {
        if lum[gy][gx] > thr {
            bright_rows[gy] = true;
            bright_cols[gx] = true;
            n_bright += 1;
            cx_sum += gx as f32;
            cy_sum += gy as f32;
        }
    }}
    if n_bright == 0 { return IconType::Unknown; }

    // Centre-of-mass (0 = top/left, 1 = bottom/right)
    let cm_x = cx_sum / n_bright as f32 / (G-1) as f32;
    let cm_y = cy_sum / n_bright as f32 / (G-1) as f32;

    // Bounding box of bright region
    let row_lo = bright_rows.iter().position(|&b| b).unwrap_or(0)    as f32 / (G-1) as f32;
    let row_hi = bright_rows.iter().rposition(|&b| b).unwrap_or(G-1) as f32 / (G-1) as f32;
    let col_lo = bright_cols.iter().position(|&b| b).unwrap_or(0)    as f32 / (G-1) as f32;
    let col_hi = bright_cols.iter().rposition(|&b| b).unwrap_or(G-1) as f32 / (G-1) as f32;

    let bb_h   = (row_hi - row_lo).max(0.01);
    let bb_w   = (col_hi - col_lo).max(0.01);
    let aspect = bb_w / bb_h;            // > 1 wide,  < 1 tall
    let fill   = n_bright as f32 / (G*G) as f32;  // 0 – 1

    // Left / right symmetry score
    let l: f32 = (0..G).map(|gy| (0..G/2).map(|gx| lum[gy][gx]).sum::<f32>()).sum();
    let r: f32 = (0..G).map(|gy| (G/2..G).map(|gx| lum[gy][gx]).sum::<f32>()).sum();
    let symmetry = 1.0 - (l - r).abs() / (l + r + 0.001);

    let _ = cm_x; // reserved for future use

    // ② FullModel — complete warframe pose or full weapon render.
    //    Fills the card frame densely and relatively evenly.
    //    Text below gives "[Name]" → result is "[Name] Prime Blueprint".
    if fill > 0.55 && avg_lum > 70.0 { return IconType::FullModel; }

    // ③ Neuroptics — helmet silhouette, rounded top.
    //    CoM upper half, symmetric left/right, roughly square bounding box.
    if cm_y < 0.45 && symmetry > 0.72 && (0.5..=2.0).contains(&aspect) {
        return IconType::Component("neuroptics");
    }

    // ④ Systems — round mechanical ring / gear.
    //    Central CoM, compact, relatively symmetric and circular.
    if cm_y > 0.35 && cm_y < 0.65 && symmetry > 0.68 && (0.6..=1.7).contains(&aspect) && fill > 0.20 {
        return IconType::Component("systems");
    }

    // ⑤ Chassis — larger torso / body piece.
    //    CoM centre-to-low, more filled, wider than neuroptics.
    if cm_y > 0.42 && fill > 0.28 && (0.7..=2.2).contains(&aspect) {
        return IconType::Component("chassis");
    }

    // ⑥ Barrel / Stock / Receiver — elongated horizontal.
    //    Bounding box much wider than tall (aspect > 2).
    if aspect > 2.0 { return IconType::Component("barrel"); }

    // ⑦ Handle / Grip — elongated vertical (melee handle).
    //    Bounding box much taller than wide (aspect < 0.5).
    if aspect < 0.5 { return IconType::Component("handle"); }

    // ⑧ Blade — flat, angular, asymmetric.
    //    Moderate aspect but low left/right symmetry.
    if symmetry < 0.60 && (0.7..=3.0).contains(&aspect) {
        return IconType::Component("blade");
    }

    // ⑨ Upper / Lower Limb — curved bow piece (arc = low fill, hollow centre).
    if fill < 0.22 && (0.7..=2.5).contains(&aspect) {
        return if cm_y < 0.50 {
            IconType::Component("upper limb")
        } else {
            IconType::Component("lower limb")
        };
    }

    IconType::Unknown
}

/// Given a word set from OCR text, extract the most likely item NAME
/// (strip known non-name words: "prime", "blueprint", component names, "owned", etc.)
fn extract_item_name_words(words: &std::collections::HashSet<String>) -> Vec<String> {
    const SKIP: &[&str] = &[
        "prime", "blueprint", "owned", "crafted", "bl", "neuroptics", "systems",
        "chassis", "barrel", "stock", "receiver", "handle", "blade", "grip",
        "limb", "upper", "lower", "string", "link", "carapace", "cerebrum",
        "forma", "riven", "sliver", "ayatan",
    ];
    words.iter()
        .filter(|w| w.len() >= 3 && !SKIP.contains(&w.as_str()))
        .cloned()
        .collect()
}

/// Sanity-check detected bar centers.
/// Rejects detections caused by card artwork (orange forma gear, gold weapons)
/// which produce centers that are bunched together or out of range.
/// Valid 4-card centers span ~0.52 (e.g. 0.24→0.76); false-positive clusters
/// span much less (e.g. 0.372→0.706 = 0.334, seen with forma-heavy rewards).
fn bar_centers_are_valid(centers: &[f32], ui_scale: f32) -> bool {
    let n = centers.len();
    if n == 0 { return false; }
    // Outermost centers must be in a plausible screen zone. At lower UI scale the
    // cards cluster toward centre, so this only ever needs to stay inside [0.15,0.85].
    if centers[0] < 0.15 || centers[n - 1] > 0.85 { return false; }
    if n < 2 { return true; }
    // Reject if any two adjacent bars are closer than the expected gap. The gap
    // shrinks linearly with UI scale, so the "double detection" floor does too.
    // Bars within this of each other are a double-detection of the same bar
    // or a false positive from card artwork — they'd leave one column with no
    // OCR text and another column absorbing text from two cards at once.
    for pair in centers.windows(2) {
        if pair[1] - pair[0] < 0.08 * ui_scale { return false; }
    }
    let span = centers[n - 1] - centers[0];
    // Expected spans per card count (measured from real captures at 100% UI),
    // scaled down for smaller in-game UI.
    let expected = (match n {
        2 => 0.34f32,
        3 => 0.46,
        _ => 0.52, // 4 cards
    }) * ui_scale;
    (span - expected).abs() < 0.10 * ui_scale
}

/// Evenly-distributed card X centers (fraction of image width) for N cards.
/// Calibrated from bar-detected centers on 1920×1080 captures: 4-card spread
/// is 0.31→0.69 (spacing ≈0.127), not the old 0.24→0.76.
/// Used as the fallback when rarity bar detection fails.
fn hardcoded_card_centers(n: usize) -> Vec<f32> {
    match n {
        1 => vec![0.50],
        2 => vec![0.435, 0.565],
        3 => vec![0.37, 0.50, 0.63],
        _ => vec![0.31, 0.44, 0.56, 0.69], // 4 cards (default / full squad)
    }
}

/// How many distinct X positions a base/identifier word occupies in the OCR.
/// Each prime set sits on ONE physical card, so its base word (e.g. "atlas")
/// appears at one X cluster. Used to cap same-set duplicates: if matching produced
/// two "atlas" items but "atlas" only appears at one X, the second is a phantom
/// from a split card. Two players holding different parts of the same set occupy
/// two well-separated X positions and are correctly counted as two.
fn base_word_x_clusters(base: &str, ocr_words: &[(String, f32)]) -> usize {
    if base.len() < 3 { return 1; }
    let mut xs: Vec<f32> = ocr_words.iter()
        .filter(|(w, _)| {
            let n = normalise(w);
            if n.len() < 3 { return false; }
            n == base
                || (base.len() >= 4 && n.starts_with(base))
                || (n.len() + 1 >= base.len() && lev_dist(&n, base) <= 1)
        })
        .map(|(_, x)| *x)
        .collect();
    if xs.is_empty() { return 1; }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut clusters = 1;
    let mut last = xs[0];
    for &x in &xs[1..] {
        if x - last > 0.10 { clusters += 1; }
        last = x;
    }
    clusters
}

// ─── Matching helpers (standalone fns — no closure capture issues) ────────────

fn build_word_set(texts: &[String]) -> std::collections::HashSet<String> {
    let corrected = texts.join(" ")
        .replace('@', "bl").replace(')', "d").replace('&', " p");
    normalise(&corrected).chars()
        .map(|c| if c.is_ascii_alphabetic() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter(|w| w.len() >= 3)
        .map(|s| s.to_string())
        .collect()
}

fn score_item(display_name: &str, words: &std::collections::HashSet<String>) -> f32 {
    let norm = normalise(display_name);
    let item_words: Vec<&str> = norm.split_whitespace().collect();
    if item_words.is_empty() { return 0.0; }

    // The base name (first word, e.g. "yareli", "gauss", "burston") is the most
    // distinctive part of the item name.  Generic suffix words like "prime",
    // "blueprint", "chassis" appear across many items and should contribute less.
    const GENERIC: &[&str] = &[
        "prime", "blueprint", "chassis", "systems", "neuroptics",
        "barrel", "receiver", "blade", "handle", "grip", "stock",
        "limb", "upper", "lower", "guard", "hilt", "link", "gauntlet",
        "carapace", "cerebrum", "head", "strike", "boot",
    ];

    let mut total_weight = 0.0f32;
    let mut matched_weight = 0.0f32;
    let mut base_name_missing = false;

    for (idx, &w) in item_words.iter().enumerate() {
        let weight = if idx == 0 { 3.0 } // base name
            else if GENERIC.contains(&w) { 0.25 }
            else { 1.0 };
        total_weight += weight;
        if word_found_in_set(w, words) {
            matched_weight += weight;
        } else if idx == 0 {
            base_name_missing = true;
        }
    }

    let mut score = if total_weight > 0.0 { matched_weight / total_weight } else { 0.0 };

    // If the distinctive base name is absent, the item is almost certainly wrong.
    // Cap the score so generic suffix matches alone can't reach the 0.75 threshold.
    if base_name_missing {
        score = score * 0.3;
    }

    // Small length-affinity bonus for unmatched words (preserves old tie-breaking).
    let len_bonus: f32 = item_words.iter()
        .filter(|&&w| !word_found_in_set(w, words))
        .map(|&cw| {
            words.iter()
                .map(|ow| {
                    let diff = (cw.len() as isize - ow.len() as isize).unsigned_abs();
                    if diff == 0 { 0.05_f32 } else if diff == 1 { 0.02 } else { 0.0 }
                })
                .fold(0.0_f32, f32::max)
        })
        .sum::<f32>() / item_words.len() as f32;

    score + len_bonus
}

// ─── Reward item extraction ───────────────────────────────────────────────────

/// Relic reward detection.
///
/// 1. Find rarity bars → card X positions + bar Y (reliable visual anchor).
/// 2. Full-frame raw OCR → text with word X positions.
/// 3. Assign each OCR word to the nearest card (by X).
/// 4. Per-card word set → prefix + fuzzy match against relic catalog.
/// 5. Full-frame fallback if bar detection fails.
///
/// `acc_col_words` (optional): accumulated words per column from previous
/// attempts within the same reward-screen session.  When OCR is inconsistent
/// across captures, a base name visible in Attempt 2 but missing in Attempt
/// 13 can still help match the correct item.  The function both reads from
/// and writes to this accumulator.
pub fn extract_reward_items_twophase(
    pixels: &[u8], pix_w: u32, pix_h: u32, _game_h: u32,
    catalog: &[(String, String)],
    capture_info: &str,
    hint_squad_size: Option<usize>,
    ui_scale: f32,
    mut acc_col_words: Option<&mut std::collections::HashMap<usize, std::collections::HashSet<String>>>,
) -> (bool, bool, Vec<String>, Vec<f32>, String) {

    // ── 1. Raw OCR ────────────────────────────────────────────────────────────
    // Full-frame OCR with thresholding.  We parse TSV at WORD level so that
    // even when Tesseract merges text from adjacent cards into one "line",
    // each word still carries its own X position and can be assigned to the
    // correct card independently.
    #[cfg(target_os = "windows")]
    let (raw_full, ocr_words) =
        match run_ocr(to_bmp(pixels, pix_w, pix_h), pix_w) {
            Ok(r) => r,
            Err(e) => return (false, false, vec![], vec![],
                format!("├─ Capture  : {}\n└─ OCR error: {}", capture_info, e)),
        };
    #[cfg(not(target_os = "windows"))]
    let (raw_full, ocr_words) =
        match run_ocr(pixels, pix_w, pix_h, _game_h, ui_scale) {
            Ok(r) => r,
            Err(e) => return (false, false, vec![], vec![],
                format!("├─ Capture  : {}\n└─ OCR error: {}", capture_info, e)),
        };
    // Dark frames are where the whole-band pass garbles a 2nd/3rd card and the tight
    // per-column re-OCR fallback earns its cost; gate that fallback on this so bright
    // frames neither pay the extra Tesseract passes nor risk a re-OCR on a slightly
    // off hardcoded column producing a wrong card.
    let band_dark = avg_brightness(pixels) < 80;
    if raw_full.len() < 4 {
        let debug_bmp = std::env::temp_dir().join("frameforge_capture_debug.bmp");
        let _ = std::fs::write(&debug_bmp, to_bmp(pixels, pix_w, pix_h));
        let avg = avg_brightness(pixels);
        let kind = if avg < 30 { "dark-frame" } else { "ocr-empty" };
        return (false, false, vec![], vec![], format!(
            "├─ Capture  : {}\n└─ OCR      : returned no text ({}, avg={})\n   Saved: {}",
            capture_info, kind, avg, debug_bmp.display()
        ));
    }

    // Relic selection / ESC screens contain " relic"; reward screen never does.
    if raw_full.to_lowercase().contains(" relic") {
        return (false, true, vec![], vec![], format!(
            "├─ Capture  : {}\n└─ OCR      : relic selection screen detected (skipped)",
            capture_info
        ));
    }

    // ── 2. Find card positions from rarity bars ───────────────────────────────
    // Rarity bars are always present regardless of Owned/Crafted labels.
    // If detection fails, fall back to X-gap grouping of OCR lines.
    let (bar_result, bar_diag) = find_rarity_bars(pixels, pix_w, pix_h, ui_scale);

    let (card_centers, _bar_y): (Vec<f32>, f32) = match &bar_result {
        Some((centers, by)) => (centers.clone(), *by),
        None => (vec![], 0.0),
    };

    // ── 2b. Card count — prime+forma word count ──────────────────────────────
    // Every fissure reward is a prime item ("Prime" in name) or Forma Blueprint.
    // OCR frequently garbles "Prime" into "+rime", "Prtme", or merges it with the
    // next word ("Primeteüroptics").  Count any word that is "prime"-like:
    //   • starts with "prim"         → catches merged tokens like "primete..."
    //   • within edit-distance 1     → catches "+rime", "pnme", "prlme" etc.
    //   • "forma" or ≤1 edit of it  → catches "rorma", "torma" etc.
    let raw_norm = normalise(&raw_full);
    let is_prime_like = |w: &str| -> bool {
        if w.starts_with("prim") && w.len() >= 4 { return true; }
        if w.len() >= 3 && w.len() <= 7 { return lev_dist(w, "prime") <= 1; }
        false
    };
    let is_forma_like = |w: &str| -> bool {
        if w == "forma" { return true; }
        if w.len() >= 4 && w.len() <= 6 { return lev_dist(w, "forma") <= 1; }
        false
    };
    let prime_count = raw_norm.split_whitespace().filter(|&w| is_prime_like(w)).count();
    let forma_count  = raw_norm.split_whitespace().filter(|&w| is_forma_like(w)).count();

    // Count distinct x-position clusters in OCR output.
    // Each card's text groups at a consistent x — gaps > 10% of width mark a new card.
    // Uses centroid-based clustering (not single-linkage) so that a single off-centre
    // OCR line between two adjacent card columns doesn't bridge them together.
    // Example: cards at 0.41 and 0.59 with a bridge line at 0.50 →
    //   single-linkage: 0.50-0.41=0.09 < 0.10 (merged), 0.59-0.50=0.09 < 0.10 (merged) → 1 cluster
    //   centroid:       0.50-0.41=0.09 < 0.10 (extend, center→0.455), 0.59-0.455=0.135 > 0.10 → 2 clusters
    let ocr_cluster_count: usize = {
        let mut xs: Vec<f32> = ocr_words.iter()
            .filter(|(t, _)| t.trim().len() >= 3)
            .map(|(_, x)| *x)
            .collect();
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if xs.is_empty() { 0 }
        else {
            let mut count = 1usize;
            let mut cluster_sum = xs[0];
            let mut cluster_n   = 1usize;
            for &x in &xs[1..] {
                let center = cluster_sum / cluster_n as f32;
                if x - center > 0.10 {
                    count += 1;
                    cluster_sum = x;
                    cluster_n   = 1;
                } else {
                    cluster_sum += x;
                    cluster_n   += 1;
                }
            }
            count.min(4)
        }
    };
    // Distinct strong catalog matches across the whole frame.
    // When OCR garbles "Prime" or merges adjacent columns, prime_count and the
    // x-cluster count can undercount (a 4-player screen reading as 3 — which then
    // drops a card entirely). Counting the distinct item BASE NAMES that score
    // highly against the catalog recovers the true count: each physical card is
    // one prime item, so N distinct strong matches ⇒ at least N cards. The 0.80
    // threshold mirrors the full-frame fill path below, so this count never
    // exceeds what that path can actually populate (avoids unreachable targets).
    let match_card_count: usize = {
        let all_words = build_word_set(
            &raw_full.lines().map(|l| l.to_string()).collect::<Vec<_>>()
        );
        let mut bases: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (_unique, display_name) in catalog {
            if display_name.len() < 5 { continue; }
            if score_item(display_name, &all_words) >= 0.80 {
                if let Some(first) = normalise(display_name).split_whitespace().next() {
                    bases.insert(first.to_string());
                }
            }
        }
        bases.len()
    };

    // Two separate card counts, because the squad hint and the on-screen evidence
    // each fail in opposite directions:
    //
    //   evidence_count — what we can SEE/READ on screen. Drives COMPLETENESS (when
    //     we trust and lock the result). Can UNDER-count badly: OCR garbles
    //     "Prime"/"Forma" tokens, two parts of one set share a base name, and text
    //     X-clusters merge — a real 4-card screen can read as 2.
    //
    //   layout_count — how many COLUMNS to carve the band into. Includes the EE.log
    //     squad size so genuine full squads get enough columns to physically
    //     separate adjacent cards even when every evidence signal undercounts. The
    //     squad size can OVER-count (a squadmate with no relic, or who left), but
    //     two guards neutralise that: (1) the same-set dedup after matching removes
    //     any phantom an over-wide layout splits out of one card, and (2)
    //     COMPLETENESS is tied to evidence_count, so a stale hint never blocks
    //     locking the real (smaller) count.
    let evidence_count = (prime_count + forma_count)
        .max(ocr_cluster_count)
        .max(match_card_count)
        .clamp(1, 4);
    let layout_count = evidence_count
        .max(hint_squad_size.unwrap_or(0))
        .clamp(1, 4);
    // Column layout / fill target use the generous count.
    let word_card_count = layout_count;

    // ── 2c. Assign OCR words to card columns ──────────────────────────────────
    // We parse TSV at WORD level.  Even when Tesseract merges adjacent cards
    // into one "line", each word still has its own bounding-box X centre, so
    // we can assign it to the correct card independently.
    let bars_trusted = !card_centers.is_empty()
        && card_centers.len() == word_card_count
        && bar_centers_are_valid(&card_centers, ui_scale);
    let active_centers: Vec<f32> = if bars_trusted {
        // Bar centres come straight from the image, so they are already at the
        // correct (scaled) positions — no UI-scale adjustment needed.
        card_centers.clone()
    } else {
        // Hardcoded centres are measured at 100% UI; pull them toward screen
        // centre proportionally for smaller UI scales (the reward box stays
        // centred and scales linearly).
        hardcoded_card_centers(word_card_count)
            .iter()
            .map(|&c| 0.5 + (c - 0.5) * ui_scale)
            .collect()
    };

    let columns: Vec<(Vec<String>, f32)> = {
        let mut cols: Vec<(Vec<String>, f32)> =
            active_centers.iter().map(|&cx| (Vec::new(), cx)).collect();
        for (word, x) in &ocr_words {
            let idx = active_centers.iter().enumerate()
                .min_by(|(_, a), (_, b)| {
                    (x - *a).abs().partial_cmp(&(x - *b).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i)
                .unwrap_or(0);
            cols[idx].0.push(word.clone());
        }
        // Join words into a single text blob per column — build_word_set normalises
        // and splits on whitespace anyway, so a joined string is fine.
        cols.into_iter().map(|(words, cx)| (vec![words.join(" ")], cx)).collect()
    };

    // ── 3a. Per-card matching (only when rarity bars gave reliable columns) ─────
    // X-gap fallback columns are unreliable: OCR clusters all right-side card text
    // into the same column (wrong X positions), so per-column matching on fallback
    // columns produces wrong items. Only use per-column when bars were detected.
    let mut items: Vec<String> = Vec::new();
    let mut positions: Vec<f32> = Vec::new();
    // Match confidence per accepted item — used by the same-set dedup below to keep
    // the strongest copy when an over-wide layout produces a phantom sibling.
    let mut item_scores: Vec<f32> = Vec::new();

    let (_bar_y_frac, have_bars) = match &bar_result {
        Some((_, by)) => (*by, true),
        None => (0.0f32, false),
    };

    let mut col_match_log: Vec<String> = Vec::new();

    // Per-column matching keeps each card's OCR text isolated, which is what
    // disambiguates same-frame siblings: a card reading "...Systems Blueprint"
    // matches ONLY "Xaku Prime Systems Blueprint", not its Chassis/Neuroptics
    // siblings. (Pooling the whole frame's words — as the fill path does — makes
    // every sibling score high off the shared "xaku/prime/blueprint" tokens and
    // floods the result with one frame's parts.) The earlier dropped-card bug was
    // purely a COUNT problem (3 columns instead of 4), now fixed by the
    // match_card_count signal above — so per-column runs on the hardcoded-centre
    // fallback too, just with the correct number of columns.
    for (col_idx, (col_texts, cx)) in columns.iter().enumerate() {
        if items.len() >= active_centers.len() { break; }
        let mut words = build_word_set(col_texts);

        // Merge accumulated words from previous attempts in this session.
        // OCR is often inconsistent: the base name may be clear in one capture
        // and garbled in another.  Accumulating gives us the best of both.
        let mut acc_used = false;
        if let Some(ref acc) = acc_col_words {
            if let Some(prev) = acc.get(&col_idx) {
                for w in prev {
                    if words.insert(w.clone()) { acc_used = true; }
                }
            }
        }

        // Save current words into the accumulator for future attempts.
        if let Some(ref mut acc) = acc_col_words {
            let entry = acc.entry(col_idx).or_default();
            for w in &words { entry.insert(w.clone()); }
        }

        // Log what OCR text this column contains
        let col_preview: Vec<&str> = col_texts.iter().take(4).map(|s| s.trim()).collect();
        if words.is_empty() {
            col_match_log.push(format!(
                "  Col[{}] x={:.2}: (no words) — skipped\n    OCR: {:?}",
                col_idx, cx, col_preview));
            continue;
        }

        // ── Text-based scoring ───────────────────────────────────────────────
        let mut best_score = 0.0f32;
        let mut best_word_count = 0usize; // tiebreaker: more catalog words = more specific match
        let mut best_unique: Option<String> = None;
        for (unique_name, display_name) in catalog {
            if display_name.len() < 5 { continue; }
            let s = score_item(display_name, &words);
            let wc = normalise(display_name).split_whitespace().count();
            if s > best_score || (s >= best_score - 1e-6 && wc > best_word_count) {
                best_score = s;
                best_word_count = wc;
                best_unique = Some(unique_name.clone());
            }
        }

        // ── Icon-based fallback when text match is weak ──────────────────────
        // If text gives < 67 % confidence AND we have rarity-bar positions,
        // classify the icon and use the item name words to narrow the catalog.
        if best_score < 0.67 && have_bars {
            let bar_y = _bar_y_frac;
            // Use card center from column; left/right estimated from spacing
            let half_w = if columns.len() > 1 { 0.56 / columns.len() as f32 / 2.0 } else { 0.10 };
            let icon_type = classify_card_icon(
                pixels, pix_w, pix_h,
                (cx - half_w).max(0.0), (cx + half_w).min(1.0), bar_y
            );

            let name_words = extract_item_name_words(&words);

            // Determine which component suffix the icon implies
            let component_filter: Option<&str> = match &icon_type {
                IconType::Component(c) => Some(c),
                IconType::Forma        => Some("forma"),
                // Full 3D model → always "[Name] Prime Blueprint"
                IconType::FullModel    => Some("blueprint"),
                IconType::Unknown      => None,
            };

            if let Some(comp) = component_filter {
                // Find catalog items that contain the component keyword
                // AND any of the partial name words
                let comp_norm = normalise(comp);
                let mut icon_best_score = 0.0f32;
                let mut icon_best_unique: Option<String> = None;

                for (unique_name, display_name) in catalog {
                    if display_name.len() < 5 { continue; }
                    let dn = normalise(display_name);
                    if !dn.contains(comp_norm.as_str()) { continue; }
                    let name_matched = name_words.iter()
                        .filter(|nw| dn.contains(nw.as_str()))
                        .count();
                    let s = if name_words.is_empty() { 0.5 }
                            else { name_matched as f32 / name_words.len() as f32 };
                    if s > icon_best_score {
                        icon_best_score = s;
                        icon_best_unique = Some(unique_name.clone());
                    }
                }
                // Accept icon-based match if it found something reasonable
                if icon_best_score >= 0.4 {
                    best_score = icon_best_score;
                    best_unique = icon_best_unique;
                }
            }
        }

        // ── Tight per-column re-OCR fallback ─────────────────────────────────
        // When the whole-band pass garbles a column (common on dark frames, where a
        // 2nd card's name reads as noise because the brighter card dominates the
        // single-pass threshold), re-OCR a tight crop around just this column with
        // contrast preprocessing. The final 0.75 gate below still guards quality, so
        // this can only rescue a failing column — passing columns never reach here.
        // Gated to dark frames (the case it was built for) to avoid re-OCR cost and
        // wrong-card risk on bright frames.
        if best_score < 0.75 && band_dark {
            let half_w = if columns.len() > 1 { 0.06 } else { 0.10 };
            let rx0 = (cx - half_w).max(0.0);
            let rx1 = (cx + half_w).min(1.0);
            if let Ok(tight) = ocr_pixels_rect(pixels, pix_w, pix_h, rx0, rx1, 0.42, 0.70) {
                let tight_words = build_word_set(std::slice::from_ref(&tight));
                if !tight_words.is_empty() {
                    for (unique_name, display_name) in catalog {
                        if display_name.len() < 5 { continue; }
                        let s = score_item(display_name, &tight_words);
                        let wc = normalise(display_name).split_whitespace().count();
                        if s > best_score || (s >= best_score - 1e-6 && wc > best_word_count) {
                            best_score = s;
                            best_word_count = wc;
                            best_unique = Some(unique_name.clone());
                        }
                    }
                }
            }
        }

        // Log the match result for this column
        let best_display = best_unique.as_ref()
            .and_then(|u| catalog.iter().find(|(k, _)| k == u))
            .map(|(_, n)| n.as_str())
            .unwrap_or("—");
        let col_preview: Vec<&str> = col_texts.iter().take(4).map(|s| s.trim()).collect();
        let acc_label = if acc_used { " (+acc)" } else { "" };
        col_match_log.push(format!(
            "  Col[{}] x={:.2}: score={:.2}{} → \"{}\"\n    OCR: {:?}",
            col_idx, cx, best_score, acc_label, best_display, col_preview
        ));

        // Require 0.67 for per-column. Items where only "prime"+"blueprint" match
        // score exactly 0.667 (still rejected). A specific word matched via suffix
        // or Levenshtein + one generic word scores ≥0.69 and is now accepted,
        // preventing the fallback which can cross-contaminate words from other columns.
        if best_score < 0.67 {
            // Unknown item (WFCD not yet updated or OCR garbled).
            // Emit raw OCR text with a "?:" prefix so the overlay can still show something.
            let raw = col_texts.iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .take(3)
                .collect::<Vec<_>>()
                .join(" ");
            if !raw.is_empty() {
                items.push(format!("?:{}", raw));
                positions.push(*cx);
            }
            continue;
        }
        let unique = match best_unique { Some(u) => u, None => continue };
        // No dedup here — each column is a distinct physical card.
        // Two players cracking the same relic legitimately show the same reward twice.
        // Same-set phantoms from an over-wide column layout are removed later by the
        // base-word X-cluster dedup, which uses these scores to keep the strongest.
        items.push(unique);
        positions.push(*cx);
        item_scores.push(best_score);
        let _ = col_idx;
    }

    // ── 3b. Full-frame fill ───────────────────────────────────────────────────
    // Expected card count from on-screen evidence only (see word_card_count above):
    //   • word_card_count   (prime+forma / x-clusters / distinct strong matches)
    //   • rarity bar count   (visual, only when bars passed spacing validation)
    // The EE.log squad size is deliberately NOT used here — it can exceed the real
    // card count (player with no relic, or who left), which previously fabricated a
    // phantom extra card.
    // IMPORTANT: only include bar count when bars_trusted. Rejected bars can give
    // wrong counts (e.g. 4 bars detected on a 3-card screen) that keep the OCR
    // loop retrying forever on a number it can never reach.
    let estimated_cards = word_card_count
        .max(if bars_trusted { card_centers.len() } else { 0 })
        .max(1);

    if items.len() < estimated_cards {
        let all_words = build_word_set(
            &ocr_words.iter()
                .map(|(t, _)| t.clone())
                .collect::<Vec<_>>()
        );

        // Words that appear in almost every reward and carry no item-specific
        // information. Excluded when finding which OCR line "anchors" each item
        // (for left-to-right ordering), but still used in scoring.
        const GENERIC: &[&str] = &["prime", "owned", "crafted", "blueprint"];

        // Find candidates with score ≥ 0.80 and sort by leftmost X position.
        // OCR words carry individual bounding-box centres, so we find the leftmost
        // word that matches a key word for each candidate.  This gives the correct
        // left→right overlay order without relying on Tesseract's line grouping.
        let mut candidates: Vec<(f32, f32, usize, String)> = Vec::new(); // (x_pos, score, name_len, unique)
        for (unique_name, display_name) in catalog {
            if display_name.len() < 5 { continue; }
            let s = score_item(display_name, &all_words);
            if s < 0.80 { continue; }

            let norm_dn = normalise(display_name);
            let key_words: Vec<&str> = norm_dn.split_whitespace()
                .filter(|w| w.len() >= 4 && !GENERIC.contains(w))
                .collect();

            // Find the leftmost OCR word that contains one of this item's key words
            let leftmost_x = if key_words.is_empty() {
                0.999f32 // no unique identifier → sort after items with known positions
            } else {
                ocr_words.iter()
                    .filter(|(word_text, _)| {
                        let wt = normalise(word_text);
                        key_words.iter().any(|&w| wt.contains(w))
                    })
                    .map(|(_, x)| *x)
                    .fold(0.999f32, f32::min)
            };

            candidates.push((leftmost_x, s, display_name.len(), unique_name.clone()));
        }
        // Primary: leftmost X (left → right). Secondary: score. Tertiary: name length.
        candidates.sort_by(|a, b|
            a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
                .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
                .then(b.2.cmp(&a.2))
        );

        // Seed base-name dedup from items already found by per-column matching.
        // Also track per-column duplicate counts: an item that appeared in N different
        // columns is legitimately repeated N times (4 players cracking the same relic).
        // We only re-allow it in the fill if it genuinely appeared multiple times.
        let mut seen_bases: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut per_col_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for un in &items {
            *per_col_counts.entry(un.clone()).or_insert(0) += 1;
            if let Some((_, dn)) = catalog.iter().find(|(u, _)| u == un) {
                let norm = normalise(dn);
                let ws: Vec<&str> = norm.split_whitespace().collect();
                if ws.len() >= 2 { seen_bases.insert(ws[..ws.len()-1].join(" ")); }
            }
        }

        for (_, s, _, unique) in candidates {
            if items.len() >= estimated_cards { break; }
            let dn = match catalog.iter().find(|(u, _)| *u == unique) {
                Some((_, n)) => n.clone(),
                None => continue,
            };
            let dk = normalise(&dn);
            let current_count = items.iter().filter(|u| *u == &unique).count();
            let col_count = per_col_counts.get(&unique).copied().unwrap_or(0);
            let is_exact_duplicate = current_count > 0;
            let ws: Vec<&str> = dk.split_whitespace().collect();

            if is_exact_duplicate {
                // Only allow adding another copy if per-column matching confirmed
                // the same item in ≥2 columns (genuine multi-player duplicate).
                // Prevents filling missing-column gaps with re-copies of already-found items.
                if col_count < 2 || current_count >= col_count { continue; }
            } else {
                // Sibling dedup: block a DIFFERENT item from the same base name
                // (e.g. "Dual Zoren Prime Handle" blocked if "Dual Zoren Prime Blueprint" found)
                if ws.len() >= 2 {
                    let base = ws[..ws.len()-1].join(" ");
                    if seen_bases.contains(&base) { continue; }
                    seen_bases.insert(base);
                }
            }
            items.push(unique);
            item_scores.push(s);
        }

        // Assign positions using the estimated card count for even spacing.
        // Cards are evenly distributed across the central ~70% of the screen.
        if !items.is_empty() {
            let n = estimated_cards.max(items.len());
            let spacing = 0.70 / (n as f32 + 1.0);
            positions = (0..items.len())
                .map(|i| 0.15 + spacing * (i as f32 + 1.0))
                .collect();
        }
    }

    // ── 3c. Same-set phantom dedup ─────────────────────────────────────────────
    // A column layout wider than the real card count (squad hint > cards on screen
    // — a squadmate with no relic, or who left) can split one card's text across
    // two columns and match two DIFFERENT parts of the same set (e.g. Atlas Prime
    // Blueprint + Atlas Prime Chassis Blueprint). Bound each base name to the number
    // of distinct X positions its identifier word actually occupies on screen: one
    // physical card → one X cluster. Two players holding different parts of the same
    // set sit at well-separated X positions, so they survive. Highest score wins.
    if items.len() > 1 && items.len() == item_scores.len() {
        let base_of = |u: &str| -> String {
            catalog.iter().find(|(k, _)| k == u)
                .and_then(|(_, n)| normalise(n).split_whitespace().next().map(|s| s.to_string()))
                .unwrap_or_default()
        };
        let mut by_base: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
        for (i, u) in items.iter().enumerate() {
            by_base.entry(base_of(u)).or_default().push(i);
        }
        let mut drop = vec![false; items.len()];
        for (base, idxs) in &by_base {
            if base.is_empty() || idxs.len() <= 1 { continue; }
            let allowed = base_word_x_clusters(base, &ocr_words).max(1);
            if idxs.len() <= allowed { continue; }
            let mut by_score = idxs.clone();
            by_score.sort_by(|&a, &b| item_scores[b]
                .partial_cmp(&item_scores[a]).unwrap_or(std::cmp::Ordering::Equal));
            for &i in &by_score[allowed..] { drop[i] = true; }
        }
        if drop.iter().any(|&d| d) {
            let (mut ni, mut np) = (Vec::new(), Vec::new());
            for i in 0..items.len() {
                if !drop[i] {
                    ni.push(items[i].clone());
                    np.push(*positions.get(i).unwrap_or(&0.5));
                }
            }
            items = ni; positions = np;
        }
    }

    // ── Diagnostic string ─────────────────────────────────────────────────────
    let col_mode = if bars_trusted { "bar columns (validated)" }
                   else if have_bars { "hardcoded (bars rejected)" }
                   else { "hardcoded (no bars)" };
    let ff_items: Vec<&str> = items.iter().map(|s| {
        catalog.iter().find(|(u,_)| u == s).map(|(_,n)| n.as_str()).unwrap_or(s.as_str())
    }).collect();
    // is_complete gates the double-confirmation lock in lib.rs. It is tied to the
    // EVIDENCE count (not the layout/hint count): we lock once we've matched at
    // least as many cards as the screen actually shows evidence for. Using the
    // layout count here would demand the hint's (possibly inflated) number and
    // block locking the correct, smaller deduped result.
    let complete_target = evidence_count
        .max(if bars_trusted { card_centers.len() } else { 0 })
        .max(1);
    let is_complete = !items.is_empty() && items.len() >= complete_target;
    // Count source for diagnostics. The EE.log squad hint is shown on its own line
    // below but is no longer a count source — the card count is evidence-driven.
    let expected_src = if bars_trusted && card_centers.len() >= word_card_count {
        "bars"
    } else if match_card_count >= ocr_cluster_count && match_card_count >= prime_count + forma_count {
        "matches"
    } else if ocr_cluster_count > prime_count + forma_count {
        "x-clusters"
    } else {
        "prime+forma"
    };
    let ee_hint_str = match hint_squad_size {
        Some(n) => format!("{} players (from EE.log)", n),
        None    => "(not available — VoidProjections sequence not seen yet)".into(),
    };
    let debug = format!(
        "├─ Capture  : {}\n\
         ├─ OCR      : {} chars, {} lines\n\
         ├─ Bars     : {}\n\
         ├─ Prime/Forma: {}p + {}f + {}x + {}m = {} evidence ({} layout cols)\n\
         ├─ EE hint  : {}\n\
         ├─ Expected : {} cards (from {}){}\n\
         ├─ Match    : {} — {} formed\n\
         {}\n\
         └─ Items    : {:?}",
        capture_info,
        raw_full.len(), ocr_words.len(),
        bar_diag,
        prime_count, forma_count, ocr_cluster_count, match_card_count, evidence_count, word_card_count,
        ee_hint_str,
        complete_target, expected_src,
        if is_complete { " ✅ complete" } else { " ⚡ partial" },
        col_mode, columns.len(),
        col_match_log.join("\n"),
        ff_items,
    );

    (is_complete, false, items, positions, debug)
}



#[cfg(not(target_os = "windows"))]
pub fn capture_warframe_reward_area() -> Option<(Vec<u8>, u32, u32, u32, String)> {
    let windows = xcap::Window::all().ok()?;
    let warframe = windows.into_iter().find(|w| {
        w.title().map(|t| t.to_lowercase().contains("warframe")).unwrap_or(false)
    })?;

    let image = warframe.capture_image().ok()?;
    let full_w = image.width();
    let full_h = image.height();
    if full_w < 100 || full_h < 100 { return None; }

    // Capture ONLY the reward text band (centre of screen, y ≈ 30–55 %).
    // The overlay window is placed at y ≈ 74 % so it is never in this band.
    let cap_y = (full_h as f32 * 0.30) as u32;
    let cap_h = (full_h as f32 * 0.25) as u32;
    let cap_bot = (cap_y + cap_h).min(full_h);
    let actual_cap_h = cap_bot - cap_y;

    let mut pixels = Vec::with_capacity((full_w * actual_cap_h * 4) as usize);
    for y in cap_y..cap_bot {
        for x in 0..full_w {
            let px = image.get_pixel(x, y);
            pixels.push(px[2]); // B
            pixels.push(px[1]); // G
            pixels.push(px[0]); // R
            pixels.push(px[3]); // A
        }
    }

    let avg = avg_brightness(&pixels);
    let info = format!(
        "xcap  {}×{}px (band {}–{}px)  avg_brightness={}",
        full_w, full_h, cap_y, cap_bot, avg
    );
    Some((pixels, full_w, actual_cap_h, full_h, info))
}

/// Parse Tesseract TSV at WORD level.
#[cfg(not(target_os = "windows"))]
fn parse_tsv_words(tsv: &str, img_w: u32) -> Vec<(String, f32)> {
    let mut words: Vec<(String, f32)> = Vec::new();
    for row in tsv.lines().skip(1) {
        let cols: Vec<&str> = row.split('\t').collect();
        if cols.len() < 12 { continue; }
        let level: i32 = cols[0].parse().unwrap_or(0);
        if level != 5 && level != 4 { continue; }
        let text = cols[11].to_string();
        if text.trim().is_empty() { continue; }
        let left: f32 = cols[6].parse().unwrap_or(0.0);
        let width: f32 = cols[8].parse().unwrap_or(0.0);
        let x_frac = if img_w > 0 { (left + width / 2.0) / img_w as f32 } else { 0.5 };
        words.push((text, x_frac));
    }
    words
}

/// Encode RGB pixels as a 24-bit BGR BMP for Tesseract set_image_from_mem.
#[cfg(not(target_os = "windows"))]
fn rgb_to_bmp(pixels_rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let row_bytes = width * 3;
    let padding   = (4 - row_bytes % 4) % 4;
    let row_stride = row_bytes + padding;
    let image_size = row_stride * height;
    let file_size  = 54 + image_size;

    let mut bmp = Vec::with_capacity(file_size as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&(width as i32).to_le_bytes());
    bmp.extend_from_slice(&(-(height as i32)).to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&24u16.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&image_size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());

    for row in 0..height {
        for col in 0..width {
            let i = ((row * width + col) * 3) as usize;
            // BMP is BGR, input is RGB
            bmp.push(pixels_rgb[i + 2]); // B
            bmp.push(pixels_rgb[i + 1]); // G
            bmp.push(pixels_rgb[i]);     // R
        }
        for _ in 0..padding { bmp.push(0); }
    }
    bmp
}

/// Main entry point for Linux.
///
/// Strategy (learned from testing on real captures):
///   1. Crop horizontally to the centred reward box (~968 px at 1080p).
///      This removes the Warframe character visible on the sides.
///   2. Crop vertically to the text band inside the reward box.
///      This removes item icons above and rarity bars below.
///   3. Convert to greyscale.  Tesseract's internal adaptive thresholding
///      handles low-contrast grey text on dark cards far better than our
///      crude theme-based binary thresholding, which was destroying text.
///   4. OCR the whole band in ONE pass (PSM 6) so each item name stays intact.
///      (The old per-strip split sliced cards straddling a strip boundary.)
///   5. Map each word's position back to full-image fractions for the caller's
///      column assignment — this function does NOT decide the card count.
#[cfg(not(target_os = "windows"))]
pub fn run_ocr(pixels: &[u8], pix_w: u32, pix_h: u32, full_h: u32, ui_scale: f32) -> Result<(String, Vec<(String, f32)>), String> {
    let scale = full_h as f32 / 1080.0;

    // ── 1. Horizontal crop to centred reward box ────────────────────────────
    // wfinfo-ng constants: reward box is 968 px wide at 1080p, centred. The box
    // (and the cards inside it) shrink linearly with the in-game UI scale, so we
    // narrow the crop to match — keeping it centred means the 4 equal columns
    // below land on the actual card positions at any UI scale.
    let most_w = (968.0 * scale * ui_scale) as u32;
    let x_off = if pix_w > most_w { (pix_w - most_w) / 2 } else { 0 };
    let crop_w = most_w.min(pix_w);

    // ── 2. Vertical crop to text band inside the reward box ─────────────────
    // The captured band is 25 % of the full height (y ≈ 30–55 %).
    //
    // Empirically calibrated from real 1080p captures:
    //   • Item text sits roughly at band y = 90–150 within a 270-px band.
    //   • Player names appear below the cards at band y ≈ 180+.
    //   • Shifting the crop UP by 40 px captures 3-line item names that extend
    //     higher; the height excludes player names that used to leak into OCR.
    //   • A 130-px crop gives the single-pass OCR enough context for 2–3 line
    //     names. (The reward icon sits in the top of this band, but the single
    //     full-frame OCR below tolerates it; a tighter "icon-free" crop was tried
    //     and actually read 2-line names WORSE and clipped cards whose text sits
    //     higher in the band, so the taller band wins.)
    // The item text shrinks with UI scale, so the band offset and height scale too.
    // A 60 px floor keeps enough rows for Tesseract at the smallest supported scale.
    let text_y_start = (pix_h as f32 / 3.0 - 40.0 * ui_scale).max(0.0);  // shifted up ~40 px
    let text_y_end   = (text_y_start + (130.0 * ui_scale).max(60.0)).min(pix_h as f32);

    let crop_y = text_y_start as u32;
    let crop_h = ((text_y_end - text_y_start).max(1.0) as u32).min(pix_h.saturating_sub(crop_y));

    // ── 3. Convert cropped region to greyscale ──────────────────────────────
    let mut grey = Vec::with_capacity((crop_w * crop_h) as usize);
    for y in crop_y..(crop_y + crop_h) {
        for x in x_off..(x_off + crop_w).min(pix_w) {
            let i = ((y * pix_w + x) * 4) as usize;
            let lum = if i + 2 < pixels.len() {
                ((pixels[i+2] as u32 + pixels[i+1] as u32 + pixels[i] as u32) / 3) as u8
            } else { 128 };
            grey.push(lum);
        }
    }

    // Debug: save cropped greyscale
    let debug_grey = std::env::temp_dir().join("frameforge_ocr_debug_grey.bmp");
    let grey_rgb: Vec<u8> = grey.iter().flat_map(|&l| [l, l, l]).collect();
    let _ = std::fs::write(&debug_grey, rgb_to_bmp(&grey_rgb, crop_w, crop_h));

    // ── 4. OCR the whole text band in ONE pass ──────────────────────────────
    // A single wide image keeps every item name intact. The previous approach
    // OCR'd 4 fixed-width strips, which SLICED any card straddling a strip
    // boundary — e.g. "FORMA BLUEPRINT" became "Fo" in one strip + "rma
    // Blueprint" in the next, so that card matched nothing and was lost. We still
    // parse the TSV at WORD level, so every word keeps its own X position for the
    // downstream column assignment; no card-count guess happens in this function.
    //
    // Trim a few px off each side to drop the card border dividers ("| name |"),
    // which otherwise OCR as phantom "|" / "I" glyphs. Scales with resolution / UI.
    let border_crop = (5.0 * scale * ui_scale).round() as u32;
    let bx0 = border_crop.min(crop_w.saturating_sub(1));
    let bx1 = crop_w.saturating_sub(border_crop).max(bx0 + 1);
    let band_w = bx1 - bx0;
    let mut band_grey = Vec::with_capacity((band_w * crop_h) as usize);
    for y in 0..crop_h {
        for x in bx0..bx1 {
            band_grey.push(grey[(y * crop_w + x) as usize]);
        }
    }

    // Dark-frame contrast stretch (adaptive — only when the band is genuinely dark,
    // so bright captures behave identically). On dim tilesets the faint card text
    // sits below Tesseract's adaptive threshold, so a 2nd/3rd card's words are
    // dropped and its X-cluster vanishes. Stretching [median, p99.5] → [0,255]
    // recovers those words so the card-count clustering sees both cards; the tight
    // per-column re-OCR then reads each garbled name.
    if !band_grey.is_empty() {
        let mean: u32 = band_grey.iter().map(|&v| v as u32).sum::<u32>() / band_grey.len() as u32;
        if mean < 80 {
            let mut hist = [0u32; 256];
            for &v in &band_grey { hist[v as usize] += 1; }
            let total = band_grey.len() as u32;
            let pct = |p: f32| -> u8 {
                let target = (total as f32 * p) as u32;
                let mut acc = 0u32;
                for (v, &c) in hist.iter().enumerate() {
                    acc += c;
                    if acc >= target { return v as u8; }
                }
                255
            };
            let lo = pct(0.50) as f32;
            let hi = (pct(0.995) as f32).max(lo + 1.0);
            for v in band_grey.iter_mut() {
                *v = (((*v as f32 - lo) * 255.0 / (hi - lo)).clamp(0.0, 255.0)) as u8;
            }
        }
    }

    let band_rgb: Vec<u8> = band_grey.iter().flat_map(|&l| [l, l, l]).collect();
    let bmp = rgb_to_bmp(&band_rgb, band_w, crop_h);

    // PSM 6 (assume a single uniform block of text) reads the card names
    // left→right across the band. It beat the default (3), sparse (11/12) and
    // column (4) modes on real captures — those dropped or merged whole cards.
    let tess = tesseract::Tesseract::new(None, Some("eng"))
        .map_err(|e| e.to_string())?
        .set_variable("tessedit_pageseg_mode", "6")
        .map_err(|e| e.to_string())?;
    let mut tess = match tess.set_image_from_mem(&bmp) {
        Ok(t) => t.recognize().map_err(|e| e.to_string())?,
        Err(e) => return Err(e.to_string()),
    };

    let full_text = tess.get_text().unwrap_or_default();
    let tsv = tess.get_tsv_text(0).unwrap_or_default();
    let x0_frac    = (x_off + bx0) as f32 / pix_w as f32;
    let scale_frac = band_w as f32 / pix_w as f32;
    let all_words: Vec<(String, f32)> = parse_tsv_words(&tsv, band_w)
        .into_iter()
        .map(|(w, x)| (w, x0_frac + x * scale_frac))
        .collect();

    Ok((full_text, all_words))
}

// ─── Linux riven OCR primitives ──────────────────────────────────────────────
// Mirror the Windows capture_warframe_pixels / ocr_pixels_rect[_raw] interface
// so the cross-platform riven commands (ocr_riven_screen, riven_screen_status,
// riven_screen_visible) work on Linux. Capture uses xcap; OCR uses Tesseract —
// the same engine the relic reward pipeline already uses — in place of WinRT OCR.

/// Capture the full Warframe client area as a BGRA buffer.
/// The byte layout matches the Windows capture_warframe_pixels output so the
/// shared preprocess_for_ocr and the rect-crop math below are identical across
/// platforms.
#[cfg(not(target_os = "windows"))]
pub fn capture_warframe_pixels() -> Result<(Vec<u8>, u32, u32), String> {
    let windows = xcap::Window::all().map_err(|e| format!("xcap: {e}"))?;
    let warframe = windows
        .into_iter()
        .find(|w| w.title().map(|t| t.to_lowercase().contains("warframe")).unwrap_or(false))
        .ok_or_else(|| "Warframe window not found".to_string())?;

    let image = warframe.capture_image().map_err(|e| format!("capture: {e}"))?;
    let full_w = image.width();
    let full_h = image.height();
    if full_w < 100 || full_h < 100 { return Err("Window too small".into()); }

    let mut pixels = vec![0u8; (full_w * full_h * 4) as usize];
    for y in 0..full_h {
        for x in 0..full_w {
            let px = image.get_pixel(x, y); // xcap returns RGBA
            let i = ((y * full_w + x) * 4) as usize;
            pixels[i]     = px[2]; // B
            pixels[i + 1] = px[1]; // G
            pixels[i + 2] = px[0]; // R
            pixels[i + 3] = px[3]; // A
        }
    }
    Ok((pixels, full_w, full_h))
}

/// Linux diagnostics capture. xcap grabs the Warframe window's own surface
/// (XWayland composited), so any overlay drawn on top by the compositor is NOT
/// guaranteed to be included — unlike the Windows desktop BitBlt path — but it
/// still captures the game state for scanner/OCR debugging. Returns BGRA pixels.
#[cfg(not(target_os = "windows"))]
pub fn capture_screen_for_diagnostics() -> Result<(Vec<u8>, u32, u32), String> {
    capture_warframe_pixels()
}

/// Crop a fractional BGRA rect and encode it as a 24-bit RGB BMP for Tesseract.
/// When `preprocess` is set, the crop is grayscaled + contrast-stretched first
/// (helps colored stat-element glyphs read as neutral text, as on Windows).
#[cfg(not(target_os = "windows"))]
fn bgra_rect_to_bmp(
    pixels: &[u8], full_w: u32, full_h: u32,
    x_start: f32, x_end: f32, y_start: f32, y_end: f32,
    preprocess: bool,
) -> Result<Vec<u8>, String> {
    let col_s = (full_w as f32 * x_start.clamp(0.0, 1.0)) as usize;
    let col_e = ((full_w as f32 * x_end.clamp(0.0, 1.0)) as usize).min(full_w as usize);
    let row_s = (full_h as f32 * y_start.clamp(0.0, 1.0)) as usize;
    let row_e = ((full_h as f32 * y_end.clamp(0.0, 1.0)) as usize).min(full_h as usize);
    if col_e <= col_s || row_e <= row_s { return Err("Region too small".into()); }
    let rect_w = (col_e - col_s) as u32;
    let rect_h = (row_e - row_s) as u32;
    if rect_w < 4 || rect_h < 4 { return Err("Region too small".into()); }

    let src_stride = full_w as usize * 4;
    let dst_stride = rect_w as usize * 4;
    let mut cropped = vec![0u8; dst_stride * rect_h as usize];
    for row in 0..rect_h as usize {
        let src = (row_s + row) * src_stride + col_s * 4;
        let dst = row * dst_stride;
        cropped[dst..dst + dst_stride].copy_from_slice(&pixels[src..src + dst_stride]);
    }

    let bgra = if preprocess {
        preprocess_for_ocr(&cropped, rect_w, rect_h).0
    } else {
        cropped
    };

    // BGRA → RGB for rgb_to_bmp.
    let mut rgb = Vec::with_capacity((rect_w * rect_h * 3) as usize);
    for px in bgra.chunks_exact(4) {
        rgb.push(px[2]); // R
        rgb.push(px[1]); // G
        rgb.push(px[0]); // B
    }
    Ok(rgb_to_bmp(&rgb, rect_w, rect_h))
}

/// Run Tesseract over a BMP and return the recognized text (PSM 6, single block).
#[cfg(not(target_os = "windows"))]
fn run_tesseract_text(bmp: &[u8]) -> Result<String, String> {
    let tess = tesseract::Tesseract::new(None, Some("eng"))
        .map_err(|e| e.to_string())?
        .set_variable("tessedit_pageseg_mode", "6")
        .map_err(|e| e.to_string())?;
    let mut tess = tess
        .set_image_from_mem(bmp)
        .map_err(|e| e.to_string())?
        .recognize()
        .map_err(|e| e.to_string())?;
    Ok(tess.get_text().unwrap_or_default())
}

/// OCR a fractional rectangle from a pre-captured BGRA buffer (with preprocessing).
#[cfg(not(target_os = "windows"))]
pub fn ocr_pixels_rect(
    pixels: &[u8], full_w: u32, full_h: u32,
    x_start: f32, x_end: f32, y_start: f32, y_end: f32,
) -> Result<String, String> {
    let bmp = bgra_rect_to_bmp(pixels, full_w, full_h, x_start, x_end, y_start, y_end, true)?;
    run_tesseract_text(&bmp)
}

/// OCR a fractional rectangle WITHOUT preprocessing — white-on-dark UI text
/// (e.g. "INVENTORY", "FITS IN") reads fine raw.
#[cfg(not(target_os = "windows"))]
pub fn ocr_pixels_rect_raw(
    pixels: &[u8], full_w: u32, full_h: u32,
    x_start: f32, x_end: f32, y_start: f32, y_end: f32,
) -> Result<String, String> {
    let bmp = bgra_rect_to_bmp(pixels, full_w, full_h, x_start, x_end, y_start, y_end, false)?;
    run_tesseract_text(&bmp)
}

#[cfg(test)]
mod tests {
    // 4-card reward screen: Gyre Prime Blueprint, Paris Prime Upper Limb,
    // Xaku Prime Chassis Blueprint, Kompressa Prime Blueprint.
    #[cfg(not(target_os = "windows"))]
    const FIXTURE_4CARD: &str =
        "/workspace/Warframe/rewards_gyrebp_parisupper_xakuchassis_kompressabp.png";
    // 3-card reward screen: Fang Prime Handle, Atlas Prime Blueprint, Braton Prime Stock.
    #[cfg(not(target_os = "windows"))]
    const FIXTURE_3CARD: &str =
        "/workspace/Warframe/rewards_fanghandle_atlasbp_bratonstock.png";

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_run_ocr_on_reward_screenshot() {
        use image::GenericImageView;
        let img = image::open(FIXTURE_4CARD).unwrap();
        let (w, h) = img.dimensions();

        // Convert to BGRA (same as xcap produces)
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let px = img.get_pixel(x, y);
                pixels.push(px[2]); // B
                pixels.push(px[1]); // G
                pixels.push(px[0]); // R
                pixels.push(px[3]); // A
            }
        }

        // Simulate capture_warframe_reward_area: crop to y=30%-55%
        let cap_y = (h as f32 * 0.30) as u32;
        let cap_h = (h as f32 * 0.25) as u32;
        let mut band_pixels = Vec::with_capacity((w * cap_h * 4) as usize);
        for y in cap_y..(cap_y + cap_h) {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                band_pixels.push(pixels[i]);
                band_pixels.push(pixels[i+1]);
                band_pixels.push(pixels[i+2]);
                band_pixels.push(pixels[i+3]);
            }
        }

        let (full_text, words) = super::run_ocr(&band_pixels, w, cap_h, h, 1.0).unwrap();

        println!("OCR full_text:\n{}", full_text);
        println!("OCR words: {:?}", words);

        // The screenshot contains: Gyre Prime BP, Paris Prime Upper Limb,
        // Xaku Prime Chassis BP, Kompressa Prime BP
        let combined = full_text.to_lowercase();
        assert!(
            combined.contains("gyre") || combined.contains("prime"),
            "Expected at least some prime item text, got: {}", combined
        );

        // Check that we got words from multiple cards (at least 3 cards should have some text)
        let cards_with_text: usize = (0..4)
            .filter(|&card| {
                let cx = match card {
                    0 => 0.31, 1 => 0.44, 2 => 0.56, _ => 0.69,
                };
                words.iter().any(|(_, x)| (x - cx).abs() < 0.12)
            })
            .count();
        assert!(
            cards_with_text >= 2,
            "Expected words in at least 2 cards, found in {}. Words: {:?}",
            cards_with_text, words
        );
    }

    /// Regression for the dropped-4th-card bug: a 4-card screen with NO EE squad
    /// hint must still resolve all four items. Previously the card count came out
    /// as 3 (garbled "Prime" + merged x-clusters), so "Paris Prime Upper Limb"
    /// was split across columns and lost. The distinct-match count signal fixes it.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_extract_four_cards_no_hint() {
        use image::GenericImageView;
        let img = image::open(FIXTURE_4CARD).unwrap();
        let (w, h) = img.dimensions();

        let cap_y = (h as f32 * 0.30) as u32;
        let cap_h = (h as f32 * 0.25) as u32;
        let mut band = Vec::with_capacity((w * cap_h * 4) as usize);
        for y in cap_y..(cap_y + cap_h) {
            for x in 0..w {
                let px = img.get_pixel(x, y);
                band.push(px[2]); band.push(px[1]); band.push(px[0]); band.push(px[3]);
            }
        }

        // Catalog includes the four real rewards AND same-frame siblings
        // (Xaku Systems/Neuroptics, Paris Lower Limb, Gyre/Kompressa parts).
        // These siblings share base words with the real drops, so a matcher that
        // pools the whole frame's text would wrongly surface several of them —
        // this is the "all Xaku parts as rewards" regression. Per-column matching
        // must pick exactly one item per card.
        let catalog: Vec<(String, String)> = vec![
            ("/Lotus/Types/Recipes/Warframes/GyrePrimeBlueprint".into(),    "Gyre Prime Blueprint".into()),
            ("/Lotus/Types/Recipes/Warframes/GyrePrimeChassis".into(),      "Gyre Prime Chassis Blueprint".into()),
            ("/Lotus/Types/Recipes/Warframes/GyrePrimeSystems".into(),      "Gyre Prime Systems Blueprint".into()),
            ("/Lotus/Types/Recipes/Weapons/ParisPrimeUpperLimb".into(),     "Paris Prime Upper Limb".into()),
            ("/Lotus/Types/Recipes/Weapons/ParisPrimeLowerLimb".into(),     "Paris Prime Lower Limb".into()),
            ("/Lotus/Types/Recipes/Weapons/ParisPrimeGrip".into(),          "Paris Prime Grip".into()),
            ("/Lotus/Types/Recipes/Warframes/XakuPrimeChassis".into(),      "Xaku Prime Chassis Blueprint".into()),
            ("/Lotus/Types/Recipes/Warframes/XakuPrimeSystems".into(),      "Xaku Prime Systems Blueprint".into()),
            ("/Lotus/Types/Recipes/Warframes/XakuPrimeNeuroptics".into(),   "Xaku Prime Neuroptics Blueprint".into()),
            ("/Lotus/Types/Recipes/Warframes/XakuPrimeBlueprint".into(),    "Xaku Prime Blueprint".into()),
            ("/Lotus/Types/Recipes/Weapons/KompressaPrimeBlueprint".into(), "Kompressa Prime Blueprint".into()),
            ("/Lotus/Types/Recipes/Weapons/KompressaPrimeReceiver".into(),  "Kompressa Prime Receiver".into()),
            ("/Lotus/Types/Recipes/Warframes/MagPrimeBlueprint".into(),     "Mag Prime Blueprint".into()),
            ("/Lotus/Types/Recipes/Weapons/BurstonPrimeBarrel".into(),      "Burston Prime Barrel".into()),
        ];

        let (complete, _relic, items, positions, dbg) = super::extract_reward_items_twophase(
            &band, w, cap_h, h, &catalog, "test", None, 1.0, None,
        );
        println!("{}", dbg);
        println!("items={:?} positions={:?} complete={}", items, positions, complete);

        let bases = ["gyre", "paris", "xaku", "kompressa"];
        let found: Vec<&str> = bases.iter().copied().filter(|b| {
            items.iter().any(|u| {
                catalog.iter().find(|(k, _)| k == u)
                    .map(|(_, n)| n.to_lowercase().contains(b)).unwrap_or(false)
            })
        }).collect();
        assert_eq!(found.len(), 4,
            "Expected all 4 distinct items, found {:?} (items={:?})", found, items);
        // Exactly 4 cards — no sibling flooding (e.g. multiple Xaku parts).
        assert_eq!(items.len(), 4,
            "Expected exactly 4 reward cards, got {} (items={:?})", items.len(), items);
        // Each detected item must belong to a distinct frame/weapon (distinct base
        // name) — catches the "all Xaku parts" regression directly.
        let mut bases_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for u in &items {
            let dn = catalog.iter().find(|(k, _)| k == u).map(|(_, n)| n.clone()).unwrap_or_default();
            let base = dn.split_whitespace().next().unwrap_or("").to_lowercase();
            assert!(bases_seen.insert(base.clone()),
                "duplicate base frame {:?} among items {:?}", base, items);
        }
        // Positions must be sorted left→right so the overlay places them correctly.
        for win in positions.windows(2) {
            assert!(win[0] <= win[1] + 1e-3,
                "positions not left→right ordered: {:?}", positions);
        }
    }

    /// Regression for the dropped-card / sliced-Forma bug on a real 4-player drop
    /// (Yareli Prime Chassis, Braton Prime Blueprint, Braton Prime Stock, Forma
    /// Blueprint ×2). Two failures combined: every OCR evidence signal undercounted
    /// (two Braton parts share a base, "Prime"/"Forma" garbled), and the old
    /// fixed 4-strip OCR sliced "FORMA BLUEPRINT" across a strip boundary so it
    /// matched nothing. Now: the layout uses the squad hint (4 columns), the
    /// single full-frame OCR keeps each name intact, and the two Braton parts stay
    /// distinct.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_four_player_drop_with_forma() {
        use image::GenericImageView;
        let img = image::open("/workspace/Warframe/rewards_missed_4p.png").unwrap();
        let (w, h) = img.dimensions();
        let cap_y = (h as f32 * 0.30) as u32;
        let cap_h = (h as f32 * 0.25) as u32;
        let mut band = Vec::with_capacity((w * cap_h * 4) as usize);
        for y in cap_y..(cap_y + cap_h) {
            for x in 0..w {
                let px = img.get_pixel(x, y);
                band.push(px[2]); band.push(px[1]); band.push(px[0]); band.push(px[3]);
            }
        }
        let catalog: Vec<(String, String)> = vec![
            ("/a".into(),  "Yareli Prime Chassis Blueprint".into()),
            ("/a2".into(), "Yareli Prime Systems Blueprint".into()),
            ("/a3".into(), "Yareli Prime Neuroptics Blueprint".into()),
            ("/a4".into(), "Yareli Prime Blueprint".into()),
            ("/b".into(),  "Braton Prime Blueprint".into()),
            ("/c".into(),  "Braton Prime Stock".into()),
            ("/d".into(),  "Braton Prime Barrel".into()),
            ("/e".into(),  "Braton Prime Receiver".into()),
            ("/f".into(),  "Forma Blueprint".into()),
            ("/g".into(),  "Mag Prime Blueprint".into()),
            ("/h".into(),  "Burston Prime Barrel".into()),
        ];
        let (complete, _r, items, positions, dbg) = super::extract_reward_items_twophase(
            &band, w, cap_h, h, &catalog, "test", Some(4), 1.0, None,
        );
        println!("{}", dbg);
        println!("complete={} items={:?} positions={:?}", complete, items, positions);
        let display = |u: &String| catalog.iter().find(|(k, _)| k == u)
            .map(|(_, n)| n.to_lowercase()).unwrap_or_default();
        let names: Vec<String> = items.iter().map(display).collect();

        // All four cards detected (the old strip OCR dropped to 2–3).
        assert_eq!(items.len(), 4,
            "Expected all 4 cards on a 4-player drop, got {} ({:?})", items.len(), names);
        // Forma must be present — it was the sliced/lost card.
        assert!(names.iter().any(|n| n.contains("forma")),
            "Forma Blueprint missing (was sliced by the per-strip OCR): {:?}", names);
        // Both distinct Braton parts present — not collapsed into one or duplicated.
        assert!(names.iter().any(|n| n.contains("braton") && n.contains("blueprint")),
            "Braton Prime Blueprint missing: {:?}", names);
        assert!(names.iter().any(|n| n.contains("braton") && n.contains("stock")),
            "Braton Prime Stock missing: {:?}", names);
        // A Yareli card is present (part may vary with OCR quality).
        assert!(names.iter().any(|n| n.contains("yareli")),
            "Yareli card missing: {:?}", names);
        assert_eq!(positions.len(), items.len(), "positions/items length mismatch");
    }

    /// Regression for the phantom-4th-card bug: a 3-card screen with a STALE EE
    /// squad hint of 4 (a squadmate left, or ran no relic) must still resolve
    /// exactly 3 items — never a fabricated 4th. Previously `.max(hint_squad_size)`
    /// forced a 4-column layout onto 3 cards; the centre Atlas card's text split
    /// across two columns and matched a phantom sibling ("Atlas Prime Chassis
    /// Blueprint" alongside the real "Atlas Prime Blueprint"). The card count is
    /// now evidence-driven, so the hint can no longer inflate it.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_three_cards_stale_hint_no_phantom() {
        use image::GenericImageView;
        let img = image::open(FIXTURE_3CARD).unwrap();
        let (w, h) = img.dimensions();

        let cap_y = (h as f32 * 0.30) as u32;
        let cap_h = (h as f32 * 0.25) as u32;
        let mut band = Vec::with_capacity((w * cap_h * 4) as usize);
        for y in cap_y..(cap_y + cap_h) {
            for x in 0..w {
                let px = img.get_pixel(x, y);
                band.push(px[2]); band.push(px[1]); band.push(px[0]); band.push(px[3]);
            }
        }

        // Catalog includes the three real rewards AND same-SET siblings of each —
        // especially "Atlas Prime Chassis Blueprint", the exact phantom that the
        // old over-count produced next to the real "Atlas Prime Blueprint".
        let catalog: Vec<(String, String)> = vec![
            ("/Lotus/Types/Recipes/Weapons/WeaponParts/PrimeFangHandle".into(), "Fang Prime Handle".into()),
            ("/Lotus/Types/Recipes/Weapons/FangPrimeBlade".into(),              "Fang Prime Blade".into()),
            ("/Lotus/Types/Recipes/Weapons/FangPrimeBlueprint".into(),          "Fang Prime Blueprint".into()),
            ("/Lotus/Types/Recipes/Warframes/AtlasPrimeBlueprint".into(),       "Atlas Prime Blueprint".into()),
            ("/Lotus/Types/Recipes/Warframes/AtlasPrimeChassis".into(),         "Atlas Prime Chassis Blueprint".into()),
            ("/Lotus/Types/Recipes/Warframes/AtlasPrimeSystems".into(),         "Atlas Prime Systems Blueprint".into()),
            ("/Lotus/Types/Recipes/Warframes/AtlasPrimeNeuroptics".into(),      "Atlas Prime Neuroptics Blueprint".into()),
            ("/Lotus/Types/Recipes/Weapons/WeaponParts/BratonPrimeStock".into(),"Braton Prime Stock".into()),
            ("/Lotus/Types/Recipes/Weapons/BratonPrimeBarrel".into(),           "Braton Prime Barrel".into()),
            ("/Lotus/Types/Recipes/Weapons/BratonPrimeReceiver".into(),         "Braton Prime Receiver".into()),
            ("/Lotus/Types/Recipes/Weapons/BratonPrimeBlueprint".into(),        "Braton Prime Blueprint".into()),
            ("/Lotus/Types/Recipes/Warframes/MagPrimeBlueprint".into(),         "Mag Prime Blueprint".into()),
            ("/Lotus/Types/Recipes/Weapons/BurstonPrimeBarrel".into(),          "Burston Prime Barrel".into()),
        ];

        // Stale squad hint of 4 — the failing condition.
        let (_complete, _relic, items, positions, dbg) = super::extract_reward_items_twophase(
            &band, w, cap_h, h, &catalog, "test", Some(4), 1.0, None,
        );
        println!("{}", dbg);
        println!("items={:?} positions={:?}", items, positions);

        let display = |u: &String| catalog.iter().find(|(k, _)| k == u)
            .map(|(_, n)| n.to_lowercase()).unwrap_or_default();

        // Core regression: exactly 3 cards, no fabricated 4th.
        assert_eq!(items.len(), 3,
            "Expected exactly 3 reward cards (stale hint=4 must not add a 4th), got {} (items={:?})",
            items.len(), items.iter().map(display).collect::<Vec<_>>());

        // The three real bases are present.
        for base in ["fang", "atlas", "braton"] {
            assert!(items.iter().any(|u| display(u).contains(base)),
                "missing expected base {:?} in {:?}", base, items.iter().map(display).collect::<Vec<_>>());
        }

        // No duplicate base — directly catches "Atlas Blueprint + Atlas Chassis".
        let mut bases_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for u in &items {
            let base = display(u).split_whitespace().next().unwrap_or("").to_string();
            assert!(bases_seen.insert(base.clone()),
                "duplicate base {:?} among items {:?}", base, items.iter().map(display).collect::<Vec<_>>());
        }

        for win in positions.windows(2) {
            assert!(win[0] <= win[1] + 1e-3,
                "positions not left→right ordered: {:?}", positions);
        }
    }

    // ── Riven scan-area sizing (Phase 3) ──────────────────────────────────────
    // Single-card reroll screenshots; expected stats encoded in the filenames.
    #[cfg(not(target_os = "windows"))]
    const RIVEN_IMG_AKARIUS: &str =
        "/workspace/Warframe/Akarius_Hexa-lexitox_+1.4_Punch_Through_+46.3%_Status_Chance_+39.5%_<toxinemoji>_Toxin_MR8.png";
    #[cfg(not(target_os = "windows"))]
    const RIVEN_IMG_AKLATO: &str =
        "/workspace/Warframe/Aklato_Crita-toxilis_+188%_Critical_Chance_+98.3%_<toxinemoji>_Toxin_+87.2%_Zoom_MR8.png";

    #[cfg(not(target_os = "windows"))]
    fn load_bgra(path: &str) -> (Vec<u8>, u32, u32) {
        use image::GenericImageView;
        let img = image::open(path).unwrap();
        let (w, h) = img.dimensions();
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let p = img.get_pixel(x, y);
                px.push(p[2]); px.push(p[1]); px.push(p[0]); px.push(p[3]);
            }
        }
        (px, w, h)
    }

    /// Phase 3 regression: the tightened, centred riven crop (the ui_scale=1.0
    /// case, x 0.40–0.60 / y 0.60–0.78) must cleanly OCR the weapon name and every
    /// rolled stat for both reference rolls (expected values are in the filenames).
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_riven_crop_reads_stats() {
        let (x0, x1, y0, y1) = (0.40f32, 0.60f32, 0.60f32, 0.78f32);
        let cases: &[(&str, &[&str])] = &[
            (RIVEN_IMG_AKARIUS, &["akarius", "punch through", "status chance", "toxin"]),
            (RIVEN_IMG_AKLATO,  &["aklato", "critical chance", "toxin", "zoom"]),
        ];
        for (path, expected) in cases {
            let (px, w, h) = load_bgra(path);
            let text = super::ocr_pixels_rect(&px, w, h, x0, x1, y0, y1)
                .unwrap_or_default()
                .to_lowercase();
            for needle in *expected {
                assert!(text.contains(needle),
                    "riven crop missing {:?} for {}\nOCR text:\n{}", needle, path, text);
            }
        }
    }

    /// Regression: a short catalog base name must NOT match as an interior/suffix
    /// substring of a longer DIFFERENT OCR word. "gara" inside "akjagara" wrongly
    /// matched "Gara Prime Blueprint" on an Akjagara Prime card.
    #[test]
    fn test_word_match_no_interior_substring() {
        use std::collections::HashSet;
        let mk = |ws: &[&str]| ws.iter().map(|s| s.to_string()).collect::<HashSet<String>>();

        let akja = mk(&["akjagara", "prime", "barrel", "blueprint"]);
        assert!(!super::word_found_in_set("gara", &akja),
            "'gara' must not match inside 'akjagara'");
        assert!(super::word_found_in_set("akjagara", &akja),
            "'akjagara' should match itself");

        // Merged-token prefix matching must still work (Sevagoth Prime → sevagotfirime).
        let merged = mk(&["sevagotfirime"]);
        assert!(super::word_found_in_set("sevagoth", &merged),
            "merged-token prefix 'sevagoth' should still match 'sevagotfirime'");

        // A real Gara card (OCR token 'gara') must still match.
        let gara = mk(&["gara", "prime", "blueprint"]);
        assert!(super::word_found_in_set("gara", &gara),
            "'gara' should match an actual Gara card");

        // End-to-end: on an Akjagara card, Akjagara must outscore Gara.
        let s_akja = super::score_item("Akjagara Prime Barrel Blueprint", &akja);
        let s_gara = super::score_item("Gara Prime Blueprint", &akja);
        assert!(s_akja > s_gara,
            "Akjagara card: expected Akjagara ({s_akja}) > Gara ({s_gara})");
    }

    /// Regression for the dropped-2nd-card bug on a DARK frame (Dual Zoren Prime
    /// Blueprint + Yareli Prime Chassis Blueprint). The dim tileset left the 2nd
    /// card's name faint, OCR garbled "Yareli"→"Vorelo", and the spurious bar
    /// segments were discarded → only 1 card emitted. Dark-frame contrast stretch +
    /// bar-segment salvage must recover both cards.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_dualzoren_yareli_two_cards() {
        use image::GenericImageView;
        let img = image::open("/workspace/Warframe/rewards_dualzoren_yarelichassis.png").unwrap();
        let (w, h) = img.dimensions();
        let cap_y = (h as f32 * 0.30) as u32;
        let cap_h = (h as f32 * 0.25) as u32;
        let mut band = Vec::with_capacity((w * cap_h * 4) as usize);
        for y in cap_y..(cap_y + cap_h) {
            for x in 0..w {
                let px = img.get_pixel(x, y);
                band.push(px[2]); band.push(px[1]); band.push(px[0]); band.push(px[3]);
            }
        }
        let catalog: Vec<(String, String)> = vec![
            ("/dz".into(),  "Dual Zoren Prime Blueprint".into()),
            ("/dzb".into(), "Dual Zoren Prime Blade".into()),
            ("/dzh".into(), "Dual Zoren Prime Handle".into()),
            ("/yc".into(),  "Yareli Prime Chassis Blueprint".into()),
            ("/ys".into(),  "Yareli Prime Systems Blueprint".into()),
            ("/yn".into(),  "Yareli Prime Neuroptics Blueprint".into()),
            ("/yb".into(),  "Yareli Prime Blueprint".into()),
            ("/gara".into(),"Gara Prime Blueprint".into()),
            ("/mag".into(), "Mag Prime Blueprint".into()),
            ("/burst".into(),"Burston Prime Barrel".into()),
        ];
        let (complete, _r, items, positions, dbg) = super::extract_reward_items_twophase(
            &band, w, cap_h, h, &catalog, "test", None, 1.0, None,
        );
        println!("{}", dbg);
        println!("complete={} items={:?} positions={:?}", complete, items, positions);
        let display = |u: &String| catalog.iter().find(|(k, _)| k == u)
            .map(|(_, n)| n.to_lowercase()).unwrap_or_default();
        let names: Vec<String> = items.iter().map(display).collect();
        assert!(names.iter().any(|n| n.contains("dual zoren")),
            "Dual Zoren card missing: {:?}", names);
        assert!(names.iter().any(|n| n.contains("yareli")),
            "Yareli card missing (the dropped 2nd card): {:?}", names);
    }
}
