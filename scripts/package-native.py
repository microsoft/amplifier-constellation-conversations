#!/usr/bin/env python3
"""Build a native, relocatable bundle from already acquired and pinned inputs.

This is a release-build tool, never an end-user prerequisite. It neither downloads
dependencies nor invokes npm, a compiler, the service, or an agent/model.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import struct
import tarfile
import tempfile
import zipfile


TARGETS = {
    "windows-x64": ("windows", "x64", "x86_64-pc-windows-msvc"),
    "windows-arm64": ("windows", "arm64", "aarch64-pc-windows-msvc"),
    "macos-x64": ("macos", "x64", "x86_64-apple-darwin"),
    "macos-arm64": ("macos", "arm64", "aarch64-apple-darwin"),
    "linux-x64": ("linux", "x64", "x86_64-unknown-linux-gnu"),
    "linux-arm64": ("linux", "arm64", "aarch64-unknown-linux-gnu"),
}
HEX = re.compile(r"[0-9a-f]{64}\Z")
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?\Z")
SUPPORT_FILES = {"docs/native-installation.md", "docs/native-updates.md", "ops/pam-agent-conversations", "ops/companion.example.json"}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def encode(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def relative(value: str) -> str:
    require(isinstance(value, str) and bool(value), "empty relative path")
    parts = PurePosixPath(value).parts
    require(not value.startswith("/") and "\\" not in value and ":" not in value
            and all(p not in {"", ".", ".."} for p in parts)
            and str(PurePosixPath(value)) == value
            and not any(ord(c) < 32 for c in value)
            and not any(p.endswith((" ", ".")) or re.fullmatch(r"(?i)(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\..*)?", p)
                        for p in parts), "unsafe relative path")
    return value


def read_regular(path: Path) -> bytes:
    require(path.is_file() and not path.is_symlink(), f"not a regular input file: {path.name}")
    return path.read_bytes()


def pinned_file(record: dict) -> bytes:
    require(isinstance(record, dict) and set(record) == {"path", "sha256"}, "invalid file input")
    require(isinstance(record["sha256"], str) and HEX.fullmatch(record["sha256"]), "invalid input digest")
    path = Path(record["path"])
    require(path.is_absolute(), "input paths must be absolute")
    data = read_regular(path)
    require(sha(data) == record["sha256"], f"input digest mismatch: {path.name}")
    return data


def executable_identity(data: bytes) -> tuple[str, str]:
    """Inspect the executable header; never execute an unqualified artifact."""
    if len(data) >= 64 and data[:4] == b"\x7fELF":
        require(data[4:6] == b"\x02\x01", "ELF must be 64-bit little endian")
        kind, machine = struct.unpack_from("<HH", data, 16)
        require(kind in {2, 3}, "ELF is not an executable/PIE")
        require(machine in {62, 183}, "unsupported ELF architecture")
        entry, headers = struct.unpack_from("<QQ", data, 24)
        header_size, count = struct.unpack_from("<HH", data, 54)
        require(entry != 0 and header_size == 56 and 0 < count < 1000
                and 64 <= headers <= len(data) and count * header_size <= len(data) - headers,
                "ELF has no valid executable entrypoint/program headers")
        executable_entry = False
        for index in range(count):
            kind, flags, offset, address, _, size, memory_size, _ = struct.unpack_from(
                "<IIQQQQQQ", data, headers + index * header_size)
            if kind == 1 and flags & 1 and memory_size >= size and offset <= len(data) and size <= len(data) - offset:
                executable_entry |= address <= entry and entry - address < size
        # Static PIE need not have PT_INTERP. The entry must nevertheless point
        # into executable, file-backed load bytes; a generic ET_DYN DSO is not a launcher.
        require(executable_entry, "ELF entrypoint is not in an executable load segment")
        return "linux", {62: "x64", 183: "arm64"}[machine]
    if len(data) >= 32 and data[:4] == b"\xcf\xfa\xed\xfe":
        cpu, _, kind = struct.unpack_from("<III", data, 4)
        require(kind == 2 and cpu in {0x01000007, 0x0100000C}, "unsupported Mach-O executable")
        return "macos", {0x01000007: "x64", 0x0100000C: "arm64"}[cpu]
    if len(data) >= 64 and data[:2] == b"MZ":
        offset = struct.unpack_from("<I", data, 60)[0]
        require(64 <= offset <= len(data) - 26 and data[offset:offset + 4] == b"PE\0\0", "invalid PE header")
        machine = struct.unpack_from("<H", data, offset + 4)[0]
        characteristics = struct.unpack_from("<H", data, offset + 22)[0]
        magic = struct.unpack_from("<H", data, offset + 24)[0]
        require(machine in {0x8664, 0xAA64} and magic == 0x20B
                and characteristics & 2 and not characteristics & 0x2000, "unsupported PE executable")
        return "windows", {0x8664: "x64", 0xAA64: "arm64"}[machine]
    raise ValueError("unknown executable format (thin native ELF/Mach-O/PE required)")


def tree_files(root: Path) -> dict[str, bytes]:
    require(root.is_dir() and not root.is_symlink(), "input tree must be a real directory")
    files: dict[str, bytes] = {}
    for base, directories, names in os.walk(root, followlinks=False):
        # npm creates command shims here. The application never invokes them;
        # excluding them also avoids packaging external or platform-specific links.
        directories[:] = sorted(d for d in directories if d not in {".bin", ".cache", "__pycache__", ".git"})
        for name in directories:
            require(not (Path(base) / name).is_symlink(), "linked dependency directory")
        for name in sorted(names):
            if name.endswith((".pyc", ".pyo")):
                continue
            p = Path(base) / name
            rel = relative(p.relative_to(root).as_posix())
            files[rel] = read_regular(p)
    require(files, "empty input tree")
    require(len({name.casefold() for name in files}) == len(files), "case-colliding input paths")
    return files


def tree_digest(files: dict[str, bytes]) -> str:
    return sha(encode([{"path": p, "sha256": sha(data), "bytes": len(data)}
                       for p, data in sorted(files.items())]))


def bridge_files(root: Path) -> dict[str, bytes]:
    require(root.is_dir() and not root.is_symlink(), "bridge must be a real directory")
    files = {p.name: read_regular(p) for p in sorted(root.iterdir())
             if p.suffix in {".mjs", ".py"} or p.name in {"package.json", "package-lock.json"}}
    require({"live.mjs", "companion-mcp.mjs", "package.json", "package-lock.json"} <= files.keys(),
            "incomplete bridge sources")
    return files


def production_dependencies(bridge: dict[str, bytes], installed: dict[str, bytes]) -> tuple[dict[str, bytes], list[dict]]:
    package = json.loads(bridge["package.json"])
    lock = json.loads(bridge["package-lock.json"])
    require(lock.get("lockfileVersion") == 3 and isinstance(lock.get("packages"), dict), "npm lockfile v3 required")
    locked = lock["packages"]
    require(locked.get("", {}).get("dependencies") == package.get("dependencies"), "package and lock dependencies differ")
    require(bool(package.get("dependencies")), "bridge has no SDK dependencies")
    for name, version in package["dependencies"].items():
        require(locked.get("node_modules/" + name, {}).get("version") == version,
                "direct SDK dependencies must use exact locked versions")
    result: dict[str, bytes] = {}
    inventory = []
    for location, metadata in sorted(locked.items()):
        if not location or metadata.get("dev"):
            continue
        relative(location)
        require(location.startswith("node_modules/") and not metadata.get("link")
                and not metadata.get("optional"), "unsupported linked/optional dependency; qualify explicitly")
        require(isinstance(metadata.get("version"), str) and metadata.get("integrity")
                and str(metadata.get("resolved", "")).startswith("https://"), "dependency lacks immutable registry provenance")
        local = location.removeprefix("node_modules/")
        package_key = local + "/package.json"
        require(package_key in installed, f"missing production dependency: {location}")
        descriptor = json.loads(installed[package_key])
        require(descriptor.get("version") == metadata["version"], f"installed dependency version mismatch: {location}")
        copied = []
        for path, data in installed.items():
            if not path.startswith(local + "/"):
                continue
            child = path[len(local) + 1:]
            # Each nested package is validated from its own lock entry.
            if child.startswith("node_modules/"):
                continue
            require(not path.endswith((".node", ".dll", ".dylib", ".so")), "native npm dependency needs separate target qualification")
            result[path] = data
            copied.append(child)
        require(copied, f"empty production dependency: {location}")
        notices = [p for p in copied if re.match(r"(?i)(license|licence|copying|notice)(\.|$)", PurePosixPath(p).name)]
        inventory.append({"path": location, "name": descriptor.get("name"), "version": metadata["version"],
                          "resolved": metadata["resolved"], "integrity": metadata["integrity"],
                          "license": descriptor.get("license", metadata.get("license")), "notice_files": notices})
    require(result, "no production dependency files")
    validate_app_imports(bridge, result, set(package["dependencies"]))
    return result, inventory


def validate_app_imports(bridge: dict[str, bytes], installed: dict[str, bytes], dependencies: set[str]) -> None:
    """Check the bridge's literal runtime imports, not every SDK export.

    Some official SDK packages declare unused root/type-only exports whose files
    are absent. Requiring those paths rejected a valid MCP SDK installation.
    This catches incomplete *used* entrypoints without executing a foreign Node;
    actual packaged-Node import smoke remains the recursive-resolution authority.
    """
    imports = set()
    static = re.compile(r"(?m)^\s*(?:import|export)\s+(?:[^;\n]*?\s+from\s+)?(['\"])([^'\"\n]+)\1")
    dynamic = re.compile(r"\bimport\s*\(\s*(['\"])([^'\"\n]+)\1\s*\)")
    for name, data in bridge.items():
        if name.endswith(".mjs"):
            text = data.decode("utf-8")
            imports.update(match[1] for match in static.findall(text))
            imports.update(match[1] for match in dynamic.findall(text))

    def selected(value: object) -> str | None:
        if isinstance(value, str):
            return value
        if isinstance(value, dict):
            # Conditional export key order is significant. `types`, `require`,
            # browser and custom conditions are not this Node ESM import path.
            for condition, item in value.items():
                if condition in {"node", "import", "default"}:
                    chosen = selected(item)
                    if chosen is not None:
                        return chosen
        if isinstance(value, list):
            for item in value:
                chosen = selected(item)
                if chosen is not None:
                    return chosen
        return None

    for specifier in sorted(imports):
        if specifier.startswith("node:"):
            continue
        if specifier.startswith("./"):
            require(relative(specifier[2:]) in bridge, f"missing local bridge import: {specifier}")
            continue
        parts = specifier.split("/")
        root_count = 2 if specifier.startswith("@") else 1
        name = "/".join(parts[:root_count])
        require(name in dependencies, f"undeclared bridge runtime dependency: {specifier}")
        subpath = "/".join(parts[root_count:])
        key = "./" + subpath if subpath else "."
        descriptor = json.loads(installed[name + "/package.json"])
        if "exports" in descriptor:
            exports = descriptor["exports"]
            chosen, capture = None, None
            if isinstance(exports, dict) and any(k.startswith(".") for k in exports):
                if key in exports:
                    chosen = selected(exports[key])
                else:
                    matches = []
                    for pattern, value in exports.items():
                        if pattern.count("*") != 1:
                            continue
                        prefix, suffix = pattern.split("*")
                        if key.startswith(prefix) and key.endswith(suffix) and len(key) >= len(prefix) + len(suffix):
                            matches.append((len(prefix), len(suffix), pattern, value))
                    if matches:
                        _, _, pattern, value = max(matches, key=lambda row: row[:2])
                        prefix, suffix = pattern.split("*")
                        capture = key[len(prefix):len(key) - len(suffix) if suffix else None]
                        chosen = selected(value)
            elif key == ".":
                chosen = selected(exports)
            require(chosen is not None and chosen.startswith("./"), f"no Node import export: {specifier}")
            if capture is not None:
                chosen = chosen.replace("*", capture)
            entry = relative(chosen[2:])
            # ESM exports are exact paths; extension/directory probing would
            # claim an invalid install is usable when Node itself rejects it.
            require(name + "/" + entry in installed, f"missing used package entrypoint: {specifier}")
        else:
            entry = subpath or descriptor.get("main", "index.js")
            require(isinstance(entry, str), f"invalid package entrypoint: {specifier}")
            entry = relative(entry.removeprefix("./"))
            candidates = [entry] if subpath else [entry, entry + ".js", entry + ".json", entry + "/index.js"]
            require(any(name + "/" + p in installed for p in candidates), f"missing used package entrypoint: {specifier}")


def output_inside_input(output: Path, source: Path) -> bool:
    """Resolve aliases, including /var -> /private/var and case aliases."""
    destination, source = output.resolve(), source.resolve(strict=True)
    if destination.is_relative_to(source):
        return True
    return any(parent.exists() and os.path.samefile(parent, source)
               for parent in (destination, *destination.parents))


def build_bundle(spec: dict, output: Path) -> dict:
    require(isinstance(spec, dict) and set(spec) == {"schema_version", "target", "version", "source_revision",
            "service", "node", "bridge", "license", "third_party_notices", "support_files"}, "invalid package input fields")
    require(spec["schema_version"] == 1 and spec["target"] in TARGETS, "unsupported package target/schema")
    require(isinstance(spec["version"], str) and VERSION.fullmatch(spec["version"]), "invalid release version")
    require(re.fullmatch(r"[0-9a-f]{40}", spec["source_revision"]), "full source revision required")
    operating_system, arch, triple = TARGETS[spec["target"]]
    suffix = ".exe" if operating_system == "windows" else ""
    node = spec["node"]
    require(isinstance(node, dict) and set(node) == {"executable", "version", "license"}, "invalid Node inputs")
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", node["version"])
            and tuple(map(int, node["version"].split("."))) >= (22, 6, 0), "Node 22.6 or later required")
    service_data, node_data = pinned_file(spec["service"]), pinned_file(node["executable"])
    for label, data in [("service", service_data), ("Node", node_data)]:
        require(executable_identity(data) == (operating_system, arch), f"{label} architecture/OS does not match target")
    source = spec["bridge"]
    require(isinstance(source, dict) and set(source) == {"path", "source_sha256", "node_modules", "node_modules_sha256"}, "invalid bridge input fields")
    require(Path(source["path"]).is_absolute() and Path(source["node_modules"]).is_absolute(), "absolute bridge input paths required")
    sources, installed = bridge_files(Path(source["path"])), tree_files(Path(source["node_modules"]))
    require(tree_digest(sources) == source["source_sha256"], "bridge source digest mismatch")
    require(tree_digest(installed) == source["node_modules_sha256"], "dependency input tree digest mismatch")
    production, inventory = production_dependencies(sources, installed)
    files = {"agent-conversations" + suffix: service_data, "runtime/node" + suffix: node_data,
             "licenses/Conversations-LICENSE": pinned_file(spec["license"]),
             "licenses/Node-LICENSE": pinned_file(node["license"]),
             "licenses/THIRD-PARTY-NOTICES.txt": pinned_file(spec["third_party_notices"]),
             "licenses/npm-dependencies.json": encode(inventory)}
    require(all(files[p].strip() for p in files if p.startswith("licenses/")), "empty license/notice input")
    support = spec["support_files"]
    require(isinstance(support, dict) and set(support) == SUPPORT_FILES, "fixed native support files required")
    for path, record in support.items():
        files[path] = pinned_file(record)
        require(files[path].strip(), "empty native installation support file")
    template = json.loads(files["ops/companion.example.json"])
    require(isinstance(template, dict) and set(template) == {"enabled", "node", "bridge", "profiles"}
            and template["enabled"] is False and template["profiles"] == {}
            and isinstance(template["node"], str) and isinstance(template["bridge"], str),
            "Companion template must be disabled, empty of accounts and contain no credentials")
    files.update({"bridge/" + path: data for path, data in sources.items()})
    files.update({"bridge/node_modules/" + path: data for path, data in production.items()})
    for path in files:
        relative(path)
    require(len({p.casefold() for p in files}) == len(files), "case-colliding bundle paths")
    executables = {"agent-conversations" + suffix, "runtime/node" + suffix}
    manifest = {"schema_version": 1, "component": "conversations", "version": spec["version"],
                "source_revision": spec["source_revision"], "target": spec["target"],
                "platform": {"os": operating_system, "arch": arch, "rust_target": triple},
                "entrypoints": {"service": "agent-conversations" + suffix, "node": "runtime/node" + suffix,
                                "bridge": "bridge/live.mjs", "mcp": "bridge/companion-mcp.mjs"},
                "node_version": node["version"], "bridge_lock_sha256": sha(sources["package-lock.json"]),
                "build_inputs": {"service_sha256": sha(service_data), "node_sha256": sha(node_data),
                                 "bridge_source_sha256": source["source_sha256"], "node_modules_sha256": source["node_modules_sha256"]},
                "qualification": {"native_header_checked": True, "clean_machine_launch": "not_established_by_packaging",
                                  "microphone_camera": "not_established_by_packaging", "signing": "not_established_by_packaging"},
                "files": [{"path": p, "sha256": sha(data), "bytes": len(data), "executable": p in executables}
                          for p, data in sorted(files.items())]}
    files["manifest.json"] = encode(manifest)
    output = output.resolve()
    require(not any(output_inside_input(output, Path(p)) for p in [source["path"], source["node_modules"]]),
            "output cannot mutate an input tree")
    output.mkdir(parents=True, exist_ok=True)
    name = f"conversations-{spec['version']}-{spec['target']}"
    extension = ".zip" if operating_system == "windows" else ".tar.gz"
    archive = output / (name + extension)
    receipt = output / (name + ".json")
    require(not archive.exists() and not receipt.exists(), "refusing to replace an existing release artifact")
    with tempfile.TemporaryDirectory(prefix=".native-package-", dir=output) as scratch:
        staged = Path(scratch) / (name + extension)
        if operating_system == "windows":
            with zipfile.ZipFile(staged, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as target:
                for path, data in sorted(files.items()):
                    info = zipfile.ZipInfo(path, (1980, 1, 1, 0, 0, 0))
                    info.create_system = 3
                    info.external_attr = ((stat.S_IFREG | (0o755 if path in executables else 0o644)) << 16)
                    info.compress_type = zipfile.ZIP_DEFLATED
                    target.writestr(info, data)
        else:
            import gzip
            import io
            with staged.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as gz:
                with tarfile.open(fileobj=gz, mode="w") as target:
                    for path, data in sorted(files.items()):
                        info = tarfile.TarInfo(path)
                        info.size, info.mode = len(data), 0o755 if path in executables else 0o644
                        info.uid = info.gid = info.mtime = 0
                        target.addfile(info, io.BytesIO(data))
        result = {"schema_version": 1, "target": spec["target"], "archive": archive.name,
                  "archive_sha256": sha(staged.read_bytes()), "manifest_sha256": sha(files["manifest.json"]),
                  "source_revision": spec["source_revision"], "file_count": len(files)}
        # Only immutable output artifacts are installed; no application home exists here.
        with archive.open("xb") as final, staged.open("rb") as source_stream:
            shutil.copyfileobj(source_stream, final)
        with receipt.open("xb") as final:
            final.write(encode(result))
    return result


def inspect_archive(path: Path, expected_sha256: str) -> tuple[dict, dict[str, bytes]]:
    """Build/CI verification before extraction; rejects links and extra payloads.

    The native installer independently performs its own validation. This helper
    is for release tests and must not become an end-user Python dependency.
    """
    require(HEX.fullmatch(expected_sha256) is not None and sha(read_regular(path)) == expected_sha256,
            "archive digest mismatch")
    files: dict[str, bytes] = {}

    def accept(name: str, data: bytes) -> None:
        relative(name)
        require(name not in files and name.casefold() not in {p.casefold() for p in files},
                "duplicate archive member")
        files[name] = data

    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            for item in archive.infolist():
                require(not item.is_dir() and stat.S_IFMT(item.external_attr >> 16) in {0, stat.S_IFREG},
                        "archive contains a nonregular member")
                accept(item.filename, archive.read(item))
    else:
        with tarfile.open(path, "r:gz") as archive:
            for item in archive:
                require(item.isfile() and not item.issym() and not item.islnk(), "archive contains a nonregular member")
                stream = archive.extractfile(item)
                require(stream is not None, "archive member unreadable")
                accept(item.name, stream.read())
    require("manifest.json" in files, "archive manifest missing")
    manifest = json.loads(files["manifest.json"])
    require(manifest.get("schema_version") == 1 and manifest.get("component") == "conversations"
            and manifest.get("target") in TARGETS, "invalid bundle manifest")
    expected = {"manifest.json"}
    for row in manifest.get("files", []):
        require(isinstance(row, dict) and set(row) == {"path", "sha256", "bytes", "executable"}, "invalid file record")
        name = relative(row["path"])
        require(name not in expected and name in files, "duplicate or missing manifest payload")
        require(type(row["bytes"]) is int and type(row["executable"]) is bool
                and row["bytes"] == len(files[name]) and sha(files[name]) == row["sha256"], "payload digest/size mismatch")
        expected.add(name)
    require(set(files) == expected, "unlisted archive payload")
    for entrypoint in manifest["entrypoints"].values():
        require(relative(entrypoint) in files and entrypoint != "manifest.json", "missing bundle entrypoint")
    target = TARGETS[manifest["target"]][:2]
    for entrypoint in ["service", "node"]:
        require(executable_identity(files[manifest["entrypoints"][entrypoint]]) == target, "manifest executable architecture mismatch")
    return manifest, files


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    command = parser.add_subparsers(dest="command", required=True)
    digest = command.add_parser("digest", help="Inspect immutable bridge inputs without executing them")
    digest.add_argument("--bridge", type=Path, required=True)
    digest.add_argument("--node-modules", type=Path, required=True)
    build = command.add_parser("build", help="Package supplied artifacts; no dependency downloads or builds")
    build.add_argument("--inputs", type=Path, required=True)
    build.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "digest":
        print(json.dumps({"source_sha256": tree_digest(bridge_files(args.bridge)),
                          "node_modules_sha256": tree_digest(tree_files(args.node_modules))}, sort_keys=True))
    else:
        print(json.dumps(build_bundle(json.loads(args.inputs.read_text()), args.output), sort_keys=True))


if __name__ == "__main__":
    main()
