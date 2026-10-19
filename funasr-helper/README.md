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
