//! Cross-platform system audio capture abstraction.
//!
//! - **macOS**: Uses ScreenCaptureKit for audio-only capture of system output.
//! - **Windows**: Uses WASAPI loopback via cpal (build_input_stream on output device).
//! - **Linux**: Uses PulseAudio monitor sources via cpal.

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
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    };

    /// The native sample rate ScreenCaptureKit reliably delivers audio at.
    /// Requesting other rates (e.g. 16kHz) can cause SCK to silently produce
    /// no audio on some hardware. Always capture at 48kHz and let the caller
    /// resample downstream if needed.
    const SCK_NATIVE_SAMPLE_RATE: u32 = 48000;

    /// The native channel count for ScreenCaptureKit audio capture.
    /// SCK may not support mono output directly; always request stereo and
    /// down-mix to mono in the callback.
    const SCK_NATIVE_CHANNELS: u32 = 2;

    /// Handler that receives audio callbacks from ScreenCaptureKit and
    /// forwards mono f32 samples over an mpsc channel.
    struct AudioHandler {
        sample_tx: Mutex<mpsc::Sender<Vec<f32>>>,
        running: Arc<AtomicBool>,
        /// Tracks whether we have received the first valid audio buffer.
        /// Early buffers from SCK can be empty or contain warmup artifacts.
        warmed_up: AtomicBool,
        /// Total number of buffers received from SCK (for periodic logging).
        buffer_count: AtomicU64,
        /// Total number of mono samples sent downstream.
        total_samples: AtomicU64,
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
                None => {
                    log::warn!("SCK: audio_buffer_list() returned None");
                    return;
                }
            };

            // Parse audio data using safe f32::from_le_bytes (no unsafe pointer casts).
            // SCK delivers 32-bit float PCM in little-endian byte order.
            let raw_buffer_count = audio_buffers.iter().count();
            let buffers: Vec<Vec<f32>> = audio_buffers
                .iter()
                .enumerate()
                .map(|(i, buffer)| {
                    let data = buffer.data();
                    let samples: Vec<f32> = data
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                        .collect();
                    let count = self.buffer_count.load(Ordering::Relaxed);
                    if count < 3 {
                        log::debug!(
                            "SCK: raw audio_buffer[{i}]: raw_bytes={}, parsed_samples={}",
                            data.len(),
                            samples.len()
                        );
                    }
                    samples
                })
                .filter(|samples| !samples.is_empty())
                .collect();

            if buffers.is_empty() {
                log::warn!(
                    "SCK: no valid samples in buffer (raw_buffer_count={})",
                    raw_buffer_count
                );
                return;
            }

            let prev_count = self.buffer_count.fetch_add(1, Ordering::Relaxed);

            // Warmup detection: skip until we get the first non-empty buffer
            if !self.warmed_up.load(Ordering::Relaxed) {
                self.warmed_up.store(true, Ordering::Relaxed);
                log::info!(
                    "SCK: First system audio buffer received! raw_buffers={}, samples_per_buffer={:?}",
                    buffers.len(),
                    buffers.iter().map(|b| b.len()).collect::<Vec<_>>()
                );
            }

            // Convert to mono by handling both planar and interleaved layouts.
            let mono_samples = if buffers.len() >= 2 {
                // Planar layout: each buffer is a separate channel.
                // Average corresponding samples across all channels.
                let frame_count = buffers.iter().map(|b| b.len()).min().unwrap_or(0);
                let num_channels = buffers.len() as f32;
                (0..frame_count)
                    .map(|i| {
                        let sum: f32 = buffers.iter().map(|ch| ch[i]).sum();
                        sum / num_channels
                    })
                    .collect::<Vec<f32>>()
            } else {
                // Single buffer with interleaved channels.
                // De-interleave and average to mono.
                let data = &buffers[0];
                let channels = SCK_NATIVE_CHANNELS as usize;
                data.chunks_exact(channels)
                    .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                    .collect::<Vec<f32>>()
            };

            if !mono_samples.is_empty() {
                let mono_len = mono_samples.len() as u64;
                let new_total =
                    self.total_samples.fetch_add(mono_len, Ordering::Relaxed) + mono_len;

                // Log every 100th buffer with RMS for signal level diagnostics
                if (prev_count + 1) % 100 == 0 {
                    let rms = (mono_samples.iter().map(|s| s * s).sum::<f32>()
                        / mono_samples.len() as f32)
                        .sqrt();
                    log::info!(
                        "SCK: received {} buffers, {} total samples, rms={:.6}",
                        prev_count + 1,
                        new_total,
                        rms
                    );
                }

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
        actual_sample_rate: u32,
    }

    impl SystemAudioCapture {
        /// Create a new system audio capture.
        ///
        /// `target_sample_rate` is accepted for API compatibility but ignored:
        /// ScreenCaptureKit always captures at 48kHz stereo (its native format).
        /// The caller is responsible for resampling downstream if needed.
        pub fn new(_target_sample_rate: u32) -> Result<Self, SystemAudioError> {
            // Get shareable content to find the primary display
            let content = SCShareableContent::get().map_err(|e| {
                SystemAudioError::PermissionDenied(format!(
                    "Failed to get shareable content (Screen Recording permission may be required): {e:?}"
                ))
            })?;

            let displays = content.displays();
            log::info!("SCK: found {} display(s)", displays.len());
            let display = displays.into_iter().next().ok_or_else(|| {
                SystemAudioError::InitFailed("No display found for audio capture".into())
            })?;
            log::info!(
                "SCK: using display width={}, height={}",
                display.width(),
                display.height()
            );

            // Use real display dimensions and native audio settings.
            // Using 1x1 video and non-native audio rates/channels can cause
            // SCK to silently fail to deliver audio buffers.
            let config = SCStreamConfiguration::new()
                .with_width(display.width())
                .with_height(display.height())
                .with_captures_audio(true)
                .with_sample_rate(SCK_NATIVE_SAMPLE_RATE as i32)
                .with_channel_count(SCK_NATIVE_CHANNELS as i32);
            // NOTE: Do NOT call .with_excludes_current_process_audio() —
            // it can interfere with audio delivery on some macOS versions.

            let filter = SCContentFilter::create()
                .with_display(&display)
                .with_excluding_windows(&[])
                .build();

            let (sample_tx, sample_rx) = mpsc::channel::<Vec<f32>>();
            let running = Arc::new(AtomicBool::new(false));

            let handler = AudioHandler {
                sample_tx: Mutex::new(sample_tx),
                running: running.clone(),
                warmed_up: AtomicBool::new(false),
                buffer_count: AtomicU64::new(0),
                total_samples: AtomicU64::new(0),
            };

            let mut stream = SCStream::new(&filter, &config);
            stream.add_output_handler(handler, SCStreamOutputType::Audio);
            log::info!("SCK: SCStream created successfully, audio output handler added");

            log::info!(
                "SCK capture configured: {}x{}, {}Hz, {} channels",
                display.width(),
                display.height(),
                SCK_NATIVE_SAMPLE_RATE,
                SCK_NATIVE_CHANNELS,
            );

            Ok(Self {
                sample_rx: Some(sample_rx),
                stream: Some(stream),
                running,
                actual_sample_rate: SCK_NATIVE_SAMPLE_RATE,
            })
        }

        /// Returns the actual sample rate used by ScreenCaptureKit (always 48000 Hz).
        pub fn sample_rate(&self) -> u32 {
            self.actual_sample_rate
        }

        pub fn start(&mut self) -> Result<(), SystemAudioError> {
            if let Some(ref mut stream) = self.stream {
                log::info!("SCK: Starting SCStream capture...");
                self.running.store(true, Ordering::Relaxed);
                stream.start_capture().map_err(|e| {
                    self.running.store(false, Ordering::Relaxed);
                    log::error!("SCK: start_capture() FAILED: {e:?}");
                    SystemAudioError::InitFailed(format!("Failed to start SCStream: {e:?}"))
                })?;
                log::info!("SCK: start_capture() succeeded — system audio capture started");
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
            let samples = self.sample_rx.as_ref()?.try_recv().ok();
            if let Some(ref s) = samples {
                log::trace!("SCK: try_recv_samples got {} samples", s.len());
            }
            samples
        }

        /// Drain all currently buffered system audio samples into a single Vec.
        pub fn drain_samples(&self) -> Vec<f32> {
            let mut all = Vec::new();
            if let Some(ref rx) = self.sample_rx {
                while let Ok(chunk) = rx.try_recv() {
                    all.extend_from_slice(&chunk);
                }
            }
            if !all.is_empty() {
                log::debug!("SCK: drain_samples returning {} samples", all.len());
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

// ─── Windows implementation (WASAPI loopback via cpal) ──────────────────────

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::Stream;

    pub struct SystemAudioCapture {
        sample_rx: Option<mpsc::Receiver<Vec<f32>>>,
        /// The default output device used for loopback capture.
        device: cpal::Device,
        /// The native sample rate of the output device.
        device_sample_rate: u32,
        /// The sample rate requested by the caller (informational — no resampling here).
        _target_sample_rate: u32,
        /// The active loopback stream (present while capturing).
        stream: Option<Stream>,
        /// Sender side kept alive so the channel doesn't close prematurely.
        sample_tx: Option<mpsc::Sender<Vec<f32>>>,
    }

    impl SystemAudioCapture {
        /// Create a new system audio capture.
        ///
        /// On Windows, cpal's WASAPI backend automatically enables loopback when
        /// `build_input_stream()` is called on a render (output) device.
        pub fn new(target_sample_rate: u32) -> Result<Self, SystemAudioError> {
            let host = cpal::default_host();
            let device = host.default_output_device().ok_or_else(|| {
                SystemAudioError::InitFailed("No default output device found".into())
            })?;

            let config = device.default_output_config().map_err(|e| {
                SystemAudioError::InitFailed(format!("Failed to get default output config: {e}"))
            })?;

            let device_sample_rate = config.sample_rate().0;
            log::info!(
                "Windows loopback: output device rate={device_sample_rate}, target={target_sample_rate}"
            );

            let (sample_tx, sample_rx) = mpsc::channel::<Vec<f32>>();

            Ok(Self {
                sample_rx: Some(sample_rx),
                device,
                device_sample_rate,
                _target_sample_rate: target_sample_rate,
                stream: None,
                sample_tx: Some(sample_tx),
            })
        }

        /// Returns the actual sample rate of the loopback capture device.
        pub fn sample_rate(&self) -> u32 {
            self.device_sample_rate
        }

        pub fn start(&mut self) -> Result<(), SystemAudioError> {
            if self.stream.is_some() {
                // Already running
                return Ok(());
            }

            let tx = self.sample_tx.clone().ok_or(SystemAudioError::NotRunning)?;
            let channels = self
                .device
                .default_output_config()
                .map_err(|e| {
                    SystemAudioError::InitFailed(format!("Failed to query output config: {e}"))
                })?
                .channels() as usize;

            let config = cpal::StreamConfig {
                channels: channels as u16,
                sample_rate: cpal::SampleRate(self.device_sample_rate),
                buffer_size: cpal::BufferSize::Default,
            };

            // Build an *input* stream on the *output* device — WASAPI treats this as loopback.
            let stream = self
                .device
                .build_input_stream(
                    &config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        if data.is_empty() {
                            return;
                        }
                        let mono = if channels == 1 {
                            data.to_vec()
                        } else {
                            data.chunks_exact(channels)
                                .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                                .collect()
                        };
                        let _ = tx.send(mono);
                    },
                    |err| {
                        log::error!("WASAPI loopback stream error: {err}");
                    },
                    None, // no timeout
                )
                .map_err(|e| {
                    SystemAudioError::InitFailed(format!(
                        "Failed to build WASAPI loopback stream: {e}"
                    ))
                })?;

            stream.play().map_err(|e| {
                SystemAudioError::InitFailed(format!("Failed to start loopback stream: {e}"))
            })?;

            self.stream = Some(stream);
            log::info!("System audio capture started (WASAPI loopback)");
            Ok(())
        }

        pub fn stop(&mut self) {
            if let Some(stream) = self.stream.take() {
                drop(stream);
                log::info!("System audio capture stopped (WASAPI loopback)");
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

        /// List available system audio sources (output devices usable for loopback).
        pub fn list_sources() -> Vec<SystemAudioSource> {
            let host = cpal::default_host();
            let default_name = host.default_output_device().and_then(|d| d.name().ok());

            host.output_devices()
                .map(|devices| {
                    devices
                        .filter_map(|d| {
                            let name = d.name().ok()?;
                            let is_default = default_name.as_deref() == Some(name.as_str());
                            Some(SystemAudioSource {
                                id: name.clone(),
                                name,
                                is_default,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    impl Drop for SystemAudioCapture {
        fn drop(&mut self) {
            self.stop();
        }
    }
}

// ─── Linux implementation (PulseAudio monitor via cpal) ─────────────────────

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::Stream;

    /// Check if a device name looks like a PulseAudio monitor source.
    fn is_monitor_device(name: &str) -> bool {
        let lower = name.to_lowercase();
        lower.contains("monitor")
    }

    pub struct SystemAudioCapture {
        sample_rx: Option<mpsc::Receiver<Vec<f32>>>,
        /// The monitor input device used for capture.
        device: cpal::Device,
        /// The native sample rate of the monitor device.
        device_sample_rate: u32,
        /// The sample rate requested by the caller (informational — no resampling here).
        _target_sample_rate: u32,
        /// The active capture stream (present while capturing).
        stream: Option<Stream>,
        /// Sender side kept alive so the channel doesn't close prematurely.
        sample_tx: Option<mpsc::Sender<Vec<f32>>>,
    }

    impl SystemAudioCapture {
        /// Create a new system audio capture using a PulseAudio monitor source.
        ///
        /// We enumerate input devices and pick the first one whose name contains
        /// "Monitor" — this is the standard PulseAudio convention for loopback
        /// sources that mirror an output sink.
        pub fn new(target_sample_rate: u32) -> Result<Self, SystemAudioError> {
            // Use the default host; on Linux with PulseAudio installed, cpal will
            // use the PulseAudio backend when available, falling back to ALSA.
            let host = cpal::default_host();

            let device = host
                .input_devices()
                .map_err(|e| {
                    SystemAudioError::InitFailed(format!("Failed to enumerate input devices: {e}"))
                })?
                .find(|d| d.name().map(|n| is_monitor_device(&n)).unwrap_or(false))
                .ok_or_else(|| {
                    SystemAudioError::InitFailed(
                        "No PulseAudio monitor source found. Ensure PulseAudio is running \
                         and a monitor device is available (pactl list sources | grep Monitor)"
                            .into(),
                    )
                })?;

            let device_name = device.name().unwrap_or_else(|_| "unknown".into());
            let config = device.default_input_config().map_err(|e| {
                SystemAudioError::InitFailed(format!(
                    "Failed to get default config for monitor device '{device_name}': {e}"
                ))
            })?;

            let device_sample_rate = config.sample_rate().0;
            log::info!(
                "Linux monitor: device='{device_name}' rate={device_sample_rate}, target={target_sample_rate}"
            );

            let (sample_tx, sample_rx) = mpsc::channel::<Vec<f32>>();

            Ok(Self {
                sample_rx: Some(sample_rx),
                device,
                device_sample_rate,
                _target_sample_rate: target_sample_rate,
                stream: None,
                sample_tx: Some(sample_tx),
            })
        }

        /// Returns the actual sample rate of the monitor capture device.
        pub fn sample_rate(&self) -> u32 {
            self.device_sample_rate
        }

        pub fn start(&mut self) -> Result<(), SystemAudioError> {
            if self.stream.is_some() {
                return Ok(());
            }

            let tx = self.sample_tx.clone().ok_or(SystemAudioError::NotRunning)?;
            let channels = self
                .device
                .default_input_config()
                .map_err(|e| {
                    SystemAudioError::InitFailed(format!("Failed to query input config: {e}"))
                })?
                .channels() as usize;

            let config = cpal::StreamConfig {
                channels: channels as u16,
                sample_rate: cpal::SampleRate(self.device_sample_rate),
                buffer_size: cpal::BufferSize::Default,
            };

            let stream = self
                .device
                .build_input_stream(
                    &config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        if data.is_empty() {
                            return;
                        }
                        let mono = if channels == 1 {
                            data.to_vec()
                        } else {
                            data.chunks_exact(channels)
                                .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                                .collect()
                        };
                        let _ = tx.send(mono);
                    },
                    |err| {
                        log::error!("PulseAudio monitor stream error: {err}");
                    },
                    None,
                )
                .map_err(|e| {
                    SystemAudioError::InitFailed(format!(
                        "Failed to build PulseAudio monitor stream: {e}"
                    ))
                })?;

            stream.play().map_err(|e| {
                SystemAudioError::InitFailed(format!("Failed to start monitor stream: {e}"))
            })?;

            self.stream = Some(stream);
            log::info!("System audio capture started (PulseAudio monitor)");
            Ok(())
        }

        pub fn stop(&mut self) {
            if let Some(stream) = self.stream.take() {
                drop(stream);
                log::info!("System audio capture stopped (PulseAudio monitor)");
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

        /// List available system audio sources (PulseAudio monitor devices).
        pub fn list_sources() -> Vec<SystemAudioSource> {
            let host = cpal::default_host();

            host.input_devices()
                .map(|devices| {
                    devices
                        .filter_map(|d| {
                            let name = d.name().ok()?;
                            if !is_monitor_device(&name) {
                                return None;
                            }
                            Some(SystemAudioSource {
                                id: name.clone(),
                                name,
                                is_default: false,
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .map(|mut sources| {
                    // Mark the first monitor source as default
                    if let Some(first) = sources.first_mut() {
                        first.is_default = true;
                    }
                    sources
                })
                .unwrap_or_default()
        }
    }

    impl Drop for SystemAudioCapture {
        fn drop(&mut self) {
            self.stop();
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

        pub fn sample_rate(&self) -> u32 {
            0
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
