use anyhow::Result;
use rubato::{Resampler, SincFixedIn};
use std::collections::VecDeque;

/// Keep partial input blocks and drain the filter tail at stop.
pub(super) struct CaptureResampler {
    resampler: SincFixedIn<f32>,
    pending: VecDeque<f32>,
    input_rate: u32,
    output_rate: u32,
    input_frames: u64,
    output_frames: u64,
    finished: bool,
}

impl CaptureResampler {
    pub(super) fn new(resampler: SincFixedIn<f32>, input_rate: u32, output_rate: u32) -> Self {
        Self {
            resampler,
            pending: VecDeque::new(),
            input_rate,
            output_rate,
            input_frames: 0,
            output_frames: 0,
            finished: false,
        }
    }

    pub(super) fn process(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.input_frames += input.len() as u64;
        self.pending.extend(input);
        let mut output = Vec::new();
        while self.pending.len() >= self.resampler.input_frames_next() {
            let block: Vec<f32> = self
                .pending
                .drain(..self.resampler.input_frames_next())
                .collect();
            output.extend(self.process_block(block)?);
        }
        self.output_frames += output.len() as u64;
        Ok(output)
    }

    pub(super) fn finish(&mut self) -> Result<Vec<f32>> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        // Round up to retain the last input sample. Padding must not extend the recording.
        let expected =
            (self.input_frames * self.output_rate as u64).div_ceil(self.input_rate as u64);
        let remaining = expected.saturating_sub(self.output_frames) as usize;
        let mut output = Vec::new();
        while output.len() < remaining {
            let mut block: Vec<f32> = self.pending.drain(..).collect();
            block.resize(self.resampler.input_frames_next(), 0.0);
            output.extend(self.process_block(block)?);
        }
        output.truncate(remaining);
        self.output_frames += output.len() as u64;
        Ok(output)
    }

    fn process_block(&mut self, block: Vec<f32>) -> Result<Vec<f32>> {
        let mut channels = self.resampler.process(&[block], None)?;
        Ok(channels.pop().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rubato::{SincInterpolationParameters, SincInterpolationType, WindowFunction};

    fn resampler(rate: u32) -> CaptureResampler {
        let params = SincInterpolationParameters {
            sinc_len: 512,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Cubic,
            oversampling_factor: 512,
            window: WindowFunction::BlackmanHarris2,
        };
        CaptureResampler::new(
            SincFixedIn::new(48000.0 / rate as f64, 2.0, params, 512, 1).unwrap(),
            rate,
            48000,
        )
    }

    #[test]
    fn drain_preserves_final_marker_and_exact_duration() {
        for rate in [8000, 16000, 44100, 96000, 192000] {
            for length in [1, 127, 511, 512, 513, 1024, 1457, 4096] {
                let mut input = vec![0.0; length];
                let marker_start = length.saturating_sub(80);
                input[marker_start..].fill(0.5);
                let mut resampler = resampler(rate);
                let mut output = Vec::new();
                for chunk in input.chunks(137) {
                    output.extend(resampler.process(chunk).unwrap());
                }
                output.extend(resampler.finish().unwrap());
                let expected = (length * 48000).div_ceil(rate as usize);
                assert_eq!(output.len(), expected, "rate={rate}, length={length}");
                let marker_output = marker_start * 48000 / rate as usize;
                assert!(
                    output[marker_output..]
                        .iter()
                        .any(|sample| sample.abs() > 0.01),
                    "Final marker lost: rate={rate}, length={length}"
                );
                assert!(output.iter().all(|sample| sample.is_finite()));
                assert!(resampler.finish().unwrap().is_empty());
                assert!(resampler.process(&input).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn block_boundaries_do_not_change_audio() {
        for rate in [16000, 44100, 96000] {
            let input: Vec<f32> = (0..5000).map(|i| (i as f32 * 0.08).sin() * 0.3).collect();
            let mut whole = resampler(rate);
            let mut expected = whole.process(&input).unwrap();
            expected.extend(whole.finish().unwrap());
            for chunk_size in [1, 320, 512, 1024] {
                let mut chunked = resampler(rate);
                let mut actual = Vec::new();
                for chunk in input.chunks(chunk_size) {
                    actual.extend(chunked.process(chunk).unwrap());
                }
                actual.extend(chunked.finish().unwrap());
                assert_eq!(actual, expected, "rate={rate}, chunk size={chunk_size}");
            }
        }
    }

    #[test]
    fn empty_capture_has_no_padding() {
        let mut resampler = resampler(44100);
        assert!(resampler.finish().unwrap().is_empty());
    }
}
