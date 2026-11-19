use std::mem::size_of;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Storage::Streams::DataWriter;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, MONITORINFO, MONITOR_DEFAULTTONEAREST, SRCCOPY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

#[derive(Debug, Clone)]
pub struct CapturedFrame {
    /// Monitor left coordinate in virtual desktop space.
    pub monitor_x: i32,
    /// Monitor top coordinate in virtual desktop space.
    pub monitor_y: i32,
    /// Width of the captured monitor in physical pixels.
    pub width: u32,
    /// Height of the captured monitor in physical pixels.
    pub height: u32,
    /// Monitor DPI scale factor (`1.0` at 100% / 96 DPI, `1.25` at 125%, `1.5` at 150%, etc.).
    pub dpi_scale: f32,
    /// Click X coordinate relative to this monitor (`0..width`).
    pub local_click_x: f32,
    /// Click Y coordinate relative to this monitor (`0..height`).
    pub local_click_y: f32,
    /// Raw top-down BGRA8 pixel buffer (`width * height * 4` bytes).
    pub pixels_bgra: Vec<u8>,
}

impl CapturedFrame {
    /// Converts this BGRA8 frame into a WinRT `SoftwareBitmap` ready for `Windows.Media.Ocr`.
    #[allow(dead_code)]
    pub fn to_software_bitmap(&self) -> Result<SoftwareBitmap, String> {
        let writer = DataWriter::new().map_err(|e| format!("Gagal membuat DataWriter: {e}"))?;
        writer
            .WriteBytes(&self.pixels_bgra)
            .map_err(|e| format!("Gagal menulis byte bitmap ke stream buffer: {e}"))?;
        let buffer = writer
            .DetachBuffer()
            .map_err(|e| format!("Gagal mengambil IBuffer dari DataWriter: {e}"))?;

        SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            self.width as i32,
            self.height as i32,
            BitmapAlphaMode::Ignore,
        )
        .map_err(|e| format!("Gagal membuat SoftwareBitmap dari buffer: {e}"))
    }
}

/// Captures the monitor that contains the given screen coordinate `(screen_x, screen_y)`.
pub fn capture_monitor_at_point(screen_x: i32, screen_y: i32) -> Result<CapturedFrame, String> {
    unsafe {
        let pt = POINT {
            x: screen_x,
            y: screen_y,
        };
        let hmonitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);

        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            rcMonitor: RECT::default(),
            rcWork: RECT::default(),
            dwFlags: 0,
        };

        if !GetMonitorInfoW(hmonitor, &mut mi).as_bool() {
            return Err("Gagal membaca informasi monitor aktif.".to_string());
        }

        let mon_x = mi.rcMonitor.left;
        let mon_y = mi.rcMonitor.top;
        let width = (mi.rcMonitor.right - mi.rcMonitor.left).max(1) as u32;
        let height = (mi.rcMonitor.bottom - mi.rcMonitor.top).max(1) as u32;

        let mut dpi_x: u32 = 96;
        let mut dpi_y: u32 = 96;
        let _ = GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let dpi_scale = (dpi_x as f32 / 96.0).clamp(1.0, 3.0);

        let hdc_screen = GetDC(HWND::default());
        if hdc_screen.is_invalid() {
            return Err("Gagal mendapatkan DC layar utama.".to_string());
        }

        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem.is_invalid() {
            let _ = ReleaseDC(HWND::default(), hdc_screen);
            return Err("Gagal membuat memory DC.".to_string());
        }

        let hbm = CreateCompatibleBitmap(hdc_screen, width as i32, height as i32);
        if hbm.is_invalid() {
            let _ = DeleteDC(hdc_mem);
            let _ = ReleaseDC(HWND::default(), hdc_screen);
            return Err("Gagal membuat compatible bitmap untuk tangkapan layar.".to_string());
        }

        let old_obj = SelectObject(hdc_mem, hbm);

        // Try with CAPTUREBLT first (captures layered windows), and fallback to plain SRCCOPY
        let blt_res = BitBlt(
            hdc_mem,
            0,
            0,
            width as i32,
            height as i32,
            hdc_screen,
            mon_x,
            mon_y,
            SRCCOPY | CAPTUREBLT,
        )
        .or_else(|_| {
            BitBlt(
                hdc_mem,
                0,
                0,
                width as i32,
                height as i32,
                hdc_screen,
                mon_x,
                mon_y,
                SRCCOPY,
            )
        });

        if let Err(e) = blt_res {
            let _ = SelectObject(hdc_mem, old_obj);
            let _ = DeleteObject(hbm);
            let _ = DeleteDC(hdc_mem);
            let _ = ReleaseDC(HWND::default(), hdc_screen);
            return Err(format!(
                "Gagal melakukan BitBlt tangkapan layar ({}x{} at {},{}): {e}",
                width, height, mon_x, mon_y
            ));
        }

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                // Negative height forces a top-down DIB (origin at top-left)
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: width * height * 4,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: Default::default(),
        };

        let mut pixels_bgra = vec![0u8; (width * height * 4) as usize];
        let lines = GetDIBits(
            hdc_mem,
            hbm,
            0,
            height,
            Some(pixels_bgra.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        let _ = SelectObject(hdc_mem, old_obj);
        let _ = DeleteObject(hbm);
        let _ = DeleteDC(hdc_mem);
        let _ = ReleaseDC(HWND::default(), hdc_screen);

        if lines == 0 {
            return Err("GetDIBits gagal mengekstrak piksel layar.".to_string());
        }

        // Ensure all alpha channels are 255 (opaque) for Direct2D & SoftwareBitmap compatibility
        for chunk in pixels_bgra.chunks_exact_mut(4) {
            chunk[3] = 255;
        }

        let local_click_x = ((screen_x - mon_x) as f32).clamp(0.0, width as f32);
        let local_click_y = ((screen_y - mon_y) as f32).clamp(0.0, height as f32);

        Ok(CapturedFrame {
            monitor_x: mon_x,
            monitor_y: mon_y,
            width,
            height,
            dpi_scale,
            local_click_x,
            local_click_y,
            pixels_bgra,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_synthetic_frame_to_software_bitmap() {
        let frame = CapturedFrame {
            monitor_x: 0,
            monitor_y: 0,
            width: 64,
            height: 32,
            dpi_scale: 1.0,
            local_click_x: 10.0,
            local_click_y: 10.0,
            pixels_bgra: vec![255u8; 64 * 32 * 4],
        };
        let sb = frame
            .to_software_bitmap()
            .expect("SoftwareBitmap creation should succeed");
        assert_eq!(sb.PixelWidth().unwrap(), 64);
        assert_eq!(sb.PixelHeight().unwrap(), 32);
    }
}
