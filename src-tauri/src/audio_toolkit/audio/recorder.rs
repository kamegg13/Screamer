use std::{
    io::Error,
    panic::{self, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
        Arc, Mutex,
    },
    time::Duration,
};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, Sample, SizedSample,
};

use crate::audio_toolkit::{
    audio::{
        mixer::{AudioAccumulator, AudioMixer},
        system_audio::SystemAudioCapture,
        AudioVisualiser, FrameResampler,
    },
    constants,
    vad::{self, VadFrame},
    VoiceActivityDetector,
};

enum Cmd {
    Start,
    Stop(mpsc::Sender<Vec<f32>>),
    Shutdown,
}

pub struct AudioRecorder {
    device: Option<Device>,
    cmd_tx: Option<mpsc::Sender<Cmd>>,
    worker_handle: Option<std::thread::JoinHandle<()>>,
    vad: Option<Arc<Mutex<Box<dyn vad::VoiceActivityDetector>>>>,
    level_cb: Option<Arc<dyn Fn(Vec<f32>) + Send + Sync + 'static>>,
    pause_flag: Option<Arc<AtomicBool>>,
    system_audio: Option<SystemAudioCapture>,
    mixer: Option<AudioMixer>,
}

impl AudioRecorder {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(AudioRecorder {
            device: None,
            cmd_tx: None,
            worker_handle: None,
            vad: None,
            level_cb: None,
            pause_flag: None,
            system_audio: None,
            mixer: None,
        })
    }

    pub fn with_vad(mut self, vad: Box<dyn VoiceActivityDetector>) -> Self {
        self.vad = Some(Arc::new(Mutex::new(vad)));
        self
    }

    pub fn with_level_callback<F>(mut self, cb: F) -> Self
    where
        F: Fn(Vec<f32>) + Send + Sync + 'static,
    {
        self.level_cb = Some(Arc::new(cb));
        self
    }

    pub fn with_pause_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.pause_flag = Some(flag);
        self
    }

    pub fn with_system_audio(mut self, capture: SystemAudioCapture, mixer: AudioMixer) -> Self {
        self.system_audio = Some(capture);
        self.mixer = Some(mixer);
        self
    }

    pub fn open(&mut self, device: Option<Device>) -> Result<(), Box<dyn std::error::Error>> {
        if self.worker_handle.is_some() {
            return Ok(()); // already open
        }

        let (sample_tx, sample_rx) = mpsc::channel::<Vec<f32>>();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();

        let host = crate::audio_toolkit::get_cpal_host();
        let device = match device {
            Some(dev) => dev,
            None => host
                .default_input_device()
                .ok_or_else(|| Error::new(std::io::ErrorKind::NotFound, "No input device found"))?,
        };

        let thread_device = device.clone();
        let vad = self.vad.clone();
        // Move the optional level callback into the worker thread
        let level_cb = self.level_cb.clone();
        let pause_flag = self.pause_flag.clone();

        // Move system audio + mixer into the worker thread
        let mut system_audio = self.system_audio.take();
        let mixer = self.mixer.take();
        let sys_audio_rate = system_audio.as_ref().map(|sa| sa.sample_rate());

        // Start system audio capture before spawning the worker
        if let Some(ref mut sa) = system_audio {
            if let Err(e) = sa.start() {
                log::warn!("System audio capture failed to start, continuing with mic only: {e}");
                system_audio = None;
            }
        }

        let worker = std::thread::spawn(move || {
            let worker_result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
                let config = AudioRecorder::get_preferred_config(&thread_device)
                    .map_err(|err| format!("failed to fetch preferred config: {err}"))?;

                let sample_rate = config.sample_rate().0;
                let channels = config.channels() as usize;

                log::info!(
                    "Using device: {:?}\nSample rate: {}\nChannels: {}\nFormat: {:?}",
                    thread_device.name(),
                    sample_rate,
                    channels,
                    config.sample_format()
                );

                let stream = match config.sample_format() {
                    cpal::SampleFormat::U8 => AudioRecorder::build_stream::<u8>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                    ),
                    cpal::SampleFormat::I8 => AudioRecorder::build_stream::<i8>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                    ),
                    cpal::SampleFormat::I16 => AudioRecorder::build_stream::<i16>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                    ),
                    cpal::SampleFormat::I32 => AudioRecorder::build_stream::<i32>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                    ),
                    cpal::SampleFormat::F32 => AudioRecorder::build_stream::<f32>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                    ),
                    other => {
                        return Err(format!("unsupported sample format: {other:?}"));
                    }
                }
                .map_err(|err| format!("failed to build input stream: {err}"))?;

                stream
                    .play()
                    .map_err(|err| format!("failed to start input stream: {err}"))?;

                // Keep the stream alive while we process samples.
                run_consumer(
                    sample_rate,
                    vad,
                    sample_rx,
                    cmd_rx,
                    level_cb,
                    pause_flag,
                    system_audio,
                    mixer,
                    sys_audio_rate,
                );
                Ok(())
            }));

            match worker_result {
                Ok(Ok(())) => {
                    log::debug!("Audio recorder worker exited cleanly");
                }
                Ok(Err(err)) => {
                    log::error!("Audio recorder worker exited with error: {err}");
                }
                Err(panic_payload) => {
                    let panic_message = if let Some(message) = panic_payload.downcast_ref::<&str>()
                    {
                        (*message).to_string()
                    } else if let Some(message) = panic_payload.downcast_ref::<String>() {
                        message.clone()
                    } else {
                        "non-string panic payload".to_string()
                    };
                    log::error!("Audio recorder worker panicked: {panic_message}");
                }
            }
        });

        self.device = Some(device);
        self.cmd_tx = Some(cmd_tx);
        self.worker_handle = Some(worker);

        Ok(())
    }

    pub fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(tx) = &self.cmd_tx {
            tx.send(Cmd::Start)?;
        }
        Ok(())
    }

    pub fn stop(&self) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        let (resp_tx, resp_rx) = mpsc::channel();
        if let Some(tx) = &self.cmd_tx {
            tx.send(Cmd::Stop(resp_tx))?;
        }
        Ok(resp_rx.recv()?) // wait for the samples
    }

    pub fn close(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(Cmd::Shutdown);
        }
        if let Some(h) = self.worker_handle.take() {
            let _ = h.join();
        }
        self.device = None;
        Ok(())
    }

    fn build_stream<T>(
        device: &cpal::Device,
        config: &cpal::SupportedStreamConfig,
        sample_tx: mpsc::Sender<Vec<f32>>,
        channels: usize,
    ) -> Result<cpal::Stream, cpal::BuildStreamError>
    where
        T: Sample + SizedSample + Send + 'static,
        f32: cpal::FromSample<T>,
    {
        let mut output_buffer = Vec::new();

        let stream_cb = move |data: &[T], _: &cpal::InputCallbackInfo| {
            output_buffer.clear();

            if channels == 1 {
                // Direct conversion without intermediate Vec
                output_buffer.extend(data.iter().map(|&sample| sample.to_sample::<f32>()));
            } else {
                // Convert to mono directly
                let frame_count = data.len() / channels;
                output_buffer.reserve(frame_count);

                for frame in data.chunks_exact(channels) {
                    let mono_sample = frame
                        .iter()
                        .map(|&sample| sample.to_sample::<f32>())
                        .sum::<f32>()
                        / channels as f32;
                    output_buffer.push(mono_sample);
                }
            }

            if sample_tx.send(output_buffer.clone()).is_err() {
                log::error!("Failed to send samples");
            }
        };

        device.build_input_stream(
            &config.clone().into(),
            stream_cb,
            |err| log::error!("Stream error: {}", err),
            None,
        )
    }

    pub(crate) fn get_preferred_config(
        device: &cpal::Device,
    ) -> Result<cpal::SupportedStreamConfig, Box<dyn std::error::Error>> {
        let supported_configs = device.supported_input_configs()?;
        let mut best_config: Option<cpal::SupportedStreamConfigRange> = None;

        // Try to find a config that supports 16kHz, prioritizing better formats
        for config_range in supported_configs {
            if config_range.min_sample_rate().0 <= constants::WHISPER_SAMPLE_RATE
                && config_range.max_sample_rate().0 >= constants::WHISPER_SAMPLE_RATE
            {
                match best_config {
                    None => best_config = Some(config_range),
                    Some(ref current) => {
                        // Prioritize F32 > I16 > I32 > others
                        let score = |fmt: cpal::SampleFormat| match fmt {
                            cpal::SampleFormat::F32 => 4,
                            cpal::SampleFormat::I16 => 3,
                            cpal::SampleFormat::I32 => 2,
                            _ => 1,
                        };

                        if score(config_range.sample_format()) > score(current.sample_format()) {
                            best_config = Some(config_range);
                        }
                    }
                }
            }
        }

        if let Some(config) = best_config {
            return Ok(config.with_sample_rate(cpal::SampleRate(constants::WHISPER_SAMPLE_RATE)));
        }

        // If no config supports 16kHz, fall back to default
        Ok(device.default_input_config()?)
    }
}

fn run_consumer(
    in_sample_rate: u32,
    vad: Option<Arc<Mutex<Box<dyn vad::VoiceActivityDetector>>>>,
    sample_rx: mpsc::Receiver<Vec<f32>>,
    cmd_rx: mpsc::Receiver<Cmd>,
    level_cb: Option<Arc<dyn Fn(Vec<f32>) + Send + Sync + 'static>>,
    pause_flag: Option<Arc<AtomicBool>>,
    system_audio: Option<SystemAudioCapture>,
    mixer: Option<AudioMixer>,
    system_audio_sample_rate: Option<u32>,
) {
    let mut frame_resampler = FrameResampler::new(
        in_sample_rate as usize,
        constants::WHISPER_SAMPLE_RATE as usize,
        Duration::from_millis(30),
    );

    // Real-time mixing: accumulator resamples system audio to mic rate so we
    // can mix sample-by-sample before the shared resampler → VAD pipeline.
    let mut sys_accumulator = if let Some(rate) = system_audio_sample_rate {
        Some(AudioAccumulator::new(rate, in_sample_rate))
    } else {
        None
    };

    log::info!(
        "run_consumer: mic_sample_rate={}, system_audio_present={}, system_audio_rate={:?}, mixer_present={}",
        in_sample_rate,
        system_audio.is_some(),
        system_audio_sample_rate,
        mixer.is_some()
    );

    let mut processed_samples = Vec::<f32>::new();
    let mut recording = false;

    // ---------- spectrum visualisation setup ---------------------------- //
    const BUCKETS: usize = 16;
    const WINDOW_SIZE: usize = 512;
    let mut visualizer = AudioVisualiser::new(
        in_sample_rate,
        WINDOW_SIZE,
        BUCKETS,
        400.0,  // vocal_min_hz
        4000.0, // vocal_max_hz
    );

    fn handle_frame(
        samples: &[f32],
        recording: bool,
        vad: &Option<Arc<Mutex<Box<dyn vad::VoiceActivityDetector>>>>,
        out_buf: &mut Vec<f32>,
    ) {
        if !recording {
            return;
        }

        if let Some(vad_arc) = vad {
            let mut det = vad_arc.lock().unwrap();
            match det.push_frame(samples).unwrap_or(VadFrame::Speech(samples)) {
                VadFrame::Speech(buf) => out_buf.extend_from_slice(buf),
                VadFrame::Noise => {}
            }
        } else {
            out_buf.extend_from_slice(samples);
        }
    }

    /// Handle a Stop command: drain remaining audio, flush resampler, return samples.
    fn handle_stop(
        sample_rx: &mpsc::Receiver<Vec<f32>>,
        frame_resampler: &mut FrameResampler,
        vad: &Option<Arc<Mutex<Box<dyn vad::VoiceActivityDetector>>>>,
        processed_samples: &mut Vec<f32>,
        system_audio: &Option<SystemAudioCapture>,
        sys_accumulator: &mut Option<AudioAccumulator>,
        mixer: &Option<AudioMixer>,
        reply_tx: mpsc::Sender<Vec<f32>>,
    ) {
        // Drain remaining mic samples and mix in real-time
        while let Ok(remaining) = sample_rx.try_recv() {
            // Drain system audio into accumulator
            if let (Some(ref sa), Some(ref mut acc)) = (system_audio, sys_accumulator.as_mut()) {
                let sys_samples = sa.drain_samples();
                if !sys_samples.is_empty() {
                    acc.push(&sys_samples);
                }
            }

            // Mix mic + system audio in real-time
            let mixed = if let (Some(ref mut acc), Some(ref m)) = (sys_accumulator.as_mut(), mixer)
            {
                let sys_chunk = acc.consume(remaining.len());
                m.mix(&remaining, &sys_chunk)
            } else {
                remaining
            };

            frame_resampler.push(&mixed, &mut |frame: &[f32]| {
                handle_frame(frame, true, vad, processed_samples)
            });
        }
        frame_resampler
            .finish(&mut |frame: &[f32]| handle_frame(frame, true, vad, processed_samples));

        let _ = reply_tx.send(std::mem::take(processed_samples));
    }

    loop {
        // --- process pending commands ----------------------------------- //
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Cmd::Start => {
                    processed_samples.clear();
                    if let Some(ref mut acc) = sys_accumulator {
                        acc.reset();
                    }
                    recording = true;
                    visualizer.reset();
                    if let Some(v) = &vad {
                        v.lock().unwrap().reset();
                    }
                }
                Cmd::Stop(reply_tx) => {
                    recording = false;
                    handle_stop(
                        &sample_rx,
                        &mut frame_resampler,
                        &vad,
                        &mut processed_samples,
                        &system_audio,
                        &mut sys_accumulator,
                        &mixer,
                        reply_tx,
                    );
                }
                Cmd::Shutdown => return,
            }
        }

        let raw = match sample_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(s) => s,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };

        // ---------- spectrum processing ---------------------------------- //
        if let Some(buckets) = visualizer.feed(&raw) {
            if let Some(cb) = &level_cb {
                cb(buckets);
            }
        }

        // ---------- real-time mix + resampler + VAD pipeline ------------- //
        let is_paused = pause_flag
            .as_ref()
            .map_or(false, |f| f.load(Ordering::Relaxed));
        if !is_paused {
            // 1. Drain all pending system audio into the accumulator
            if let (Some(ref sa), Some(ref mut acc)) = (&system_audio, &mut sys_accumulator) {
                let sys_samples = sa.drain_samples();
                if !sys_samples.is_empty() {
                    acc.push(&sys_samples);

                    // Log periodically for diagnostics
                    static DRAIN_LOG_COUNTER: std::sync::atomic::AtomicU64 =
                        std::sync::atomic::AtomicU64::new(0);
                    let count =
                        DRAIN_LOG_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if count % 500 == 0 {
                        log::info!(
                            "SysAudio[{}]: drained {} samples, accumulator={} samples, recording={}",
                            count,
                            sys_samples.len(),
                            acc.available(),
                            recording,
                        );
                    }
                }
            }

            // 2. Mix mic + system audio in real-time (equal-length, silence-padded)
            let mixed = if let (Some(ref mut acc), Some(ref m)) = (&mut sys_accumulator, &mixer) {
                let sys_chunk = acc.consume(raw.len());
                m.mix(&raw, &sys_chunk)
            } else {
                raw
            };

            // 3. Continue with existing pipeline: resampler → VAD → output buffer
            frame_resampler.push(&mixed, &mut |frame: &[f32]| {
                handle_frame(frame, recording, &vad, &mut processed_samples)
            });
        }

        // non-blocking check for a command
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Cmd::Start => {
                    processed_samples.clear();
                    if let Some(ref mut acc) = sys_accumulator {
                        acc.reset();
                    }
                    recording = true;
                    visualizer.reset();
                    if let Some(v) = &vad {
                        v.lock().unwrap().reset();
                    }
                }
                Cmd::Stop(reply_tx) => {
                    recording = false;
                    handle_stop(
                        &sample_rx,
                        &mut frame_resampler,
                        &vad,
                        &mut processed_samples,
                        &system_audio,
                        &mut sys_accumulator,
                        &mixer,
                        reply_tx,
                    );
                }
                Cmd::Shutdown => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{run_consumer, Cmd};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn shutdown_command_exits_without_waiting_for_samples() {
        let (sample_tx, sample_rx) = mpsc::channel::<Vec<f32>>();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();

        let worker = std::thread::spawn(move || {
            run_consumer(
                16_000, None, sample_rx, cmd_rx, None, None, None, None, None,
            );
        });

        cmd_tx.send(Cmd::Shutdown).expect("send shutdown");

        let (joined_tx, joined_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = worker.join();
            let _ = joined_tx.send(());
        });

        joined_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker should exit after shutdown");

        drop(sample_tx);
    }
}
