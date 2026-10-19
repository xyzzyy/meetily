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
