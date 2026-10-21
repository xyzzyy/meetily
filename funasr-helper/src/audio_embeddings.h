#pragma once

#include <cstddef>
#include <stdexcept>

// Nano enables use_low_frame_rate. Its reference inference code injects the
// first ceil(LFR frames / 8) adaptor rows, even though the adaptor preserves T.
// Those rows contain attention over the full input. Do not treat them as the
// first corresponding seconds of PCM, or replace this with the full T rows.
inline int funasr_audio_tokens(int frames, std::size_t values,
                               int embedding_size, int decoder_size) {
    if (frames <= 0 || embedding_size <= 0 || embedding_size != decoder_size ||
        values / static_cast<std::size_t>(embedding_size) != static_cast<std::size_t>(frames) ||
        values % static_cast<std::size_t>(embedding_size) != 0) {
        throw std::runtime_error("FUN-ASR encoder returned an invalid embedding shape");
    }
    return frames / 8 + (frames % 8 != 0);
}
