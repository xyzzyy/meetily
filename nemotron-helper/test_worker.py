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


class Processor:
    """Matches the aligner's splitter: punctuation is dropped, CJK characters split."""
    def clean_token(self, token):
        return ''.join(ch for ch in token if ch.isalnum())

    def split_segment_with_chinese(self, segment):
        return list(segment) if all('\u4e00' <= ch <= '\u9fff' for ch in segment) else [segment]


def items(*spans):
    return [types.SimpleNamespace(text=str(i), start_time=s, end_time=e) for i, (s, e) in enumerate(spans)]


class Audio(list):
    def copy(self):
        return self


class WorkerTests(unittest.TestCase):
    def run_worker(self, requests, tokens=3):
        np = types.ModuleType('numpy')
        np.frombuffer = lambda data, dtype: Audio(struct.unpack('<' + 'f' * (len(data) // 4), data))
        np.isfinite = lambda audio: types.SimpleNamespace(all=lambda: all(abs(x) < float('inf') for x in audio))
        calls, aligned, loads = [], [], []
        self.aligned, self.loads = aligned, loads

        def generate(audio, **kwargs):
            calls.append(kwargs)
            print('runtime log')
            return types.SimpleNamespace(text='A test transcript.', generation_tokens=tokens)

        def align(audio, text, language):
            aligned.append((text, language))
            print('aligner log')
            return types.SimpleNamespace(items=items((0.5, 0.75), (0.8, 1.0), (1.0, 1.5)))

        def load(path, strict):
            self.assertTrue(strict)
            print('model load log')
            loads.append(path)
            if path == '/local/aligner':
                return types.SimpleNamespace(generate=align, aligner_processor=Processor())
            self.assertEqual(path, '/local/model')
            return types.SimpleNamespace(generate=generate)

        stt = types.ModuleType('mlx_audio.stt')
        stt.load = load
        output = io.StringIO()
        with patch.dict(sys.modules, {'numpy': np, 'mlx_audio': types.ModuleType('mlx_audio'), 'mlx_audio.stt': stt}), patch.object(sys, 'argv', ['worker', '/local/model', '/local/aligner']), patch.object(sys, 'stdin', io.StringIO('\n'.join(json.dumps(r) for r in requests))), contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()):
            worker.main()
        return [json.loads(line) for line in output.getvalue().splitlines()], calls

    def test_ready_and_multiple_requests_keep_logs_off_protocol(self):
        audio = base64.b64encode(struct.pack('<1600f', *([0.] * 1600))).decode()
        replies, calls = self.run_worker([{'audio': audio}, {'audio': audio}])
        self.assertEqual(replies, [{'ready': True}, {'text': 'A test transcript.'}, {'text': 'A test transcript.'}])
        self.assertEqual(calls[0]['task'], 'asr')
        self.assertFalse(calls[0]['enable_thinking'])

    def test_aligned_requests_return_word_times_and_load_the_aligner_once(self):
        audio = base64.b64encode(struct.pack('<1600f', *([0.] * 1600))).decode()
        replies, _ = self.run_worker([{'audio': audio}, {'audio': audio, 'align': True}, {'audio': audio, 'align': True}])
        self.assertEqual(replies[1], {'text': 'A test transcript.'})
        self.assertEqual(replies[2], {'text': 'A test transcript.', 'words': [['A', 0.5, 0.75], ['test', 0.8, 1.0], ['transcript.', 1.0, 1.5]]})
        self.assertEqual(self.loads, ['/local/model', '/local/aligner'])
        self.assertEqual(self.aligned, [('A test transcript.', 'English')] * 2)

    def test_word_times_keep_punctuation_and_join_cjk_characters(self):
        words = worker.timed_words('— Hi, 你好 there !', items((0, 1), (1, 2), (2, 3), (3, 4)), Processor())
        self.assertEqual(words, [['— Hi,', 0, 1], ['你好', 1, 3], ['there !', 3, 4]])
        with self.assertRaises(ValueError):
            worker.timed_words('Hi there', items((0, 1)), Processor())
        with self.assertRaises(ValueError):
            worker.timed_words('Hi', items((0, 1), (1, 2)), Processor())

    def test_invalid_audio_and_truncated_output_return_errors(self):
        replies, calls = self.run_worker([{'audio': 'invalid!'}])
        self.assertIn('error', replies[1])
        self.assertFalse(calls)
        audio = base64.b64encode(struct.pack('<1600f', *([0.] * 1600))).decode()
        replies, _ = self.run_worker([{'audio': audio}], tokens=4096)
        self.assertIn('output limit', replies[1]['error'])


if __name__ == '__main__':
    unittest.main()
