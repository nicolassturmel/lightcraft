#!/usr/bin/env python3
"""Fetch Apple's pinned Apache-2.0 model as a build input, never at application runtime."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import tempfile
import urllib.request


def verified(path, size, checksum):
    if not path.is_file() or path.stat().st_size != size:
        return False
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(65536), b''):
            digest.update(chunk)
    return digest.hexdigest() == checksum


def prepare(manifest, cache):
    with manifest.open('rb') as source:
        data = source.read(65537)
    if len(data) > 65536:
        raise ValueError('Oversized Apple model manifest')
    model = json.loads(data)
    if model['repository'] != 'apple/coreml-detr-semantic-segmentation' or not re.fullmatch(r'[0-9a-f]{40}', model['revision']):
        raise ValueError('Unpinned Apple model source')
    package = model['package']
    if not re.fullmatch(r'[A-Za-z0-9]+\.mlpackage', package) or not 0 < len(model['files']) <= 64:
        raise ValueError('Invalid Apple model package')
    if sum(item['size'] for item in model['files']) > 128000000:
        raise ValueError('Oversized Apple model package')
    for item in model['files']:
        relative = PurePosixPath(item['path'])
        size, checksum = item['size'], item['sha256']
        if relative.is_absolute() or '..' in relative.parts or relative.parts[0] != package or not 0 < size <= 100000000 or not re.fullmatch(r'[0-9a-f]{64}', checksum):
            raise ValueError('Invalid Apple model asset')
        destination = cache.joinpath(*relative.parts)
        destination.parent.mkdir(parents=True, exist_ok=True)
        if verified(destination, size, checksum):
            continue
        url = 'https://huggingface.co/' + model['repository'] + '/resolve/' + model['revision'] + '/' + str(relative)
        temporary = None
        try:
            digest, count = hashlib.sha256(), 0
            with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output:
                temporary = Path(output.name)
                with urllib.request.urlopen(url, timeout=60) as source:
                    while True:
                        chunk = source.read(65536)
                        if not chunk:
                            break
                        count += len(chunk)
                        if count > size:
                            raise ValueError('Apple model asset exceeds its pinned size')
                        digest.update(chunk)
                        output.write(chunk)
                output.flush()
                os.fsync(output.fileno())
            if count != size or digest.hexdigest() != checksum:
                raise ValueError('Apple model asset checksum mismatch')
            os.replace(temporary, destination)
            print('Fetched ' + relative.name + ' (' + str(count) + ' bytes)')
        finally:
            if temporary and temporary.exists():
                temporary.unlink()
    cache.mkdir(parents=True, exist_ok=True)
    marker = cache / 'apple-sky-source.sha256'
    marker.write_text(hashlib.sha256(data).hexdigest() + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('cache', type=Path)
    args = parser.parse_args()
    prepare(args.manifest, args.cache)
