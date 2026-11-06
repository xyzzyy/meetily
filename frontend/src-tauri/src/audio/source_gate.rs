//! Keep each capture source in the live transcription input only while that
//! source has its own speech.
//!
//! Live transcription detects speech on the microphone + system mix. A headset
//! microphone can carry steady background noise near speech level for minutes
//! while the user is muted in the meeting app. In the mix, that noise hid the
//! other speakers from the speech detector: on one recorded meeting it kept 67%
//! of remote speech, and as little as 28% in some minutes. The same detector run
//! on the system source alone kept 94-97% of remote speech in all eight
//! recordings checked.
//!
//! Each source therefore gets its own detector. A source's audio enters the
//! transcription mix only inside that source's speech, including the detector's
//! padding. When both sources have speech (double talk, quick replies, a
//! microphone that hears the speakers), both stay in the mix, as before. The
//! saved recording and diarization stems are not gated.

use super::vad::{ContinuousVadProcessor, SpeechSegment};
use anyhow::Result;
use log::warn;
use std::collections::VecDeque;

/// How long the transcription input waits for each source's speech decision.
/// The live detector confirms speech once 250 ms of it have passed and starts
/// the segment 300 ms before the first speech frame, so a segment start is known
/// about 600 ms after its first sample. The extra 100 ms covers the detector's
/// 30 ms frame buffering.
pub(crate) const SOURCE_GATE_DELAY_MS: u32 = 700;

/// Speech intervals of one source, in samples at the pipeline rate.
struct SourceGate {
    detector: ContinuousVadProcessor,
    sample_rate: u32,
    /// Ended speech, in time order.
    spans: VecDeque<(usize, usize)>,
    /// Start of speech that has not ended yet.
    open_since: Option<usize>,
    /// After a detector error the source stays in the mix, as without gating.
    failed: bool,
    name: &'static str,
}

impl SourceGate {
    fn new(sample_rate: u32, redemption_ms: u32, name: &'static str) -> Result<Self> {
        Ok(Self {
            detector: ContinuousVadProcessor::new(sample_rate, redemption_ms)?,
            sample_rate,
            spans: VecDeque::new(),
            open_since: None,
            failed: false,
            name,
        })
    }

    fn samples_at(&self, ms: f64) -> usize {
        (ms * self.sample_rate as f64 / 1000.0).round().max(0.0) as usize
    }

    fn observe(&mut self, result: Result<Vec<SpeechSegment>>) {
        match result {
            Ok(segments) => {
                for segment in segments {
                    let span = (
                        self.samples_at(segment.start_timestamp_ms),
                        self.samples_at(segment.end_timestamp_ms),
                    );
                    self.spans.push_back(span);
                }
                self.open_since = self
                    .detector
                    .open_speech_start_ms()
                    .map(|ms| self.samples_at(ms));
            }
            Err(error) => {
                warn!(
                    "{} speech detection failed; keeping this source in the transcription mix: {error}",
                    self.name
                );
                self.failed = true;
            }
        }
    }

    fn push(&mut self, samples: &[f32]) {
        if !self.failed {
            let result = self.detector.process_audio(samples);
            self.observe(result);
        }
    }

    fn finish(&mut self) {
        if !self.failed {
            let result = self.detector.flush();
            self.observe(result);
        }
    }

    /// Whether the sample at `index` is inside this source's speech. Indexes
    /// must not decrease between calls.
    fn contains(&mut self, index: usize) -> bool {
        if self.failed {
            return true;
        }
        while self.spans.front().is_some_and(|&(_, end)| end <= index) {
            self.spans.pop_front();
        }
        self.spans
            .front()
            .is_some_and(|&(start, end)| start <= index && index < end)
            || self.open_since.is_some_and(|start| start <= index)
    }
}

/// Delays both sources until each one's speech decision is known, then
/// silences each source outside its own speech.
pub(crate) struct SourceGatedInput {
    microphone: SourceGate,
    system: SourceGate,
    pending_microphone: VecDeque<f32>,
    pending_system: VecDeque<f32>,
    received: usize,
    released: usize,
    delay: usize,
    finished: bool,
}

impl SourceGatedInput {
    pub(crate) fn new(sample_rate: u32, redemption_ms: u32) -> Result<Self> {
        Ok(Self {
            microphone: SourceGate::new(sample_rate, redemption_ms, "Microphone")?,
            system: SourceGate::new(sample_rate, redemption_ms, "System audio")?,
            pending_microphone: VecDeque::new(),
            pending_system: VecDeque::new(),
            received: 0,
            released: 0,
            delay: (sample_rate as u64 * SOURCE_GATE_DELAY_MS as u64 / 1000) as usize,
            finished: false,
        })
    }

    /// Add one aligned mixer window. Returns the gated microphone and system
    /// samples whose speech decision is now known, starting where the previous
    /// call stopped. Sample positions match the input timeline.
    pub(crate) fn push(&mut self, microphone: &[f32], system: &[f32]) -> (Vec<f32>, Vec<f32>) {
        if self.finished {
            return (Vec::new(), Vec::new());
        }
        let length = microphone.len().max(system.len());
        let padded = |samples: &[f32]| {
            let mut samples = samples.to_vec();
            samples.resize(length, 0.0);
            samples
        };
        let (microphone, system) = (padded(microphone), padded(system));
        self.microphone.push(&microphone);
        self.system.push(&system);
        self.pending_microphone.extend(microphone);
        self.pending_system.extend(system);
        self.received += length;
        self.release(self.received.saturating_sub(self.delay))
    }

    /// End both detectors and return every remaining sample. Later calls
    /// return nothing.
    pub(crate) fn finish(&mut self) -> (Vec<f32>, Vec<f32>) {
        if self.finished {
            return (Vec::new(), Vec::new());
        }
        self.finished = true;
        self.microphone.finish();
        self.system.finish();
        self.release(self.received)
    }

    fn release(&mut self, until: usize) -> (Vec<f32>, Vec<f32>) {
        let count = until.saturating_sub(self.released);
        let mut microphone = Vec::with_capacity(count);
        let mut system = Vec::with_capacity(count);
        for index in self.released..until {
            let mic = self.pending_microphone.pop_front().unwrap_or(0.0);
            let sys = self.pending_system.pop_front().unwrap_or(0.0);
            microphone.push(if self.microphone.contains(index) {
                mic
            } else {
                0.0
            });
            system.push(if self.system.contains(index) {
                sys
            } else {
                0.0
            });
        }
        self.released = until.max(self.released);
        (microphone, system)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16000;

    /// Harmonic signal that Silero scores as speech (same as the pipeline tests).
    fn speech(seconds: f32) -> Vec<f32> {
        (0..(seconds * RATE as f32) as usize)
            .map(|i| {
                let time = i as f32 / RATE as f32;
                let f1 = 200.0 + (time * 50.0).sin() * 100.0;
                let amplitude = 0.3 + 0.1 * (time * 5.0).sin();
                amplitude
                    * (0.5 * (2.0 * std::f32::consts::PI * f1 * time).sin()
                        + 0.3 * (2.0 * std::f32::consts::PI * 2.0 * f1 * time).sin()
                        + 0.2 * (2.0 * std::f32::consts::PI * 3.0 * f1 * time).sin())
            })
            .collect()
    }

    /// Steady broadband noise near speech level, like the raised headset floor.
    fn noise(seconds: f32) -> Vec<f32> {
        let mut state = 0x2545_f491_u32;
        (0..(seconds * RATE as f32) as usize)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as f32 / u32::MAX as f32 - 0.5) * 0.08
            })
            .collect()
    }

    fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (seconds * RATE as f32) as usize]
    }

    fn run(microphone: &[f32], system: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let mut gate = SourceGatedInput::new(RATE, 500).unwrap();
        let window = RATE as usize * 600 / 1000;
        let (mut mic, mut sys) = (Vec::new(), Vec::new());
        for (m, s) in microphone.chunks(window).zip(system.chunks(window)) {
            let (a, b) = gate.push(m, s);
            mic.extend(a);
            sys.extend(b);
        }
        let (a, b) = gate.finish();
        mic.extend(a);
        sys.extend(b);
        (mic, sys)
    }

    /// Speech spans the same detector finds on one source alone.
    fn detected(source: &[f32]) -> Vec<(usize, usize)> {
        let mut detector = ContinuousVadProcessor::new(RATE, 500).unwrap();
        let window = RATE as usize * 600 / 1000;
        let mut segments = Vec::new();
        for chunk in source.chunks(window) {
            segments.extend(detector.process_audio(chunk).unwrap());
        }
        segments.extend(detector.flush().unwrap());
        segments
            .iter()
            .map(|s| {
                let at = |ms: f64| (ms * RATE as f64 / 1000.0).round() as usize;
                (at(s.start_timestamp_ms), at(s.end_timestamp_ms))
            })
            .collect()
    }

    /// The gated source equals the input inside its own detected speech and is
    /// silent everywhere else.
    fn assert_follows_own_speech(source: &[f32], gated: &[f32]) -> usize {
        let spans = detected(source);
        let mut inside = 0;
        for (index, (original, kept)) in source.iter().zip(gated).enumerate() {
            if spans
                .iter()
                .any(|&(start, end)| start <= index && index < end)
            {
                assert_eq!(kept, original, "speech sample {index} must pass unchanged");
                inside += 1;
            } else {
                assert_eq!(
                    *kept, 0.0,
                    "sample {index} outside this source's speech must be silent"
                );
            }
        }
        inside
    }

    #[test]
    fn every_sample_is_released_once_on_the_input_timeline() {
        let microphone = noise(5.3);
        let system = [silence(1.0), speech(3.0), silence(1.3)].concat();
        let (mic, sys) = run(&microphone, &system);
        assert_eq!(mic.len(), microphone.len());
        assert_eq!(sys.len(), system.len());
    }

    #[test]
    fn steady_microphone_noise_is_removed_while_remote_speech_is_kept() {
        let microphone = noise(12.0);
        let system = [silence(2.0), speech(8.0), silence(2.0)].concat();
        let (mic, sys) = run(&microphone, &system);
        assert!(
            assert_follows_own_speech(&system, &sys) > RATE as usize,
            "remote speech must pass"
        );
        assert_follows_own_speech(&microphone, &mic);
        let passed = mic.iter().filter(|s| **s != 0.0).count();
        assert!(
            passed < microphone.len() / 20,
            "steady noise must be removed, passed {passed}"
        );
    }

    #[test]
    fn speech_on_both_sources_keeps_both() {
        let microphone = [silence(1.0), speech(4.0), silence(1.0)].concat();
        let system = [silence(1.0), speech(4.0), silence(1.0)].concat();
        let (mic, sys) = run(&microphone, &system);
        assert!(assert_follows_own_speech(&microphone, &mic) > RATE as usize);
        assert!(assert_follows_own_speech(&system, &sys) > RATE as usize);
    }

    #[test]
    fn quick_turns_keep_each_sources_speech_from_its_start() {
        // Remote speaker, a reply 300 ms later, then the remote speaker again.
        let system = [speech(4.0), silence(4.3), speech(4.0), silence(1.0)].concat();
        let microphone = [silence(4.3), speech(4.0), silence(5.0)].concat();
        let (mic, sys) = run(&microphone, &system);
        // Each source passes its detector's full span, including the padding
        // before the first speech frame, even though the decision arrives later.
        assert!(assert_follows_own_speech(&system, &sys) > 0);
        assert!(
            assert_follows_own_speech(&microphone, &mic) > 0,
            "the reply must be detected"
        );
    }

    #[test]
    fn finishing_twice_returns_nothing_the_second_time() {
        let mut gate = SourceGatedInput::new(RATE, 500).unwrap();
        let (a, b) = gate.push(&speech(1.0), &silence(1.0));
        let (c, d) = gate.finish();
        assert_eq!(a.len() + c.len(), RATE as usize);
        assert_eq!(b.len() + d.len(), RATE as usize);
        assert_eq!(gate.finish(), (Vec::new(), Vec::new()));
        assert_eq!(
            gate.push(&speech(1.0), &silence(1.0)),
            (Vec::new(), Vec::new())
        );
    }
}
