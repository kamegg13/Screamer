use log::{debug, info, warn};
use pyannote_rs::{EmbeddingExtractor, EmbeddingManager};
use std::path::PathBuf;

/// A single diarization segment with speaker assignment
#[derive(Clone, Debug)]
pub struct DiarizationSegment {
    /// Start time in seconds
    pub start: f64,
    /// End time in seconds
    pub end: f64,
    /// 0-based speaker index
    pub speaker: u32,
}

/// Manages speaker diarization using pyannote-rs ONNX models
pub struct DiarizationManager {
    segmentation_model_path: PathBuf,
    embedding_model_path: PathBuf,
}

impl DiarizationManager {
    pub fn new(segmentation_path: PathBuf, embedding_path: PathBuf) -> Self {
        Self {
            segmentation_model_path: segmentation_path,
            embedding_model_path: embedding_path,
        }
    }

    /// Run diarization on audio samples (16kHz mono f32).
    ///
    /// Returns segments with speaker assignments sorted by start time.
    /// This is a CPU-bound operation and should be called from a blocking context.
    pub fn diarize(
        &self,
        samples: &[f32],
        sample_rate: u32,
        max_speakers: usize,
    ) -> Result<Vec<DiarizationSegment>, String> {
        let start_time = std::time::Instant::now();

        // pyannote-rs expects i16 samples, convert from f32
        let samples_i16: Vec<i16> = samples
            .iter()
            .map(|&s| (s * i16::MAX as f32).clamp(i16::MIN as f32, i16::MAX as f32) as i16)
            .collect();

        let segments =
            pyannote_rs::get_segments(&samples_i16, sample_rate, &self.segmentation_model_path)
                .map_err(|e| format!("Segmentation failed: {e}"))?;

        let mut extractor = EmbeddingExtractor::new(&self.embedding_model_path)
            .map_err(|e| format!("Embedding extractor init failed: {e}"))?;
        let mut manager = EmbeddingManager::new(max_speakers);

        let threshold = 0.5;
        let mut result = Vec::new();

        for segment in segments {
            match segment {
                Ok(seg) => match extractor.compute(&seg.samples) {
                    Ok(embedding) => {
                        let embedding_vec: Vec<f32> = embedding.collect();
                        let speaker = manager
                            .search_speaker(embedding_vec, threshold)
                            .unwrap_or(0);

                        result.push(DiarizationSegment {
                            start: seg.start,
                            end: seg.end,
                            speaker: speaker as u32,
                        });
                    }
                    Err(e) => {
                        warn!(
                            "Failed to compute embedding for segment [{:.2}-{:.2}]: {:?}",
                            seg.start, seg.end, e
                        );
                    }
                },
                Err(e) => {
                    warn!("Segment error: {:?}", e);
                }
            }
        }

        let unique_speakers = manager.get_all_speakers().len();
        let elapsed = start_time.elapsed();
        info!(
            "Diarization completed in {}ms: {} segments, {} speakers",
            elapsed.as_millis(),
            result.len(),
            unique_speakers
        );
        debug!("Diarization segments: {:?}", result);

        Ok(result)
    }

    /// Check if both required model files exist
    pub fn models_available(&self) -> bool {
        self.segmentation_model_path.exists() && self.embedding_model_path.exists()
    }
}

/// Format diarized text by distributing transcription text proportionally
/// across diarization segments based on time.
///
/// Since we don't have word-level timestamps from the transcription engine,
/// we distribute the transcription text proportionally across the audio duration.
/// Each diarization segment gets a portion of the text based on its time share.
pub fn format_diarized_text(
    transcription: &str,
    segments: &[DiarizationSegment],
    audio_duration_secs: f64,
) -> String {
    if segments.is_empty() || transcription.trim().is_empty() {
        return transcription.to_string();
    }

    // Check if there's only one speaker - no need for labels
    let has_multiple_speakers = segments.iter().any(|s| s.speaker != segments[0].speaker);
    if !has_multiple_speakers {
        return transcription.to_string();
    }

    let words: Vec<&str> = transcription.split_whitespace().collect();
    if words.is_empty() {
        return transcription.to_string();
    }

    let total_duration = if audio_duration_secs > 0.0 {
        audio_duration_secs
    } else {
        segments.last().map(|s| s.end).unwrap_or(1.0)
    };

    // Assign words to segments proportionally based on time
    let mut output_parts: Vec<String> = Vec::new();
    let mut word_index = 0;
    let mut prev_speaker: Option<u32> = None;
    let mut current_text = String::new();

    for seg in segments {
        let seg_duration = seg.end - seg.start;
        let proportion = seg_duration / total_duration;
        let word_count = ((proportion * words.len() as f64).round() as usize).max(1);
        let end_index = (word_index + word_count).min(words.len());

        if word_index >= words.len() {
            break;
        }

        let segment_words = &words[word_index..end_index];
        let segment_text = segment_words.join(" ");

        if prev_speaker == Some(seg.speaker) {
            // Same speaker continues, append to current text
            current_text.push(' ');
            current_text.push_str(&segment_text);
        } else {
            // New speaker - flush previous
            if let Some(prev) = prev_speaker {
                output_parts.push(format!("[Speaker {}] {}", prev + 1, current_text));
            }
            current_text = segment_text;
            prev_speaker = Some(seg.speaker);
        }

        word_index = end_index;
    }

    // Flush remaining text
    if let Some(prev) = prev_speaker {
        // Append any remaining words to the last speaker
        if word_index < words.len() {
            current_text.push(' ');
            current_text.push_str(&words[word_index..].join(" "));
        }
        output_parts.push(format!("[Speaker {}] {}", prev + 1, current_text));
    }

    output_parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_diarized_text_single_speaker() {
        let segments = vec![DiarizationSegment {
            start: 0.0,
            end: 5.0,
            speaker: 0,
        }];
        let result = format_diarized_text("Hello world how are you", &segments, 5.0);
        // Single speaker - no labels
        assert_eq!(result, "Hello world how are you");
    }

    #[test]
    fn test_format_diarized_text_two_speakers() {
        let segments = vec![
            DiarizationSegment {
                start: 0.0,
                end: 2.5,
                speaker: 0,
            },
            DiarizationSegment {
                start: 2.5,
                end: 5.0,
                speaker: 1,
            },
        ];
        let result = format_diarized_text(
            "Hello world how are you doing today fine thanks",
            &segments,
            5.0,
        );
        assert!(result.contains("[Speaker 1]"));
        assert!(result.contains("[Speaker 2]"));
    }

    #[test]
    fn test_format_diarized_text_empty() {
        let segments: Vec<DiarizationSegment> = vec![];
        let result = format_diarized_text("Hello", &segments, 5.0);
        assert_eq!(result, "Hello");
    }

    #[test]
    fn test_format_diarized_text_empty_transcription() {
        let segments = vec![DiarizationSegment {
            start: 0.0,
            end: 5.0,
            speaker: 0,
        }];
        let result = format_diarized_text("", &segments, 5.0);
        assert_eq!(result, "");
    }

    #[test]
    fn test_format_diarized_text_consecutive_same_speaker() {
        let segments = vec![
            DiarizationSegment {
                start: 0.0,
                end: 2.0,
                speaker: 0,
            },
            DiarizationSegment {
                start: 2.0,
                end: 4.0,
                speaker: 0,
            },
            DiarizationSegment {
                start: 4.0,
                end: 6.0,
                speaker: 1,
            },
        ];
        let result = format_diarized_text("one two three four five six", &segments, 6.0);
        // Speaker 0 segments should be merged
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("[Speaker 1]"));
        assert!(lines[1].starts_with("[Speaker 2]"));
    }
}
