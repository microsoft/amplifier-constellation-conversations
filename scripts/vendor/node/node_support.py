"""Pinned build-time Node inputs. No installer, npm invocation or shared runtime.

Extracts the official executable and its complete distributor license only.
Adapted from Companion's native-ci-inputs.py at exact source 50945b8.
"""
import hashlib
import json
from pathlib import Path
import stat
import tarfile
import urllib.request
import uuid
import zipfile

ROOT = Path(__file__).resolve().parent
MAX_ARCHIVE = 128 * 1024 * 1024
MAX_EXECUTABLE = 256 * 1024 * 1024
MAX_LICENSE = 4 * 1024 * 1024

def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()

def asset_for(target):
    lock = json.loads((ROOT / 'lock.json').read_text())
    if target not in lock['targets']:
        raise ValueError('No pinned Node distribution for this target')
    row = lock['targets'][target]
    if Path(row['file']).name != row['file'] or '\\' in row['file']:
        raise ValueError('Invalid Node distribution filename')
    return lock, row

def download(target, cache):
    lock, row = asset_for(target)
    cache = Path(cache)
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / row['file']
    if archive.is_symlink():
        raise ValueError('Node cache input must be an ordinary file')
    url = 'https://nodejs.org/dist/v' + lock['version'] + '/' + row['file']
    if not archive.exists():
        temporary = cache / ('.node-download-' + uuid.uuid4().hex)
        try:
            with urllib.request.urlopen(url, timeout=90) as incoming, temporary.open('xb') as output:
                total = 0
                while block := incoming.read(1024 * 1024):
                    total += len(block)
                    if total > MAX_ARCHIVE:
                        raise ValueError('Node download exceeds its bound')
                    output.write(block)
            if digest(temporary) != row['sha256']:
                raise ValueError('Official Node archive checksum mismatch')
            temporary.replace(archive)
        finally:
            temporary.unlink(missing_ok=True)
    if not archive.is_file() or archive.stat().st_size > MAX_ARCHIVE or digest(archive) != row['sha256']:
        raise ValueError('Cached Node archive checksum mismatch')
    return archive

def unpack(archive, filename, windows):
    """Only exact top-level executable/LICENSE names become fixed output names."""
    prefix = filename
    for suffix in ('.tar.gz', '.tar.xz', '.zip'):
        prefix = prefix.removesuffix(suffix)
    executable = 'node.exe' if windows else 'node'
    selected = {prefix + ('/node.exe' if windows else '/bin/node'): executable,
                prefix + '/LICENSE': 'LICENSE'}
    output = {}
    def consume(name, size, regular, incoming):
        key = selected[name]
        limit = MAX_LICENSE if key == 'LICENSE' else MAX_EXECUTABLE
        if key in output or not regular or not 0 < size <= limit:
            raise ValueError('Ambiguous or invalid Node archive member')
        data = incoming.read(limit + 1)
        if len(data) != size or len(data) > limit:
            raise ValueError('Node archive member exceeds its bound')
        output[key] = data
    if windows:
        with zipfile.ZipFile(archive) as source:
            for member in source.infolist():
                if member.filename in selected:
                    kind = stat.S_IFMT(member.external_attr >> 16)
                    with source.open(member) as incoming:
                        consume(member.filename, member.file_size,
                                not member.is_dir() and kind in (0, stat.S_IFREG), incoming)
    else:
        with tarfile.open(archive) as source:
            total = 0
            for member in source:
                total += member.size
                if total > 2 * 1024**3:
                    raise ValueError('Node distribution expands beyond its bound')
                if member.name in selected:
                    if not member.isfile():
                        raise ValueError('Node executable and license must be ordinary files')
                    with source.extractfile(member) as incoming:
                        consume(member.name, member.size, True, incoming)
    if set(output) != {executable, 'LICENSE'}:
        raise ValueError('Incomplete Node executable or license')
    return output

def prepare(target, cache, destination):
    """Return exact native executable/license metadata at a new directory."""
    lock, row = asset_for(target)
    destination = Path(destination)
    if destination.exists() or destination.is_symlink():
        raise ValueError('Use a new Node package destination')
    archive = download(target, cache)
    files = unpack(archive, row['file'], target.startswith('windows-'))
    destination.mkdir(parents=True)
    for name, data in files.items():
        path = destination / name
        with path.open('xb') as output:
            output.write(data)
        path.chmod(0o644 if name == 'LICENSE' else 0o755)
    executable = 'node.exe' if target.startswith('windows-') else 'node'
    return {'version': lock['version'], 'target': target, 'archive': row['file'],
            'archive_sha256': row['sha256'], 'checksum_source': lock['source'],
            'executable': str((destination / executable).resolve()),
            'executable_sha256': digest(destination / executable),
            'license': str((destination / 'LICENSE').resolve()),
            'license_sha256': digest(destination / 'LICENSE')}
