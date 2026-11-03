//! Keep nearby Nemotron speech in one request, with a bounded live wait.
use super::super::recording_state::AudioChunk;
use std::time::Duration;
use tokio::{
    sync::mpsc::UnboundedReceiver,
    time::{timeout_at, Instant},
};

const CONTEXT_SECONDS: f64 = 30.0;

/// Live VAD sends 16 kHz segments. Keep silence between them so their recording
/// positions, including the interval used for speaker matching, stay correct.
pub(super) async fn next_context(
    receiver: &mut UnboundedReceiver<AudioChunk>,
    pending: &mut Option<AudioChunk>,
    wait: Duration,
) -> Option<AudioChunk> {
    let mut group = match pending.take() {
        Some(chunk) => chunk,
        None => receiver.recv().await?,
    };
    let deadline = Instant::now() + wait;
    while group.data.len() < (CONTEXT_SECONDS * group.sample_rate as f64) as usize {
        let next = match timeout_at(deadline, receiver.recv()).await {
            Ok(Some(chunk)) => chunk,
            _ => break, // Stop and timeout both send the remaining speech.
        };
        let offset = ((next.timestamp - group.timestamp) * group.sample_rate as f64).round();
        let end = offset.max(0.0) as usize + next.data.len();
        if next.sample_rate != group.sample_rate
            || offset < 0.0
            || end > (CONTEXT_SECONDS * group.sample_rate as f64) as usize
        {
            *pending = Some(next);
            break;
        }
        let offset = offset as usize;
        let old_len = group.data.len();
        group.data.resize(old_len.max(end), 0.0);
        group.data[offset..end].copy_from_slice(&next.data);
        group.end_timestamp =
            Some(group.timestamp + group.data.len() as f64 / group.sample_rate as f64);
        group.source_activity = match (group.source_activity, next.source_activity) {
            (Some(mut left), Some(right)) => {
                left.microphone_active |= right.microphone_active;
                left.system_active |= right.system_active;
                left.system_available |= right.system_available;
                left.overlap_detected |= right.overlap_detected;
                Some(left)
            }
            // Without both intervals, the combined source is unknown.
            _ => None,
        };
    }
    Some(group)
}

#[cfg(test)]
mod tests {
    use super::super::super::recording_state::{DeviceType, SourceActivity};
    use super::*;

    fn chunk(start: f64, seconds: f64, value: f32) -> AudioChunk {
        AudioChunk {
            data: vec![value; (seconds * 16000.0) as usize],
            sample_rate: 16000,
            timestamp: start,
            end_timestamp: Some(start + seconds),
            chunk_id: 1,
            device_type: DeviceType::Microphone,
            source_activity: Some(SourceActivity {
                microphone_active: false,
                system_active: true,
                system_available: true,
                overlap_detected: false,
            }),
        }
    }

    #[tokio::test]
    async fn joins_short_speech_with_silence_and_flushes_on_stop() {
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        send.send(chunk(480.0, 5.0, 0.1)).unwrap();
        send.send(chunk(487.26, 2.23, 0.2)).unwrap();
        send.send(chunk(490.05, 1.24, 0.3)).unwrap();
        drop(send);
        let mut pending = None;
        let group = next_context(&mut receive, &mut pending, Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(group.timestamp, 480.0);
        assert_eq!(group.end_timestamp, Some(491.29));
        assert_eq!(&group.data[..80000], vec![0.1; 80000]);
        assert!(group.data[80000..116160].iter().all(|s| *s == 0.0));
        assert_eq!(&group.data[116160..151840], vec![0.2; 35680]);
        assert_eq!(&group.data[160800..], vec![0.3; 19840]);
        assert!(next_context(&mut receive, &mut pending, Duration::ZERO)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn limits_context_and_keeps_the_next_segment() {
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        send.send(chunk(0.0, 2.0, 0.1)).unwrap();
        send.send(chunk(29.0, 3.0, 0.2)).unwrap();
        drop(send);
        let mut pending = None;
        let first = next_context(&mut receive, &mut pending, Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(first.data.len(), 32000);
        let second = next_context(&mut receive, &mut pending, Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(second.timestamp, 29.0);
        assert_eq!(second.data, vec![0.2; 48000]);
    }

    #[tokio::test]
    async fn sends_speech_when_the_wait_expires_without_channel_close() {
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        send.send(chunk(0.0, 2.0, 0.1)).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            next_context(&mut receive, &mut None, Duration::from_millis(1)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.data.len(), 32000);
        drop(send);
    }

    #[tokio::test]
    async fn overlapping_padding_does_not_repeat_audio() {
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        send.send(chunk(0.0, 3.0, 0.1)).unwrap();
        send.send(chunk(2.5, 2.0, 0.2)).unwrap();
        drop(send);
        let result = next_context(&mut receive, &mut None, Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(result.data.len(), 72000);
        assert_eq!(&result.data[40000..], vec![0.2; 32000]);
    }
}
