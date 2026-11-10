// diarization/fbank.rs
//
// Kaldi-compatible 80-dim log-mel filterbank features for the WeSpeaker
// speaker-embedding model. Matches kaldi-native-fbank defaults with
// dither disabled (inference): 25ms povey-windowed frames, 10ms shift,
// pre-emphasis 0.97, DC removal, snip_edges, 512-point FFT, mel range
// 20Hz..Nyquist, natural log. Cepstral mean normalization (CMN) is applied
// over time, matching the WeSpeaker inference recipe.

use realfft::RealFftPlanner;

pub const SAMPLE_RATE: usize = 16_000;
pub const NUM_MEL_BINS: usize = 80;

const FRAME_LENGTH: usize = 400; // 25ms @ 16kHz
const FRAME_SHIFT: usize = 160; // 10ms @ 16kHz
const FFT_SIZE: usize = 512;
const PREEMPHASIS: f32 = 0.97;
const LOW_FREQ: f32 = 20.0;

#[inline]
fn mel(freq: f32) -> f32 {
    1127.0 * (1.0 + freq / 700.0).ln()
}

/// Triangular mel filterbank weights over FFT power-spectrum bins,
/// computed in the mel domain exactly like Kaldi's MelBanks.
fn mel_filterbank() -> Vec<Vec<(usize, f32)>> {
    let nyquist = SAMPLE_RATE as f32 / 2.0;
    let num_fft_bins = FFT_SIZE / 2; // Kaldi ignores the Nyquist bin
    let fft_bin_width = SAMPLE_RATE as f32 / FFT_SIZE as f32;

    let mel_low = mel(LOW_FREQ);
    let mel_high = mel(nyquist);
    let mel_delta = (mel_high - mel_low) / (NUM_MEL_BINS + 1) as f32;

    (0..NUM_MEL_BINS)
        .map(|bin| {
            let left = mel_low + bin as f32 * mel_delta;
            let center = mel_low + (bin + 1) as f32 * mel_delta;
            let right = mel_low + (bin + 2) as f32 * mel_delta;

            let mut weights = Vec::new();
            for fft_bin in 0..num_fft_bins {
                let freq_mel = mel(fft_bin_width * fft_bin as f32);
                if freq_mel > left && freq_mel < right {
                    let weight = if freq_mel <= center {
                        (freq_mel - left) / (center - left)
                    } else {
                        (right - freq_mel) / (right - center)
                    };
                    weights.push((fft_bin, weight));
                }
            }
            weights
        })
        .collect()
}

fn povey_window() -> Vec<f32> {
    (0..FRAME_LENGTH)
        .map(|n| {
            let hann = 0.5
                - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / (FRAME_LENGTH - 1) as f32).cos();
            hann.powf(0.85)
        })
        .collect()
}

pub struct FbankComputer {
    window: Vec<f32>,
    filterbank: Vec<Vec<(usize, f32)>>,
    fft: std::sync::Arc<dyn realfft::RealToComplex<f32>>,
}

impl FbankComputer {
    pub fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        Self {
            window: povey_window(),
            filterbank: mel_filterbank(),
            fft: planner.plan_fft_forward(FFT_SIZE),
        }
    }

    /// Compute CMN-normalized log-mel features from normalized audio samples.
    /// Returns (num_frames, NUM_MEL_BINS) row-major; empty if audio is shorter
    /// than one frame.
    pub fn compute(&self, samples: &[f32]) -> Vec<[f32; NUM_MEL_BINS]> {
        if samples.len() < FRAME_LENGTH {
            return Vec::new();
        }
        let num_frames = 1 + (samples.len() - FRAME_LENGTH) / FRAME_SHIFT;

        let mut frame = vec![0.0f32; FFT_SIZE];
        let mut spectrum = self.fft.make_output_vec();
        let mut features = Vec::with_capacity(num_frames);

        for f in 0..num_frames {
            let start = f * FRAME_SHIFT;
            // The exported WeSpeaker model expects features computed at
            // PCM16 scale. At normalized scale, quiet mel energies hit the
            // log-energy floor and lose information before CMN can remove
            // the constant scale offset.
            for (output, sample) in frame[..FRAME_LENGTH]
                .iter_mut()
                .zip(&samples[start..start + FRAME_LENGTH])
            {
                *output = sample * 32768.0;
            }
            frame[FRAME_LENGTH..].fill(0.0);

            // Remove DC offset
            let mean = frame[..FRAME_LENGTH].iter().sum::<f32>() / FRAME_LENGTH as f32;
            for s in &mut frame[..FRAME_LENGTH] {
                *s -= mean;
            }

            // Pre-emphasis (in-place, back to front, Kaldi semantics)
            for i in (1..FRAME_LENGTH).rev() {
                frame[i] -= PREEMPHASIS * frame[i - 1];
            }
            frame[0] -= PREEMPHASIS * frame[0];

            for (s, w) in frame[..FRAME_LENGTH].iter_mut().zip(&self.window) {
                *s *= w;
            }

            // realfft requires a fresh scratch-free call; process ignores prior content
            if self.fft.process(&mut frame, &mut spectrum).is_err() {
                continue;
            }

            let mut mel_energies = [0.0f32; NUM_MEL_BINS];
            for (bin, weights) in self.filterbank.iter().enumerate() {
                let mut energy = 0.0f32;
                for &(fft_bin, weight) in weights {
                    energy += weight * spectrum[fft_bin].norm_sqr();
                }
                mel_energies[bin] = energy.max(f32::EPSILON).ln();
            }
            features.push(mel_energies);
        }

        // Cepstral mean normalization over time (WeSpeaker recipe)
        if !features.is_empty() {
            let mut means = [0.0f32; NUM_MEL_BINS];
            for frame in &features {
                for (m, v) in means.iter_mut().zip(frame.iter()) {
                    *m += v;
                }
            }
            let n = features.len() as f32;
            for m in &mut means {
                *m /= n;
            }
            for frame in &mut features {
                for (v, m) in frame.iter_mut().zip(means.iter()) {
                    *v -= m;
                }
            }
        }

        features
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_count_matches_snip_edges() {
        let computer = FbankComputer::new();
        // 1 second of a 440Hz tone
        let samples: Vec<f32> = (0..SAMPLE_RATE)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / SAMPLE_RATE as f32).sin())
            .collect();
        let feats = computer.compute(&samples);
        assert_eq!(feats.len(), 1 + (SAMPLE_RATE - FRAME_LENGTH) / FRAME_SHIFT);
    }

    #[test]
    fn short_audio_returns_empty() {
        let computer = FbankComputer::new();
        assert!(computer.compute(&[0.0; 100]).is_empty());
    }

    #[test]
    fn cmn_zero_means() {
        let computer = FbankComputer::new();
        let samples: Vec<f32> = (0..SAMPLE_RATE)
            .map(|i| (0.3 * i as f32).sin() * 0.5)
            .collect();
        let feats = computer.compute(&samples);
        for bin in 0..NUM_MEL_BINS {
            let mean: f32 = feats.iter().map(|f| f[bin]).sum::<f32>() / feats.len() as f32;
            assert!(mean.abs() < 1e-3, "bin {} mean {}", bin, mean);
        }
    }
}

#[cfg(test)]
mod reference_tests {
    use super::*;

    #[test]
    fn features_match_kaldi_pcm16_reference_across_signal_levels() {
        let mut state = 1u32;
        let audio: Vec<f32> = (0..16000)
            .map(|index| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                let pcm = (state >> 16) as u16 as i16;
                let amplitude = if index < 5333 {
                    1.0
                } else if index < 10666 {
                    0.001
                } else {
                    0.000001
                };
                pcm as f32 / 32768.0 * amplitude
            })
            .collect();
        // kaldi-native-fbank 1.22.4: 80 bins, dither=0, snip_edges=true,
        // Povey window, PCM16-scale input, followed by per-bin global CMN.
        let expected: [[f32; 6]; 5] = [
            [12.62087, 12.43986, 13.12594, 13.83922, 13.67427, 14.03211],
            [13.50927, 12.74745, 10.87887, 14.22611, 13.93155, 12.98238],
            [0.67554, -0.74461, -0.40820, -0.24904, -0.09458, -0.88456],
            [-12.92466, -15.09246, -12.98428, -14.41079, -13.00979, -14.10608],
            [-14.78739, -14.43381, -13.21029, -13.21656, -13.48486, -14.63280],
        ];
        let features = FbankComputer::new().compute(&audio);
        assert_eq!(features.len(), 98);
        for (row, frame) in [0, 20, 50, 80, 97].into_iter().enumerate() {
            for (column, bin) in [0, 7, 20, 40, 60, 79].into_iter().enumerate() {
                assert!(
                    (features[frame][bin] - expected[row][column]).abs() < 0.002,
                    "frame {frame}, bin {bin}: {} vs {}",
                    features[frame][bin],
                    expected[row][column]
                );
            }
        }
    }
}
