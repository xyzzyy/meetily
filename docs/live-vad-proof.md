# Live speech preservation proof

PR #124 fixes audio loss before transcription. This test does not establish a
Branch-versus-stable regression or a general model accuracy improvement.

The replay used the same November 3 recording, decoded once to mono 16 kHz
little-endian float32. Both detector versions received the full recording from
sample zero in 9,600-sample blocks, with a 500 ms redemption time. Only the
detector implementation changed. There was no added context wait.

| Confirmed speech at 07:15–07:25 | Before: `5877a31a` | Fixed: `0f9c3e8d` |
|---|---:|---:|
| Source samples | 160,000 | 160,000 |
| Samples sent to transcription | 0 | 160,000 |
| Speech preservation test | Fail | Pass |

The test also checks that retained samples match the source samples at their
reported positions. It does not use a second speech detector as the expected
result. The source PCM SHA-256 was
`0512941bff0c8d62c4821120e829a045587efd54d26f7236f9b7aba74f02b7b2`.

The same new `recording_keeps_confirmed_speech` test was compiled against both
pinned production implementations with the same cached dependencies. The old
implementation failed with `0/160000`; the corrected implementation passed
with `160000/160000`. The normal application test build also passed this case.

For a transcript check, both detector outputs went through the same loaded
Whisper large-v3 model, English setting, and live decode settings, with a fresh
decode state per segment. The old detector supplied a 1.78-second fragment at
07:08.46–07:10.24 and no further segment until after 07:30. The corrected
detector supplied the 23.516-second segment at 07:08.18–07:31.696. The corrected
output recovered the discussion in the omitted interval. Some words remained
incorrect. This is not a measured word error rate.

The recording, audio clips, and transcript text stay local. To repeat the
recording test, decode the original recording from its start, then run:

```sh
ffmpeg -i "$RECORDING" -vn -ac 1 -ar 16000 -f f32le "$PCM"

MEETILY_VAD_PCM="$PCM" \
MEETILY_VAD_SPEECH_START=435 \
MEETILY_VAD_SPEECH_END=445 \
cargo test --release --lib recording_keeps_confirmed_speech -- --ignored --nocapture
```

Use an interval confirmed from the source recording when testing other audio.
The test is ignored in CI because it needs a private recording. The regular
VAD tests cover live/batch sample equality and Stop behavior, including the
minimum duration and artificial padding boundaries.
