#!/usr/bin/env python3
"""Acquire the fixed published predecessor for native update qualification.

Build/test tooling only. There is no latest-release lookup or caller-supplied URL;
the committed lock binds both public files for each native target. Nothing is
installed, launched or extracted onto a consumer installation.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import urllib.parse
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
LOCK = ROOT / "scripts/native-update-baseline.json"
RELEASE = "https://github.com/microsoft/amplifier-constellation-conversations/releases/download/v0.13.4/"
MAX_ARCHIVE = 128 * 1024 * 1024
MAX_RECEIPT = 64 * 1024


def package_module():
    spec = importlib.util.spec_from_file_location("baseline_package", ROOT / "scripts/package-native.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def require(value, message):
    if not value:
        raise ValueError(message)


def load_lock():
    value = json.loads(LOCK.read_text(encoding="utf-8"))
    require(set(value) == {"schema_version", "version", "source_revision", "release_url", "targets"}
            and value["schema_version"] == 1 and value["version"] == "0.13.4"
            and value["release_url"] == RELEASE
            and re.fullmatch(r"[0-9a-f]{40}", value["source_revision"] or ""), "Invalid predecessor release lock")
    require(set(value["targets"]) == set(package_module().TARGETS), "Predecessor must pin all six native targets")
    for target, row in value["targets"].items():
        require(set(row) == {"archive", "receipt", "manifest_sha256"}, "Invalid predecessor target fields")
        extension = ".zip" if target.startswith("windows-") else ".tar.gz"
        for key, suffix in [("archive", extension), ("receipt", ".json")]:
            item = row[key]
            require(set(item) == {"file", "sha256"}
                    and item["file"] == f"conversations-0.13.4-{target}{suffix}"
                    and re.fullmatch(r"[0-9a-f]{64}", item["sha256"] or ""), "Invalid predecessor asset pin")
        require(re.fullmatch(r"[0-9a-f]{64}", row["manifest_sha256"] or ""), "Invalid predecessor manifest pin")
    return value


def fetch(item, output, maximum, opener):
    """Use the same bounded, checksum-first acquisition pattern as Node inputs."""
    destination = output / item["file"]
    temporary = output / (".download-" + uuid.uuid4().hex)
    digest, size = hashlib.sha256(), 0
    try:
        with opener(RELEASE + item["file"], timeout=90) as incoming, temporary.open("xb") as stream:
            require(urllib.parse.urlparse(incoming.geturl()).scheme == "https", "Predecessor download lost HTTPS")
            while block := incoming.read(1024 * 1024):
                size += len(block)
                require(size <= maximum, "Predecessor download exceeds its bound")
                digest.update(block)
                stream.write(block)
        require(size > 0 and digest.hexdigest() == item["sha256"], "Predecessor asset checksum mismatch")
        require(not destination.exists() and not destination.is_symlink(), "Predecessor output already exists")
        temporary.replace(destination)
    finally:
        temporary.unlink(missing_ok=True)
    return destination


def acquire(target, output, *, opener=urllib.request.urlopen):
    lock = load_lock()
    require(target in lock["targets"], "Unknown native predecessor target")
    output = Path(output)
    require(output.is_absolute() and not output.exists() and not output.is_symlink(), "Use a fresh absolute predecessor output directory")
    output.mkdir(parents=True)
    row = lock["targets"][target]
    receipt = fetch(row["receipt"], output, MAX_RECEIPT, opener)
    recorded = json.loads(receipt.read_text(encoding="utf-8"))
    require(recorded.get("schema_version") == 1 and recorded.get("target") == target
            and recorded.get("source_revision") == lock["source_revision"]
            and recorded.get("archive") == row["archive"]["file"]
            and recorded.get("archive_sha256") == row["archive"]["sha256"]
            and recorded.get("manifest_sha256") == row["manifest_sha256"], "Published predecessor receipt differs from lock")
    archive = fetch(row["archive"], output, MAX_ARCHIVE, opener)
    manifest, files = package_module().inspect_archive(archive, row["archive"]["sha256"])
    require(hashlib.sha256(files["manifest.json"]).hexdigest() == row["manifest_sha256"]
            and manifest["version"] == lock["version"] and manifest["target"] == target
            and manifest["source_revision"] == lock["source_revision"], "Published predecessor manifest differs from lock")
    result = {"schema_version": 1, "version": lock["version"], "target": target,
              "source_revision": lock["source_revision"], "archive": str(archive), "receipt": str(receipt),
              "archive_sha256": row["archive"]["sha256"], "receipt_sha256": row["receipt"]["sha256"],
              "manifest_sha256": row["manifest_sha256"], "release_url": RELEASE,
              "scope": "published_predecessor_verified_for_disposable_update_test", "launched": False}
    with (output / "baseline.json").open("x", encoding="utf-8") as stream:
        stream.write(json.dumps(result, indent=2) + "\n")
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    print(json.dumps(acquire(arguments.target, arguments.output)))
