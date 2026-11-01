#!/usr/bin/env python3
"""Build pinned Android targets and emit verified native artifacts (Python 3.11+)."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import subprocess
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def execute(arguments: list[str], *, env: dict[str, str] | None = None) -> str:
    return subprocess.run(
        arguments, cwd=ROOT, env=env, check=True, text=True, stdout=subprocess.PIPE, timeout=600
    ).stdout.strip()


def ndk_revision(path: Path) -> str:
    match = re.search(r"^Pkg.Revision\s*=\s*(\S+)", (path / "source.properties").read_text(), re.M)
    if match is None:
        raise ValueError("NDK source.properties has no Pkg.Revision")
    return match[1]


def main() -> None:
    config = json.loads((ROOT / "android-build.json").read_text())
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ndk", type=Path, default=os.environ.get("ANDROID_NDK_HOME"))
    parser.add_argument("--target", action="append", choices=list(config["targets"]))
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    if args.ndk is None:
        parser.error("pass --ndk or set ANDROID_NDK_HOME")
    ndk = args.ndk.resolve()
    revision = ndk_revision(ndk)
    if revision != config["ndk_version"]:
        parser.error(f"NDK {config['ndk_version']} required; found {revision}")
    host_tag = {"Linux": "linux-x86_64", "Darwin": "darwin-x86_64"}.get(platform.system())
    if host_tag is None:
        parser.error("this build helper currently supports Linux and macOS hosts")
    tools = ndk / "toolchains/llvm/prebuilt" / host_tag / "bin"
    clang, readelf = tools / "clang", tools / "llvm-readelf"
    if not clang.is_file() or not readelf.is_file():
        parser.error("NDK host tools are missing")
    rust = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    rustc = execute(["rustc", "--version"])
    if not rustc.startswith(f"rustc {rust} "):
        parser.error(f"Rust {rust} required; found {rustc}")
    package = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]
    output = args.output.resolve()
    manifest = {
        "schema_version": 1,
        "agent_version": package["version"],
        "rustc": rustc,
        "ndk_version": revision,
        "android_api": config["android_api"],
        "reference_api": config["reference_api"],
        "artifacts": [],
    }
    target_dir = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    if not target_dir.is_absolute():
        target_dir = ROOT / target_dir
    for target in args.target or list(config["targets"]):
        env = os.environ.copy()
        env["CARGO_TARGET_" + target.upper().replace("-", "_") + "_LINKER"] = str(clang)
        flags = env.get("CARGO_ENCODED_RUSTFLAGS")
        rustflags = flags.split("\x1f") if flags else shlex.split(env.get("RUSTFLAGS", ""))
        rustflags += [
            "-C",
            f"link-arg=--target={config['targets'][target]}{config['android_api']}",
            "-C",
            f"link-arg=-Wl,-z,max-page-size={config['elf_page_size']}",
        ]
        env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(rustflags)
        execute(["cargo", "build", "--release", "--locked", "--target", target], env=env)
        binary = target_dir / target / "release/carlo"
        header = execute([str(readelf), "--file-header", str(binary)])
        expected = "AArch64" if target.startswith("aarch64") else "X86-64"
        if "DYN" not in header or expected not in header:
            raise ValueError("artifact is not the expected Android PIE ELF: " + target)
        program_headers = execute([str(readelf), "--program-headers", str(binary)])
        alignments = [
            int(line.split()[-1], 16)
            for line in program_headers.splitlines()
            if line.strip().startswith("LOAD ")
        ]
        if not alignments or any(value < config["elf_page_size"] for value in alignments):
            raise ValueError("ELF LOAD alignment is below configured page size")
        dynamic = execute([str(readelf), "--dynamic", str(binary)])
        needed = re.findall(r"\(NEEDED\).*?\[(.*?)\]", dynamic)
        destination = output / target / "carlo"
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(binary, destination)
        manifest["artifacts"].append(
            {
                "target": target,
                "file": str(destination.relative_to(output)),
                "sha256": hashlib.sha256(destination.read_bytes()).hexdigest(),
                "bytes": destination.stat().st_size,
                "needed": needed,
                "load_alignments": alignments,
            }
        )
        print(f"Verified {target}: {destination.stat().st_size} bytes", flush=True)
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(output / "manifest.json")


if __name__ == "__main__":
    main()
