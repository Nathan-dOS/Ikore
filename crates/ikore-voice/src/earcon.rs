//! Earcons (ADR-029 V6): short tones that tell the developer what happened
//! without a window. Generated once in memory and played asynchronously.
use crate::wav;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Earcon {
    /// Key down: recording.
    Start,
    /// The transcript is stored as a note.
    Saved,
    /// Nothing was stored: no speech, no project, setup missing or a failure.
    Failed,
}

const RATE: u32 = 22_050;

/// One sine tone with a 10 ms fade at both ends, so it does not click.
fn tone(frequency: f32, millis: u32) -> Vec<i16> {
    let count = RATE * millis / 1000;
    let fade = RATE as f32 * 0.01;
    (0..count)
        .map(|index| {
            let t = index as f32 / RATE as f32;
            let edge = (count - index).min(index) as f32;
            let envelope = (edge / fade).min(1.0);
            let value = (t * frequency * std::f32::consts::TAU).sin() * 0.22 * envelope;
            (value * f32::from(i16::MAX)) as i16
        })
        .collect()
}

fn render(earcon: Earcon) -> Vec<u8> {
    let samples = match earcon {
        Earcon::Start => tone(880.0, 90),
        Earcon::Saved => [tone(988.0, 70), tone(1_319.0, 110)].concat(),
        Earcon::Failed => [tone(330.0, 120), tone(247.0, 160)].concat(),
    };
    wav::encode_mono_16(&samples, RATE)
}

/// The WAV bytes of an earcon; stable for the life of the process, as the
/// asynchronous player requires.
pub fn bytes(earcon: Earcon) -> &'static [u8] {
    static RENDERED: OnceLock<[Vec<u8>; 3]> = OnceLock::new();
    let rendered = RENDERED.get_or_init(|| {
        [
            render(Earcon::Start),
            render(Earcon::Saved),
            render(Earcon::Failed),
        ]
    });
    match earcon {
        Earcon::Start => &rendered[0],
        Earcon::Saved => &rendered[1],
        Earcon::Failed => &rendered[2],
    }
}

/// Plays without waiting. A new earcon replaces one still playing.
#[cfg(windows)]
pub fn play(earcon: Earcon) {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
    let bytes = bytes(earcon);
    // SAFETY: SND_MEMORY reads a complete WAV image from `bytes`, which is
    // 'static, so it outlives the asynchronous playback.
    unsafe {
        PlaySoundW(
            bytes.as_ptr().cast(),
            std::ptr::null_mut(),
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}

#[cfg(not(windows))]
pub fn play(_earcon: Earcon) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earcons_are_short_distinct_wav_images() {
        let lengths = [Earcon::Start, Earcon::Saved, Earcon::Failed].map(|earcon| {
            let wav = bytes(earcon);
            assert_eq!(&wav[0..4], b"RIFF");
            wav.len()
        });
        // 90 ms, 180 ms and 280 ms at 22 050 Hz, 16-bit, plus the header.
        assert_eq!(lengths, [44 + 1_984 * 2, 44 + 3_968 * 2, 44 + 6_174 * 2]);
        assert!(std::ptr::eq(bytes(Earcon::Saved), bytes(Earcon::Saved)));
    }
}
