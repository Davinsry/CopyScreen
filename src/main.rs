#![cfg_attr(not(test), windows_subsystem = "windows")]

mod capture;
mod clipboard;
mod hook;
mod ocr;
mod overlay;
mod selection;
mod tray;
mod ui;

use crate::capture::capture_monitor_at_point;
use crate::hook::{
    get_active_shortcut, is_overlay_active, load_saved_shortcut, set_overlay_active,
    GlobalInputHooks, WM_APP_TRIGGER_OVERLAY,
};
use crate::overlay::run_overlay_session;
use crate::selection::SelectionMode;
use crate::tray::{
    create_copyscreen_logo_icon, ensure_default_autostart, SystemTray, WM_APP_TRAY_ICON,
};
use crate::ui::{run_dashboard_ui, trim_process_working_set, DashboardAction};
use std::thread;
use std::time::Duration;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW,
    GetWindowLongPtrW, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW,
    SetWindowLongPtrW, TranslateMessage, CREATESTRUCTW, GWLP_USERDATA, MB_ICONERROR, MB_OK, MSG,
    WM_APP, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NCCREATE, WM_RBUTTONUP, WNDCLASSW,
};

const WM_APP_OPEN_DASHBOARD: u32 = WM_APP + 20;

struct DaemonState {
    hwnd: HWND,
    tray: Option<SystemTray>,
    default_mode: SelectionMode,
    dashboard_open: bool,
}

impl DaemonState {
    fn trigger_at(&mut self, screen_x: i32, screen_y: i32) {
        if is_overlay_active() {
            return;
        }
        set_overlay_active(true);

        match capture_monitor_at_point(screen_x, screen_y) {
            Ok(frame) => {
                if let Ok(updated_mode) = run_overlay_session(frame, self.default_mode) {
                    self.default_mode = updated_mode;
                }
            }
            Err(err) => {
                if let Some(tray) = &self.tray {
                    tray.show_balloon("CopyScreen Error", &err);
                }
            }
        }

        set_overlay_active(false);
        trim_process_working_set();
    }

    fn open_dashboard(&mut self) {
        if self.dashboard_open || is_overlay_active() {
            return;
        }
        self.dashboard_open = true;
        set_overlay_active(true);

        let action = run_dashboard_ui(self.default_mode);
        if let Some(tray) = &self.tray {
            tray.update_tooltip();
        }

        set_overlay_active(false);
        self.dashboard_open = false;
        trim_process_working_set();

        match action {
            DashboardAction::MinimizeToTray(mode) => {
                self.default_mode = mode;
            }
            DashboardAction::TriggerScanNow(mode) => {
                self.default_mode = mode;
                // Wait briefly so the closed dashboard window completely disappears before screen capture
                thread::sleep(Duration::from_millis(120));
                unsafe {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let _ = PostMessageW(
                        self.hwnd,
                        WM_APP_TRIGGER_OVERLAY,
                        WPARAM(pt.x as usize),
                        LPARAM(pt.y as isize),
                    );
                }
            }
            DashboardAction::ExitApplication => unsafe {
                PostQuitMessage(0);
            },
        }
    }
}

fn main() {
    unsafe {
        // Enable Per-Monitor V2 DPI Awareness so screen capture and overlay coordinates are 1:1 sharp
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--once" || a == "--trigger-now") {
        unsafe {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            if let Ok(frame) = capture_monitor_at_point(pt.x, pt.y) {
                let _ = run_overlay_session(frame, SelectionMode::Linear);
            }
            CoUninitialize();
        }
        return;
    }

    let start_minimized = args.iter().any(|a| a == "--minimized" || a == "--tray");

    if let Err(err) = run_daemon(start_minimized) {
        unsafe {
            let msg_utf16: Vec<u16> = err.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = MessageBoxW(
                HWND::default(),
                PCWSTR(msg_utf16.as_ptr()),
                w!("CopyScreen Error"),
                MB_OK | MB_ICONERROR,
            );
            CoUninitialize();
        }
    } else {
        unsafe {
            CoUninitialize();
        }
    }
}

fn run_daemon(start_minimized: bool) -> Result<(), String> {
    unsafe {
        let _ = load_saved_shortcut();
        ensure_default_autostart();
        let class_name = w!("CopyScreenDaemonHiddenWindowClass");

        // Single-instance mutex: if already running, tell the existing daemon to open the Dashboard UI!
        let mutex = CreateMutexW(None, true, w!("Local\\CopyScreenSingleInstanceMutex"))
            .map_err(|e| format!("Gagal membuat mutex aplikasi: {e}"))?;

        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(mutex);
            if let Ok(existing_hwnd) = FindWindowW(class_name, w!("CopyScreen Daemon")) {
                if !existing_hwnd.is_invalid() {
                    let _ = PostMessageW(
                        existing_hwnd,
                        WM_APP_OPEN_DASHBOARD,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
            return Ok(());
        }

        let hinstance = GetModuleHandleW(None)
            .map_err(|e| format!("Gagal mendapatkan module handle: {e}"))?;
        let app_icon = create_copyscreen_logo_icon().unwrap_or_default();

        let wc = WNDCLASSW {
            lpfnWndProc: Some(daemon_wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            hIcon: app_icon,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);

        let state = Box::new(DaemonState {
            hwnd: HWND::default(),
            tray: None,
            default_mode: SelectionMode::Linear,
            dashboard_open: false,
        });
        let state_ptr = Box::into_raw(state);

        let hwnd = CreateWindowExW(
            Default::default(),
            class_name,
            w!("CopyScreen Daemon"),
            Default::default(),
            0,
            0,
            0,
            0,
            HWND::default(),
            None,
            hinstance,
            Some(state_ptr as *const _),
        )
        .map_err(|e| {
            let _ = Box::from_raw(state_ptr);
            let _ = CloseHandle(mutex);
            format!("Gagal membuat hidden daemon window: {e}")
        })?;

        (*state_ptr).hwnd = hwnd;

        let _hooks = GlobalInputHooks::install(hwnd)?;
        // Warm up the OneOCR neural pipeline on a background thread so the first shortcut press is instant
        thread::spawn(|| {
            crate::ocr::warmup_ocr_engine();
        });
        let tray = SystemTray::new(hwnd)?;
        let current_sc = get_active_shortcut();
        tray.show_balloon(
            "CopyScreen Aktif di System Tray",
            &format!(
                "Tekan {} di mana saja untuk salin teks layar.",
                current_sc.display_name()
            ),
        );
        (*state_ptr).tray = Some(tray);

        trim_process_working_set();

        // Open the modern Dashboard UI on initial launch (unless started silently via --minimized)
        if !start_minimized {
            let _ = PostMessageW(hwnd, WM_APP_OPEN_DASHBOARD, WPARAM(0), LPARAM(0));
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = Box::from_raw(state_ptr);
        let _ = CloseHandle(mutex);
        Ok(())
    }
}

unsafe extern "system" fn daemon_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = lparam.0 as *const CREATESTRUCTW;
        if !cs.is_null() {
            let state_ptr = (*cs).lpCreateParams as isize;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr);
        }
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DaemonState;
    if state_ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let state = &mut *state_ptr;

    match msg {
        WM_APP_TRIGGER_OVERLAY => {
            let click_x = wparam.0 as i32;
            let click_y = lparam.0 as i32;
            state.trigger_at(click_x, click_y);
            LRESULT(0)
        }

        WM_APP_OPEN_DASHBOARD => {
            state.open_dashboard();
            LRESULT(0)
        }

        WM_APP_TRAY_ICON => {
            let event = (lparam.0 & 0xFFFF) as u32;
            // Both Left-Click and Right-Click on the System Tray logo open the modern CopyScreen Dashboard UI!
            if event == WM_LBUTTONUP || event == WM_RBUTTONUP || event == WM_LBUTTONDBLCLK {
                let _ = PostMessageW(hwnd, WM_APP_OPEN_DASHBOARD, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }

        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
