# Speaker benchmark

Use Scribe v2 as a machine-generated reference for local speaker analysis. The
reference can be wrong. These scores are agreement with Scribe, not verified
speaker accuracy or a standard diarization error rate.

Keep audio, transcripts, keys, and result files outside the repository. The
transcription command uploads the audio to ElevenLabs and uses account credit.
It sends one request with `scribe_v2`, speaker labels, and word timestamps. It
does not set a speaker count. A saved reference is reused for every comparison.

If needed, extract the audio stream without changing the audio samples:

```sh
ffmpeg -i recording.mp4 -map 0:a:0 -vn -c:a copy benchmark-audio.m4a
```

Use `ELEVENLABS_API_KEY` from the environment or a private file with the line
`ELEVENLABS_API_KEY=...`. The script does not print the key. It stops on an API
error and does not retry a paid request. It also refuses to overwrite a saved
reference with another API request.

```sh
python3 diarization-sidecar/Benchmarks/scribe_benchmark.py \
  --transcribe --audio /private/path/benchmark-audio.m4a \
  --key-file /private/path/elevenlabs.env \
  --reference /private/path/scribe-v2.json
```

Generate local results by sending a `process` JSON request to the helper. The
request includes `modelCachePath`, `outputPath`, and `sources` with `kind` and
`path`. Optional `tuning` fields allow a benchmark to override
`clusteringThreshold`, `warmStartFa`, `warmStartFb`, `minSpeakers`, `maxSpeakers`,
or `numSpeakers`. App requests omit these fields and use automatic speaker count.
For the old clustering default, set `tuning.clusteringThreshold` to `0.6`.

Compare the resulting helper JSON files:

```sh
python3 diarization-sidecar/Benchmarks/scribe_benchmark.py \
  --reference /private/path/scribe-v2.json \
  --hypothesis baseline=/private/path/baseline.json \
  --hypothesis corrected=/private/path/corrected.json \
  --report /private/path/speaker-comparison.json
```

The comparison uses Scribe word time spans. It finds the best one-to-one speaker
mapping, so different speaker ID numbers are not errors. It reports missing
speech and ambiguous overlaps separately. Agreement on all reference speech
also counts those periods as unmatched. Silence between reference words is not
scored. The report includes the largest disagreement intervals for inspection;
it does not copy word text.

On the local meeting used for calibration, Scribe found 10 speaker IDs. The old
local default found 3, and assigned 94% of speech to one. A similarity threshold
of 0.90 found 9. Agreement on all Scribe word spans rose from 51.8% to 78.3%; on
speech with a single local label, it rose from 54.5% to 83.8%. Reference speech
coverage fell from 95.1% to 93.5%. Fixed-count clustering with 8 speakers scored
worse. Higher thresholds of 0.92 and 0.95 also scored worse than 0.90. A
57-second excerpt labelled as one speaker by Scribe produced one local speaker
with 0.90 and two with the old default. These results cover one meeting and do
not establish performance on other recordings.

Run the scorer checks without API access or audio files:

```sh
python3 -m unittest discover -s diarization-sidecar/Benchmarks -p 'test_*.py' -v
```

## Remembered-voice benchmark

Use the local Rust evaluator to compare a profile blend with that blend plus its
saved meeting samples. It uses the app's decoder, feature extractor, embedding
model, profile blending, threshold, and margin. It does not send audio to a
service or change the app database.

The checked-in recipe selects 64 recordings from 16 speakers in
[LibriSpeech test-clean](https://www.openslr.org/12/), by Vassil Panayotov,
Guoguo Chen, Daniel Povey, and Sanjeev Khudanpur (CC BY 4.0). Eight speakers each
have two enrolment recordings and two separate probes. The other eight speakers
have no enrolled profile and supply 32 unknown-speaker probes. This is read
English speech, not meeting audio. The recipe records every selected file's hash.

Prepare the files in an external cache. This streams the public archive until
all selected files are found; it can download more data than it keeps. A complete
cache is reused. Changed files are not overwritten.

```sh
python3 diarization-sidecar/Benchmarks/prepare_voice_profiles.py \
  --output-directory /tmp/meetily-voice-evaluation

cargo run --locked --release -p meetily --example evaluate_voice_profiles -- \
  /path/to/wespeaker_en_voxceleb_CAM++.onnx \
  /tmp/meetily-voice-evaluation/manifest.json \
  /tmp/meetily-voice-evaluation/results.json
```

The evaluator checks model and audio hashes. It rejects repeated recordings,
including copies with another filename, and refuses to overwrite a report.
Each recording contributes at most its first 30 seconds. Profiles use the same
speech-duration weighting as the app. Results include individual decisions,
input and model hashes, the embedding version, and the runtime build.

The corrected macOS ARM run uses the PCM16 feature recipe and pooling fix from
PRs #101 and #103. It produced these counts with 16 known-speaker probes and
32 unknown-speaker probes, at threshold 0.65 and margin 0.08:

| References | Correct known names | Wrong known names | Missed known names | False names on unknown speakers |
|---|---:|---:|---:|---:|
| Blend only | 15 | 0 | 1 | 0 |
| Blend and saved samples | 15 | 0 | 1 | 0 |

This small clean-speech set does not show a benefit from saved samples after
the extractor corrections. It does not establish meeting accuracy or justify
a threshold change. Sample matching remains a draft until representative
meeting samples show a useful benefit. Generated reports stay outside the
repository; the fixed recipe and hashes permit another run.

Run the evaluator's score and data-leak checks without a model or audio download:

```sh
cargo test --locked --release -p meetily --example evaluate_voice_profiles
python3 -m unittest discover -s diarization-sidecar/Benchmarks -p 'test_*.py' -v
```
