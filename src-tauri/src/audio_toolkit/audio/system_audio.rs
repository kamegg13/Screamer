//! Cross-platform system audio capture abstraction.
//!
//! - **macOS**: Uses ScreenCaptureKit for audio-only capture of system output.
//! - **Windows**: Stub (WASAPI loopback — future implementation).
//! - **Linux**: Stub (PulseAudio monitor — future implementation).

use std::sync::mpsc;

/// Describes a system audio source that can be captured.
#[derive(Debug, Clone)]
pub struct SystemAudioSource {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// Errors specific to system audio capture.
#[derive(Debug)]
pub enum SystemAudioError {
    /// Platform does not support system audio capture (yet).
    UnsupportedPlatform,
    /// Permission was denied (e.g., macOS Screen Recording permission).
    PermissionDenied(String),
    /// Failed to initialize the capture pipeline.
    InitFailed(String),
    /// Capture is not currently running.
    NotRunning,
}

impl std::fmt::Display for SystemAudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                write!(f, "System audio capture not supported on this platform")
            }
            Self::PermissionDenied(msg) => write!(f, "Permission denied: {msg}"),
            Self::InitFailed(msg) => write!(f, "System audio init failed: {msg}"),
            Self::NotRunning => write!(f, "System audio capture is not running"),
        }
    }
}

impl std::error::Error for SystemAudioError {}

// ─── macOS implementation ────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use screencapturekit::prelude::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };

    /// Handler that receives audio callbacks from ScreenCaptureKit and
    /// forwards mono f32 samples over an mpsc channel.
    struct AudioHandler {
        sample_tx: Mutex<mpsc::Sender<Vec<f32>>>,
        channels: u32,
        running: Arc<AtomicBool>,
    }

    impl SCStreamOutputTrait for AudioHandler {
        fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
            if !matches!(of_type, SCStreamOutputType::Audio) {
                return;
            }
            if !self.running.load(Ordering::Relaxed) {
                return;
            }

            let audio_buffers = match sample.audio_buffer_list() {
                Some(list) => list,
                None => return,
            };

            // Extract f32 samples from all audio buffers and convert to mono
            let mut mono_samples = Vec::new();

            for buffer in &audio_buffers {
                let raw_bytes = buffer.data();
                if raw_bytes.is_empty() {
                    continue;
                }

                // ScreenCaptureKit delivers f32 PCM (kAudioFormatFlagIsFloat | kAudioFormatFlagIsNonInterleaved)
                assert!(
                    raw_bytes.as_ptr() as usize % std::mem::align_of::<f32>() == 0,
                    "Audio buffer is not f32-aligned"
                );
                assert!(
                    raw_bytes.len() % std::mem::size_of::<f32>() == 0,
                    "Audio buffer length is not a multiple of f32 size"
                );
                let float_samples: &[f32] = unsafe {
                    std::slice::from_raw_parts(
                        raw_bytes.as_ptr().cast::<f32>(),
                        raw_bytes.len() / std::mem::size_of::<f32>(),
                    )
                };

                if self.channels == 1 || audio_buffers.num_buffers() > 1 {
                    // Non-interleaved: each AudioBuffer is one channel.
                    // For mono output, we average all channel buffers.
                    if mono_samples.is_empty() {
                        mono_samples = float_samples.to_vec();
                    } else {
                        // Add to running sum for averaging
                        let len = mono_samples.len().min(float_samples.len());
                        for i in 0..len {
                            mono_samples[i] += float_samples[i];
                        }
                    }
                } else {
                    // Interleaved stereo in a single buffer
                    let channels = self.channels as usize;
                    let frame_count = float_samples.len() / channels;
                    mono_samples.reserve(frame_count);
                    for frame in float_samples.chunks_exact(channels) {
                        let mono = frame.iter().sum::<f32>() / channels as f32;
                        mono_samples.push(mono);
                    }
                }
            }

            // If we summed multiple non-interleaved buffers, average them
            let num_bufs = audio_buffers.num_buffers();
            if num_bufs > 1 {
                let divisor = num_bufs as f32;
                for sample in &mut mono_samples {
                    *sample /= divisor;
                }
            }

            if !mono_samples.is_empty() {
                if let Ok(tx) = self.sample_tx.lock() {
                    let _ = tx.send(mono_samples);
                }
            }
        }
    }

    pub struct SystemAudioCapture {
        sample_rx: Option<mpsc::Receiver<Vec<f32>>>,
        stream: Option<SCStream>,
        running: Arc<AtomicBool>,
    }

    impl SystemAudioCapture {
        /// Create a new system audio capture configured for the given sample rate.
        ///
        /// ScreenCaptureKit supports 8000, 16000, 24000, 48000 Hz.
        /// We request mono + excludes_current_process_audio to avoid feedback.
        pub fn new(target_sample_rate: u32) -> Result<Self, SystemAudioError> {
            // Validate sample rate — SCK only supports specific values
            let sck_rate = match target_sample_rate {
                8000 | 16000 | 24000 | 48000 => target_sample_rate as i32,
                _ => {
                    log::warn!(
                        "Unsupported SCK sample rate {target_sample_rate}, falling back to 48000"
                    );
                    48000
                }
            };

            // Get shareable content to find the primary display
            let content = SCShareableContent::get().map_err(|e| {
                SystemAudioError::PermissionDenied(format!(
                    "Failed to get shareable content (Screen Recording permission may be required): {e:?}"
                ))
            })?;

            let display = content.displays().into_iter().next().ok_or_else(|| {
                SystemAudioError::InitFailed("No display found for audio capture".into())
            })?;

            // Audio-only configuration: smallest possible video (1x1) to minimize overhead
            let config = SCStreamConfiguration::new()
                .with_width(1)
                .with_height(1)
                .with_captures_audio(true)
                .with_sample_rate(sck_rate)
                .with_channel_count(1) // mono
                .with_excludes_current_process_audio(true);

            let filter = SCContentFilter::create()
                .with_display(&display)
                .with_excluding_windows(&[])
                .build();

            let (sample_tx, sample_rx) = mpsc::channel::<Vec<f32>>();
            let running = Arc::new(AtomicBool::new(false));

            let handler = AudioHandler {
                sample_tx: Mutex::new(sample_tx),
                channels: 1,
                running: running.clone(),
            };

            let mut stream = SCStream::new(&filter, &config);
            stream.add_output_handler(handler, SCStreamOutputType::Audio);

            Ok(Self {
                sample_rx: Some(sample_rx),
                stream: Some(stream),
                running,
            })
        }

        pub fn start(&mut self) -> Result<(), SystemAudioError> {
            if let Some(ref mut stream) = self.stream {
                self.running.store(true, Ordering::Relaxed);
                stream.start_capture().map_err(|e| {
                    self.running.store(false, Ordering::Relaxed);
                    SystemAudioError::InitFailed(format!("Failed to start SCStream: {e:?}"))
                })?;
                log::info!("System audio capture started (ScreenCaptureKit)");
                Ok(())
            } else {
                Err(SystemAudioError::NotRunning)
            }
        }

        pub fn stop(&mut self) {
            self.running.store(false, Ordering::Relaxed);
            if let Some(ref mut stream) = self.stream {
                let _ = stream.stop_capture();
                log::info!("System audio capture stopped");
            }
        }

        /// Non-blocking receive of available system audio samples.
        pub fn try_recv_samples(&self) -> Option<Vec<f32>> {
            self.sample_rx.as_ref()?.try_recv().ok()
        }

        /// Drain all currently buffered system audio samples into a single Vec.
        pub fn drain_samples(&self) -> Vec<f32> {
            let mut all = Vec::new();
            if let Some(ref rx) = self.sample_rx {
                while let Ok(chunk) = rx.try_recv() {
                    all.extend_from_slice(&chunk);
                }
            }
            all
        }

        /// List available system audio sources.
        /// On macOS, there is typically a single "System Audio" source.
        pub fn list_sources() -> Vec<SystemAudioSource> {
            vec![SystemAudioSource {
                id: "system_default".into(),
                name: "System Audio".into(),
                is_default: true,
            }]
        }
    }

    impl Drop for SystemAudioCapture {
        fn drop(&mut self) {
            self.stop();
        }
    }
}

// ─── Windows stub ────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod platform {
    use super::*;

    pub struct SystemAudioCapture;

    impl SystemAudioCapture {
        pub fn new(_target_sample_rate: u32) -> Result<Self, SystemAudioError> {
            // TODO: Implement WASAPI loopback capture via cpal output device
            Err(SystemAudioError::UnsupportedPlatform)
        }

        pub fn start(&mut self) -> Result<(), SystemAudioError> {
            Err(SystemAudioError::UnsupportedPlatform)
        }

        pub fn stop(&mut self) {}

        pub fn try_recv_samples(&self) -> Option<Vec<f32>> {
            None
        }

        pub fn drain_samples(&self) -> Vec<f32> {
            Vec::new()
        }

        pub fn list_sources() -> Vec<SystemAudioSource> {
            Vec::new()
        }
    }
}

// ─── Linux stub ──────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    pub struct SystemAudioCapture;

    impl SystemAudioCapture {
        pub fn new(_target_sample_rate: u32) -> Result<Self, SystemAudioError> {
            // TODO: Implement PulseAudio monitor source capture
            Err(SystemAudioError::UnsupportedPlatform)
        }

        pub fn start(&mut self) -> Result<(), SystemAudioError> {
            Err(SystemAudioError::UnsupportedPlatform)
        }

        pub fn stop(&mut self) {}

        pub fn try_recv_samples(&self) -> Option<Vec<f32>> {
            None
        }

        pub fn drain_samples(&self) -> Vec<f32> {
            Vec::new()
        }

        pub fn list_sources() -> Vec<SystemAudioSource> {
            Vec::new()
        }
    }
}

// ─── Fallback for other platforms ────────────────────────────────────────────

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    use super::*;

    pub struct SystemAudioCapture;

    impl SystemAudioCapture {
        pub fn new(_target_sample_rate: u32) -> Result<Self, SystemAudioError> {
            Err(SystemAudioError::UnsupportedPlatform)
        }

        pub fn start(&mut self) -> Result<(), SystemAudioError> {
            Err(SystemAudioError::UnsupportedPlatform)
        }

        pub fn stop(&mut self) {}

        pub fn try_recv_samples(&self) -> Option<Vec<f32>> {
            None
        }

        pub fn drain_samples(&self) -> Vec<f32> {
            Vec::new()
        }

        pub fn list_sources() -> Vec<SystemAudioSource> {
            Vec::new()
        }
    }
}

pub use platform::SystemAudioCapture;
