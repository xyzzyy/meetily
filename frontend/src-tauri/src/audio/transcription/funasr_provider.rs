use super::provider::{TranscriptResult, TranscriptionError, TranscriptionProvider};
use async_trait::async_trait;
use std::sync::Arc;

/// One 30 ms frame at 16 kHz, the unit used to find a quiet split point.
const FRAME_SAMPLES: usize = 480;
/// Never leave less than this after a split, so no piece is a fragment.
const MIN_PIECE_SAMPLES: usize = 16_000;

/// Split audio longer than one helper request into contiguous pieces.
///
/// Each cut is placed at the quietest 30 ms frame in the back half of the
/// allowed window, where a pause between words is most likely. The pieces
/// cover the input exactly, so no audio is dropped or sent twice.
pub(crate) fn request_ranges(audio: &[f32], max_samples: usize) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while audio.len() - start > max_samples {
        let window_end = (start + max_samples).min(audio.len() - MIN_PIECE_SAMPLES);
        let window_start = (start + max_samples / 2).min(window_end);
        let cut = (window_start..window_end.saturating_sub(FRAME_SAMPLES).max(window_start + 1))
            .step_by(FRAME_SAMPLES)
            .min_by(|&a, &b| {
                let energy = |at: usize| -> f32 {
                    audio[at..(at + FRAME_SAMPLES).min(audio.len())].iter().map(|v| v * v).sum()
                };
                energy(a).total_cmp(&energy(b))
            })
            .unwrap_or(window_end);
        ranges.push(start..cut);
        start = cut;
    }
    ranges.push(start..audio.len());
    ranges
}

pub struct FunAsrProvider {
    engine: Arc<crate::funasr_engine::FunAsrEngine>,
}

impl FunAsrProvider {
    pub fn new(engine: Arc<crate::funasr_engine::FunAsrEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl TranscriptionProvider for FunAsrProvider {
    async fn transcribe(
        &self,
        audio: Vec<f32>,
        language: Option<String>,
    ) -> std::result::Result<TranscriptResult, TranscriptionError> {
        if audio.len() < 1600 {
            return Err(TranscriptionError::AudioTooShort {
                samples: audio.len(),
                minimum: 1600,
            });
        }
        let requested_language = language.unwrap_or_else(|| "auto".to_string());
        let language = crate::funasr_engine::normalize_language(Some(&requested_language))
            .map_err(|_| TranscriptionError::UnsupportedLanguage(requested_language))?;
        // The helper takes at most 30 s per request, but live speech segments
        // and imported audio can be longer.
        let mut pieces = Vec::new();
        for range in request_ranges(&audio, crate::funasr_engine::MAX_AUDIO_SAMPLES) {
            let text = self
                .engine
                .transcribe(audio[range].to_vec(), language)
                .await
                .map_err(|error| TranscriptionError::EngineFailed(error.to_string()))?;
            let text = text.trim();
            if !text.is_empty() {
                pieces.push(text.to_string());
            }
        }
        Ok(TranscriptResult {
            text: pieces.join(" "),
            confidence: None,
            is_partial: false,
        })
    }

    async fn is_model_loaded(&self) -> bool {
        self.engine.is_model_loaded().await
    }

    async fn get_current_model(&self) -> Option<String> {
        self.engine.get_current_model().await
    }

    fn provider_name(&self) -> &'static str {
        "FUN-ASR"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(samples: usize) -> Vec<f32> {
        (0..samples).map(|i| ((i as f32) * 0.05).sin() * 0.5).collect()
    }

    fn assert_contiguous(ranges: &[std::ops::Range<usize>], len: usize, max: usize) {
        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end, len);
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "pieces must not gap or overlap");
        }
        for range in ranges {
            assert!(range.len() <= max, "piece of {} exceeds {}", range.len(), max);
            assert!(range.len() > 0);
        }
    }

    #[test]
    fn short_audio_is_sent_whole() {
        let audio = tone(10 * 16_000);
        assert_eq!(request_ranges(&audio, 30 * 16_000), vec![0..audio.len()]);
    }

    #[test]
    fn long_audio_is_split_into_contiguous_pieces_within_the_limit() {
        let max = 30 * 16_000;
        for seconds in [31, 45, 87, 90, 301] {
            let audio = tone(seconds * 16_000 + 123);
            let ranges = request_ranges(&audio, max);
            assert_contiguous(&ranges, audio.len(), max);
            assert!(ranges.iter().all(|range| range.len() >= MIN_PIECE_SAMPLES), "{seconds}s left a fragment");
        }
    }

    #[test]
    fn splits_land_in_a_pause_when_there_is_one() {
        let max = 30 * 16_000;
        let mut audio = tone(40 * 16_000);
        // A half-second pause at 22 s, in the back half of the first window.
        let pause = 22 * 16_000;
        audio[pause..pause + 8_000].iter_mut().for_each(|v| *v = 0.0);
        let ranges = request_ranges(&audio, max);
        assert_eq!(ranges.len(), 2);
        let cut = ranges[0].end;
        assert!((pause..pause + 8_000).contains(&cut), "cut at {cut}, pause at {pause}");
    }
}
