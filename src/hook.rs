use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU,
    VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetCursorPos, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK,
    KBDLLHOOKSTRUCT, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_CLOSE, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

pub const WM_APP_TRIGGER_OVERLAY: u32 = WM_APP + 10;
pub const WM_APP_OVERLAY_COPY: u32 = WM_APP + 12;

const SETTINGS_REG_SUBKEY: PCWSTR = w!("Software\\CopyScreen");
const SHORTCUT_REG_VALUE: PCWSTR = w!("Shortcut");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerInput {
    MouseLeftClick,
    MouseMiddleClick,
    MouseRightClick,
    /// Mouse side button (Back / XButton1) — can be used without any keyboard modifiers for 100% stealth mode.
    MouseXButton1,
    Key(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutConfig {
    pub ctrl: bool,
    pub win: bool,
    pub alt: bool,
    pub shift: bool,
    pub trigger: TriggerInput,
}

impl Default for ShortcutConfig {
    fn default() -> Self {
        Self {
            ctrl: true,
            win: true,
            alt: false,
            shift: false,
            trigger: TriggerInput::MouseLeftClick,
        }
    }
}

impl ShortcutConfig {
    pub fn presets() -> [Self; 7] {
        [
            // 0: Default (Ctrl + Win + Klik Kiri)
            Self::default(),
            // 1: Ctrl + Shift + Klik Kiri (Tanpa tombol Win)
            Self {
                ctrl: true,
                win: false,
                alt: false,
                shift: true,
                trigger: TriggerInput::MouseLeftClick,
            },
            // 2: Alt + Win + Klik Kiri
            Self {
                ctrl: false,
                win: true,
                alt: true,
                shift: false,
                trigger: TriggerInput::MouseLeftClick,
            },
            // 3: Ctrl + Alt + Klik Kiri
            Self {
                ctrl: true,
                win: false,
                alt: true,
                shift: false,
                trigger: TriggerInput::MouseLeftClick,
            },
            // 4: Ctrl + Win + Klik Tengah
            Self {
                ctrl: true,
                win: true,
                alt: false,
                shift: false,
                trigger: TriggerInput::MouseMiddleClick,
            },
            // 5: Tombol Samping Mouse / Back (100% Tanpa Keyboard — Aman untuk Web Ketat)
            Self {
                ctrl: false,
                win: false,
                alt: false,
                shift: false,
                trigger: TriggerInput::MouseXButton1,
            },
            // 6: Ctrl + Win + T (Keyboard saja)
            Self {
                ctrl: true,
                win: true,
                alt: false,
                shift: false,
                trigger: TriggerInput::Key(b'T' as u16),
            },
        ]
    }

    pub fn display_name(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.win {
            parts.push("Win".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }

        let trigger_str = match self.trigger {
            TriggerInput::MouseLeftClick => "Klik Kiri".to_string(),
            TriggerInput::MouseMiddleClick => "Klik Tengah".to_string(),
            TriggerInput::MouseRightClick => "Klik Kanan".to_string(),
            TriggerInput::MouseXButton1 => "Tombol Samping Mouse (Back)".to_string(),
            TriggerInput::Key(vk) => vk_to_name(vk),
        };
        parts.push(trigger_str);
        parts.join(" + ")
    }

    pub fn to_storage_string(&self) -> String {
        let mut tokens = Vec::new();
        if self.ctrl {
            tokens.push("CTRL".to_string());
        }
        if self.win {
            tokens.push("WIN".to_string());
        }
        if self.alt {
            tokens.push("ALT".to_string());
        }
        if self.shift {
            tokens.push("SHIFT".to_string());
        }
        match self.trigger {
            TriggerInput::MouseLeftClick => tokens.push("LCLICK".to_string()),
            TriggerInput::MouseMiddleClick => tokens.push("MCLICK".to_string()),
            TriggerInput::MouseRightClick => tokens.push("RCLICK".to_string()),
            TriggerInput::MouseXButton1 => tokens.push("XBUTTON1".to_string()),
            TriggerInput::Key(vk) => tokens.push(format!("KEY:{vk}")),
        }
        tokens.join("+")
    }

    pub fn from_storage_string(s: &str) -> Option<Self> {
        let mut cfg = Self {
            ctrl: false,
            win: false,
            alt: false,
            shift: false,
            trigger: TriggerInput::MouseLeftClick,
        };
        let mut has_trigger = false;

        for part in s.split('+').map(|p| p.trim().to_uppercase()) {
            match part.as_str() {
                "CTRL" => cfg.ctrl = true,
                "WIN" => cfg.win = true,
                "ALT" => cfg.alt = true,
                "SHIFT" => cfg.shift = true,
                "LCLICK" => {
                    cfg.trigger = TriggerInput::MouseLeftClick;
                    has_trigger = true;
                }
                "MCLICK" => {
                    cfg.trigger = TriggerInput::MouseMiddleClick;
                    has_trigger = true;
                }
                "RCLICK" => {
                    cfg.trigger = TriggerInput::MouseRightClick;
                    has_trigger = true;
                }
                "XBUTTON1" => {
                    cfg.trigger = TriggerInput::MouseXButton1;
                    has_trigger = true;
                }
                other if other.starts_with("KEY:") => {
                    let code: u16 = other.trim_start_matches("KEY:").parse().ok()?;
                    if code > 0 {
                        cfg.trigger = TriggerInput::Key(code);
                        has_trigger = true;
                    }
                }
                _ => return None,
            }
        }

        let has_modifier = cfg.ctrl || cfg.win || cfg.alt || cfg.shift;
        let allows_no_modifier = matches!(
            cfg.trigger,
            TriggerInput::MouseXButton1 | TriggerInput::MouseMiddleClick
        );

        if has_trigger && (has_modifier || allows_no_modifier) {
            Some(cfg)
        } else {
            None
        }
    }

    pub fn to_packed_u64(self) -> u64 {
        let mut bits = 0u64;
        if self.ctrl {
            bits |= 1 << 0;
        }
        if self.win {
            bits |= 1 << 1;
        }
        if self.alt {
            bits |= 1 << 2;
        }
        if self.shift {
            bits |= 1 << 3;
        }
        let (kind, code): (u64, u64) = match self.trigger {
            TriggerInput::MouseLeftClick => (1, 0),
            TriggerInput::MouseMiddleClick => (2, 0),
            TriggerInput::MouseRightClick => (3, 0),
            TriggerInput::Key(vk) => (4, vk as u64),
            TriggerInput::MouseXButton1 => (5, 0),
        };
        bits | (kind << 8) | (code << 16)
    }

    pub fn from_packed_u64(bits: u64) -> Self {
        if bits == 0 {
            return Self::default();
        }
        let ctrl = (bits & (1 << 0)) != 0;
        let win = (bits & (1 << 1)) != 0;
        let alt = (bits & (1 << 2)) != 0;
        let shift = (bits & (1 << 3)) != 0;
        let kind = (bits >> 8) & 0xFF;
        let code = ((bits >> 16) & 0xFFFF) as u16;
        let trigger = match kind {
            1 => TriggerInput::MouseLeftClick,
            2 => TriggerInput::MouseMiddleClick,
            3 => TriggerInput::MouseRightClick,
            4 => TriggerInput::Key(code),
            5 => TriggerInput::MouseXButton1,
            _ => TriggerInput::MouseLeftClick,
        };
        Self {
            ctrl,
            win,
            alt,
            shift,
            trigger,
        }
    }
}

pub fn vk_to_name(vk: u16) -> String {
    match vk {
        0x20 => "Space".to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        0x30..=0x39 | 0x41..=0x5A => (vk as u8 as char).to_string(),
        0xC0 => "`".to_string(),
        _ => format!("Key(0x{vk:02X})"),
    }
}

static DAEMON_HWND: AtomicIsize = AtomicIsize::new(0);
static OVERLAY_HWND: AtomicIsize = AtomicIsize::new(0);
static OVERLAY_ACTIVE: AtomicBool = AtomicBool::new(false);
static SWALLOW_NEXT_LEFT_UP: AtomicBool = AtomicBool::new(false);
static SWALLOW_NEXT_MIDDLE_UP: AtomicBool = AtomicBool::new(false);
static SWALLOW_NEXT_RIGHT_UP: AtomicBool = AtomicBool::new(false);
static SWALLOW_NEXT_XBUTTON_UP: AtomicBool = AtomicBool::new(false);
static ACTIVE_SHORTCUT_PACKED: AtomicU64 = AtomicU64::new(0);

pub fn get_active_shortcut() -> ShortcutConfig {
    let packed = ACTIVE_SHORTCUT_PACKED.load(Ordering::SeqCst);
    ShortcutConfig::from_packed_u64(packed)
}

pub fn set_active_shortcut(cfg: ShortcutConfig) {
    ACTIVE_SHORTCUT_PACKED.store(cfg.to_packed_u64(), Ordering::SeqCst);
}

pub fn set_overlay_hwnd(hwnd: HWND) {
    OVERLAY_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
}

pub fn load_saved_shortcut() -> ShortcutConfig {
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, SETTINGS_REG_SUBKEY, 0, KEY_READ, &mut hkey).is_err() {
            let def = ShortcutConfig::default();
            set_active_shortcut(def);
            return def;
        }

        let mut buf = [0u16; 128];
        let mut byte_len = (buf.len() * size_of::<u16>()) as u32;
        let ok = RegQueryValueExW(
            hkey,
            SHORTCUT_REG_VALUE,
            None,
            None,
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut byte_len),
        )
        .is_ok();
        let _ = RegCloseKey(hkey);

        if ok {
            let u16_len = (byte_len as usize / 2).saturating_sub(1);
            let s = String::from_utf16_lossy(&buf[..u16_len.min(buf.len())]);
            let trimmed = s.trim_end_matches('\0');
            if let Some(parsed) = ShortcutConfig::from_storage_string(trimmed) {
                set_active_shortcut(parsed);
                return parsed;
            }
        }

        let def = ShortcutConfig::default();
        set_active_shortcut(def);
        def
    }
}

pub fn save_shortcut(cfg: ShortcutConfig) -> Result<(), String> {
    set_active_shortcut(cfg);
    unsafe {
        let mut hkey = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            SETTINGS_REG_SUBKEY,
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
        .ok()
        .map_err(|e| format!("Gagal membuka registry pengaturan CopyScreen: {e}"))?;

        let s = cfg.to_storage_string();
        let utf16: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(
            utf16.as_ptr() as *const u8,
            utf16.len() * size_of::<u16>(),
        );
        let res = RegSetValueExW(hkey, SHORTCUT_REG_VALUE, 0, REG_SZ, Some(bytes))
            .ok()
            .map_err(|e| format!("Gagal menyimpan shortcut ke registry: {e}"));
        let _ = RegCloseKey(hkey);
        res
    }
}

pub struct GlobalInputHooks {
    mouse_hook: HHOOK,
    keyboard_hook: HHOOK,
}

impl GlobalInputHooks {
    pub fn install(daemon_hwnd: HWND) -> Result<Self, String> {
        DAEMON_HWND.store(daemon_hwnd.0 as isize, Ordering::SeqCst);
        let _ = load_saved_shortcut();

        unsafe {
            let mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(low_level_mouse_proc), None, 0)
                .map_err(|e| format!("Gagal memasang hook mouse global (WH_MOUSE_LL): {e}"))?;

            let keyboard_hook =
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(low_level_keyboard_proc), None, 0).map_err(
                    |e| {
                        let _ = UnhookWindowsHookEx(mouse_hook);
                        format!("Gagal memasang hook keyboard global (WH_KEYBOARD_LL): {e}")
                    },
                )?;

            Ok(Self {
                mouse_hook,
                keyboard_hook,
            })
        }
    }
}

impl Drop for GlobalInputHooks {
    fn drop(&mut self) {
        unsafe {
            if !self.mouse_hook.is_invalid() {
                let _ = UnhookWindowsHookEx(self.mouse_hook);
            }
            if !self.keyboard_hook.is_invalid() {
                let _ = UnhookWindowsHookEx(self.keyboard_hook);
            }
        }
    }
}

pub fn set_overlay_active(active: bool) {
    OVERLAY_ACTIVE.store(active, Ordering::SeqCst);
    if !active {
        OVERLAY_HWND.store(0, Ordering::SeqCst);
    }
}

pub fn is_overlay_active() -> bool {
    OVERLAY_ACTIVE.load(Ordering::SeqCst)
}

unsafe fn current_modifiers_match(cfg: &ShortcutConfig) -> bool {
    let ctrl_down = (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_LCONTROL.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RCONTROL.0 as i32) as u16 & 0x8000) != 0;

    let win_down = (GetAsyncKeyState(VK_LWIN.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RWIN.0 as i32) as u16 & 0x8000) != 0;

    let alt_down = (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_LMENU.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RMENU.0 as i32) as u16 & 0x8000) != 0;

    let shift_down = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_LSHIFT.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RSHIFT.0 as i32) as u16 & 0x8000) != 0;

    ctrl_down == cfg.ctrl && win_down == cfg.win && alt_down == cfg.alt && shift_down == cfg.shift
}

/// Only used when the `Win` key is part of a shortcut Without `Ctrl`/`Alt` held,
/// to prevent Windows from opening the Start Menu when `Win` is released.
unsafe fn suppress_start_menu_if_needed(cfg: &ShortcutConfig) {
    if cfg.win && !cfg.ctrl && !cfg.alt {
        let inputs = [
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0xFF),
                        wScan: 0,
                        dwFlags: Default::default(),
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0xFF),
                        wScan: 0,
                        dwFlags: KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
        ];
        let _ = SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

unsafe fn post_trigger(click_x: i32, click_y: i32) {
    let hwnd_val = DAEMON_HWND.load(Ordering::SeqCst);
    if hwnd_val != 0 {
        let _ = PostMessageW(
            HWND(hwnd_val as *mut _),
            WM_APP_TRIGGER_OVERLAY,
            WPARAM(click_x as usize),
            LPARAM(click_y as isize),
        );
    }
}

unsafe extern "system" fn low_level_mouse_proc(
    ncode: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if ncode >= 0 {
        let msg = wparam.0 as u32;
        if msg == WM_LBUTTONUP && SWALLOW_NEXT_LEFT_UP.swap(false, Ordering::SeqCst) {
            return LRESULT(1);
        }
        if msg == WM_MBUTTONUP && SWALLOW_NEXT_MIDDLE_UP.swap(false, Ordering::SeqCst) {
            return LRESULT(1);
        }
        if msg == WM_RBUTTONUP && SWALLOW_NEXT_RIGHT_UP.swap(false, Ordering::SeqCst) {
            return LRESULT(1);
        }
        if msg == WM_XBUTTONUP && SWALLOW_NEXT_XBUTTON_UP.swap(false, Ordering::SeqCst) {
            return LRESULT(1);
        }

        // When the overlay is open, swallow Right-Click at the OS hook level to cancel the overlay
        // without EVER leaking a right-click (`contextmenu`) event to the browser underneath!
        if is_overlay_active() {
            if msg == WM_RBUTTONDOWN {
                SWALLOW_NEXT_RIGHT_UP.store(true, Ordering::SeqCst);
                let ov_hwnd = OVERLAY_HWND.load(Ordering::SeqCst);
                if ov_hwnd != 0 {
                    let _ = PostMessageW(
                        HWND(ov_hwnd as *mut _),
                        WM_CLOSE,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
                return LRESULT(1);
            }
        } else {
            let cfg = get_active_shortcut();
            let matched = match (msg, cfg.trigger) {
                (WM_LBUTTONDOWN, TriggerInput::MouseLeftClick) => current_modifiers_match(&cfg),
                (WM_MBUTTONDOWN, TriggerInput::MouseMiddleClick) => current_modifiers_match(&cfg),
                (WM_RBUTTONDOWN, TriggerInput::MouseRightClick) => current_modifiers_match(&cfg),
                (WM_XBUTTONDOWN, TriggerInput::MouseXButton1) => current_modifiers_match(&cfg),
                _ => false,
            };

            if matched {
                let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
                match cfg.trigger {
                    TriggerInput::MouseLeftClick => {
                        SWALLOW_NEXT_LEFT_UP.store(true, Ordering::SeqCst)
                    }
                    TriggerInput::MouseMiddleClick => {
                        SWALLOW_NEXT_MIDDLE_UP.store(true, Ordering::SeqCst)
                    }
                    TriggerInput::MouseRightClick => {
                        SWALLOW_NEXT_RIGHT_UP.store(true, Ordering::SeqCst)
                    }
                    TriggerInput::MouseXButton1 => {
                        SWALLOW_NEXT_XBUTTON_UP.store(true, Ordering::SeqCst)
                    }
                    _ => {}
                }
                suppress_start_menu_if_needed(&cfg);
                post_trigger(info.pt.x, info.pt.y);
                return LRESULT(1);
            }
        }
    }

    CallNextHookEx(None, ncode, wparam, lparam)
}

unsafe extern "system" fn low_level_keyboard_proc(
    ncode: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if ncode >= 0 {
        let msg = wparam.0 as u32;
        if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
            let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);

            // Because the overlay uses WS_EX_NOACTIVATE (to avoid triggering `window.onblur` on websites),
            // intercept `Esc` and `Ctrl+C` here while the overlay is open and swallow them so they don't hit the browser!
            let ov_hwnd = OVERLAY_HWND.load(Ordering::SeqCst);
            if is_overlay_active() && ov_hwnd != 0 {
                if kb.vkCode == VK_ESCAPE.0 as u32 {
                    let _ = PostMessageW(
                        HWND(ov_hwnd as *mut _),
                        WM_CLOSE,
                        WPARAM(0),
                        LPARAM(0),
                    );
                    return LRESULT(1);
                }
                let ctrl_down = (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
                if ctrl_down && kb.vkCode == b'C' as u32 {
                    let _ = PostMessageW(
                        HWND(ov_hwnd as *mut _),
                        WM_APP_OVERLAY_COPY,
                        WPARAM(0),
                        LPARAM(0),
                    );
                    return LRESULT(1);
                }
            } else if !is_overlay_active() {
                let cfg = get_active_shortcut();
                if let TriggerInput::Key(target_vk) = cfg.trigger {
                    if kb.vkCode == target_vk as u32 && current_modifiers_match(&cfg) {
                        suppress_start_menu_if_needed(&cfg);
                        let mut pt = POINT::default();
                        let _ = GetCursorPos(&mut pt);
                        post_trigger(pt.x, pt.y);
                        return LRESULT(1);
                    }
                }
            }
        }
    }

    CallNextHookEx(None, ncode, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shortcut_is_ctrl_win_left_click() {
        let def = ShortcutConfig::default();
        assert_eq!(def.display_name(), "Ctrl + Win + Klik Kiri");
        let s = def.to_storage_string();
        assert_eq!(s, "CTRL+WIN+LCLICK");
        assert_eq!(ShortcutConfig::from_storage_string(&s), Some(def));
    }

    #[test]
    fn roundtrips_packed_u64_and_storage_strings_for_all_presets() {
        for preset in ShortcutConfig::presets() {
            let packed = preset.to_packed_u64();
            assert_eq!(ShortcutConfig::from_packed_u64(packed), preset);

            let stored = preset.to_storage_string();
            assert_eq!(ShortcutConfig::from_storage_string(&stored), Some(preset));
        }
    }
}
