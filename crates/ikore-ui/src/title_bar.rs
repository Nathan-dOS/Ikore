//! The native Windows caption painted in the app's own colours.
//!
//! GPUI only turns on DWM's dark caption when the *system* theme is dark, and a system with
//! "show accent colour on title bars" paints it the accent blue — a bright bar over an app that
//! is always dark. Windows 11 lets the caption, its text and the window border take explicit
//! colours; older builds reject the attributes and keep their default, which is harmless.

use gpui::Window;

/// `0x00BBGGRR` for DWM from a `0xRRGGBB` theme colour.
#[cfg(any(target_os = "windows", test))]
fn colorref(rgb: u32) -> u32 {
    ((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff)
}

#[cfg(target_os = "windows")]
pub(crate) fn paint(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: isize,
            attribute: u32,
            value: *const std::ffi::c_void,
            size: u32,
        ) -> i32;
    }
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_CAPTION_COLOR: u32 = 35;
    const DWMWA_TEXT_COLOR: u32 = 36;

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get();
    let dark: i32 = 1;
    let attributes: [(u32, u32); 3] = [
        (DWMWA_CAPTION_COLOR, colorref(crate::theme::CANVAS_RGB)),
        (DWMWA_TEXT_COLOR, colorref(crate::theme::TEXT_MUTED_RGB)),
        (DWMWA_BORDER_COLOR, colorref(crate::theme::BORDER_RGB)),
    ];
    // SAFETY: `hwnd` is the live top-level window GPUI just created; every call passes a
    // pointer to a stack value of exactly the size DWM expects for that attribute. Failures
    // (pre-Windows 11 builds) only leave the default caption in place.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark as *const i32).cast(),
            std::mem::size_of::<i32>() as u32,
        );
        for (attribute, value) in attributes {
            DwmSetWindowAttribute(
                hwnd,
                attribute,
                (&value as *const u32).cast(),
                std::mem::size_of::<u32>() as u32,
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn paint(_window: &Window) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorref_swaps_red_and_blue() {
        assert_eq!(colorref(0x06080b), 0x000b0806);
        assert_eq!(colorref(0x123456), 0x00563412);
    }
}
