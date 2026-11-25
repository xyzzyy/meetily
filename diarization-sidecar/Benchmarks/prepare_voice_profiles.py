#!/usr/bin/env python3
"""Download only the public recordings named by a voice-profile manifest."""
import argparse
import hashlib
import json
from pathlib import Path
import tarfile
import urllib.request


def checksum(data):
    return hashlib.sha256(data).hexdigest()


def prepare(manifest_path, output, open_url=urllib.request.urlopen):
    recipe = manifest_path.read_bytes()
    manifest = json.loads(recipe)
    wanted = {}
    clips = [clip for profile in manifest['profiles'] for clip in profile['clips']]
    clips += [probe['clip'] for probe in manifest['probes']]
    for clip in clips:
        name = clip['path']
        if not name or Path(name).name != name or name in ('.', '..') or '\\' in name:
            raise ValueError('Recording paths must be simple filenames')
        if name in wanted:
            raise ValueError('The manifest repeats a recording')
        wanted[name] = clip['sha256']
    output.mkdir(parents=True, exist_ok=True)
    saved_manifest = output / 'manifest.json'
    if saved_manifest.exists() and saved_manifest.read_bytes() != recipe:
        raise FileExistsError('The output has a different manifest; use another directory')
    pending = dict(wanted)
    for name, expected in wanted.items():
        path = output / name
        if path.is_symlink():
            raise ValueError('The cache contains a symbolic link')
        if path.exists():
            if checksum(path.read_bytes()) != expected:
                raise ValueError('A cached recording has changed: ' + name)
            pending.pop(name)
    if pending:
        # Stream the archive and stop after the selected files. Never extract
        # archive paths, links, transcripts, or other members into the cache.
        with open_url(manifest['source'], timeout=45) as response:
            with tarfile.open(fileobj=response, mode='r|gz') as archive:
                for member in archive:
                    name = Path(member.name).name
                    if not member.isfile() or name not in pending:
                        continue
                    stream = archive.extractfile(member)
                    data = stream.read()
                    if checksum(data) != pending[name]:
                        raise ValueError('Downloaded checksum mismatch: ' + name)
                    with (output / name).open('xb') as destination:
                        destination.write(data)
                    pending.pop(name)
                    print('Saved', name, flush=True)
                    if not pending:
                        break
        if pending:
            raise ValueError('The archive is missing selected recordings: ' + ', '.join(sorted(pending)))
    if not saved_manifest.exists():
        with saved_manifest.open('xb') as destination:
            destination.write(recipe)
    return saved_manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, default=Path(__file__).with_name('voice_profiles_librispeech.json'))
    parser.add_argument('--output-directory', type=Path, required=True)
    args = parser.parse_args()
    print(prepare(args.manifest, args.output_directory))


if __name__ == '__main__':
    main()
