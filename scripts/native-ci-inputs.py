#!/usr/bin/env python3
"""Acquire pinned build inputs for native CI; never shipped as an installer.

Only the selected official Node executable and license are extracted. npm and
Cargo dependency acquisition happen in explicit build steps, not at app startup.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tarfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def digest(path: Path) -> dict:
    return {"path": str(path.resolve()), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def package_module():
    spec = importlib.util.spec_from_file_location("native_package", ROOT / "scripts/package-native.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def node_inputs(target: str, scratch: Path) -> dict:
    lock = json.loads((ROOT / "scripts/native-node-lock.json").read_text())
    row = lock["targets"][target]
    url = f'https://nodejs.org/dist/v{lock["version"]}/{row["file"]}'
    archive = scratch / row["file"]
    if not archive.exists():
        temporary = archive.with_suffix(archive.suffix + ".part")
        try:
            with urllib.request.urlopen(url, timeout=90) as response, temporary.open("xb") as output:
                while block := response.read(1024 * 1024):
                    output.write(block)
            if digest(temporary)["sha256"] != row["sha256"]:
                raise ValueError("Official Node archive checksum does not match frozen input")
            temporary.replace(archive)
        finally:
            temporary.unlink(missing_ok=True)
    if digest(archive)["sha256"] != row["sha256"]:
        raise ValueError("Cached Node archive checksum does not match frozen input")
    windows = target.startswith("windows-")
    name = "node.exe" if windows else "node"
    suffix = "/node.exe" if windows else "/bin/node"
    prefix = row["file"]
    for extension in (".tar.gz", ".tar.xz", ".zip"):
        prefix = prefix.removesuffix(extension)
    names = {prefix + suffix: name, prefix + "/LICENSE": "LICENSE"}
    extracted = {}
    if windows:
        with zipfile.ZipFile(archive) as stream:
            for member in stream.infolist():
                if member.filename in names:
                    key = names[member.filename]
                    if key in extracted or member.is_dir():
                        raise ValueError("Ambiguous official Node archive inputs")
                    extracted[key] = stream.read(member)
    else:
        with tarfile.open(archive) as stream:
            for member in stream:
                if member.name in names:
                    key = names[member.name]
                    if key in extracted or not member.isfile():
                        raise ValueError("Ambiguous official Node archive inputs")
                    extracted[key] = stream.extractfile(member).read()
    if set(extracted) != {name, "LICENSE"}:
        raise ValueError("Incomplete official Node archive")
    destination = scratch / "node"
    destination.mkdir(exist_ok=True)
    for key, data in extracted.items():
        (destination / key).write_bytes(data)
    (destination / name).chmod(0o755)
    (scratch / "node-provenance.json").write_text(json.dumps({**row, "url": url,
        "checksum_source": lock["source"], "version": lock["version"]}, indent=2) + "\n")
    return {"executable": digest(destination / name), "license": digest(destination / "LICENSE"),
            "version": lock["version"]}


def rust_notices(scratch: Path) -> Path:
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1"], cwd=ROOT))
    # Full texts are retained wherever upstream distributes them. Metadata also
    # preserves declared license and repository for every locked dependency.
    records = []
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        home = Path(package["manifest_path"]).parent
        texts = []
        for path in sorted(home.iterdir()):
            if path.is_file() and path.name.lower().startswith(("license", "licence", "copying", "notice")):
                texts.append(f'--- {path.name} ---\n{path.read_text(errors="replace")}')
        license_file = package.get("license_file")
        if license_file and (home / license_file).is_file() and not texts:
            texts.append((home / license_file).read_text(errors="replace"))
        records.append(f'{package["name"]} {package["version"]}\nLicense: {package.get("license")}\n'
                       f'Source: {package.get("source")}\nRepository: {package.get("repository")}\n'
                       + "\n".join(texts))
    path = scratch / "THIRD-PARTY-NOTICES.txt"
    path.write_text("Locked Rust dependency notices\n\n" + "\n\n".join(records), encoding="utf-8")
    return path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--service", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    package = package_module()
    if args.target not in package.TARGETS:
        raise ValueError("Unknown native target")
    status = subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=all"], cwd=ROOT, text=True)
    if status.strip():
        raise ValueError("Freeze a clean committed source tree before generating native package provenance")
    node = node_inputs(args.target, args.output)
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], cwd=ROOT))
    current = next(p for p in metadata["packages"] if p["name"] == "agent-conversations")
    bridge = ROOT / "bridge"
    source = {"path": str(bridge), "source_sha256": package.tree_digest(package.bridge_files(bridge)),
              "node_modules": str(bridge / "node_modules"),
              "node_modules_sha256": package.tree_digest(package.tree_files(bridge / "node_modules"))}
    inputs = {"schema_version": 1, "target": args.target, "version": current["version"],
              "source_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
              "service": digest(args.service), "node": node, "bridge": source,
              "license": digest(ROOT / "LICENSE"), "third_party_notices": digest(rust_notices(args.output))}
    (args.output / "inputs.json").write_text(json.dumps(inputs, indent=2) + "\n")


if __name__ == "__main__":
    main()
