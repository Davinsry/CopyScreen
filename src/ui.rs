use crate::hook::{get_active_shortcut, save_shortcut, ShortcutConfig, TriggerInput};
use crate::selection::{RectF, SelectionMode};
use crate::tray::{
    create_copyscreen_logo_icon, ensure_default_autostart, is_autostart_enabled,
    set_autostart_enabled,
};
use std::ffi::c_void;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_POINT_2F, D2D_RECT_F,
    D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_BOLD,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, MonitorFromPoint, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU,
    VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, GetClientRect,
    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, IsWindow, LoadCursorW, RegisterClassW,
    SendMessageW, SetCursor, SetForegroundWindow, SetWindowLongPtrW, ShowWindow, TranslateMessage,
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HICON, ICON_BIG, ICON_SMALL, IDC_ARROW,
    IDC_HAND, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT,
    WM_RBUTTONDOWN, WM_SETCURSOR, WM_SETICON, WM_SIZE, WM_SYSKEYDOWN, WM_XBUTTONDOWN, WNDCLASSW,
    WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_VISIBLE,
};

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> isize;
    fn SetProcessWorkingSetSize(hprocess: isize, min: usize, max: usize) -> i32;
}

/// Immediately releases unused physical RAM pages back to the Windows kernel so idle RAM stays ~1.5-3 MB.
pub fn trim_process_working_set() {
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardAction {
    MinimizeToTray(SelectionMode),
    TriggerScanNow(SelectionMode),
    ExitApplication,
}

const WIN_WIDTH: u32 = 520;
const WIN_HEIGHT: u32 = 560;
const TOTAL_FOCUS_ITEMS: usize = 12;

struct DashboardState {
    hwnd: HWND,
    d2d_factory: ID2D1Factory,
    _dwrite_factory: IDWriteFactory,
    fmt_title: IDWriteTextFormat,
    fmt_section: IDWriteTextFormat,
    fmt_body_left: IDWriteTextFormat,
    fmt_body_center: IDWriteTextFormat,
    fmt_small_left: IDWriteTextFormat,
    fmt_small_right: IDWriteTextFormat,
    fmt_small_center: IDWriteTextFormat,
    rt: Option<ID2D1HwndRenderTarget>,

    active_shortcut: ShortcutConfig,
    recording_custom: bool,
    selection_mode: SelectionMode,
    autostart: bool,
    status_note: String,

    hover: (f32, f32),
    dpi_scale: f32,
    focused_index: Option<usize>,
    final_action: DashboardAction,
}

impl DashboardState {
    // Control 0: Primary Scan Now button
    fn btn_scan_now_rect(&self) -> RectF {
        RectF::new(348.0, 22.0, 148.0, 34.0)
    }

    // Control 1: Custom Shortcut Recorder button
    fn record_custom_btn_rect(&self) -> RectF {
        RectF::new(364.0, 108.0, 132.0, 34.0)
    }

    // Controls 2..=7: 6 Preset rows (2 columns x 3 rows)
    fn preset_card_rect(&self, index: usize) -> RectF {
        let col = index % 2;
        let row = index / 2;
        let x = if col == 0 { 24.0 } else { 264.0 };
        let y = 152.0 + (row as f32) * 42.0;
        RectF::new(x, y, 232.0, 34.0)
    }

    // Control 8: Linear Mode Segment
    fn mode_linear_rect(&self) -> RectF {
        RectF::new(24.0, 326.0, 232.0, 56.0)
    }

    // Control 9: Box Mode Segment
    fn mode_box_rect(&self) -> RectF {
        RectF::new(264.0, 326.0, 232.0, 56.0)
    }

    // Control 10: Auto-Start Toggle Row
    fn autostart_row_rect(&self) -> RectF {
        RectF::new(24.0, 410.0, 472.0, 52.0)
    }

    // Control 11: Hide to Tray Button
    fn btn_hide_tray_rect(&self) -> RectF {
        RectF::new(24.0, 492.0, 286.0, 38.0)
    }

    // Control 12 (index 11): Exit App Button
    fn btn_exit_app_rect(&self) -> RectF {
        RectF::new(320.0, 492.0, 176.0, 38.0)
    }

    fn control_rect_by_index(&self, idx: usize) -> RectF {
        match idx {
            0 => self.btn_scan_now_rect(),
            1 => self.record_custom_btn_rect(),
            2..=7 => self.preset_card_rect(idx - 2),
            8 => self.mode_linear_rect(),
            9 => self.mode_box_rect(),
            10 => self.autostart_row_rect(),
            11 => self.btn_hide_tray_rect(),
            _ => self.btn_exit_app_rect(),
        }
    }

    fn activate_control(&mut self, idx: usize) {
        unsafe {
            match idx {
                0 => {
                    self.final_action = DashboardAction::TriggerScanNow(self.selection_mode);
                    let _ = DestroyWindow(self.hwnd);
                }
                1 => {
                    self.recording_custom = !self.recording_custom;
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
                2..=7 => {
                    let presets = ShortcutConfig::presets();
                    let chosen = presets[idx - 2];
                    let _ = save_shortcut(chosen);
                    self.active_shortcut = chosen;
                    self.recording_custom = false;
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
                8 => {
                    self.selection_mode = SelectionMode::Linear;
                    self.final_action = DashboardAction::MinimizeToTray(self.selection_mode);
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
                9 => {
                    self.selection_mode = SelectionMode::Box;
                    self.final_action = DashboardAction::MinimizeToTray(self.selection_mode);
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
                10 => {
                    let next = !self.autostart;
                    if set_autostart_enabled(next).is_ok() {
                        self.autostart = next;
                        self.status_note = if next {
                            "Aktif di Windows Registry (berjalan otomatis saat PC menyala)"
                                .to_string()
                        } else {
                            "Nonaktif (jalankan manual saat dibutuhkan)".to_string()
                        };
                    }
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
                11 => {
                    self.final_action = DashboardAction::MinimizeToTray(self.selection_mode);
                    let _ = DestroyWindow(self.hwnd);
                }
                _ => {
                    self.final_action = DashboardAction::ExitApplication;
                    let _ = DestroyWindow(self.hwnd);
                }
            }
        }
    }

    fn is_interactive_at(&self, x: f32, y: f32) -> bool {
        if self.btn_scan_now_rect().contains(x, y)
            || self.record_custom_btn_rect().contains(x, y)
            || self.mode_linear_rect().contains(x, y)
            || self.mode_box_rect().contains(x, y)
            || self.autostart_row_rect().contains(x, y)
            || self.btn_hide_tray_rect().contains(x, y)
            || self.btn_exit_app_rect().contains(x, y)
        {
            return true;
        }
        for i in 0..6 {
            if self.preset_card_rect(i).contains(x, y) {
                return true;
            }
        }
        false
    }

    unsafe fn ensure_rt(&mut self) {
        if self.rt.is_some() {
            return;
        }
        let mut rc = windows::Win32::Foundation::RECT::default();
        let _ = GetClientRect(self.hwnd, &mut rc);
        let w = (rc.right - rc.left).max(WIN_WIDTH as i32) as u32;
        let h = (rc.bottom - rc.top).max(WIN_HEIGHT as i32) as u32;

        let dpi = GetDpiForWindow(self.hwnd).max(96) as f32;
        self.dpi_scale = (dpi / 96.0).clamp(1.0, 3.0);

        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi,
            dpiY: dpi,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: Default::default(),
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: self.hwnd,
            pixelSize: D2D_SIZE_U {
                width: w,
                height: h,
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        if let Ok(rt) = self.d2d_factory.CreateHwndRenderTarget(&props, &hwnd_props) {
            self.rt = Some(rt);
        }
    }

    unsafe fn render(&mut self) {
        self.ensure_rt();
        let Some(rt) = self.rt.clone() else { return };

        let (hx, hy) = self.hover;

        rt.BeginDraw();
        // Base canvas: exact #111111 (r: 0.067, g: 0.067, b: 0.067) matte neutral carbon
        rt.Clear(Some(&D2D1_COLOR_F {
            r: 0.067,
            g: 0.067,
            b: 0.067,
            a: 1.0,
        }));

        // Restrained 3-tier neutral dark palette + 1 functional green switch accent (R-29)
        let surface_bg = brush(&rt, 0.106, 0.106, 0.106, 1.0); // #1B1B1B
        let surface_hover = brush(&rt, 0.145, 0.145, 0.145, 1.0); // #252525
        let surface_selected = brush(&rt, 0.137, 0.137, 0.137, 1.0); // #232323
        let border_subtle = brush(&rt, 0.176, 0.176, 0.176, 1.0); // #2D2D2D
        let divider_line = brush(&rt, 0.149, 0.149, 0.149, 1.0); // #262626
        let border_active = brush(&rt, 0.929, 0.929, 0.929, 1.0); // #EDEDED
        let cta_primary_bg = brush(&rt, 0.949, 0.949, 0.949, 1.0); // #F2F2F2
        let cta_primary_hover = brush(&rt, 0.850, 0.850, 0.850, 1.0); // #D9D9D9
        let text_dark = brush(&rt, 0.067, 0.067, 0.067, 1.0); // #111111 (on #F2F2F2 button -> 16.8:1 contrast)
        let text_primary = brush(&rt, 0.949, 0.949, 0.949, 1.0); // #F2F2F2 (16.8:1 on #111111)
        let text_secondary = brush(&rt, 0.635, 0.635, 0.635, 1.0); // #A2A2A2 (7.1:1 on #111111)
        let switch_on_green = brush(&rt, 0.133, 0.773, 0.369, 1.0); // #22C55E
        let danger_surface = brush(&rt, 0.160, 0.090, 0.090, 1.0);
        let danger_border = brush(&rt, 0.520, 0.200, 0.200, 1.0);

        // ─────────────────────────────────────────────────────────────
        // 1. ASYMMETRIC TOP HEADER (With Option 2 Overlapping Copy Sheets Logo)
        // ─────────────────────────────────────────────────────────────
        draw_option2_header_logo(&rt, RectF::new(24.0, 15.0, 44.0, 44.0));

        if let Some(tp) = &text_primary {
            draw_text(
                &rt,
                &self.fmt_title,
                "CopyScreen",
                RectF::new(78.0, 16.0, 220.0, 24.0),
                tp,
            );
        }
        if let Some(ts) = &text_secondary {
            draw_text(
                &rt,
                &self.fmt_small_left,
                "v0.1.0 (MIT)  ·  Aktif di System Tray",
                RectF::new(78.0, 42.0, 260.0, 18.0),
                ts,
            );
        }

        // Primary CTA: Solid high-contrast #F2F2F2 button (zero gradient, zero decorative emoji)
        let scan_r = self.btn_scan_now_rect();
        if scan_r.contains(hx, hy) {
            fill_rrect(&rt, scan_r, 6.0, &cta_primary_hover);
        } else {
            fill_rrect(&rt, scan_r, 6.0, &cta_primary_bg);
        }
        if let Some(td) = &text_dark {
            draw_text(&rt, &self.fmt_body_center, "Scan Layar Sekarang", scan_r, td);
        }

        draw_hline(&rt, 24.0, 496.0, 74.0, &divider_line);

        // ─────────────────────────────────────────────────────────────
        // 2. PRIMARY FOCAL POINT: SHORTCUT PEMICU
        // ─────────────────────────────────────────────────────────────
        if let Some(ts) = &text_secondary {
            draw_text(
                &rt,
                &self.fmt_section,
                "Shortcut Pemicu",
                RectF::new(24.0, 84.0, 260.0, 18.0),
                ts,
            );
        }

        // Keycap Readout Box
        let active_bar = RectF::new(24.0, 108.0, 330.0, 34.0);
        fill_rrect(&rt, active_bar, 6.0, &surface_bg);
        if self.recording_custom {
            stroke_rrect(&rt, active_bar, 6.0, 1.5, &border_active);
            if let Some(tp) = &text_primary {
                draw_text(
                    &rt,
                    &self.fmt_body_left,
                    "Tekan kombinasi tombol atau klik di kotak ini...",
                    RectF::new(active_bar.x + 12.0, active_bar.y, active_bar.width - 20.0, active_bar.height),
                    tp,
                );
            }
        } else {
            stroke_rrect(&rt, active_bar, 6.0, 1.0, &border_subtle);
            if let Some(ts) = &text_secondary {
                draw_text(
                    &rt,
                    &self.fmt_small_left,
                    "Aktif",
                    RectF::new(active_bar.x + 12.0, active_bar.y, 44.0, active_bar.height),
                    ts,
                );
            }
            if let Some(tp) = &text_primary {
                draw_text(
                    &rt,
                    &self.fmt_body_left,
                    &self.active_shortcut.display_name(),
                    RectF::new(active_bar.x + 56.0, active_bar.y, active_bar.width - 66.0, active_bar.height),
                    tp,
                );
            }
        }

        // Custom Shortcut Button
        let rec_r = self.record_custom_btn_rect();
        if self.recording_custom {
            fill_rrect(&rt, rec_r, 6.0, &surface_selected);
            stroke_rrect(&rt, rec_r, 6.0, 1.5, &border_active);
        } else if rec_r.contains(hx, hy) {
            fill_rrect(&rt, rec_r, 6.0, &surface_hover);
            stroke_rrect(&rt, rec_r, 6.0, 1.0, &border_subtle);
        } else {
            fill_rrect(&rt, rec_r, 6.0, &surface_bg);
            stroke_rrect(&rt, rec_r, 6.0, 1.0, &border_subtle);
        }
        if let Some(tp) = &text_primary {
            let rec_txt = if self.recording_custom {
                "Batal Rekam"
            } else {
                "Ubah Kustom"
            };
            draw_text(&rt, &self.fmt_small_center, rec_txt, rec_r, tp);
        }

        // 6 Preset Options
        let presets = ShortcutConfig::presets();
        let preset_labels = [
            "Ctrl + Win + Klik Kiri (Default)",
            "Ctrl + Shift + Klik Kiri",
            "Alt + Win + Klik Kiri",
            "Ctrl + Alt + Klik Kiri",
            "Ctrl + Win + Klik Tengah",
            "Tombol Samping Mouse (Stealth)",
        ];

        for i in 0..6 {
            let rect = self.preset_card_rect(i);
            let is_selected = self.active_shortcut == presets[i] && !self.recording_custom;
            if is_selected {
                fill_rrect(&rt, rect, 6.0, &surface_selected);
                stroke_rrect(&rt, rect, 6.0, 1.5, &border_active);
            } else if rect.contains(hx, hy) {
                fill_rrect(&rt, rect, 6.0, &surface_hover);
                stroke_rrect(&rt, rect, 6.0, 1.0, &border_subtle);
            } else {
                fill_rrect(&rt, rect, 6.0, &surface_bg);
                stroke_rrect(&rt, rect, 6.0, 1.0, &border_subtle);
            }

            let label_color = if is_selected {
                &text_primary
            } else {
                &text_secondary
            };
            if let Some(b) = label_color {
                draw_text(
                    &rt,
                    &self.fmt_small_left,
                    preset_labels[i],
                    RectF::new(rect.x + 12.0, rect.y, rect.width - 24.0, rect.height),
                    b,
                );
            }
        }

        draw_hline(&rt, 24.0, 496.0, 290.0, &divider_line);

        // ─────────────────────────────────────────────────────────────
        // 3. SEGMENTED MODE SELECTOR
        // ─────────────────────────────────────────────────────────────
        if let Some(ts) = &text_secondary {
            draw_text(
                &rt,
                &self.fmt_section,
                "Mode Seleksi Default",
                RectF::new(24.0, 300.0, 260.0, 18.0),
                ts,
            );
        }

        let m_lin = self.mode_linear_rect();
        let lin_active = self.selection_mode == SelectionMode::Linear;
        if lin_active {
            fill_rrect(&rt, m_lin, 6.0, &surface_selected);
            stroke_rrect(&rt, m_lin, 6.0, 1.5, &border_active);
        } else if m_lin.contains(hx, hy) {
            fill_rrect(&rt, m_lin, 6.0, &surface_hover);
            stroke_rrect(&rt, m_lin, 6.0, 1.0, &border_subtle);
        } else {
            fill_rrect(&rt, m_lin, 6.0, &surface_bg);
            stroke_rrect(&rt, m_lin, 6.0, 1.0, &border_subtle);
        }
        if let Some(tp) = &text_primary {
            draw_text(
                &rt,
                &self.fmt_body_left,
                "Teks Linear (I-Beam)",
                RectF::new(m_lin.x + 12.0, m_lin.y + 7.0, m_lin.width - 24.0, 20.0),
                tp,
            );
        }
        if let Some(ts) = &text_secondary {
            draw_text(
                &rt,
                &self.fmt_small_left,
                "Seret kata per kata sesuai baris",
                RectF::new(m_lin.x + 12.0, m_lin.y + 29.0, m_lin.width - 24.0, 20.0),
                ts,
            );
        }

        let m_box = self.mode_box_rect();
        let box_active = self.selection_mode == SelectionMode::Box;
        if box_active {
            fill_rrect(&rt, m_box, 6.0, &surface_selected);
            stroke_rrect(&rt, m_box, 6.0, 1.5, &border_active);
        } else if m_box.contains(hx, hy) {
            fill_rrect(&rt, m_box, 6.0, &surface_hover);
            stroke_rrect(&rt, m_box, 6.0, 1.0, &border_subtle);
        } else {
            fill_rrect(&rt, m_box, 6.0, &surface_bg);
            stroke_rrect(&rt, m_box, 6.0, 1.0, &border_subtle);
        }
        if let Some(tp) = &text_primary {
            draw_text(
                &rt,
                &self.fmt_body_left,
                "Kotak Area (Box)",
                RectF::new(m_box.x + 12.0, m_box.y + 7.0, m_box.width - 24.0, 20.0),
                tp,
            );
        }
        if let Some(ts) = &text_secondary {
            draw_text(
                &rt,
                &self.fmt_small_left,
                "Seret kotak persegi untuk tabel/kolom",
                RectF::new(m_box.x + 12.0, m_box.y + 29.0, m_box.width - 24.0, 20.0),
                ts,
            );
        }

        draw_hline(&rt, 24.0, 496.0, 396.0, &divider_line);

        // ─────────────────────────────────────────────────────────────
        // 4. INLINE SYSTEM ROW: AUTO-START WINDOWS
        // ─────────────────────────────────────────────────────────────
        let auto_r = self.autostart_row_rect();
        if auto_r.contains(hx, hy) {
            fill_rrect(&rt, auto_r, 6.0, &surface_hover);
        } else {
            fill_rrect(&rt, auto_r, 6.0, &surface_bg);
        }
        stroke_rrect(&rt, auto_r, 6.0, 1.0, &border_subtle);

        if let Some(tp) = &text_primary {
            draw_text(
                &rt,
                &self.fmt_body_left,
                "Jalankan Otomatis Saat Windows Menyala (Auto-Start)",
                RectF::new(auto_r.x + 14.0, auto_r.y + 6.0, 380.0, 20.0),
                tp,
            );
        }
        if let Some(ts) = &text_secondary {
            draw_text(
                &rt,
                &self.fmt_small_left,
                &self.status_note,
                RectF::new(auto_r.x + 14.0, auto_r.y + 26.0, 380.0, 20.0),
                ts,
            );
        }

        // Boolean Switch Track (pill radius 12px reserved strictly for toggle switch)
        let sw_rect = RectF::new(auto_r.right() - 58.0, auto_r.y + 14.0, 44.0, 24.0);
        if self.autostart {
            fill_rrect(&rt, sw_rect, 12.0, &switch_on_green);
            let knob = RectF::new(sw_rect.right() - 21.0, sw_rect.y + 3.0, 18.0, 18.0);
            fill_rrect(&rt, knob, 9.0, &text_primary);
        } else {
            fill_rrect(&rt, sw_rect, 12.0, &surface_bg);
            stroke_rrect(&rt, sw_rect, 12.0, 1.0, &border_subtle);
            let knob = RectF::new(sw_rect.x + 3.0, sw_rect.y + 3.0, 18.0, 18.0);
            fill_rrect(&rt, knob, 9.0, &text_secondary);
        }

        draw_hline(&rt, 24.0, 496.0, 476.0, &divider_line);

        // ─────────────────────────────────────────────────────────────
        // 5. FOOTER ACTIONS
        // ─────────────────────────────────────────────────────────────
        let hide_r = self.btn_hide_tray_rect();
        if hide_r.contains(hx, hy) {
            fill_rrect(&rt, hide_r, 6.0, &surface_hover);
            stroke_rrect(&rt, hide_r, 6.0, 1.2, &border_active);
        } else {
            fill_rrect(&rt, hide_r, 6.0, &surface_bg);
            stroke_rrect(&rt, hide_r, 6.0, 1.0, &border_subtle);
        }
        if let Some(tp) = &text_primary {
            draw_text(
                &rt,
                &self.fmt_body_center,
                "Sembunyikan ke System Tray",
                hide_r,
                tp,
            );
        }

        let exit_r = self.btn_exit_app_rect();
        fill_rrect(&rt, exit_r, 6.0, &danger_surface);
        if exit_r.contains(hx, hy) {
            stroke_rrect(&rt, exit_r, 6.0, 1.5, &danger_border);
        } else {
            stroke_rrect(&rt, exit_r, 6.0, 1.0, &danger_border);
        }
        if let Some(tp) = &text_primary {
            draw_text(
                &rt,
                &self.fmt_body_center,
                "Matikan Aplikasi",
                exit_r,
                tp,
            );
        }

        // Keyboard Focus Ring (R-32)
        if let Some(focus_idx) = self.focused_index {
            let f_rect = self.control_rect_by_index(focus_idx);
            let padded = RectF::new(
                f_rect.x - 2.0,
                f_rect.y - 2.0,
                f_rect.width + 4.0,
                f_rect.height + 4.0,
            );
            stroke_rrect(&rt, padded, 8.0, 1.5, &border_active);
        }

        let _ = rt.EndDraw(None, None);
    }
}

unsafe fn brush(
    rt: &ID2D1HwndRenderTarget,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) -> Option<ID2D1SolidColorBrush> {
    rt.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a }, None)
        .ok()
}

/// Renders Logo Option 2 ("Overlapping Screen Copy Sheets") cleanly in Direct2D for the UI header.
unsafe fn draw_option2_header_logo(rt: &ID2D1HwndRenderTarget, bounds: RectF) {
    let x = bounds.x;
    let y = bounds.y;
    let s = bounds.width / 32.0;

    let bg = brush(rt, 0.067, 0.067, 0.067, 1.0);
    let border = brush(rt, 0.22, 0.22, 0.22, 1.0);
    let back_fill = brush(rt, 0.14, 0.14, 0.14, 1.0);
    let back_border = brush(rt, 0.34, 0.34, 0.34, 1.0);
    let front_fill = brush(rt, 0.95, 0.95, 0.95, 1.0);
    let orange = brush(rt, 0.976, 0.451, 0.086, 1.0);
    let white = brush(rt, 1.0, 1.0, 1.0, 1.0);
    let dark_bar = brush(rt, 0.09, 0.09, 0.09, 1.0);
    let muted_bar = brush(rt, 0.45, 0.45, 0.45, 1.0);

    // Outer squircle
    fill_rrect(rt, bounds, 7.5 * s, &bg);
    stroke_rrect(rt, bounds, 7.5 * s, 1.2, &border);

    // Back Screen Sheet
    let back = RectF::new(x + 5.5 * s, y + 5.2 * s, 15.2 * s, 15.2 * s);
    fill_rrect(rt, back, 2.5 * s, &back_fill);
    stroke_rrect(rt, back, 2.5 * s, 1.2, &back_border);

    // Front Copied Sheet
    let front = RectF::new(x + 10.2 * s, y + 10.0 * s, 16.0 * s, 16.0 * s);
    fill_rrect(rt, front, 2.6 * s, &front_fill);

    // Orange Selection Block + White Inner Bar
    let org = RectF::new(x + 12.2 * s, y + 12.4 * s, 12.0 * s, 4.0 * s);
    fill_rrect(rt, org, 1.0 * s, &orange);
    let wbar = RectF::new(x + 13.2 * s, y + 13.7 * s, 8.4 * s, 1.4 * s);
    fill_rrect(rt, wbar, 0.6 * s, &white);

    // Two dark text lines below
    let bar1 = RectF::new(x + 12.2 * s, y + 18.0 * s, 11.0 * s, 1.6 * s);
    fill_rrect(rt, bar1, 0.6 * s, &dark_bar);
    let bar2 = RectF::new(x + 12.2 * s, y + 20.8 * s, 7.5 * s, 1.6 * s);
    fill_rrect(rt, bar2, 0.6 * s, &muted_bar);
}

unsafe fn draw_hline(
    rt: &ID2D1HwndRenderTarget,
    x1: f32,
    x2: f32,
    y: f32,
    brush: &Option<ID2D1SolidColorBrush>,
) {
    if let Some(b) = brush {
        rt.DrawLine(
            D2D_POINT_2F { x: x1, y },
            D2D_POINT_2F { x: x2, y },
            b,
            1.0,
            None,
        );
    }
}

unsafe fn fill_rrect(
    rt: &ID2D1HwndRenderTarget,
    rect: RectF,
    radius: f32,
    brush: &Option<ID2D1SolidColorBrush>,
) {
    if let Some(b) = brush {
        rt.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: rect.x,
                    top: rect.y,
                    right: rect.right(),
                    bottom: rect.bottom(),
                },
                radiusX: radius,
                radiusY: radius,
            },
            b,
        );
    }
}

unsafe fn stroke_rrect(
    rt: &ID2D1HwndRenderTarget,
    rect: RectF,
    radius: f32,
    width: f32,
    brush: &Option<ID2D1SolidColorBrush>,
) {
    if let Some(b) = brush {
        rt.DrawRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: rect.x,
                    top: rect.y,
                    right: rect.right(),
                    bottom: rect.bottom(),
                },
                radiusX: radius,
                radiusY: radius,
            },
            b,
            width,
            None,
        );
    }
}

unsafe fn draw_text(
    rt: &ID2D1HwndRenderTarget,
    fmt: &IDWriteTextFormat,
    text: &str,
    rect: RectF,
    brush: &ID2D1SolidColorBrush,
) {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let d2d_rect = D2D_RECT_F {
        left: rect.x,
        top: rect.y,
        right: rect.right(),
        bottom: rect.bottom(),
    };
    rt.DrawText(
        &utf16,
        fmt,
        &d2d_rect,
        brush,
        windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
    );
}

unsafe fn read_modifiers() -> (bool, bool, bool, bool) {
    let ctrl = (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_LCONTROL.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RCONTROL.0 as i32) as u16 & 0x8000) != 0;
    let win = (GetAsyncKeyState(VK_LWIN.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RWIN.0 as i32) as u16 & 0x8000) != 0;
    let alt = (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_LMENU.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RMENU.0 as i32) as u16 & 0x8000) != 0;
    let shift = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_LSHIFT.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RSHIFT.0 as i32) as u16 & 0x8000) != 0;
    (ctrl, win, alt, shift)
}

/// Opens the CopyScreen Settings UI Window on a matte `#111111` surface.
pub fn run_dashboard_ui(initial_mode: SelectionMode) -> DashboardAction {
    ensure_default_autostart();

    let result = unsafe {
        (|| -> Option<DashboardAction> {
            let d2d_factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
            let dwrite_factory: IDWriteFactory =
                DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;

            let fmt_title = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    18.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_title.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let fmt_section = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    12.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_section.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let fmt_body_left = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    13.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_body_left.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
            let _ = fmt_body_left.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let fmt_body_center = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    13.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_body_center.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = fmt_body_center.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let fmt_small_left = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    12.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_small_left.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
            let _ = fmt_small_left.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let fmt_small_right = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    12.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_small_right.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
            let _ = fmt_small_right.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let fmt_small_center = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    12.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_small_center.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = fmt_small_center.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let hinstance = GetModuleHandleW(None).ok()?;
            let class_name = w!("CopyScreenModernDashboardUIClass");
            let hicon: HICON = create_copyscreen_logo_icon().unwrap_or_default();

            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(dashboard_wnd_proc),
                hInstance: hinstance.into(),
                lpszClassName: class_name,
                hIcon: hicon,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            let autostart_now = is_autostart_enabled();
            let note = if autostart_now {
                "Aktif di Windows Registry (berjalan otomatis saat PC menyala)".to_string()
            } else {
                "Nonaktif (jalankan manual saat dibutuhkan)".to_string()
            };

            let mut cursor_pt = windows::Win32::Foundation::POINT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut cursor_pt);
            let hmon = MonitorFromPoint(cursor_pt, MONITOR_DEFAULTTONEAREST);
            let mut mon_dpi_x: u32 = 96;
            let mut mon_dpi_y: u32 = 96;
            let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut mon_dpi_x, &mut mon_dpi_y);
            let dpi_scale = (mon_dpi_x as f32 / 96.0).clamp(1.0, 3.0);

            let state = Box::new(DashboardState {
                hwnd: HWND::default(),
                d2d_factory,
                _dwrite_factory: dwrite_factory,
                fmt_title,
                fmt_section,
                fmt_body_left,
                fmt_body_center,
                fmt_small_left,
                fmt_small_right,
                fmt_small_center,
                rt: None,
                active_shortcut: get_active_shortcut(),
                recording_custom: false,
                selection_mode: initial_mode,
                autostart: autostart_now,
                status_note: note,
                hover: (0.0, 0.0),
                dpi_scale,
                focused_index: None,
                final_action: DashboardAction::MinimizeToTray(initial_mode),
            });
            let state_ptr = Box::into_raw(state);

            let screen_w = GetSystemMetrics(SM_CXSCREEN);
            let screen_h = GetSystemMetrics(SM_CYSCREEN);
            let win_w = ((WIN_WIDTH as f32 + 16.0) * dpi_scale).round() as i32;
            let win_h = ((WIN_HEIGHT as f32 + 39.0) * dpi_scale).round() as i32;
            let pos_x = ((screen_w - win_w) / 2).max(20);
            let pos_y = ((screen_h - win_h) / 2).max(20);

            let hwnd = CreateWindowExW(
                Default::default(),
                class_name,
                w!("CopyScreen: Pengaturan Shortcut"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE,
                pos_x,
                pos_y,
                win_w,
                win_h,
                HWND::default(),
                None,
                hinstance,
                Some(state_ptr as *const _),
            )
            .ok()?;

            (*state_ptr).hwnd = hwnd;

            let dark_mode: i32 = 1;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark_mode as *const _ as *const c_void,
                4,
            );
            let round_pref = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &round_pref as *const _ as *const c_void,
                4,
            );

            if !hicon.is_invalid() {
                let _ = SendMessageW(
                    hwnd,
                    WM_SETICON,
                    WPARAM(ICON_SMALL as usize),
                    LPARAM(hicon.0 as isize),
                );
                let _ = SendMessageW(
                    hwnd,
                    WM_SETICON,
                    WPARAM(ICON_BIG as usize),
                    LPARAM(hicon.0 as isize),
                );
            }

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
                if !IsWindow(hwnd).as_bool() {
                    break;
                }
            }

            if !hicon.is_invalid() {
                let _ = DestroyIcon(hicon);
            }

            let final_state = Box::from_raw(state_ptr);
            let _ = &final_state.fmt_small_right;
            Some(final_state.final_action)
        })()
    };

    trim_process_working_set();
    result.unwrap_or(DashboardAction::MinimizeToTray(initial_mode))
}

unsafe extern "system" fn dashboard_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = lparam.0 as *const CREATESTRUCTW;
        if !cs.is_null() {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
        }
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DashboardState;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let state = &mut *ptr;

    match msg {
        WM_ERASEBKGND => LRESULT(1),

        WM_SIZE => {
            if let Some(rt) = &state.rt {
                let w = (lparam.0 & 0xFFFF) as u32;
                let h = ((lparam.0 >> 16) & 0xFFFF) as u32;
                if w > 0 && h > 0 {
                    let _ = rt.Resize(&D2D_SIZE_U {
                        width: w,
                        height: h,
                    });
                }
            }
            LRESULT(0)
        }

        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            state.render();
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }

        WM_MOUSEMOVE => {
            let x = ((lparam.0 & 0xFFFF) as i16 as f32) / state.dpi_scale;
            let y = (((lparam.0 >> 16) & 0xFFFF) as i16 as f32) / state.dpi_scale;
            state.hover = (x, y);
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }

        WM_SETCURSOR => {
            let (hx, hy) = state.hover;
            let cursor_id = if state.is_interactive_at(hx, hy) {
                IDC_HAND
            } else {
                IDC_ARROW
            };
            if let Ok(cur) = LoadCursorW(None, cursor_id) {
                SetCursor(cur);
            }
            LRESULT(1)
        }

        WM_LBUTTONDOWN => {
            let x = ((lparam.0 & 0xFFFF) as i16 as f32) / state.dpi_scale;
            let y = (((lparam.0 >> 16) & 0xFFFF) as i16 as f32) / state.dpi_scale;

            let active_bar = RectF::new(24.0, 108.0, 330.0, 34.0);
            if state.recording_custom && active_bar.contains(x, y) {
                let (ctrl, win, alt, shift) = read_modifiers();
                if ctrl || win || alt || shift {
                    let new_sc = ShortcutConfig {
                        ctrl,
                        win,
                        alt,
                        shift,
                        trigger: TriggerInput::MouseLeftClick,
                    };
                    let _ = save_shortcut(new_sc);
                    state.active_shortcut = new_sc;
                    state.recording_custom = false;
                    let _ = InvalidateRect(hwnd, None, false);
                    return LRESULT(0);
                }
            }

            for idx in 0..=TOTAL_FOCUS_ITEMS {
                if state.control_rect_by_index(idx).contains(x, y) {
                    state.focused_index = Some(idx);
                    state.activate_control(idx);
                    return LRESULT(0);
                }
            }

            LRESULT(0)
        }

        WM_MBUTTONDOWN => {
            if state.recording_custom {
                let (ctrl, win, alt, shift) = read_modifiers();
                let new_sc = ShortcutConfig {
                    ctrl,
                    win,
                    alt,
                    shift,
                    trigger: TriggerInput::MouseMiddleClick,
                };
                let _ = save_shortcut(new_sc);
                state.active_shortcut = new_sc;
                state.recording_custom = false;
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }

        WM_RBUTTONDOWN => {
            if state.recording_custom {
                let (ctrl, win, alt, shift) = read_modifiers();
                if ctrl || win || alt || shift {
                    let new_sc = ShortcutConfig {
                        ctrl,
                        win,
                        alt,
                        shift,
                        trigger: TriggerInput::MouseRightClick,
                    };
                    let _ = save_shortcut(new_sc);
                    state.active_shortcut = new_sc;
                    state.recording_custom = false;
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            LRESULT(0)
        }

        WM_XBUTTONDOWN => {
            if state.recording_custom {
                let (ctrl, win, alt, shift) = read_modifiers();
                let new_sc = ShortcutConfig {
                    ctrl,
                    win,
                    alt,
                    shift,
                    trigger: TriggerInput::MouseXButton1,
                };
                let _ = save_shortcut(new_sc);
                state.active_shortcut = new_sc;
                state.recording_custom = false;
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }

        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wparam.0 as u16;
            if vk == VK_ESCAPE.0 {
                if state.recording_custom {
                    state.recording_custom = false;
                    let _ = InvalidateRect(hwnd, None, false);
                } else {
                    state.final_action = DashboardAction::MinimizeToTray(state.selection_mode);
                    let _ = DestroyWindow(hwnd);
                }
                return LRESULT(0);
            }

            if state.recording_custom {
                let is_mod = matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5);
                if !is_mod {
                    let (ctrl, win, alt, shift) = read_modifiers();
                    if ctrl || win || alt || shift {
                        let new_sc = ShortcutConfig {
                            ctrl,
                            win,
                            alt,
                            shift,
                            trigger: TriggerInput::Key(vk),
                        };
                        let _ = save_shortcut(new_sc);
                        state.active_shortcut = new_sc;
                        state.recording_custom = false;
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                }
                return LRESULT(0);
            }

            // Keyboard Navigation (R-32: Tab, Shift+Tab, Enter, Space)
            if vk == 0x09 {
                // VK_TAB
                let shift_held = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
                let next_idx = match state.focused_index {
                    None => 0,
                    Some(cur) if shift_held => {
                        if cur == 0 {
                            TOTAL_FOCUS_ITEMS
                        } else {
                            cur - 1
                        }
                    }
                    Some(cur) => {
                        if cur >= TOTAL_FOCUS_ITEMS {
                            0
                        } else {
                            cur + 1
                        }
                    }
                };
                state.focused_index = Some(next_idx);
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            if vk == 0x0D || vk == 0x20 {
                // VK_RETURN or VK_SPACE
                if let Some(idx) = state.focused_index {
                    state.activate_control(idx);
                    return LRESULT(0);
                }
            }

            LRESULT(0)
        }

        WM_CLOSE => {
            state.final_action = DashboardAction::MinimizeToTray(state.selection_mode);
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }

        WM_DESTROY => {
            state.rt = None;
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
