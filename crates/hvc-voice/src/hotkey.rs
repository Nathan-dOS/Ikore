//! The push-to-talk shortcut (ADR-029 V4): a modifier combination plus one of
//! F1–F10, registered globally so it works while HVC is in the background.
use crate::VoiceError;
use std::fmt;
use std::time::Duration;

/// The modifier combinations offered. Each needs at least two keys, so the
/// shortcut does not collide with an application's plain or single-modifier
/// function keys (F5 refresh, Alt+F4 close).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifiers {
    CtrlShift,
    CtrlAlt,
    AltShift,
    CtrlAltShift,
}

impl Modifiers {
    pub const ALL: [Self; 4] = [
        Self::CtrlShift,
        Self::CtrlAlt,
        Self::AltShift,
        Self::CtrlAltShift,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::CtrlShift => "ctrl+shift",
            Self::CtrlAlt => "ctrl+alt",
            Self::AltShift => "alt+shift",
            Self::CtrlAltShift => "ctrl+alt+shift",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::CtrlShift => "Ctrl+Shift",
            Self::CtrlAlt => "Ctrl+Alt",
            Self::AltShift => "Alt+Shift",
            Self::CtrlAltShift => "Ctrl+Alt+Shift",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|modifiers| modifiers.id() == id)
    }
}

/// A modifier combination and a function key from F1 to F10.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub modifiers: Modifiers,
    function_key: u8,
}

impl Default for Hotkey {
    /// `Ctrl+Shift+F9`.
    fn default() -> Self {
        Self {
            modifiers: Modifiers::CtrlShift,
            function_key: 9,
        }
    }
}

impl Hotkey {
    pub const FUNCTION_KEYS: std::ops::RangeInclusive<u8> = 1..=10;

    pub fn new(modifiers: Modifiers, function_key: u8) -> Result<Self, VoiceError> {
        if !Self::FUNCTION_KEYS.contains(&function_key) {
            return Err(VoiceError::Hotkey(format!(
                "F{function_key} is not one of F1 to F10"
            )));
        }
        Ok(Self {
            modifiers,
            function_key,
        })
    }

    pub const fn function_key(self) -> u8 {
        self.function_key
    }

    /// The stored form, for example `ctrl+shift+f9`.
    pub fn id(self) -> String {
        format!("{}+f{}", self.modifiers.id(), self.function_key)
    }

    /// The displayed form, for example `Ctrl+Shift+F9`.
    pub fn label(self) -> String {
        format!("{}+F{}", self.modifiers.label(), self.function_key)
    }

    pub fn parse(id: &str) -> Result<Self, VoiceError> {
        let id = id.trim().to_ascii_lowercase();
        let (modifiers, key) = id
            .rsplit_once('+')
            .ok_or_else(|| VoiceError::Hotkey(format!("`{id}` has no function key")))?;
        let modifiers = Modifiers::parse(modifiers).ok_or_else(|| {
            VoiceError::Hotkey(format!(
                "`{modifiers}` is not a supported modifier combination"
            ))
        })?;
        let function_key = key
            .strip_prefix('f')
            .and_then(|number| number.parse::<u8>().ok())
            .ok_or_else(|| VoiceError::Hotkey(format!("`{key}` is not a function key")))?;
        Self::new(modifiers, function_key)
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// What the developer did with the shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushToTalkEvent {
    Pressed,
    Released { held: Duration },
}

#[cfg(windows)]
pub use windows::PushToTalk;

#[cfg(not(windows))]
/// Push-to-talk needs the Windows hotkey API; elsewhere it cannot start.
pub struct PushToTalk;

#[cfg(not(windows))]
impl PushToTalk {
    pub fn start(
        _hotkey: Hotkey,
        _on_event: impl Fn(PushToTalkEvent) + Send + 'static,
    ) -> Result<Self, VoiceError> {
        Err(VoiceError::Unsupported)
    }
}

#[cfg(windows)]
mod windows {
    use super::{Hotkey, Modifiers, PushToTalkEvent};
    use crate::VoiceError;
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey,
        UnregisterHotKey, VK_F1,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetMessageW, MSG, PostThreadMessageW, WM_HOTKEY, WM_QUIT,
    };

    const HOTKEY_ID: i32 = 0x4856; // "HV"
    /// How often the held key is sampled for its release.
    const RELEASE_POLL: Duration = Duration::from_millis(15);

    /// A registered global shortcut. `RegisterHotKey` reports only the press,
    /// so after a press the thread samples the function key until it is up.
    /// No window is created and focus never moves. Dropping it unregisters.
    pub struct PushToTalk {
        thread_id: u32,
        thread: Option<JoinHandle<()>>,
    }

    impl PushToTalk {
        pub fn start(
            hotkey: Hotkey,
            on_event: impl Fn(PushToTalkEvent) + Send + 'static,
        ) -> Result<Self, VoiceError> {
            let (ready_tx, ready_rx) = mpsc::channel();
            let thread = std::thread::Builder::new()
                .name("hvc-push-to-talk".into())
                .spawn(move || listen(hotkey, ready_tx, on_event))
                .map_err(VoiceError::Io)?;
            match ready_rx.recv() {
                Ok(Ok(thread_id)) => Ok(Self {
                    thread_id,
                    thread: Some(thread),
                }),
                Ok(Err(error)) => {
                    let _ = thread.join();
                    Err(error)
                }
                Err(_) => {
                    let _ = thread.join();
                    Err(VoiceError::Hotkey("the shortcut thread stopped".into()))
                }
            }
        }
    }

    impl Drop for PushToTalk {
        fn drop(&mut self) {
            // SAFETY: posting WM_QUIT to a thread id this value owns; the call
            // only queues a message and fails harmlessly if the thread is gone.
            unsafe {
                PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn modifier_flags(modifiers: Modifiers) -> u32 {
        let flags = match modifiers {
            Modifiers::CtrlShift => MOD_CONTROL | MOD_SHIFT,
            Modifiers::CtrlAlt => MOD_CONTROL | MOD_ALT,
            Modifiers::AltShift => MOD_ALT | MOD_SHIFT,
            Modifiers::CtrlAltShift => MOD_CONTROL | MOD_ALT | MOD_SHIFT,
        };
        flags | MOD_NOREPEAT
    }

    fn listen(
        hotkey: Hotkey,
        ready: mpsc::Sender<Result<u32, VoiceError>>,
        on_event: impl Fn(PushToTalkEvent),
    ) {
        let virtual_key = VK_F1 + u16::from(hotkey.function_key()) - 1;
        // SAFETY: a thread-level hotkey (null window) owned by this thread and
        // unregistered below before the thread returns.
        let registered = unsafe {
            RegisterHotKey(
                std::ptr::null_mut(),
                HOTKEY_ID,
                modifier_flags(hotkey.modifiers),
                u32::from(virtual_key),
            )
        };
        if registered == 0 {
            // SAFETY: reads this thread's last-error value right after the failed call.
            let code = unsafe { GetLastError() };
            let _ = ready.send(Err(VoiceError::Hotkey(format!(
                "{} is already used by another application (error {code})",
                hotkey.label()
            ))));
            return;
        }
        // SAFETY: returns the id of the current thread; no preconditions.
        let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
        // SAFETY: MSG is plain data and GetMessageW fills it before it is read.
        let mut message: MSG = unsafe { std::mem::zeroed() };
        // SAFETY: a standard message loop on the thread that owns the hotkey;
        // it ends on WM_QUIT (0) or an error (-1).
        while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
            if message.message == WM_HOTKEY {
                let pressed = Instant::now();
                on_event(PushToTalkEvent::Pressed);
                // SAFETY: GetAsyncKeyState only reads the key state; the high
                // bit is set while the key is down.
                while unsafe { GetAsyncKeyState(i32::from(virtual_key)) } as u16 & 0x8000 != 0 {
                    std::thread::sleep(RELEASE_POLL);
                }
                on_event(PushToTalkEvent::Released {
                    held: pressed.elapsed(),
                });
            }
        }
        // SAFETY: removes the hotkey this thread registered above.
        unsafe {
            UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_combination_round_trips_through_its_stored_form() {
        for modifiers in Modifiers::ALL {
            for key in Hotkey::FUNCTION_KEYS {
                let hotkey = Hotkey::new(modifiers, key).unwrap();
                assert_eq!(Hotkey::parse(&hotkey.id()).unwrap(), hotkey);
                assert!(hotkey.label().ends_with(&format!("+F{key}")));
            }
        }
        assert_eq!(Hotkey::default().id(), "ctrl+shift+f9");
        assert_eq!(Hotkey::default().label(), "Ctrl+Shift+F9");
        assert_eq!(
            Hotkey::parse(" CTRL+ALT+F2 ").unwrap(),
            Hotkey::new(Modifiers::CtrlAlt, 2).unwrap()
        );
    }

    /// Registers a real global shortcut and types it with `SendInput`. Ignored
    /// by default: it sends system-wide keystrokes to whatever has focus.
    #[cfg(windows)]
    #[test]
    #[ignore = "sends global keystrokes; run by hand on a desktop session"]
    fn push_to_talk_reports_the_press_and_the_release() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VK_CONTROL,
            VK_F10, VK_MENU, VK_SHIFT,
        };
        fn key(vk: u16, up: bool) -> INPUT {
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        wScan: 0,
                        dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        }
        fn send(inputs: &[INPUT]) {
            // SAFETY: a slice of fully initialised keyboard inputs and its length.
            unsafe {
                SendInput(
                    inputs.len() as u32,
                    inputs.as_ptr(),
                    std::mem::size_of::<INPUT>() as i32,
                );
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let hotkey = Hotkey::new(Modifiers::CtrlAltShift, 10).unwrap();
        let push_to_talk = PushToTalk::start(hotkey, move |event| {
            let _ = tx.send(event);
        })
        .unwrap();
        send(&[
            key(VK_CONTROL, false),
            key(VK_MENU, false),
            key(VK_SHIFT, false),
            key(VK_F10, false),
        ]);
        std::thread::sleep(Duration::from_millis(700));
        send(&[
            key(VK_F10, true),
            key(VK_SHIFT, true),
            key(VK_MENU, true),
            key(VK_CONTROL, true),
        ]);
        let pressed = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let released = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        drop(push_to_talk);
        assert_eq!(pressed, PushToTalkEvent::Pressed);
        let PushToTalkEvent::Released { held } = released else {
            panic!("released, got {released:?}");
        };
        assert!(held >= Duration::from_millis(600), "{held:?}");
    }

    #[test]
    fn only_two_or_more_modifiers_and_f1_to_f10_are_accepted() {
        for rejected in [
            "f9",
            "ctrl+f9",
            "shift+f9",
            "ctrl+shift+f11",
            "ctrl+shift+f0",
            "ctrl+shift+a",
            "ctrl+shift",
        ] {
            assert!(Hotkey::parse(rejected).is_err(), "{rejected}");
        }
    }
}
