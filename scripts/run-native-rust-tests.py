#!/usr/bin/env python3
"""Run native Rust qualifications and refuse a successful zero-test filter."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import time

SELECTIONS = [
    ("platform::", False),
    ("config::platform_tests::", False),
    ("credentials::", False),
    ("auth::tests::", False),
    ("rooms::migration_tests::", False),
    ("desktop::", False),
    ("native_bundle::", False),
    ("native_update_protocol::", False),
    ("native_update_clock::", False),
    ("native_update_process::", False),
    ("native_update::startup_tests::", False),
    ("native_update::cleanup::", False),
    ("native_update_identity::", False),
    ("native_browser_update::", False),
    ("native_check_diagnostics::", False),
    ("native_browser_cleanup::", False),
    ("native_recovery::", False),
    ("native_update_launch::", False),
    ("native_startup_state::", False),
    ("update_admission::", False),
    ("companion_awareness::process_tests::stalled_observer_and_descendant_are_stopped_without_facts", True),
]


def passed_counts(text: str) -> dict:
    summaries = re.findall(r"test result: ok\.\s+(\d+) passed;\s+(\d+) failed;\s+(\d+) ignored;", text)
    if len(summaries) != 1:
        raise ValueError("Native test selection did not produce one successful test summary")
    passed, failed, ignored = map(int, summaries[0])
    if not passed or failed:
        raise ValueError("Native test selection ran no passing tests")
    return {"passed": passed, "failed": failed, "ignored": ignored}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    receipt = {"schema_version": 1, "rust_target": args.target, "selections": [], "status": "running"}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    try:
        for selection, exact in SELECTIONS:
            command = ["cargo", "test", "--locked", "--target", args.target, "--lib", selection]
            if exact:
                command += ["--", "--exact"]
            start = time.monotonic()
            result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                    text=True, encoding="utf-8", errors="replace", timeout=1200)
            print(result.stdout, flush=True)
            row = {"selection": selection, "exact": exact, "elapsed_seconds": time.monotonic() - start,
                   "returncode": result.returncode}
            receipt["selections"].append(row)
            if result.returncode:
                raise RuntimeError(f"Native Rust selection failed: {selection}")
            row.update(passed_counts(result.stdout))
        receipt["status"] = "passed"
    except Exception as error:
        receipt.update(status="failed", error=str(error))
        raise
    finally:
        args.output.write_text(json.dumps(receipt, indent=2) + "\n")


if __name__ == "__main__":
    main()
