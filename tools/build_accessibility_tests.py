#!/usr/bin/env python3
"""Build the API 34 x86_64 Rust library test harness with the pinned toolchains."""

import argparse
import hashlib
import json
import os
import platform
import tomllib
from pathlib import Path

from android_session import ROOT
from build_android import execute, ndk_revision


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ndk", type=Path, default=os.environ.get("ANDROID_NDK_HOME"))
    parser.add_argument("--output", type=Path, default=ROOT / "dist/native-tests")
    args = parser.parse_args()
    if args.ndk is None:
        parser.error("pass --ndk or set ANDROID_NDK_HOME")
    rust = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    rustc = execute(["rustc", "--version"])
    if not rustc.startswith(f"rustc {rust} "):
        parser.error(f"Rust {rust} required")
    config = json.loads((ROOT / "android-build.json").read_text())
    ndk = args.ndk.resolve()
    if ndk_revision(ndk) != config["ndk_version"]:
        parser.error("the pinned NDK is required")
    host = {"Linux": "linux-x86_64", "Darwin": "darwin-x86_64"}.get(platform.system())
    if host is None:
        parser.error("Linux or macOS host required")
    tools = ndk / "toolchains/llvm/prebuilt" / host / "bin"
    target = "x86_64-linux-android"
    env = os.environ.copy()
    env["CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER"] = str(tools / "clang")
    env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join([
        "-C", f"link-arg=--target=x86_64-linux-android{config['android_api']}",
        "-C", f"link-arg=-Wl,-z,max-page-size={config['elf_page_size']}",
    ])
    result = execute(["cargo", "test", "--locked", "--lib", "--target", target,
                      "--no-run", "--message-format=json"], env=env)
    executables = [Path(item["executable"]) for line in result.splitlines()
                   if (item := json.loads(line)).get("reason") == "compiler-artifact"
                   and item.get("executable")]
    if len(executables) != 1:
        raise ValueError("expected exactly one library test harness")
    data = executables[0].read_bytes()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    (output / "carlo-tests").write_bytes(data)
    (output / "carlo-tests").chmod(0o700)
    (output / "manifest.json").write_text(json.dumps({
        "schema_version": 1, "android_api": config["android_api"],
        "rustc": rustc, "ndk_version": config["ndk_version"],
        "artifacts": [{"target": target, "file": "carlo-tests", "bytes": len(data),
                       "sha256": hashlib.sha256(data).hexdigest()}],
    }, indent=2) + "\n")
    print(output / "manifest.json")


if __name__ == "__main__":
    main()
