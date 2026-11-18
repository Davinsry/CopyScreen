use crate::hook::get_active_shortcut;
use std::mem::size_of;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, HICON, ICONINFO, WM_APP,
};

pub const WM_APP_TRAY_ICON: u32 = WM_APP + 11;

const TRAY_ICON_ID: u32 = 1;
const RUN_REG_SUBKEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_REG_VALUE_NAME: PCWSTR = w!("CopyScreen");

pub struct SystemTray {
    hwnd: HWND,
    hicon: HICON,
}

impl SystemTray {
    pub fn new(hwnd: HWND) -> Result<Self, String> {
        unsafe {
            let hicon = create_copyscreen_logo_icon()?;
            let active_sc = get_active_shortcut();

            let mut nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_ICON_ID,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                uCallbackMessage: WM_APP_TRAY_ICON,
                hIcon: hicon,
                ..Default::default()
            };

            let tip = format!(
                "CopyScreen: Shortcut {} (Klik untuk buka pengaturan)",
                active_sc.display_name()
            );
            copy_wide_str(&mut nid.szTip, &tip);

            if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
                let _ = DestroyIcon(hicon);
                return Err("Gagal menambahkan ikon logo ke System Tray Windows.".to_string());
            }

            Ok(Self { hwnd, hicon })
        }
    }

    pub fn update_tooltip(&self) {
        unsafe {
            let active_sc = get_active_shortcut();
            let mut nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_ICON_ID,
                uFlags: NIF_TIP,
                ..Default::default()
            };
            let tip = format!(
                "CopyScreen: Shortcut {} (Klik untuk buka pengaturan)",
                active_sc.display_name()
            );
            copy_wide_str(&mut nid.szTip, &tip);
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    pub fn show_balloon(&self, title: &str, body: &str) {
        unsafe {
            let mut nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_ICON_ID,
                uFlags: NIF_INFO,
                dwInfoFlags: NIIF_INFO,
                ..Default::default()
            };
            copy_wide_str(&mut nid.szInfoTitle, title);
            copy_wide_str(&mut nid.szInfo, body);
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }
}

impl Drop for SystemTray {
    fn drop(&mut self) {
        unsafe {
            let nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_ICON_ID,
                ..Default::default()
            };
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            if !self.hicon.is_invalid() {
                let _ = DestroyIcon(self.hicon);
            }
        }
    }
}

/// Renders a custom 32x32 ARGB CopyScreen logo icon:
/// Rounded cyan-to-purple gradient badge with white viewfinder brackets `[ ]` and a bold `'T'` glyph inside.
pub fn create_copyscreen_logo_icon() -> Result<HICON, String> {
    let size = 32i32;
    unsafe {
        let hdc = CreateCompatibleDC(HDC::default());
        if hdc.is_invalid() {
            return Err("Gagal membuat DC untuk ikon logo.".to_string());
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (size * size * 4) as u32,
                ..Default::default()
            },
            bmiColors: Default::default(),
        };

        let mut bits_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let hbm_color = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0)
            .map_err(|e| {
                let _ = DeleteDC(hdc);
                format!("Gagal membuat DIB untuk ikon logo: {e}")
            })?;

        let pixels = std::slice::from_raw_parts_mut(bits_ptr as *mut u8, (size * size * 4) as usize);
        draw_logo_pixels_bgra(pixels, size as usize);

        let hbm_mask = CreateBitmap(size, size, 1, 1, None);
        let icon_info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: hbm_mask,
            hbmColor: hbm_color,
        };

        let hicon = CreateIconIndirect(&icon_info).map_err(|e| {
            let _ = DeleteObject(hbm_mask);
            let _ = DeleteObject(hbm_color);
            let _ = DeleteDC(hdc);
            format!("Gagal membuat HICON logo: {e}")
        })?;

        let _ = DeleteObject(hbm_mask);
        let _ = DeleteObject(hbm_color);
        let _ = DeleteDC(hdc);

        Ok(hicon)
    }
}

fn draw_logo_pixels_bgra(pixels: &mut [u8], size: usize) {
    let center = (size as f32 - 1.0) * 0.5;
    let scale = size as f32 / 32.0;
    let half_box = 13.8 * scale;
    let corner_r = 6.5 * scale;

    for y in 0..size {
        for x in 0..size {
            let fx = x as f32;
            let fy = y as f32;

            let dx = (fx - center).abs() - (half_box - corner_r);
            let dy = (fy - center).abs() - (half_box - corner_r);
            let outside_dist = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt()
                + dx.max(dy).min(0.0)
                - corner_r;

            let idx = (y * size + x) * 4;
            if outside_dist > 0.8 {
                pixels[idx] = 0;
                pixels[idx + 1] = 0;
                pixels[idx + 2] = 0;
                pixels[idx + 3] = 0;
                continue;
            }

            let alpha = (0.8 - outside_dist).clamp(0.0, 1.0);
            let nx = (x as f32 + 0.5) / scale;
            let ny = (y as f32 + 0.5) / scale;
            let (r, g, b) = sample_option2_logo_rgb(nx, ny, outside_dist / scale);

            let a_u8 = (alpha * 255.0) as u8;
            pixels[idx] = ((b as u16 * a_u8 as u16) / 255) as u8;
            pixels[idx + 1] = ((g as u16 * a_u8 as u16) / 255) as u8;
            pixels[idx + 2] = ((r as u16 * a_u8 as u16) / 255) as u8;
            pixels[idx + 3] = a_u8;
        }
    }
}

fn sdf_rrect(px: f32, py: f32, x1: f32, y1: f32, x2: f32, y2: f32, r: f32) -> f32 {
    let cx = (x1 + x2) * 0.5;
    let cy = (y1 + y2) * 0.5;
    let hx = (x2 - x1) * 0.5 - r;
    let hy = (y2 - y1) * 0.5 - r;
    let dx = (px - cx).abs() - hx;
    let dy = (py - cy).abs() - hy;
    (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - r
}

fn sample_option2_logo_rgb(nx: f32, ny: f32, outer_sd: f32) -> (u8, u8, u8) {
    let mut r = 17.0_f32;
    let mut g = 17.0_f32;
    let mut b = 17.0_f32;
    if outer_sd > -1.1 {
        r = 48.0;
        g = 48.0;
        b = 48.0;
    }

    let back_sd = sdf_rrect(nx, ny, 5.8, 5.6, 21.0, 20.8, 2.2);
    if back_sd < 0.5 {
        let cov = (0.5 - back_sd).clamp(0.0, 1.0);
        let (br, bg, bb) = if back_sd > -0.9 {
            (82.0, 82.0, 82.0)
        } else {
            (34.0, 34.0, 34.0)
        };
        r = r * (1.0 - cov) + br * cov;
        g = g * (1.0 - cov) + bg * cov;
        b = b * (1.0 - cov) + bb * cov;
    }

    let shadow_sd = sdf_rrect(nx, ny, 9.4, 9.4, 26.2, 26.2, 2.6);
    if shadow_sd < 1.2 {
        let scov = ((1.2 - shadow_sd) / 2.4).clamp(0.0, 0.65);
        r *= 1.0 - scov;
        g *= 1.0 - scov;
        b *= 1.0 - scov;
    }

    let front_sd = sdf_rrect(nx, ny, 10.2, 10.0, 26.0, 25.8, 2.3);
    if front_sd < 0.5 {
        let cov = (0.5 - front_sd).clamp(0.0, 1.0);
        let mut fr = 242.0_f32;
        let mut fg = 242.0_f32;
        let mut fb = 242.0_f32;

        let orange_sd = sdf_rrect(nx, ny, 12.2, 12.5, 24.0, 16.3, 0.85);
        if orange_sd < 0.45 {
            let ocov = (0.45 - orange_sd).clamp(0.0, 1.0);
            fr = fr * (1.0 - ocov) + 249.0 * ocov;
            fg = fg * (1.0 - ocov) + 115.0 * ocov;
            fb = fb * (1.0 - ocov) + 22.0 * ocov;

            let wbar_sd = sdf_rrect(nx, ny, 13.2, 13.8, 21.5, 15.0, 0.4);
            if wbar_sd < 0.4 {
                let wcov = (0.4 - wbar_sd).clamp(0.0, 1.0);
                fr = fr * (1.0 - wcov) + 255.0 * wcov;
                fg = fg * (1.0 - wcov) + 255.0 * wcov;
                fb = fb * (1.0 - wcov) + 255.0 * wcov;
            }
        }

        let bar1_sd = sdf_rrect(nx, ny, 12.2, 18.0, 23.0, 19.5, 0.45);
        if bar1_sd < 0.45 {
            let bcov = (0.45 - bar1_sd).clamp(0.0, 1.0);
            fr = fr * (1.0 - bcov) + 22.0 * bcov;
            fg = fg * (1.0 - bcov) + 22.0 * bcov;
            fb = fb * (1.0 - bcov) + 22.0 * bcov;
        }

        let bar2_sd = sdf_rrect(nx, ny, 12.2, 20.8, 19.4, 22.3, 0.45);
        if bar2_sd < 0.45 {
            let bcov = (0.45 - bar2_sd).clamp(0.0, 1.0);
            fr = fr * (1.0 - bcov) + 115.0 * bcov;
            fg = fg * (1.0 - bcov) + 115.0 * bcov;
            fb = fb * (1.0 - bcov) + 115.0 * bcov;
        }

        r = r * (1.0 - cov) + fr * cov;
        g = g * (1.0 - cov) + fg * cov;
        b = b * (1.0 - cov) + fb * cov;
    }

    (r as u8, g as u8, b as u8)
}

pub fn is_autostart_enabled() -> bool {
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_REG_SUBKEY, 0, KEY_READ, &mut hkey).is_err() {
            return false;
        }
        let exists = RegQueryValueExW(hkey, RUN_REG_VALUE_NAME, None, None, None, None).is_ok();
        let _ = RegCloseKey(hkey);
        exists
    }
}

/// Ensures Auto-Start is automatically enabled by default on first run
/// (while respecting the user's choice if they manually turn it off later).
pub fn ensure_default_autostart() {
    const SETTINGS_SUBKEY: PCWSTR = w!("Software\\CopyScreen");
    const INIT_VAL: PCWSTR = w!("AutostartInitialized");
    unsafe {
        let mut hkey = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            SETTINGS_SUBKEY,
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_READ | KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
        .is_ok()
        {
            let already_init =
                RegQueryValueExW(hkey, INIT_VAL, None, None, None, None).is_ok();
            if !already_init {
                let _ = set_autostart_enabled(true);
                let one: u32 = 1;
                let bytes = std::slice::from_raw_parts(&one as *const u32 as *const u8, 4);
                let _ = RegSetValueExW(hkey, INIT_VAL, 0, REG_DWORD, Some(bytes));
            }
            let _ = RegCloseKey(hkey);
        }
    }
}

pub fn set_autostart_enabled(enable: bool) -> Result<(), String> {
    unsafe {
        let mut hkey = HKEY::default();
        RegOpenKeyExW(HKEY_CURRENT_USER, RUN_REG_SUBKEY, 0, KEY_WRITE, &mut hkey)
            .ok()
            .map_err(|e| format!("Gagal membuka registry Run: {e}"))?;

        let res = if enable {
            let exe_path = std::env::current_exe()
                .map_err(|e| format!("Gagal membaca path executable: {e}"))?;
            let quoted = format!("\"{}\" --minimized", exe_path.display());
            let utf16: Vec<u16> = quoted.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = std::slice::from_raw_parts(
                utf16.as_ptr() as *const u8,
                utf16.len() * size_of::<u16>(),
            );
            RegSetValueExW(hkey, RUN_REG_VALUE_NAME, 0, REG_SZ, Some(bytes))
                .ok()
                .map_err(|e| format!("Gagal menyimpan nilai auto-start di registry: {e}"))
        } else {
            let _ = RegDeleteValueW(hkey, RUN_REG_VALUE_NAME);
            Ok(())
        };

        let _ = RegCloseKey(hkey);
        res
    }
}

fn copy_wide_str(dest: &mut [u16], text: &str) {
    dest.fill(0);
    for (i, ch) in text.encode_utf16().take(dest.len().saturating_sub(1)).enumerate() {
        dest[i] = ch;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_custom_copyscreen_logo_icon() {
        let icon = create_copyscreen_logo_icon().expect("Logo icon creation should succeed");
        assert!(!icon.is_invalid());
        unsafe {
            let _ = DestroyIcon(icon);
        }
    }
}
