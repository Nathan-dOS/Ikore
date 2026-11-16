//! Motion policy for the shell. Every animated surface asks `reduced_motion()` first so the
//! Windows "animate controls and elements" accessibility switch turns all of it off.

use std::time::Duration;

use gpui::Animation;

/// Windows' accessibility setting, queried without changing system state. Read on every frame
/// that would animate, so flipping the switch takes effect without a restart.
pub(crate) fn reduced_motion() -> bool {
    #[cfg(target_os = "windows")]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn SystemParametersInfoW(
                action: u32,
                parameter: u32,
                value: *mut std::ffi::c_void,
                flags: u32,
            ) -> i32;
        }
        let mut enabled: i32 = 1;
        // SAFETY: SPI_GETCLIENTAREAANIMATION writes exactly one BOOL into this live stack value.
        let ok = unsafe { SystemParametersInfoW(0x1042, 0, (&mut enabled as *mut i32).cast(), 0) };
        ok != 0 && enabled == 0
    }
    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

/// A slow breathing loop for a live indicator (Home's hero dot while a stage runs). Kept off the
/// rail and the footer on purpose: a repeating animation keeps the window repainting.
pub(crate) fn pulse() -> Animation {
    Animation::new(Duration::from_millis(1_600))
        .repeat()
        .with_easing(gpui::pulsating_between(0.45, 1.0))
}
