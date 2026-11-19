use crate::capture::CapturedFrame;
use crate::clipboard::copy_text_to_clipboard;
use crate::hook::{set_overlay_hwnd, WM_APP_OVERLAY_COPY};
use crate::ocr::recognize_frame;
use crate::selection::{OcrLayout, RectF, SelectionMode, SelectionResult};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Instant;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP,
    D2D1_PIXEL_FORMAT, D2D_POINT_2F, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget,
    ID2D1LinearGradientBrush, ID2D1RadialGradientBrush, ID2D1SolidColorBrush,
    D2D1_BITMAP_PROPERTIES, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE,
    D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer,
    LoadCursorW, RegisterClassW, SetCursor, SetTimer, ShowWindow, TranslateMessage, CS_HREDRAW,
    CS_VREDRAW, IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_IBEAM, MA_NOACTIVATE, MSG, SW_SHOWNOACTIVATE,
    WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const ANIM_TIMER_ID: usize = 101;
const CLOSE_DELAY_TIMER_ID: usize = 102;
const ANIM_INTERVAL_MS: u32 = 16; // ~60 FPS

/// Pre-allocated GPU brushes so `render()` performs ZERO Direct2D resource allocations per frame.
struct CachedBrushes {
    solid: ID2D1SolidColorBrush,
    orb_purple: ID2D1RadialGradientBrush,
    orb_pink: ID2D1RadialGradientBrush,
    orb_blue: ID2D1RadialGradientBrush,
    edge_grad: ID2D1LinearGradientBrush,
}

struct OverlayState {
    hwnd: HWND,
    frame: CapturedFrame,
    d2d_factory: ID2D1Factory,
    _dwrite_factory: IDWriteFactory,
    text_format_ui: IDWriteTextFormat,
    text_format_small: IDWriteTextFormat,
    render_target: Option<ID2D1HwndRenderTarget>,
    screen_bitmap: Option<ID2D1Bitmap>,
    brushes: Option<CachedBrushes>,

    start_instant: Instant,
    ocr_rx: Option<Receiver<Result<OcrLayout, String>>>,
    ocr_layout: OcrLayout,
    line_capsules: Vec<RectF>,
    ocr_ready_instant: Option<Instant>,
    ocr_loading: bool,
    ocr_error: Option<String>,

    selection_mode: SelectionMode,
    default_mode: SelectionMode,
    is_dragging: bool,
    drag_start: (f32, f32),
    drag_current: (f32, f32),
    hover_pos: (f32, f32),
    glow_pos: (f32, f32),
    current_selection: SelectionResult,

    status_toast: Option<(String, Instant)>,
    closing_soon: bool,
}

/// Groups horizontally adjacent words on each line into smooth line-level capsules
/// (only merging words separated by a single word space `<= 18px`, never bridging across separate icons or columns).
fn compute_line_capsules(layout: &OcrLayout) -> Vec<RectF> {
    let mut capsules = Vec::new();
    for line in &layout.lines {
        let mut current: Option<RectF> = None;
        for word in &line.words {
            let wb = word.bounds;
            match current.as_mut() {
                None => current = Some(wb),
                Some(acc) => {
                    let min_h = acc.height.min(wb.height).max(8.0);
                    let max_gap = (min_h * 0.90).clamp(4.0, 18.0);
                    let same_baseline = (acc.center_y() - wb.center_y()).abs() <= min_h * 0.55;
                    if same_baseline && (wb.x - acc.right()) <= max_gap {
                        let min_x = acc.x.min(wb.x);
                        let min_y = acc.y.min(wb.y);
                        let max_x = acc.right().max(wb.right());
                        let max_y = acc.bottom().max(wb.bottom());
                        *acc = RectF::new(min_x, min_y, max_x - min_x, max_y - min_y);
                    } else {
                        capsules.push(*acc);
                        *acc = wb;
                    }
                }
            }
        }
        if let Some(last) = current {
            capsules.push(last);
        }
    }
    capsules
}

impl OverlayState {
    fn width_f(&self) -> f32 {
        self.frame.width as f32
    }

    fn height_f(&self) -> f32 {
        self.frame.height as f32
    }

    fn dpi_scale(&self) -> f32 {
        self.frame.dpi_scale.clamp(1.0, 3.0)
    }

    fn top_toolbar_rect(&self) -> RectF {
        let s = self.dpi_scale();
        let w = (410.0 * s).min(self.width_f() - 32.0 * s);
        let h = 44.0 * s;
        let x = (self.width_f() - w) * 0.5;
        let y = 22.0 * s;
        RectF::new(x, y, w, h)
    }

    fn btn_linear_rect(&self) -> RectF {
        let s = self.dpi_scale();
        let bar = self.top_toolbar_rect();
        RectF::new(bar.x + 8.0 * s, bar.y + 6.0 * s, 118.0 * s, 32.0 * s)
    }

    fn btn_box_rect(&self) -> RectF {
        let s = self.dpi_scale();
        let bar = self.top_toolbar_rect();
        RectF::new(bar.x + 132.0 * s, bar.y + 6.0 * s, 112.0 * s, 32.0 * s)
    }

    fn btn_copy_all_rect(&self) -> RectF {
        let s = self.dpi_scale();
        let bar = self.top_toolbar_rect();
        RectF::new(bar.x + 250.0 * s, bar.y + 6.0 * s, 112.0 * s, 32.0 * s)
    }

    fn btn_close_rect(&self) -> RectF {
        let s = self.dpi_scale();
        let bar = self.top_toolbar_rect();
        RectF::new(bar.right() - 40.0 * s, bar.y + 6.0 * s, 32.0 * s, 32.0 * s)
    }

    fn status_pill_rect(&self) -> RectF {
        let s = self.dpi_scale();
        let w = (540.0 * s).min(self.width_f() - 32.0 * s);
        let h = 36.0 * s;
        let x = (self.width_f() - w) * 0.5;
        let y = 74.0 * s;
        RectF::new(x, y, w, h)
    }

    fn is_point_on_toolbar(&self, x: f32, y: f32) -> bool {
        self.top_toolbar_rect().contains(x, y)
    }

    fn needs_animation_tick(&self) -> bool {
        if self.ocr_loading {
            return true;
        }
        if self.start_instant.elapsed().as_secs_f32() < 0.45 {
            return true;
        }
        if let Some(t) = self.ocr_ready_instant {
            if t.elapsed().as_secs_f32() < 0.70 {
                return true;
            }
        }
        let dx = (self.hover_pos.0 - self.glow_pos.0).abs();
        let dy = (self.hover_pos.1 - self.glow_pos.1).abs();
        dx > 0.6 || dy > 0.6
    }

    fn ensure_render_target(&mut self) -> Result<(), String> {
        if self.render_target.is_some() {
            return Ok(());
        }

        unsafe {
            let render_props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: Default::default(),
            };

            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: D2D_SIZE_U {
                    width: self.frame.width,
                    height: self.frame.height,
                },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };

            let rt = self
                .d2d_factory
                .CreateHwndRenderTarget(&render_props, &hwnd_props)
                .map_err(|e| format!("Gagal membuat Direct2D HwndRenderTarget: {e}"))?;

            let bmp_props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };

            let bmp = rt
                .CreateBitmap(
                    D2D_SIZE_U {
                        width: self.frame.width,
                        height: self.frame.height,
                    },
                    Some(self.frame.pixels_bgra.as_ptr() as *const _),
                    self.frame.width * 4,
                    &bmp_props,
                )
                .map_err(|e| format!("Gagal membuat Direct2D Bitmap layar: {e}"))?;

            let solid = rt
                .CreateSolidColorBrush(
                    &D2D1_COLOR_F {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: 1.0,
                    },
                    None,
                )
                .map_err(|e| format!("Gagal membuat SolidColorBrush: {e}"))?;

            let orb_purple = create_cached_radial_brush(
                &rt,
                (0.68, 0.22, 0.98, 0.36),
                (0.38, 0.12, 0.78, 0.14),
            )?;
            let orb_pink = create_cached_radial_brush(
                &rt,
                (0.96, 0.38, 0.62, 0.30),
                (0.68, 0.20, 0.88, 0.11),
            )?;
            let orb_blue = create_cached_radial_brush(
                &rt,
                (0.16, 0.68, 0.98, 0.28),
                (0.32, 0.28, 0.92, 0.10),
            )?;

            let border_stops = [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: D2D1_COLOR_F {
                        r: 0.18,
                        g: 0.62,
                        b: 0.98,
                        a: 0.85,
                    },
                },
                D2D1_GRADIENT_STOP {
                    position: 0.5,
                    color: D2D1_COLOR_F {
                        r: 0.62,
                        g: 0.24,
                        b: 0.98,
                        a: 0.85,
                    },
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: D2D1_COLOR_F {
                        r: 0.94,
                        g: 0.32,
                        b: 0.66,
                        a: 0.85,
                    },
                },
            ];
            let stop_col = rt
                .CreateGradientStopCollection(&border_stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
                .map_err(|e| format!("Gagal membuat GradientStopCollection: {e}"))?;
            let lin_props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: D2D_POINT_2F { x: 0.0, y: 0.0 },
                endPoint: D2D_POINT_2F {
                    x: self.width_f(),
                    y: self.height_f(),
                },
            };
            let edge_grad = rt
                .CreateLinearGradientBrush(&lin_props, None, &stop_col)
                .map_err(|e| format!("Gagal membuat LinearGradientBrush: {e}"))?;

            self.brushes = Some(CachedBrushes {
                solid,
                orb_purple,
                orb_pink,
                orb_blue,
                edge_grad,
            });
            self.screen_bitmap = Some(bmp);
            self.render_target = Some(rt);
            Ok(())
        }
    }

    fn poll_ocr_result(&mut self) {
        if let Some(rx) = &self.ocr_rx {
            if let Ok(res) = rx.try_recv() {
                self.ocr_loading = false;
                self.ocr_rx = None;
                match res {
                    Ok(layout) => {
                        self.line_capsules = compute_line_capsules(&layout);
                        self.ocr_layout = layout;
                        self.ocr_ready_instant = Some(Instant::now());
                        if self.is_dragging {
                            self.update_selection();
                        }
                    }
                    Err(err) => {
                        self.ocr_error = Some(err);
                    }
                }
            }
        }
    }

    fn effective_mode(&self) -> SelectionMode {
        unsafe {
            let shift_held = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
            let alt_held = (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0;
            if shift_held || alt_held {
                SelectionMode::Box
            } else {
                self.selection_mode
            }
        }
    }

    fn update_selection(&mut self) {
        let mode = self.effective_mode();
        self.current_selection = self.ocr_layout.compute_selection(
            self.drag_start.0,
            self.drag_start.1,
            self.drag_current.0,
            self.drag_current.1,
            mode,
        );
    }

    fn finish_copy_and_close(&mut self, text: &str) {
        match copy_text_to_clipboard(text) {
            Ok(()) => {
                let word_count = text.split_whitespace().count();
                self.status_toast = Some((
                    format!("Tersalin ke Clipboard ({word_count} kata)"),
                    Instant::now(),
                ));
                self.closing_soon = true;
                unsafe {
                    let _ = SetTimer(self.hwnd, CLOSE_DELAY_TIMER_ID, 200, None);
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
            }
            Err(err) => {
                self.status_toast = Some((err, Instant::now()));
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
            }
        }
    }

    fn render(&mut self) {
        self.poll_ocr_result();
        if self.ensure_render_target().is_err() {
            return;
        }

        // Smooth cursor trail interpolation
        self.glow_pos.0 += (self.hover_pos.0 - self.glow_pos.0) * 0.30;
        self.glow_pos.1 += (self.hover_pos.1 - self.glow_pos.1) * 0.30;

        let elapsed = self.start_instant.elapsed().as_secs_f32();
        let intro_t = (elapsed / 0.32).clamp(0.0, 1.0);
        let intro_ease = 1.0 - (1.0 - intro_t).powi(3);

        let w = self.width_f();
        let h = self.height_f();
        let (gx, gy) = self.glow_pos;

        let Some(rt) = self.render_target.clone() else {
            return;
        };

        unsafe {
            rt.BeginDraw();

            // 1. Frozen Screen Layer
            if let Some(bmp) = &self.screen_bitmap {
                let dest = D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: w,
                    bottom: h,
                };
                rt.DrawBitmap(
                    bmp,
                    Some(&dest),
                    1.0,
                    windows::Win32::Graphics::Direct2D::D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                );
            }

            if let Some(b) = &self.brushes {
                // 2. Deep Ocean-Navy Screen Wash
                b.solid.SetColor(&D2D1_COLOR_F {
                    r: 0.015,
                    g: 0.060,
                    b: 0.135,
                    a: 0.48 * intro_ease,
                });
                rt.FillRectangle(
                    &D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: w,
                        bottom: h,
                    },
                    &b.solid,
                );

                // 3. Cursor-Following Multi-Color Torch Aura (Zero allocations: updates cached brush centers!)
                paint_cached_orb(&rt, &b.orb_pink, gx + 34.0, gy - 22.0, 195.0, intro_ease);
                paint_cached_orb(&rt, &b.orb_blue, gx - 38.0, gy + 20.0, 205.0, intro_ease);
                paint_cached_orb(&rt, &b.orb_purple, gx, gy, 175.0, intro_ease);

                // 4. Screen Edge Aura (using cached linear gradient brush)
                draw_cached_edge_aura(&rt, &b.edge_grad, w, h, intro_ease);
            }

            // 5. Fast Single-Pass OCR Line Capsules & Active Selection
            self.draw_ocr_capsules_and_selection(&rt, gx, gy);

            // 6. Top Floating Toolbar & Status Text
            self.draw_floating_ui(&rt);

            let _ = rt.EndDraw(None, None);
        }
    }

    unsafe fn draw_ocr_capsules_and_selection(
        &self,
        rt: &ID2D1HwndRenderTarget,
        gx: f32,
        gy: f32,
    ) {
        let Some(b) = &self.brushes else { return };
        let solid = &b.solid;

        let ocr_elapsed = self
            .ocr_ready_instant
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);

        let w = self.width_f();
        let h = self.height_f();
        let max_diag = (w * w + h * h).sqrt().max(1.0);
        let click_x = self.frame.local_click_x;
        let click_y = self.frame.local_click_y;
        let reveal_done = ocr_elapsed >= 0.50;
        let reveal_radius = if reveal_done {
            max_diag * 2.0
        } else {
            ocr_elapsed * (max_diag / 0.45)
        };

        let pad_x = 6.5;
        let pad_y = 3.5;

        for cap in &self.line_capsules {
            let cx = cap.center_x();
            let cy = cap.center_y();

            let mut shimmer_alpha = 0.0_f32;
            if !reveal_done {
                let dist_click = ((cx - click_x).powi(2) + (cy - click_y).powi(2)).sqrt();
                if dist_click > reveal_radius {
                    continue;
                }
                let wave_age = (ocr_elapsed - (dist_click / (max_diag / 0.45))).max(0.0);
                if wave_age < 0.35 {
                    shimmer_alpha = (1.0 - (wave_age / 0.35)) * 0.42;
                }
            }

            // Fast bounding-box check for cursor proximity before computing glow
            let dx = if gx < cap.x {
                cap.x - gx
            } else if gx > cap.right() {
                gx - cap.right()
            } else {
                0.0
            };
            let dy = if gy < cap.y {
                cap.y - gy
            } else if gy > cap.bottom() {
                gy - cap.bottom()
            } else {
                0.0
            };

            let mut glow_alpha = shimmer_alpha;
            if dx < 190.0 && dy < 190.0 {
                let dist_sq = dx * dx + dy * dy;
                if dist_sq < 190.0 * 190.0 {
                    let prox = 1.0 - (dist_sq.sqrt() / 190.0);
                    glow_alpha = (glow_alpha + prox * prox * 0.48).min(0.65);
                }
            }

            let pill_rect = D2D_RECT_F {
                left: (cap.x - pad_x).max(0.0),
                top: (cap.y - pad_y).max(0.0),
                right: (cap.right() + pad_x).min(w),
                bottom: (cap.bottom() + pad_y).min(h),
            };

            // 1. Outer backlit purple-pink halo ring (drawn OUTSIDE the capsule so it never covers the text!)
            if glow_alpha > 0.03 {
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.76,
                    g: 0.32,
                    b: 0.98,
                    a: glow_alpha * 0.55,
                });
                rt.DrawRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: pill_rect.left - 3.0,
                            top: pill_rect.top - 3.0,
                            right: pill_rect.right + 3.0,
                            bottom: pill_rect.bottom + 3.0,
                        },
                        radiusX: 9.0,
                        radiusY: 9.0,
                    },
                    solid,
                    4.5,
                    None,
                );
            }

            // 2. Restore 100% original crisp un-tinted screen pixels inside the capsule
            // so the purple/blue overlay wash & cursor orb NEVER cover or dim the detected text!
            if let Some(bmp) = &self.screen_bitmap {
                rt.DrawBitmap(
                    bmp,
                    Some(&pill_rect),
                    1.0,
                    windows::Win32::Graphics::Direct2D::D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    Some(&pill_rect),
                );
            }

            // 3. Crisp subtle outline around the capsule
            let rrect = D2D1_ROUNDED_RECT {
                rect: pill_rect,
                radiusX: 6.5,
                radiusY: 6.5,
            };
            solid.SetColor(&D2D1_COLOR_F {
                r: 0.62,
                g: 0.76,
                b: 0.98,
                a: 0.28 + glow_alpha * 0.50,
            });
            rt.DrawRoundedRectangle(&rrect, solid, 1.1, None);
        }

        // Hover indicator on the specific word under the cursor
        if !self.is_dragging {
            if let Some(hovered) = self
                .ocr_layout
                .word_at_point(self.hover_pos.0, self.hover_pos.1)
            {
                let r = hovered.bounds;
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.56,
                    g: 0.80,
                    b: 1.0,
                    a: 0.70,
                });
                rt.DrawRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: r.x - 2.5,
                            top: r.y - 2.0,
                            right: r.right() + 2.5,
                            bottom: r.bottom() + 2.0,
                        },
                        radiusX: 4.5,
                        radiusY: 4.5,
                    },
                    solid,
                    1.3,
                    None,
                );
            }
        }

        // Active selection highlight rectangles (light 0.16 tint + crisp border so text stays 100% readable)
        if !self.current_selection.highlight_rects.is_empty() {
            for rect in &self.current_selection.highlight_rects {
                let pad = 3.0;
                let rrect = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: rect.x - pad,
                        top: rect.y - pad,
                        right: rect.right() + pad,
                        bottom: rect.bottom() + pad,
                    },
                    radiusX: 5.5,
                    radiusY: 5.5,
                };
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.23,
                    g: 0.55,
                    b: 0.98,
                    a: 0.18,
                });
                rt.FillRoundedRectangle(&rrect, solid);

                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.52,
                    g: 0.82,
                    b: 1.0,
                    a: 0.98,
                });
                rt.DrawRoundedRectangle(&rrect, solid, 2.0, None);
            }
        }

        // Box selection marquee
        if self.is_dragging && self.effective_mode() == SelectionMode::Box {
            let box_rect = RectF::from_points(
                self.drag_start.0,
                self.drag_start.1,
                self.drag_current.0,
                self.drag_current.1,
            );
            let d2d_box = D2D_RECT_F {
                left: box_rect.x,
                top: box_rect.y,
                right: box_rect.right(),
                bottom: box_rect.bottom(),
            };
            solid.SetColor(&D2D1_COLOR_F {
                r: 0.58,
                g: 0.25,
                b: 0.98,
                a: 0.16,
            });
            rt.FillRectangle(&d2d_box, solid);

            solid.SetColor(&D2D1_COLOR_F {
                r: 0.35,
                g: 0.78,
                b: 1.0,
                a: 0.92,
            });
            rt.DrawRectangle(&d2d_box, solid, 1.8, None);
        }
    }

    unsafe fn draw_floating_ui(&self, rt: &ID2D1HwndRenderTarget) {
        let Some(b) = &self.brushes else { return };
        let solid = &b.solid;

        let s = self.dpi_scale();
        let bar = self.top_toolbar_rect();
        let pill = self.status_pill_rect();

        let bar_rrect = D2D1_ROUNDED_RECT {
            rect: to_d2d_rect(bar),
            radiusX: 10.0 * s,
            radiusY: 10.0 * s,
        };
        solid.SetColor(&D2D1_COLOR_F {
            r: 0.067,
            g: 0.067,
            b: 0.067,
            a: 0.94,
        });
        rt.FillRoundedRectangle(&bar_rrect, solid);

        solid.SetColor(&D2D1_COLOR_F {
            r: 0.20,
            g: 0.20,
            b: 0.20,
            a: 0.90,
        });
        rt.DrawRoundedRectangle(&bar_rrect, solid, 1.2 * s, None);

        let effective = self.effective_mode();
        let (hx, hy) = self.hover_pos;
        let btn_radius = 7.0 * s;

        for (rect, label, is_active) in [
            (
                self.btn_linear_rect(),
                "Teks Linear",
                effective == SelectionMode::Linear,
            ),
            (
                self.btn_box_rect(),
                "Kotak Area",
                effective == SelectionMode::Box,
            ),
            (self.btn_copy_all_rect(), "Salin Semua", false),
            (self.btn_close_rect(), "✕", false),
        ] {
            if is_active {
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.22,
                    g: 0.22,
                    b: 0.22,
                    a: 1.0,
                });
                rt.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: to_d2d_rect(rect),
                        radiusX: btn_radius,
                        radiusY: btn_radius,
                    },
                    solid,
                );
            } else if rect.contains(hx, hy) {
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.14,
                    g: 0.14,
                    b: 0.14,
                    a: 0.95,
                });
                rt.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: to_d2d_rect(rect),
                        radiusX: btn_radius,
                        radiusY: btn_radius,
                    },
                    solid,
                );
            }

            solid.SetColor(&D2D1_COLOR_F {
                r: 0.96,
                g: 0.98,
                b: 1.0,
                a: 1.0,
            });
            draw_centered_text(rt, &self.text_format_ui, label, rect, solid);
        }

        let status_msg = if let Some((toast, _)) = &self.status_toast {
            toast.clone()
        } else if let Some(err) = &self.ocr_error {
            err.clone()
        } else if self.ocr_loading {
            "Mendeteksi tulisan di layar...".to_string()
        } else if !self.current_selection.text.is_empty() {
            let count = self.current_selection.selected_word_indices.len();
            let chars = self.current_selection.text.chars().count();
            format!("{count} kata ({chars} karakter) dipilih, lepas klik untuk menyalin")
        } else if self.ocr_layout.is_empty() {
            "Tidak ada teks terdeteksi di layar ini · Tekan Esc untuk keluar".to_string()
        } else {
            format!(
                "Seret teks dari mana ke mana untuk menyalin ({} kata terdeteksi) · Esc batal",
                self.ocr_layout.words.len()
            )
        };

        solid.SetColor(&D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.85,
        });
        let shadow_rect = RectF::new(pill.x + 1.0, pill.y + 1.5, pill.width, pill.height);
        draw_centered_text(rt, &self.text_format_small, &status_msg, shadow_rect, solid);

        solid.SetColor(&D2D1_COLOR_F {
            r: 0.82,
            g: 0.88,
            b: 0.96,
            a: 0.96,
        });
        draw_centered_text(rt, &self.text_format_small, &status_msg, pill, solid);
    }
}

unsafe fn create_cached_radial_brush(
    rt: &ID2D1HwndRenderTarget,
    inner_rgba: (f32, f32, f32, f32),
    mid_rgba: (f32, f32, f32, f32),
) -> Result<ID2D1RadialGradientBrush, String> {
    let stops = [
        D2D1_GRADIENT_STOP {
            position: 0.0,
            color: D2D1_COLOR_F {
                r: inner_rgba.0,
                g: inner_rgba.1,
                b: inner_rgba.2,
                a: inner_rgba.3,
            },
        },
        D2D1_GRADIENT_STOP {
            position: 0.55,
            color: D2D1_COLOR_F {
                r: mid_rgba.0,
                g: mid_rgba.1,
                b: mid_rgba.2,
                a: mid_rgba.3,
            },
        },
        D2D1_GRADIENT_STOP {
            position: 1.0,
            color: D2D1_COLOR_F {
                r: mid_rgba.0,
                g: mid_rgba.1,
                b: mid_rgba.2,
                a: 0.0,
            },
        },
    ];
    let col = rt
        .CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
        .map_err(|e| format!("Gagal membuat radial stop collection: {e}"))?;
    let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
        center: D2D_POINT_2F { x: 0.0, y: 0.0 },
        gradientOriginOffset: D2D_POINT_2F { x: 0.0, y: 0.0 },
        radiusX: 180.0,
        radiusY: 180.0,
    };
    rt.CreateRadialGradientBrush(&props, None, &col)
        .map_err(|e| format!("Gagal membuat RadialGradientBrush: {e}"))
}

unsafe fn paint_cached_orb(
    rt: &ID2D1HwndRenderTarget,
    brush: &ID2D1RadialGradientBrush,
    cx: f32,
    cy: f32,
    radius: f32,
    opacity: f32,
) {
    let pt = D2D_POINT_2F { x: cx, y: cy };
    brush.SetCenter(pt);
    brush.SetRadiusX(radius);
    brush.SetRadiusY(radius);
    brush.SetOpacity(opacity);
    rt.FillEllipse(
        &D2D1_ELLIPSE {
            point: pt,
            radiusX: radius,
            radiusY: radius,
        },
        brush,
    );
}

unsafe fn draw_cached_edge_aura(
    rt: &ID2D1HwndRenderTarget,
    grad_brush: &ID2D1LinearGradientBrush,
    w: f32,
    h: f32,
    intro_ease: f32,
) {
    let edge_thickness = 22.0;
    let aura_alpha = 0.28 * intro_ease;

    for i in 0..4 {
        let inset = i as f32 * (edge_thickness / 4.0);
        let layer_alpha = (1.0 - (i as f32 / 4.0)).powi(2) * aura_alpha;
        grad_brush.SetOpacity(layer_alpha);
        rt.DrawRectangle(
            &D2D_RECT_F {
                left: inset,
                top: inset,
                right: w - inset,
                bottom: h - inset,
            },
            grad_brush,
            edge_thickness / 3.5,
            None,
        );
    }

    grad_brush.SetOpacity(0.90 * intro_ease);
    rt.DrawRectangle(
        &D2D_RECT_F {
            left: 1.5,
            top: 1.5,
            right: w - 1.5,
            bottom: h - 1.5,
        },
        grad_brush,
        2.5,
        None,
    );
}

fn to_d2d_rect(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

unsafe fn draw_centered_text(
    rt: &ID2D1HwndRenderTarget,
    format: &IDWriteTextFormat,
    text: &str,
    rect: RectF,
    brush: &ID2D1SolidColorBrush,
) {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let d2d_rect = to_d2d_rect(rect);
    rt.DrawText(
        &utf16,
        format,
        &d2d_rect,
        brush,
        windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
        windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
    );
}

pub fn run_overlay_session(
    frame: CapturedFrame,
    default_mode: SelectionMode,
) -> Result<SelectionMode, String> {
    unsafe {
        let d2d_factory: ID2D1Factory =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                .map_err(|e| format!("Gagal menginisialisasi Direct2D Factory: {e}"))?;

        let dwrite_factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
            .map_err(|e| format!("Gagal menginisialisasi DirectWrite Factory: {e}"))?;

        let ui_scale = frame.dpi_scale.clamp(1.0, 3.0);

        let text_format_ui = dwrite_factory
            .CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                13.0 * ui_scale,
                w!("en-us"),
            )
            .map_err(|e| format!("Gagal membuat format font UI: {e}"))?;
        let _ = text_format_ui.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = text_format_ui.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

        let text_format_small = dwrite_factory
            .CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                12.5 * ui_scale,
                w!("en-us"),
            )
            .map_err(|e| format!("Gagal membuat format font status: {e}"))?;
        let _ = text_format_small.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = text_format_small.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

        let (tx, rx) = mpsc::channel();
        let frame_for_ocr = frame.clone();
        thread::spawn(move || {
            let res = recognize_frame(&frame_for_ocr);
            let _ = tx.send(res);
        });

        let hinstance = GetModuleHandleW(None)
            .map_err(|e| format!("Gagal membaca module handle: {e}"))?;
        let class_name: PCWSTR = w!("CopyScreenOverlayWindowClass");

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(overlay_wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);

        let mon_x = frame.monitor_x;
        let mon_y = frame.monitor_y;
        let mon_w = frame.width as i32;
        let mon_h = frame.height as i32;
        let init_x = frame.local_click_x;
        let init_y = frame.local_click_y;

        let state = Box::new(OverlayState {
            hwnd: HWND::default(),
            frame,
            d2d_factory,
            _dwrite_factory: dwrite_factory,
            text_format_ui,
            text_format_small,
            render_target: None,
            screen_bitmap: None,
            brushes: None,
            start_instant: Instant::now(),
            ocr_rx: Some(rx),
            ocr_layout: OcrLayout::default(),
            line_capsules: Vec::new(),
            ocr_ready_instant: None,
            ocr_loading: true,
            ocr_error: None,
            selection_mode: default_mode,
            default_mode,
            is_dragging: false,
            drag_start: (init_x, init_y),
            drag_current: (init_x, init_y),
            hover_pos: (init_x, init_y),
            glow_pos: (init_x, init_y),
            current_selection: SelectionResult::default(),
            status_toast: None,
            closing_soon: false,
        });

        let state_ptr = Box::into_raw(state);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_name,
            w!("CopyScreen Overlay"),
            WS_POPUP,
            mon_x,
            mon_y,
            mon_w,
            mon_h,
            HWND::default(),
            None,
            hinstance,
            Some(state_ptr as *const _),
        )
        .map_err(|e| {
            let _ = Box::from_raw(state_ptr);
            format!("Gagal membuat window overlay CopyScreen: {e}")
        })?;

        (*state_ptr).hwnd = hwnd;
        set_overlay_hwnd(hwnd);

        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetTimer(hwnd, ANIM_TIMER_ID, ANIM_INTERVAL_MS, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if !windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd).as_bool() {
                break;
            }
        }

        let final_state = Box::from_raw(state_ptr);
        crate::ui::trim_process_working_set();
        Ok(final_state.default_mode)
    }
}

unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, CREATESTRUCTW, GWLP_USERDATA, WM_NCCREATE,
    };

    if msg == WM_NCCREATE {
        let cs = lparam.0 as *const CREATESTRUCTW;
        if !cs.is_null() {
            let state_ptr = (*cs).lpCreateParams as isize;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr);
        }
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut OverlayState;
    if state_ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let state = &mut *state_ptr;

    match msg {
        WM_ERASEBKGND => LRESULT(1),

        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),

        WM_APP_OVERLAY_COPY => {
            if !state.current_selection.text.is_empty() {
                let chosen = state.current_selection.text.clone();
                state.finish_copy_and_close(&chosen);
            }
            LRESULT(0)
        }

        WM_TIMER => {
            if wparam.0 == CLOSE_DELAY_TIMER_ID {
                let _ = KillTimer(hwnd, CLOSE_DELAY_TIMER_ID);
                let _ = DestroyWindow(hwnd);
            } else if wparam.0 == ANIM_TIMER_ID {
                state.poll_ocr_result();
                if state.needs_animation_tick() {
                    let _ = InvalidateRect(hwnd, None, false);
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

        WM_SETCURSOR => {
            let (hx, hy) = state.hover_pos;
            let cursor_id = if state.is_point_on_toolbar(hx, hy) {
                IDC_HAND
            } else if state.effective_mode() == SelectionMode::Box {
                IDC_CROSS
            } else if state.ocr_layout.word_at_point(hx, hy).is_some() {
                IDC_IBEAM
            } else {
                IDC_ARROW
            };
            if let Ok(cursor) = LoadCursorW(None, cursor_id) {
                SetCursor(cursor);
            }
            LRESULT(1)
        }

        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
            if (x - state.hover_pos.0).abs() > 0.5 || (y - state.hover_pos.1).abs() > 0.5 {
                state.hover_pos = (x, y);
                if state.is_dragging && !state.closing_soon {
                    state.drag_current = (x, y);
                    state.update_selection();
                }
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }

        WM_LBUTTONDOWN => {
            if state.closing_soon {
                return LRESULT(0);
            }
            let x = (lparam.0 & 0xFFFF) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
            state.hover_pos = (x, y);

            if state.btn_linear_rect().contains(x, y) {
                state.selection_mode = SelectionMode::Linear;
                state.default_mode = SelectionMode::Linear;
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }
            if state.btn_box_rect().contains(x, y) {
                state.selection_mode = SelectionMode::Box;
                state.default_mode = SelectionMode::Box;
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }
            if state.btn_copy_all_rect().contains(x, y) {
                let all_text = state.ocr_layout.full_text();
                if !all_text.is_empty() {
                    state.finish_copy_and_close(&all_text);
                } else {
                    state.status_toast =
                        Some(("Belum ada teks untuk disalin.".to_string(), Instant::now()));
                }
                return LRESULT(0);
            }
            if state.btn_close_rect().contains(x, y) {
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }

            state.is_dragging = true;
            state.drag_start = (x, y);
            state.drag_current = (x, y);
            state.update_selection();
            let _ = SetCapture(hwnd);
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }

        WM_LBUTTONUP => {
            if state.is_dragging {
                state.is_dragging = false;
                let _ = ReleaseCapture();

                let x = (lparam.0 & 0xFFFF) as i16 as f32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
                state.drag_current = (x, y);
                state.update_selection();

                if !state.current_selection.text.is_empty() {
                    let chosen = state.current_selection.text.clone();
                    state.finish_copy_and_close(&chosen);
                } else {
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            LRESULT(0)
        }

        WM_RBUTTONUP => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }

        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
            if vk == VK_ESCAPE.0 {
                let _ = DestroyWindow(hwnd);
            } else if vk == b'C' as u16 {
                let ctrl = (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
                if ctrl && !state.current_selection.text.is_empty() {
                    let chosen = state.current_selection.text.clone();
                    state.finish_copy_and_close(&chosen);
                }
            }
            LRESULT(0)
        }

        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }

        WM_DESTROY => {
            let _ = KillTimer(hwnd, ANIM_TIMER_ID);
            let _ = KillTimer(hwnd, CLOSE_DELAY_TIMER_ID);
            state.brushes = None;
            state.screen_bitmap = None;
            state.render_target = None;
            state.frame.pixels_bgra.clear();
            state.frame.pixels_bgra.shrink_to_fit();
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
