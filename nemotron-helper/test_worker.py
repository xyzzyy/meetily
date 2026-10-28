import base64
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import struct
import sys
import types
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('worker', Path(__file__).with_name('main.py'))
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class Audio(list):
    def copy(self):
        return self


class WorkerTests(unittest.TestCase):
    def run_worker(self, requests, tokens=3):
        np = types.ModuleType('numpy')
        np.frombuffer = lambda data, dtype: Audio(struct.unpack('<' + 'f' * (len(data) // 4), data))
        np.isfinite = lambda audio: types.SimpleNamespace(all=lambda: all(abs(x) < float('inf') for x in audio))
        calls = []

        def generate(audio, **kwargs):
            calls.append(kwargs)
            print('runtime log')
            return types.SimpleNamespace(text='A test transcript.', generation_tokens=tokens)

        def load(path, strict):
            self.assertEqual(path, '/local/model')
            self.assertTrue(strict)
            print('model load log')
            return types.SimpleNamespace(generate=generate)

        stt = types.ModuleType('mlx_audio.stt')
        stt.load = load
        output = io.StringIO()
        with patch.dict(sys.modules, {'numpy': np, 'mlx_audio': types.ModuleType('mlx_audio'), 'mlx_audio.stt': stt}), patch.object(sys, 'argv', ['worker', '/local/model']), patch.object(sys, 'stdin', io.StringIO('\n'.join(json.dumps(r) for r in requests))), contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()):
            worker.main()
        return [json.loads(line) for line in output.getvalue().splitlines()], calls

    def test_ready_and_multiple_requests_keep_logs_off_protocol(self):
        audio = base64.b64encode(struct.pack('<1600f', *([0.] * 1600))).decode()
        replies, calls = self.run_worker([{'audio': audio}, {'audio': audio}])
        self.assertEqual(replies, [{'ready': True}, {'text': 'A test transcript.'}, {'text': 'A test transcript.'}])
        self.assertEqual(calls[0]['task'], 'asr')
        self.assertFalse(calls[0]['enable_thinking'])

    def test_invalid_audio_and_truncated_output_return_errors(self):
        replies, calls = self.run_worker([{'audio': 'invalid!'}])
        self.assertIn('error', replies[1])
        self.assertFalse(calls)
        audio = base64.b64encode(struct.pack('<1600f', *([0.] * 1600))).decode()
        replies, _ = self.run_worker([{'audio': audio}], tokens=4096)
        self.assertIn('output limit', replies[1]['error'])


if __name__ == '__main__':
    unittest.main()
