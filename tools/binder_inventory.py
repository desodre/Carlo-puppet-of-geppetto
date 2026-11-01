#!/usr/bin/env python3
"""Inventory API 34 system Binder exports against the pinned public NDK stub."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import subprocess
import tempfile
from pathlib import Path

from android_session import ABI_TARGETS, ROOT, Adb, SessionError
from build_android import ndk_revision

AOSP_REVISION = "bfcf75076e562945bae131a4929f29a90d0d2481"


def parse_exports(text: str) -> list[dict[str, str | None]]:
    """Parse llvm-nm POSIX defined-symbol output, preserving symbol versions."""
    exports = []
    for line in text.splitlines():
        parts = line.split()
        if len(parts) != 4 or len(parts[1]) != 1:
            raise ValueError("unexpected llvm-nm defined-symbol output")
        int(parts[2], 16)
        int(parts[3], 16)
        name, separator, version = parts[0].partition("@")
        exports.append({"name": name, "version": version.lstrip("@") if separator else None})
    if not exports:
        raise ValueError("no exported Binder symbols found")
    return sorted(exports, key=lambda symbol: (symbol["name"], symbol["version"] or ""))


def collect(adb: Adb, ndk: Path, allow_physical: bool = False) -> dict:
    """Read one target's library without installing or modifying device files."""
    if adb.shell(["getprop", "ro.kernel.qemu"]).stdout.strip() != "1" and not allow_physical:
        raise SessionError("physical device refused; explicit authorization required")
    api = int(adb.shell(["getprop", "ro.build.version.sdk"]).stdout.strip())
    abi = adb.shell(["getprop", "ro.product.cpu.abi"]).stdout.strip()
    if api != 34 or abi not in ABI_TARGETS:
        raise SessionError("inventory requires API 34 x86_64 or arm64-v8a")
    config = json.loads((ROOT / "android-build.json").read_text())
    revision = ndk_revision(ndk)
    if revision != config["ndk_version"]:
        raise ValueError("inventory requires the pinned NDK version")
    host_tag = {"Linux": "linux-x86_64", "Darwin": "darwin-x86_64"}.get(platform.system())
    if host_tag is None:
        raise ValueError("inventory supports Linux and macOS hosts")
    tools = ndk / "toolchains/llvm/prebuilt" / host_tag
    stub = tools / "sysroot/usr/lib" / ABI_TARGETS[abi] / "34/libbinder_ndk.so"
    device_path = "/system/lib64/libbinder_ndk.so"

    def exports(path: Path):
        result = subprocess.run(
            [str(tools / "bin/llvm-nm"), "--dynamic", "--defined-only", "--format=posix", str(path)],
            capture_output=True, text=True, check=True, timeout=15,
        )
        return parse_exports(result.stdout)

    with tempfile.TemporaryDirectory(prefix="carlo-binder-inventory-") as directory:
        library = Path(directory) / "libbinder_ndk.so"
        adb.run(["pull", device_path, str(library)])
        data = library.read_bytes()
        device_exports = exports(library)
    ndk_exports = exports(stub)
    public_names = {symbol["name"] for symbol in ndk_exports}
    return {
        "schema_version": 1,
        "android_api": api,
        "abi": abi,
        "fingerprint": adb.shell(["getprop", "ro.build.fingerprint"]).stdout.strip(),
        "selinux_mode": adb.shell(["getenforce"]).stdout.strip(),
        "device_library": device_path,
        "device_library_sha256": hashlib.sha256(data).hexdigest(),
        "device_library_bytes": len(data),
        "ndk_version": revision,
        "ndk_stub_sha256": hashlib.sha256(stub.read_bytes()).hexdigest(),
        "aosp_contract_reference": AOSP_REVISION,
        "aosp_reference_is_image_source_provenance": False,
        "device_exports": device_exports,
        "public_ndk_exports": ndk_exports,
        "device_only_export_names": sorted({
            symbol["name"] for symbol in device_exports if symbol["name"] not in public_names
        }),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--ndk", type=Path, default=os.environ.get("ANDROID_NDK_HOME"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--allow-physical", action="store_true")
    args = parser.parse_args()
    if args.ndk is None:
        parser.error("pass --ndk or set ANDROID_NDK_HOME")
    try:
        report = collect(Adb(args.serial), args.ndk.resolve(), args.allow_physical)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    except (OSError, ValueError, SessionError, subprocess.SubprocessError) as error:
        parser.exit(1, f"carlo-binder-inventory: {error}\n")
    print(args.output)


if __name__ == "__main__":
    main()
