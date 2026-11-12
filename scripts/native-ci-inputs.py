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
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def digest(path: Path) -> dict:
    return {"path": str(path.resolve()), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def package_module():
    spec = importlib.util.spec_from_file_location("native_package", ROOT / "scripts/package-native.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def node_module():
    # Byte-exact upstream build helper; provenance lives beside the vendor tree.
    spec = importlib.util.spec_from_file_location("constellation_node_support",
        ROOT / "scripts/vendor/node/node_support.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def node_inputs(target: str, scratch: Path) -> dict:
    prepared = node_module().prepare(target, scratch, scratch / "node")
    # Keep this component's established input/receipt shape. Acquisition, cache
    # validation and extraction belong to the accepted shared implementation.
    version = prepared["version"]
    (scratch / "node-provenance.json").write_text(json.dumps({
        "file": prepared["archive"], "sha256": prepared["archive_sha256"],
        "url": f'https://nodejs.org/dist/v{version}/{prepared["archive"]}',
        "checksum_source": prepared["checksum_source"], "version": version}, indent=2) + "\n")
    return {"executable": {"path": prepared["executable"], "sha256": prepared["executable_sha256"]},
            "license": {"path": prepared["license"], "sha256": prepared["license_sha256"]},
            "version": version}


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
              "license": digest(ROOT / "LICENSE"), "third_party_notices": digest(rust_notices(args.output)),
              "support_files": {path: digest(ROOT / path) for path in sorted(package.SUPPORT_FILES)}}
    (args.output / "inputs.json").write_text(json.dumps(inputs, indent=2) + "\n")
    triple = package.TARGETS[args.target][2]
    target_flags = "CARGO_TARGET_" + triple.upper().replace("-", "_") + "_RUSTFLAGS"
    build = {"schema_version": 1, "source_revision": inputs["source_revision"],
             "target": args.target, "rust_target": triple, "service_sha256": inputs["service"]["sha256"],
             "cargo_lock_sha256": digest(ROOT / "Cargo.lock")["sha256"],
             "rustc": subprocess.check_output(["rustc", "--version", "--verbose"], text=True).strip(),
             "flags": {"RUSTFLAGS": os.environ.get("RUSTFLAGS"), target_flags: os.environ.get(target_flags)},
             "note": "Build environment provenance; actual Windows imports are inspected separately."}
    (args.output / "native-build-provenance.json").write_text(json.dumps(build, indent=2) + "\n")


if __name__ == "__main__":
    main()
