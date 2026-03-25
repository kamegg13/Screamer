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
