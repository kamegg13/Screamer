use axum::{
    extract::{Multipart, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use log::{debug, error, info, warn};
use rodio::Source;
use rubato::{FftFixedIn, Resampler};
use serde::Serialize;
use std::io::Cursor;
use std::sync::Arc;
use tokio::sync::oneshot;
use tower_http::cors::CorsLayer;

use crate::managers::diarization::{format_diarized_text, DiarizationManager};
use crate::managers::transcription::TranscriptionManager;
use crate::settings::AppSettings;

const TARGET_SAMPLE_RATE: u32 = 16000;

/// Shared state for the API server handlers.
#[derive(Clone)]
struct AppState {
    transcription_manager: Arc<TranscriptionManager>,
    diarization_manager: Arc<DiarizationManager>,
    settings_getter: Arc<dyn Fn() -> AppSettings + Send + Sync>,
}

/// The local API server that exposes an OpenAI-compatible transcription endpoint.
pub struct ApiServer {
    port: u16,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl ApiServer {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            shutdown_tx: None,
        }
    }

    /// Start the API server on a background tokio task.
    ///
    /// `settings_getter` is a closure that returns the current AppSettings
    /// (called per-request so settings changes are picked up live).
    pub fn start(
        &mut self,
        tm: Arc<TranscriptionManager>,
        dm: Arc<DiarizationManager>,
        settings_getter: impl Fn() -> AppSettings + Send + Sync + 'static,
    ) {
        if self.shutdown_tx.is_some() {
            warn!("API server is already running, ignoring start request");
            return;
        }

        let (tx, rx) = oneshot::channel::<()>();
        self.shutdown_tx = Some(tx);

        let port = self.port;
        let state = AppState {
            transcription_manager: tm,
            diarization_manager: dm,
            settings_getter: Arc::new(settings_getter),
        };

        tokio::spawn(async move {
            let app = Router::new()
                .route("/v1/audio/transcriptions", post(transcribe_handler))
                .layer(CorsLayer::permissive())
                .with_state(state);

            let addr = format!("0.0.0.0:{port}");
            let listener = match tokio::net::TcpListener::bind(&addr).await {
                Ok(l) => l,
                Err(e) => {
                    error!("Failed to bind API server to {addr}: {e}");
                    return;
                }
            };

            info!("Local API server listening on {addr}");

            let graceful = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = rx.await;
                info!("API server received shutdown signal");
            });

            if let Err(e) = graceful.await {
                error!("API server error: {e}");
            }

            info!("API server stopped");
        });
    }

    /// Stop the running API server gracefully.
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
            info!("API server shutdown signal sent");
        }
    }

    /// Returns true if the server is currently running.
    pub fn is_running(&self) -> bool {
        self.shutdown_tx.is_some()
    }

    /// Update the port. If the server is running, it will be restarted.
    pub fn set_port(&mut self, port: u16) {
        self.port = port;
    }
}

// ---------- Response / Error types ----------

#[derive(Serialize)]
struct TranscriptionResponse {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    diarized_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speakers: Option<Vec<SpeakerInfo>>,
}

#[derive(Serialize)]
struct SpeakerInfo {
    id: u32,
    segments: Vec<SpeakerSegment>,
}

#[derive(Serialize)]
struct SpeakerSegment {
    start: f64,
    end: f64,
    text: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

fn error_response(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<ErrorResponse>) {
    let msg = msg.into();
    warn!("API error ({}): {}", status, msg);
    (status, Json(ErrorResponse { error: msg }))
}

// ---------- Audio decoding & resampling helpers ----------

/// Decode any audio format supported by rodio into interleaved f32 samples.
/// Returns (samples, sample_rate, channels).
fn decode_audio(bytes: &[u8]) -> Result<(Vec<f32>, u32, u16), String> {
    let cursor = Cursor::new(bytes.to_vec());
    let decoder =
        rodio::Decoder::new(cursor).map_err(|e| format!("Failed to decode audio: {e}"))?;

    let sample_rate = decoder.sample_rate();
    let channels = decoder.channels();
    let samples: Vec<f32> = decoder.map(|s| s as f32 / i16::MAX as f32).collect();

    if samples.is_empty() {
        return Err("Decoded audio is empty".to_string());
    }

    Ok((samples, sample_rate, channels))
}

/// Convert interleaved multi-channel audio to mono by averaging channels.
fn to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels == 1 {
        return samples.to_vec();
    }

    let ch = channels as usize;
    samples
        .chunks_exact(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect()
}

/// Resample mono f32 audio from `in_hz` to `out_hz`.
fn resample(samples: &[f32], in_hz: u32, out_hz: u32) -> Result<Vec<f32>, String> {
    if in_hz == out_hz {
        return Ok(samples.to_vec());
    }

    let chunk_size = 1024usize;
    let mut resampler = FftFixedIn::<f32>::new(in_hz as usize, out_hz as usize, chunk_size, 1, 1)
        .map_err(|e| format!("Failed to create resampler: {e}"))?;

    let mut output = Vec::with_capacity(
        (samples.len() as f64 * out_hz as f64 / in_hz as f64) as usize + chunk_size,
    );
    let mut pos = 0;

    while pos + chunk_size <= samples.len() {
        let chunk = &samples[pos..pos + chunk_size];
        match resampler.process(&[chunk], None) {
            Ok(out) => output.extend_from_slice(&out[0]),
            Err(e) => return Err(format!("Resampling failed: {e}")),
        }
        pos += chunk_size;
    }

    // Handle remaining samples by zero-padding
    if pos < samples.len() {
        let mut last_chunk = vec![0.0f32; chunk_size];
        let remaining = samples.len() - pos;
        last_chunk[..remaining].copy_from_slice(&samples[pos..]);
        match resampler.process(&[&last_chunk], None) {
            Ok(out) => {
                // Only take the proportional amount of output
                let expected = (remaining as f64 * out_hz as f64 / in_hz as f64).ceil() as usize;
                let take = expected.min(out[0].len());
                output.extend_from_slice(&out[0][..take]);
            }
            Err(e) => return Err(format!("Resampling tail failed: {e}")),
        }
    }

    Ok(output)
}

/// Full pipeline: bytes -> decode -> mono -> resample to 16kHz -> f32 samples
fn process_audio(bytes: &[u8]) -> Result<Vec<f32>, String> {
    let (samples, sample_rate, channels) = decode_audio(bytes)?;
    debug!(
        "Decoded audio: {} samples, {}Hz, {} channels",
        samples.len(),
        sample_rate,
        channels
    );

    let mono = to_mono(&samples, channels);
    let resampled = resample(&mono, sample_rate, TARGET_SAMPLE_RATE)?;
    debug!(
        "Processed audio: {} samples at {}Hz",
        resampled.len(),
        TARGET_SAMPLE_RATE
    );

    Ok(resampled)
}

// ---------- Handler ----------

async fn transcribe_handler(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<TranscriptionResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut audio_bytes: Option<Vec<u8>> = None;
    let mut diarization_requested = false;
    let mut _response_format = "json".to_string();

    // Parse multipart fields
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                audio_bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| {
                            error_response(
                                StatusCode::BAD_REQUEST,
                                format!("Failed to read file field: {e}"),
                            )
                        })?
                        .to_vec(),
                );
            }
            "diarization" => {
                let val = field.text().await.unwrap_or_default();
                diarization_requested = val == "true" || val == "1";
            }
            "response_format" => {
                _response_format = field.text().await.unwrap_or_else(|_| "json".to_string());
            }
            _ => {
                // Ignore unknown fields
                debug!("Ignoring unknown multipart field: {name}");
            }
        }
    }

    let audio_bytes = audio_bytes
        .ok_or_else(|| error_response(StatusCode::BAD_REQUEST, "Missing 'file' field"))?;

    if audio_bytes.is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Audio file is empty",
        ));
    }

    debug!(
        "Received audio: {} bytes, diarization={}",
        audio_bytes.len(),
        diarization_requested
    );

    // Decode and resample audio (CPU-bound, run on blocking thread)
    let samples = tokio::task::spawn_blocking(move || process_audio(&audio_bytes))
        .await
        .map_err(|e| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Audio processing task failed: {e}"),
            )
        })?
        .map_err(|e| error_response(StatusCode::BAD_REQUEST, e))?;

    if samples.is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "No audio samples after processing",
        ));
    }

    // Transcribe (CPU-bound)
    let tm = state.transcription_manager.clone();
    let samples_for_transcription = samples.clone();
    let text = tokio::task::spawn_blocking(move || tm.transcribe(samples_for_transcription))
        .await
        .map_err(|e| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Transcription task failed: {e}"),
            )
        })?
        .map_err(|e| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Transcription failed: {e}"),
            )
        })?;

    // Optionally run diarization
    let (diarized_text, speakers) = if diarization_requested {
        let settings = (state.settings_getter)();
        let dm = state.diarization_manager.clone();
        let max_speakers = settings.diarization_max_speakers as usize;
        let samples_for_diarization = samples.clone();
        let audio_duration = samples.len() as f64 / TARGET_SAMPLE_RATE as f64;

        let diarization_result = tokio::task::spawn_blocking(move || {
            dm.diarize(&samples_for_diarization, TARGET_SAMPLE_RATE, max_speakers)
        })
        .await
        .map_err(|e| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Diarization task failed: {e}"),
            )
        })?;

        match diarization_result {
            Ok(segments) => {
                let diarized = format_diarized_text(&text, &segments, audio_duration);

                // Build speaker info grouped by speaker ID
                let mut speaker_map: std::collections::HashMap<u32, Vec<SpeakerSegment>> =
                    std::collections::HashMap::new();
                for seg in &segments {
                    speaker_map
                        .entry(seg.speaker)
                        .or_default()
                        .push(SpeakerSegment {
                            start: seg.start,
                            end: seg.end,
                            text: String::new(), // segment-level text not available without word timestamps
                        });
                }

                let mut speaker_infos: Vec<SpeakerInfo> = speaker_map
                    .into_iter()
                    .map(|(id, segments)| SpeakerInfo { id, segments })
                    .collect();
                speaker_infos.sort_by_key(|s| s.id);

                (Some(diarized), Some(speaker_infos))
            }
            Err(e) => {
                warn!("Diarization failed (returning transcription only): {e}");
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    info!(
        "API transcription complete: {} chars{}",
        text.len(),
        if diarized_text.is_some() {
            " (with diarization)"
        } else {
            ""
        }
    );

    Ok(Json(TranscriptionResponse {
        text,
        diarized_text,
        speakers,
    }))
}
