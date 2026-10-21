# Meetily FUN-ASR helper

Persistent native sidecar for Meetily's FUN-ASR Nano transcription provider.
It loads the shared speech encoder and one Qwen3 decoder once, then accepts
versioned binary PCM requests over stdin/stdout.

The inference implementation is derived from
`modelscope/FunASR/runtime/llama.cpp/fun-asr-nano/funasr-cli` at commit
`9b918d74d7090b38211a6414c6ee08893a34afd4`. The upstream license is retained in
`LICENSE-FunASR`. The pinned llama.cpp dependency is commit
`8086439a4cea94c71a5dfb8fe4ad1546aebd640f`.

Build:

```sh
cmake -S funasr-helper -B funasr-helper/build -DCMAKE_BUILD_TYPE=Release
cmake --build funasr-helper/build --config Release --target funasr-helper
```

## Audio embedding length

Nano's `use_low_frame_rate: true` configuration deliberately injects the first
`ceil(LFR frames / 8)` adaptor rows into the decoder. The adaptor keeps the full
time dimension, but its attention uses the whole input. A shorter decoder input
does not mean that only the first corresponding seconds of PCM were encoded.
Keep this rule aligned with the reference model; do not send all adaptor rows.
The helper checks the embedding shape before it applies this rule.

References:
- [Nano model configuration](https://huggingface.co/FunAudioLLM/Fun-ASR-Nano-2512/blob/main/config.yaml)
- [Pinned reference inference](https://github.com/modelscope/FunASR/blob/9b918d74d7090b38211a6414c6ee08893a34afd4/funasr/models/fun_asr_nano/model.py#L496-L501)
- [Pinned embedding injection](https://github.com/modelscope/FunASR/blob/9b918d74d7090b38211a6414c6ee08893a34afd4/funasr/models/fun_asr_nano/model.py#L626-L627)

Run the embedding boundary and shape checks without model weights:

```sh
c++ -std=c++17 funasr-helper/tests/audio_embeddings.cpp -o /tmp/funasr-embedding-tests
/tmp/funasr-embedding-tests
```
