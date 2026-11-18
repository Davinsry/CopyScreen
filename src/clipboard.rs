use std::ptr::copy_nonoverlapping;
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

const CF_UNICODETEXT: u32 = 13;

/// Copies Unicode text into the Windows system clipboard.
/// Retries briefly if another process (e.g. Clipboard History) momentarily locks the clipboard.
pub fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("Tidak ada teks yang dipilih untuk disalin.".to_string());
    }

    let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = utf16.len() * std::mem::size_of::<u16>();

    unsafe {
        let mut opened = false;
        for _ in 0..8 {
            if OpenClipboard(HWND::default()).is_ok() {
                opened = true;
                break;
            }
            thread::sleep(Duration::from_millis(12));
        }

        if !opened {
            return Err("Gagal membuka Windows Clipboard (sedang dikunci aplikasi lain).".to_string());
        }

        let result = (|| -> Result<(), String> {
            EmptyClipboard().map_err(|e| format!("Gagal mengosongkan clipboard: {e}"))?;

            let hglobal: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, byte_len)
                .map_err(|e| format!("Gagal mengalokasikan memori clipboard: {e}"))?;

            let locked_ptr = GlobalLock(hglobal);
            if locked_ptr.is_null() {
                return Err("Gagal mengunci memori global clipboard.".to_string());
            }

            copy_nonoverlapping(utf16.as_ptr() as *const u8, locked_ptr as *mut u8, byte_len);
            let _ = GlobalUnlock(hglobal);

            SetClipboardData(CF_UNICODETEXT, HANDLE(hglobal.0))
                .map_err(|e| format!("Gagal menulis data ke clipboard: {e}"))?;

            Ok(())
        })();

        let _ = CloseClipboard();
        result
    }
}

/// Reads the current Unicode text from the Windows clipboard (used for verification/tests).
#[allow(dead_code)]
pub fn read_text_from_clipboard() -> Result<String, String> {
    unsafe {
        let mut opened = false;
        for _ in 0..8 {
            if OpenClipboard(HWND::default()).is_ok() {
                opened = true;
                break;
            }
            thread::sleep(Duration::from_millis(12));
        }
        if !opened {
            return Err("Gagal membuka clipboard untuk membaca.".to_string());
        }

        let res = (|| -> Result<String, String> {
            let handle = GetClipboardData(CF_UNICODETEXT)
                .map_err(|e| format!("Format teks Unicode tidak tersedia di clipboard: {e}"))?;
            let hglobal = HGLOBAL(handle.0);
            let ptr = GlobalLock(hglobal) as *const u16;
            if ptr.is_null() {
                return Err("Pointer memori clipboard kosong.".to_string());
            }

            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(ptr, len);
            let text = String::from_utf16_lossy(slice);
            let _ = GlobalUnlock(hglobal);
            Ok(text)
        })();

        let _ = CloseClipboard();
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_text() {
        assert!(copy_text_to_clipboard("").is_err());
    }

    #[test]
    fn roundtrips_unicode_text_via_clipboard() {
        let sample = "CopyScreen Test: Halo Dunia 123!";
        copy_text_to_clipboard(sample).expect("Clipboard write should succeed");
        let read_back = read_text_from_clipboard().expect("Clipboard read should succeed");
        assert_eq!(read_back, sample);
    }
}
