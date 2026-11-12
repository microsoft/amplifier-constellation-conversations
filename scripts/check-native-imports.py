#!/usr/bin/env python3
"""Inspect Windows PE imports using the pinned build-only pefile library.

An installed Visual Studio runtime on a CI runner must not hide a deployment
dependency. This inspection does not prove a fresh Windows install, identify
arbitrary runtime LoadLibrary calls, sign a binary or permit new DLL bundling.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re

SYSTEM_DLLS = {
    "advapi32.dll", "bcrypt.dll", "bcryptprimitives.dll", "cabinet.dll", "cfgmgr32.dll",
    "comctl32.dll", "comdlg32.dll", "crypt32.dll", "cryptbase.dll", "cryptnet.dll",
    "dbghelp.dll", "dnsapi.dll", "dwmapi.dll", "gdi32.dll", "imm32.dll", "iphlpapi.dll",
    "kernel32.dll", "kernelbase.dll", "msvcrt.dll", "mswsock.dll", "ncrypt.dll",
    "netapi32.dll", "normaliz.dll", "ntdll.dll", "ole32.dll", "oleaut32.dll",
    "powrprof.dll", "profapi.dll", "psapi.dll", "rpcrt4.dll", "secur32.dll", "setupapi.dll",
    "shell32.dll", "shlwapi.dll", "sspicli.dll", "ucrtbase.dll", "user32.dll", "userenv.dll",
    "version.dll", "wevtapi.dll", "winhttp.dll", "wininet.dll", "winmm.dll", "wintrust.dll",
    "ws2_32.dll", "wtsapi32.dll",
}


def classify(name: str) -> str:
    name = name.lower()
    if "/" in name or "\\" in name or not name.endswith(".dll"):
        return "unreviewed_import"
    if re.fullmatch(r"(?:vcruntime|msvcp|msvcr|vccorlib|concrt|vcomp|atl)[0-9].*\.dll", name) or name in {"ucrtbased.dll", "msvcrtd.dll"}:
        return "separate_redistributable_or_developer_runtime"
    if name in SYSTEM_DLLS or re.fullmatch(r"(?:api|ext)-ms-win-[a-z0-9-]+\.dll", name):
        return "windows_system"
    return "unreviewed_import"


def inspect(path: Path, expected_sha: str, arch: str, pefile) -> dict:
    actual_sha = hashlib.sha256(path.read_bytes()).hexdigest()
    if actual_sha != expected_sha:
        raise ValueError("PE input changed after native package input capture")
    image = pefile.PE(str(path), fast_load=True)
    try:
        if image.FILE_HEADER.Machine != {"x64": 0x8664, "arm64": 0xAA64}[arch] or image.OPTIONAL_HEADER.Magic != 0x20B:
            raise ValueError("PE is not the expected native 64-bit architecture")
        image.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_IMPORT"],
                                                  pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT"]])
        rows = []
        for directory, delayed in [("DIRECTORY_ENTRY_IMPORT", False), ("DIRECTORY_ENTRY_DELAY_IMPORT", True)]:
            for entry in getattr(image, directory, []):
                name = entry.dll.decode("ascii", errors="strict").lower()
                rows.append({"name": name, "delayed": delayed, "classification": classify(name)})
        if not rows:
            raise ValueError("PE inspection produced no imports; native startup not qualified")
        return {"path": str(path), "sha256": actual_sha, "architecture": arch,
                "imports": rows, "warnings": image.get_warnings(),
                "unresolved_dependencies": [row for row in rows if row["classification"] != "windows_system"]}
    finally:
        image.close()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    import pefile
    if pefile.__version__ != "2024.8.26":
        raise ValueError("Use the pinned build-only PE inspection dependency")
    inputs = json.loads(args.inputs.read_text())
    if inputs["target"] not in {"windows-x64", "windows-arm64"}:
        raise ValueError("PE inspection requires a Windows native target")
    arch = inputs["target"].removeprefix("windows-")
    result = {"schema_version": 1, "source_revision": inputs["source_revision"],
              "target": inputs["target"], "inspector": "pefile==2024.8.26",
              "scope": "static and delay-load import tables; runtime LoadLibrary and fresh consumer OS remain separate",
              "executables": {}}
    for name, value in [("service", inputs["service"]), ("node", inputs["node"]["executable"])]:
        result["executables"][name] = inspect(Path(value["path"]), value["sha256"], arch, pefile)
    unresolved = any(row["unresolved_dependencies"] for row in result["executables"].values())
    result["status"] = "requires_dependency_review" if unresolved else "system_imports_only"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    if unresolved:
        raise SystemExit("Native package has a separate runtime/unreviewed DLL dependency; inspect receipt before packaging")


if __name__ == "__main__":
    main()
