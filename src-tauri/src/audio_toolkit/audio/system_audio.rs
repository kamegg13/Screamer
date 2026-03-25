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
        actual_sample_rate: u32,
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
                actual_sample_rate: sck_rate as u32,
            })
        }

        /// Returns the actual sample rate used by ScreenCaptureKit.
        ///
        /// This may differ from the requested rate if SCK doesn't support
        /// the target rate and fell back to 48000 Hz.
        pub fn sample_rate(&self) -> u32 {
            self.actual_sample_rate
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
