"""Local Nemotron worker. Standard output contains only protocol messages."""
import base64
import contextlib
import json
import sys


def send(message):
    print(json.dumps(message), flush=True)


def timed_words(text, items, processor):
    """Put aligner times on the transcript's own words, so punctuation and case stay.

    The aligner drops punctuation and splits CJK characters. A word gets the span
    of the aligner items made from it. Punctuation alone joins the previous word.
    """
    words, used, prefix = [], 0, ""
    for token in text.split():
        cleaned = processor.clean_token(token)
        count = len(processor.split_segment_with_chinese(cleaned)) if cleaned else 0
        if count == 0:
            if words:
                words[-1][0] += " " + token
            else:
                prefix += token + " "
            continue
        span = items[used : used + count]
        used += count
        if len(span) != count:
            raise ValueError("The word alignment does not match the transcript")
        words.append([prefix + token, span[0].start_time, span[-1].end_time])
        prefix = ""
    if used != len(items):
        raise ValueError("The word alignment does not match the transcript")
    return words


def main():
    with contextlib.redirect_stdout(sys.stderr):
        import numpy as np
        from mlx_audio.stt import load
        model = load(sys.argv[1], strict=True)
    aligner = None
    send({"ready": True})
    for line in sys.stdin:
        try:
            request = json.loads(line)
            audio = np.frombuffer(base64.b64decode(request["audio"], validate=True), dtype="<f4").copy()
            if not 1600 <= len(audio) <= 30 * 16000 or not np.isfinite(audio).all():
                raise ValueError("Audio must contain 0.1 to 30 seconds of finite mono 16 kHz samples")
            with contextlib.redirect_stdout(sys.stderr):
                result = model.generate(audio, task="asr", max_tokens=4096, temperature=0, enable_thinking=False)
            if result.generation_tokens >= 4096:
                raise ValueError("The transcript reached the output limit")
            reply = {"text": result.text}
            if request.get("align"):
                text = result.text.strip()
                words = []
                if text:
                    with contextlib.redirect_stdout(sys.stderr):
                        # Loaded on first use. Live transcription does not need word times.
                        if aligner is None:
                            aligner = load(sys.argv[2], strict=True)
                        # The language only selects the aligner's word splitter. Its
                        # default splits at spaces and CJK characters, which matches
                        # timed_words for every language Nemotron writes.
                        items = aligner.generate(audio, text=text, language="English").items
                    words = timed_words(text, items, aligner.aligner_processor)
                reply["words"] = words
            send(reply)
        except Exception as error:
            send({"error": str(error)})


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        send({"error": str(error)})
        sys.exit(1)
