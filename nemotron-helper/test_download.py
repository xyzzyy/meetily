import importlib.util
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import patch

hub = types.ModuleType('huggingface_hub')
hub.snapshot_download = None
with patch.dict(sys.modules, {'huggingface_hub': hub}):
    spec = importlib.util.spec_from_file_location('download', Path(__file__).with_name('download.py'))
    download = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(download)


class DownloadTests(unittest.TestCase):
    def test_downloads_only_the_listed_files_at_the_pinned_revision_without_a_token(self):
        calls = []
        argv = ['download', 'owner/repo', 'abc123', '/local/model', 'config.json', 'model-00001-of-00004.safetensors']
        with patch.object(download, 'snapshot_download', lambda *args, **kwargs: calls.append((args, kwargs))), patch.object(sys, 'argv', argv):
            download.main()
        self.assertEqual(calls, [(('owner/repo',), {
            'revision': 'abc123',
            'local_dir': '/local/model',
            'allow_patterns': ['config.json', 'model-00001-of-00004.safetensors'],
            'token': False,
        })])


if __name__ == '__main__':
    unittest.main()
