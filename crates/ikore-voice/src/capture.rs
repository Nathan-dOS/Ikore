//! Microphone capture for push-to-talk (ADR-029 V5): the default input
//! device, 16 kHz mono 16-bit, which is what the model expects. The legacy
//! `waveIn` mapper resamples from whatever the device runs at.
use crate::VoiceError;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

pub const SAMPLE_RATE: u32 = 16_000;
/// The longest utterance kept; recording stops on its own past this.
pub const MAX_RECORDING: Duration = Duration::from_secs(120);

/// A recording in progress. `stop` returns the samples.
pub struct Recorder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<Vec<i16>, VoiceError>>>,
}

impl Recorder {
    /// Opens the default input device and starts recording. Fails when no
    /// microphone is available or the device refuses the format.
    pub fn start() -> Result<Self, VoiceError> {
        let stop = Arc::new(AtomicBool::new(false));
        let (opened_tx, opened_rx) = std::sync::mpsc::channel();
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("ikore-voice-capture".into())
            .spawn(move || platform::record(&flag, &opened_tx))
            .map_err(VoiceError::Io)?;
        match opened_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => match thread.join() {
                Ok(Err(error)) => Err(error),
                _ => Err(VoiceError::Capture("the recording thread stopped".into())),
            },
        }
    }

    pub fn stop(mut self) -> Result<Vec<i16>, VoiceError> {
        self.stop.store(true, Ordering::SeqCst);
        match self.thread.take().map(JoinHandle::join) {
            Some(Ok(result)) => result,
            _ => Err(VoiceError::Capture("the recording thread panicked".into())),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use crate::VoiceError;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::Sender;

    pub(super) fn record(
        _stop: &AtomicBool,
        opened: &Sender<Result<(), VoiceError>>,
    ) -> Result<Vec<i16>, VoiceError> {
        let _ = opened.send(Err(VoiceError::Unsupported));
        Err(VoiceError::Unsupported)
    }
}

#[cfg(windows)]
mod platform {
    use super::{MAX_RECORDING, SAMPLE_RATE};
    use crate::VoiceError;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Sender;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Media::Audio::{
        CALLBACK_NULL, HWAVEIN, WAVE_FORMAT_PCM, WAVE_MAPPER, WAVEFORMATEX, WAVEHDR, WHDR_DONE,
        waveInAddBuffer, waveInClose, waveInOpen, waveInPrepareHeader, waveInReset, waveInStart,
        waveInStop, waveInUnprepareHeader,
    };

    /// Four 100 ms buffers, recycled as the device fills them.
    const BUFFERS: usize = 4;
    const BUFFER_BYTES: usize = (SAMPLE_RATE as usize / 10) * 2;
    const POLL: Duration = Duration::from_millis(10);

    struct Device(HWAVEIN);

    impl Drop for Device {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful waveInOpen and is
            // closed once, after every buffer has been unprepared.
            unsafe {
                waveInClose(self.0);
            }
        }
    }

    fn drain(header: &WAVEHDR, samples: &mut Vec<i16>) {
        if header.dwBytesRecorded == 0 {
            return;
        }
        // SAFETY: lpData points at a buffer of BUFFER_BYTES owned by the
        // caller, and the device wrote dwBytesRecorded bytes into it.
        let bytes =
            unsafe { std::slice::from_raw_parts(header.lpData, header.dwBytesRecorded as usize) };
        samples.extend(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| i16::from_le_bytes(*pair)),
        );
    }

    pub(super) fn record(
        stop: &AtomicBool,
        opened: &Sender<Result<(), VoiceError>>,
    ) -> Result<Vec<i16>, VoiceError> {
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 1,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * 2,
            nBlockAlign: 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut handle: HWAVEIN = std::ptr::null_mut();
        // SAFETY: opens the default capture device with a format that lives
        // for the call; no callback, so the device is polled below.
        let code = unsafe { waveInOpen(&mut handle, WAVE_MAPPER, &format, 0, 0, CALLBACK_NULL) };
        if code != 0 {
            let message = format!("no microphone could be opened (waveIn error {code})");
            let _ = opened.send(Err(VoiceError::Capture(message.clone())));
            return Err(VoiceError::Capture(message));
        }
        let device = Device(handle);
        let header_size = std::mem::size_of::<WAVEHDR>() as u32;
        let mut buffers: Vec<Vec<u8>> = (0..BUFFERS).map(|_| vec![0u8; BUFFER_BYTES]).collect();
        let mut headers: Vec<WAVEHDR> = buffers
            .iter_mut()
            .map(|buffer| {
                // SAFETY: WAVEHDR is plain data; zero is its documented initial state.
                let mut header: WAVEHDR = unsafe { std::mem::zeroed() };
                header.lpData = buffer.as_mut_ptr();
                header.dwBufferLength = BUFFER_BYTES as u32;
                header
            })
            .collect();
        for header in &mut headers {
            // SAFETY: each header points at its own buffer, which outlives the
            // device: both are dropped after waveInReset and unprepare below.
            unsafe {
                waveInPrepareHeader(device.0, header, header_size);
                waveInAddBuffer(device.0, header, header_size);
            }
        }
        // SAFETY: starts the device whose buffers were queued above.
        unsafe {
            waveInStart(device.0);
        }
        let _ = opened.send(Ok(()));
        let started = Instant::now();
        let mut samples = Vec::with_capacity(SAMPLE_RATE as usize * 10);
        while !stop.load(Ordering::SeqCst) && started.elapsed() < MAX_RECORDING {
            for header in &mut headers {
                if header.dwFlags & WHDR_DONE != 0 {
                    drain(header, &mut samples);
                    header.dwFlags &= !WHDR_DONE;
                    header.dwBytesRecorded = 0;
                    // SAFETY: requeues a prepared header the device returned.
                    unsafe {
                        waveInAddBuffer(device.0, header, header_size);
                    }
                }
            }
            std::thread::sleep(POLL);
        }
        // SAFETY: stops recording and returns every queued buffer marked done.
        unsafe {
            waveInStop(device.0);
            waveInReset(device.0);
        }
        for header in &mut headers {
            if header.dwFlags & WHDR_DONE != 0 {
                drain(header, &mut samples);
            }
            // SAFETY: the device no longer owns the header after waveInReset.
            unsafe {
                waveInUnprepareHeader(device.0, header, header_size);
            }
        }
        drop(device);
        drop(buffers);
        Ok(samples)
    }
}
