import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

from prepare_voice_profiles import checksum, prepare


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.recipe = self.root / 'recipe.json'
        self.output = self.root / 'audio'
        self.data = b'public speech fixture'
        self.manifest = {'source': 'https://example.invalid/public.tar.gz',
                         'profiles': [{'id': 'one', 'clips': [{'path': 'one.flac', 'sha256': checksum(self.data)}]}],
                         'probes': []}
        self.save_recipe()

    def save_recipe(self):
        self.recipe.write_text(json.dumps(self.manifest))

    def archive(self, data):
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode='w:gz') as archive:
            for name, contents in [('dataset/one.flac', data), ('../outside', b'ignored')]:
                member = tarfile.TarInfo(name)
                member.size = len(contents)
                archive.addfile(member, io.BytesIO(contents))
        stream.seek(0)
        return lambda *_args, **_kwargs: stream

    def test_writes_only_checked_selected_members_and_reuses_the_cache(self):
        result = prepare(self.recipe, self.output, self.archive(self.data))
        self.assertEqual((self.output / 'one.flac').read_bytes(), self.data)
        self.assertEqual(result.read_bytes(), self.recipe.read_bytes())
        self.assertFalse((self.root / 'outside').exists())
        self.assertEqual(prepare(self.recipe, self.output, lambda *_a, **_k: self.fail('unexpected download')), result)

    def test_damaged_download_is_not_published(self):
        with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
            prepare(self.recipe, self.output, self.archive(b'changed'))
        self.assertFalse((self.output / 'one.flac').exists())
        self.assertFalse((self.output / 'manifest.json').exists())

    def test_changed_cache_is_preserved_without_network_access(self):
        self.output.mkdir()
        (self.output / 'one.flac').write_bytes(b'user file')
        with self.assertRaisesRegex(ValueError, 'cached recording has changed'):
            prepare(self.recipe, self.output, lambda *_a, **_k: self.fail('unexpected download'))
        self.assertEqual((self.output / 'one.flac').read_bytes(), b'user file')

    def test_paths_cannot_escape_the_output_directory(self):
        self.manifest['profiles'][0]['clips'][0]['path'] = '../outside'
        self.save_recipe()
        with self.assertRaisesRegex(ValueError, 'simple filenames'):
            prepare(self.recipe, self.output, self.archive(self.data))
        self.assertFalse((self.root / 'outside').exists())


if __name__ == '__main__':
    unittest.main()
