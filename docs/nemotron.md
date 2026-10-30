# Nemotron 3.5 Transcribe

Meetily runs NVIDIA's Nemotron 3.5 Transcribe 30B-A4B with NVIDIA's native [NeMo-Speech.cpp](https://github.com/NVIDIA/NeMo-Speech.cpp) runtime on Apple silicon. It needs no Python. Windows, Linux and Intel Macs are not supported yet.

## Models

Users choose one of two GGUF decoders from [AtomicChat's conversion](https://huggingface.co/AtomicChat/NVIDIA-Nemotron-3.5-Transcribe-30B-A4B-GGUF/tree/dbd06c5cad7a2f33c8e06be2fb3ef814dbd9aa59), pinned to one revision. Both use the same encoder file (1,670 MB), which holds the speech encoder, the speaker branch and the timestamp head.

| Model | Decoder | With the encoder | Memory |
|---|---|---|---|
| Q2_0_L | 15,548 MB | 17,218 MB | 24 GB or more |
| IQ4_NL | 19,679 MB | 21,349 MB | 32 GB or more |

AtomicChat measured both decoders within about 0.2 points of the BF16 decoder's word error on English, German, French and Spanish ([model card](https://huggingface.co/AtomicChat/NVIDIA-Nemotron-3.5-Transcribe-30B-A4B-GGUF)). The model weights use NVIDIA's OpenMDW 1.1 license.

## Runtime

The runtime is NVIDIA's Metal build of NeMo-Speech.cpp `0.2.0+nightly.66d50d9` (Apache 2.0). NVIDIA replaces its nightly archive every day and its newest release, v0.2.0, predates Nemotron support. So the archive, checked against NVIDIA's `SHA256SUMS`, is mirrored on this fork's [`nemo-speech-66d50d9` release](https://github.com/xyzzyy/meetily/releases/tag/nemo-speech-66d50d9). Move to an NVIDIA release once one includes Nemotron support.

## Download

The Download button fetches the runtime archive, the encoder and the chosen decoder into the branch's owned model folder (`models/nemotron`). Every file has a pinned size and SHA-256. Files are hashed as they arrive and are only moved into place when they match. Cancel and network errors keep the unfinished `.part` file, and the next download resumes it with an HTTP range request. Deleting a model keeps the shared encoder and runtime while another model is downloaded. Deleting the last one removes the whole `models/nemotron` folder.

## Transcription

Finish or cancel any Nemotron download in Settings before you start transcription. If a download or another model load is in progress, Meetily shows an error instead of waiting at "Loading Nemotron".

Meetily starts `nemo-speech serve` on a free loopback port with a random API key in its environment, and stops it when transcription ends. Audio goes to `/v1/chat/completions` as 16 kHz mono WAV. One text request covers up to one minute, cut at a quiet point. Longer requests can come back with fluent text that nobody said, and the runtime still reports it as aligned: on a 17 minute meeting, one 5 minute request returned an invented talk about database sharding.

- Live recording and import send each speech segment and use the text.
- Enhance without speaker labels sends sentence-length segments and builds rows from the model's word times.
- Enhance with speaker labels on (the app's speaker setting) sends the whole recording for speakers, up to one hour per request, and the text in one minute pieces. It uses the model's own speakers. Their turns go through Meetily's speaker matching, so remembered voices are still named. Rows end where the speaker changes. Speaker numbers start again in each hour of a longer recording.

The server log is `models/nemotron/server.log`.

The runtime can report a successful alignment but return no word times for a nonempty transcript. Meetily checks that the timed words retain all returned text. It retries a failed section in pieces of up to 20 seconds. If a shorter section still has incomplete times, Enhance reports an error and keeps the saved transcript.
