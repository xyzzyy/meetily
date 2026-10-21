#include "../src/audio_embeddings.h"
#include <cstdio>
#include <initializer_list>
#include <utility>

int main() {
    // Boundaries and a 30-second input must match the Python Nano reference.
    for (const auto & test : {std::pair<int, int>{1, 1}, {8, 1}, {9, 2}, {499, 63}}) {
        if (funasr_audio_tokens(test.first, test.first * 1024u, 1024, 1024) != test.second) {
            return 1;
        }
    }
    int rejected = 0;
    for (const auto & test : {std::pair<int, std::size_t>{0, 0}, {8, 1024}, {8, 8193}}) {
        try { funasr_audio_tokens(test.first, test.second, 1024, 1024); }
        catch (const std::runtime_error &) { ++rejected; }
    }
    try { funasr_audio_tokens(8, 8192, 1024, 2048); }
    catch (const std::runtime_error &) { ++rejected; }
    if (rejected != 4) return 1;
    std::puts("FUN-ASR embedding checks passed");
    return 0;
}
