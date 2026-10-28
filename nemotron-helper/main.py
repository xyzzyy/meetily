"""Local Nemotron worker. Standard output contains only protocol messages."""
import base64
import contextlib
import json
import sys


def send(message):
    print(json.dumps(message), flush=True)


def main():
    with contextlib.redirect_stdout(sys.stderr):
        import numpy as np
        from mlx_audio.stt import load
        model = load(sys.argv[1], strict=True)
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
            send({"text": result.text})
        except Exception as error:
            send({"error": str(error)})


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        send({"error": str(error)})
        sys.exit(1)
