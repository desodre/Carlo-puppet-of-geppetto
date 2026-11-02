#!/usr/bin/env python3
"""Build the controlled app under test (not an agent), without Gradle/network deps."""
from __future__ import annotations
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

def run(args):
    subprocess.run([str(arg) for arg in args], check=True, timeout=120, stdout=subprocess.PIPE)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", type=Path, default=Path(os.environ.get("ANDROID_SDK_ROOT", os.environ.get("ANDROID_HOME", ""))))
    args = parser.parse_args()
    sdk = args.sdk.resolve()
    tools = sdk / "build-tools/36.0.0"
    android = sdk / "platforms/android-35/android.jar"
    out = ROOT / "dist/node-fixture"
    out.mkdir(parents=True, exist_ok=True)
    for name in ["generated", "classes", "dex"]:
        path = out / name
        if path.exists(): shutil.rmtree(path)
        path.mkdir()
    fixture = ROOT / "tools/fixture"
    unsigned = out / "unsigned.apk"
    run([tools / "aapt", "package", "-f", "-M", fixture / "AndroidManifest.xml", "-S", fixture / "res", "-I", android, "-J", out / "generated", "-F", unsigned])
    java = sorted((fixture / "src").rglob("*.java")) + sorted((out / "generated").rglob("*.java"))
    run(["javac", "-encoding", "UTF-8", "--release", "8", "-classpath", android, "-d", out / "classes", *java])
    run([tools / "d8", "--min-api", "34", "--lib", android, "--output", out / "dex", *sorted((out / "classes").rglob("*.class"))])
    with zipfile.ZipFile(unsigned, "a") as apk:
        apk.write(out / "dex/classes.dex", "classes.dex")
    aligned = out / "aligned.apk"
    run([tools / "zipalign", "-f", "4", unsigned, aligned])
    key = out / "fixture.p12"
    if not key.exists():
        # Public disposable fixture signing password, never used for product releases.
        run(["keytool", "-genkeypair", "-keystore", key, "-storepass", "android", "-keypass", "android", "-alias", "fixture", "-keyalg", "RSA", "-keysize", "2048", "-validity", "3650", "-dname", "CN=Carlo Test Fixture"])
    apk = out / "fixture.apk"
    run([tools / "apksigner", "sign", "--ks", key, "--ks-pass", "pass:android", "--out", apk, aligned])
    run([tools / "apksigner", "verify", apk])
    manifest = {"package": "org.geppetto.carlo.fixture", "api": 34, "compile_sdk": 35, "build_tools": "36.0.0", "sha256": hashlib.sha256(apk.read_bytes()).hexdigest(), "role": "app under test, not Carlo agent", "javac": subprocess.check_output(["javac", "-version"], text=True, timeout=10).strip()}
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(apk)

if __name__ == "__main__": main()
