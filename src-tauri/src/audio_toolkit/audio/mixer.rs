use std::collections::VecDeque;
use std::time::Duration;

use super::resampler::FrameResampler;

/// Accumulates audio samples from an async source and provides
/// fixed-size chunks for synchronized mixing. Resamples internally
/// if source rate differs from target rate.
pub struct AudioAccumulator {
    buffer: VecDeque<f32>,
    resampler: Option<FrameResampler>,
}

impl AudioAccumulator {
    pub fn new(source_rate: u32, target_rate: u32) -> Self {
        let resampler = if source_rate != target_rate {
            Some(FrameResampler::new(
                source_rate as usize,
                target_rate as usize,
                Duration::from_millis(30),
            ))
        } else {
            None
        };
        Self {
            buffer: VecDeque::with_capacity(target_rate as usize), // ~1s
            resampler,
        }
    }

    /// Push raw samples from the async source. Resamples to target rate if needed.
    pub fn push(&mut self, samples: &[f32]) {
        if let Some(ref mut resampler) = self.resampler {
            let buf = &mut self.buffer;
            resampler.push(samples, &mut |resampled: &[f32]| {
                buf.extend(resampled.iter());
            });
        } else {
            self.buffer.extend(samples.iter());
        }
    }

    /// Consume exactly `n` samples. Pads with silence if not enough buffered.
    /// This guarantees the output always matches the mic chunk size.
    pub fn consume(&mut self, n: usize) -> Vec<f32> {
        let available = self.buffer.len().min(n);
        let mut out: Vec<f32> = self.buffer.drain(..available).collect();
        out.resize(n, 0.0); // silence-pad if needed
        out
    }

    pub fn available(&self) -> usize {
        self.buffer.len()
    }

    /// Reset the accumulator, clearing all buffered samples.
    pub fn reset(&mut self) {
        self.buffer.clear();
    }
}

/// Audio mixer that combines microphone and system audio streams.
///
/// Both inputs must be mono f32 at the same sample rate.
/// Uses additive mixing with soft clipping (tanh) to prevent distortion.
pub struct AudioMixer {
    mic_gain: f32,
    system_gain: f32,
}

impl AudioMixer {
    pub fn new(mic_gain: f32, system_gain: f32) -> Self {
        Self {
            mic_gain,
            system_gain,
        }
    }

    /// Mix mic and system audio samples.
    ///
    /// Both slices must be mono f32. If lengths differ, the shorter one is
    /// zero-padded implicitly (we only iterate up to the longer length).
    /// Uses additive mixing with `tanh` soft clipping to keep output in [-1, 1].
    pub fn mix(&self, mic: &[f32], system: &[f32]) -> Vec<f32> {
        let len = mic.len().max(system.len());
        let mut out = Vec::with_capacity(len);

        for i in 0..len {
            let m = mic.get(i).copied().unwrap_or(0.0) * self.mic_gain;
            let s = system.get(i).copied().unwrap_or(0.0) * self.system_gain;
            let sum = m + s;
            // Soft clipping via tanh prevents harsh digital distortion
            out.push(sum.tanh());
        }

        out
    }

    /// Passthrough when system audio is disabled — applies mic gain only.
    pub fn passthrough(&self, mic: &[f32]) -> Vec<f32> {
        mic.iter().map(|&s| (s * self.mic_gain).tanh()).collect()
    }

    pub fn set_system_gain(&mut self, gain: f32) {
        self.system_gain = gain;
    }

    pub fn set_mic_gain(&mut self, gain: f32) {
        self.mic_gain = gain;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_equal_length() {
        let mixer = AudioMixer::new(1.0, 1.0);
        let mic = vec![0.5, -0.3, 0.0];
        let sys = vec![0.2, 0.3, 0.1];
        let out = mixer.mix(&mic, &sys);
        assert_eq!(out.len(), 3);
        // 0.5 + 0.2 = 0.7 → tanh(0.7) ≈ 0.604
        assert!((out[0] - (0.7_f32).tanh()).abs() < 1e-6);
        // -0.3 + 0.3 = 0.0 → tanh(0) = 0.0
        assert!((out[1]).abs() < 1e-6);
    }

    #[test]
    fn mix_different_lengths_pads_shorter() {
        let mixer = AudioMixer::new(1.0, 1.0);
        let mic = vec![0.5, 0.5];
        let sys = vec![0.1];
        let out = mixer.mix(&mic, &sys);
        assert_eq!(out.len(), 2);
        // Second sample: mic=0.5 + sys=0.0 → tanh(0.5)
        assert!((out[1] - (0.5_f32).tanh()).abs() < 1e-6);
    }

    #[test]
    fn passthrough_applies_mic_gain() {
        let mixer = AudioMixer::new(0.8, 0.5);
        let mic = vec![0.5, -0.5];
        let out = mixer.passthrough(&mic);
        assert_eq!(out.len(), 2);
        assert!((out[0] - (0.4_f32).tanh()).abs() < 1e-6);
    }

    #[test]
    fn soft_clips_loud_signals() {
        let mixer = AudioMixer::new(1.0, 1.0);
        let loud = vec![5.0];
        let out = mixer.mix(&loud, &loud);
        // 5+5=10 → tanh(10) ≈ 1.0
        assert!(out[0] > 0.99 && out[0] <= 1.0);
    }
}
