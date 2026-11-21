#!/usr/bin/env python3
"""Prove the mirrored display shows our fixture: find its stripe marker in the decoded video.

The dumped stream and its decoded pixels are screen contents. They live only in a
private temporary directory and in memory, and are always deleted; the summary
holds counters and statistics only.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

from android_session import ROOT, Adb, SessionError, run_session
from smoke_video_encoder import nal_units

PACKAGE = "org.geppetto.carlo.fixture"

# Hue classes at multiples of 60 degrees; the marker uses only these pure colours.
RED, YELLOW, GREEN, CYAN, BLUE, MAGENTA = range(6)
NOMINAL = {
    RED: (255, 0, 0), YELLOW: (255, 255, 0), GREEN: (0, 255, 0),
    CYAN: (0, 255, 255), BLUE: (0, 0, 255), MAGENTA: (255, 0, 255),
}
# Top band, left to right; the band below it shows the same sequence reversed.
# Mirrors tools/fixture/.../MarkerDrawable.java.
SEQUENCE = (RED, GREEN, BLUE, YELLOW, MAGENTA, CYAN, RED, BLUE)
REVERSED = SEQUENCE[::-1]

MIN_CHROMA = 70        # max-min channel; rejects grey, pastel and dark pixels
MIN_VALUE = 110        # brightest channel
HUE_TOLERANCE = 24     # degrees around each class centre; 12 degrees of dead zone
MIN_RUNS = (3, 8, 20, 50)  # shortest run kept, tried in turn: drops edge blends
MIN_STRIPE = 5         # pixels; narrower stripes cannot be told from noise
MIN_BAND_ROWS = 6
EDGE_TOLERANCE = 4     # pixels between rows of a band and between both bands
MAX_COLOUR_ERROR = 72  # mean absolute error per channel of a stripe's core
MAX_FRAMES = 64


def classify(red: int, green: int, blue: int) -> int:
    """Hue class of a saturated bright pixel, or -1."""
    high, low = max(red, green, blue), min(red, green, blue)
    chroma = high - low
    if chroma < MIN_CHROMA or high < MIN_VALUE:
        return -1
    if high == red:
        hue = 60 * (green - blue) / chroma
    elif high == green:
        hue = 120 + 60 * (blue - red) / chroma
    else:
        hue = 240 + 60 * (red - green) / chroma
    nearest = round(hue / 60)
    if abs(hue - 60 * nearest) > HUE_TOLERANCE:
        return -1
    return nearest % 6


def row_runs(row: bytes, cache: dict) -> list:
    """Run-length encode the hue classes of one RGB row as (class, start, end)."""
    runs = []
    current, start = -2, 0
    for x in range(len(row) // 3):
        pixel = row[3 * x : 3 * x + 3]
        hue = cache.get(pixel)
        if hue is None:
            hue = cache[pixel] = classify(*pixel)
        if hue != current:
            if current >= 0:
                runs.append((current, start, x))
            current, start = hue, x
    if current >= 0:
        runs.append((current, start, len(row) // 3))
    return runs


def match_row(runs: list):
    """Find the stripe sequence in one row: (reversed?, left, right) or None."""
    for minimum in MIN_RUNS:
        kept = []
        for run in runs:
            if run[2] - run[1] < minimum:
                continue
            if kept and kept[-1][0] == run[0] and run[1] - kept[-1][2] <= 2:
                kept[-1] = (run[0], kept[-1][1], run[2])
            else:
                kept.append(run)
        count = len(SEQUENCE)
        for first in range(len(kept) - count + 1):
            window = kept[first : first + count]
            hues = tuple(run[0] for run in window)
            if hues != SEQUENCE and hues != REVERSED:
                continue
            widths = sorted(run[2] - run[1] for run in window)
            median = widths[count // 2]
            gaps = [window[i + 1][1] - window[i][2] for i in range(count - 1)]
            if (median >= MIN_STRIPE and widths[0] >= 0.6 * median - 1
                    and widths[-1] <= 1.4 * median + 1 and max(gaps) <= max(2, median // 3)):
                return hues == REVERSED, window[0][1], window[-1][2]
    return None


def find_bands(frame: bytes, width: int, height: int) -> list:
    """Group consecutive matching rows into bands of one direction and extent."""
    cache: dict = {}
    bands = []
    open_band = None
    for y in range(height):
        match = match_row(row_runs(frame[3 * width * y : 3 * width * (y + 1)], cache))
        if (open_band is not None and match is not None and match[0] == open_band["reversed"]
                and abs(match[1] - open_band["left"]) <= EDGE_TOLERANCE
                and abs(match[2] - open_band["right"]) <= EDGE_TOLERANCE
                and y - open_band["bottom"] <= 1):
            open_band["bottom"] = y + 1
            continue
        if match is not None:
            open_band = {"reversed": match[0], "left": match[1], "right": match[2],
                         "top": y, "bottom": y + 1}
            bands.append(open_band)
    return [band for band in bands if band["bottom"] - band["top"] >= MIN_BAND_ROWS]


def stripe_errors(frame: bytes, width: int, band: dict) -> list:
    """Mean absolute channel error of each stripe's core against its nominal colour."""
    sequence = REVERSED if band["reversed"] else SEQUENCE
    stripe = (band["right"] - band["left"]) / len(sequence)
    rows = band["bottom"] - band["top"]
    top, bottom = band["top"] + rows // 4, band["bottom"] - rows // 4
    errors = []
    for index, hue in enumerate(sequence):
        left = int(band["left"] + (index + 0.25) * stripe)
        right = max(int(band["left"] + (index + 0.75) * stripe), left + 1)
        total, samples = 0, 0
        for y in range(top, max(bottom, top + 1)):
            core = frame[3 * (width * y + left) : 3 * (width * y + right)]
            for channel in range(3):
                nominal = NOMINAL[hue][channel]
                total += sum(abs(value - nominal) for value in core[channel::3])
            samples += len(core)
        errors.append(round(total / samples, 1))
    return errors


def locate_marker(frame: bytes, width: int, height: int):
    """Geometry and colour statistics of the marker in one RGB frame, or None.

    Requires the forward band directly above the reversed one, with matching
    extents and similar heights; letterbox offset and scale do not matter.
    """
    if len(frame) != width * height * 3:
        raise SessionError("RGB frame size does not match its dimensions")
    bands = find_bands(frame, width, height)
    for upper in bands:
        if upper["reversed"]:
            continue
        for lower in bands:
            tall = upper["bottom"] - upper["top"]
            low = lower["bottom"] - lower["top"]
            slack = EDGE_TOLERANCE + (upper["right"] - upper["left"]) // 50
            if not (lower["reversed"] and 0 <= lower["top"] - upper["bottom"] <= max(tall, low)
                    and abs(lower["left"] - upper["left"]) <= slack
                    and abs(lower["right"] - upper["right"]) <= slack
                    and 0.6 * tall <= low <= tall / 0.6):
                continue
            errors = {"forward": stripe_errors(frame, width, upper),
                      "reversed": stripe_errors(frame, width, lower)}
            return {
                "left": upper["left"], "right": upper["right"],
                "stripe_width": round((upper["right"] - upper["left"]) / len(SEQUENCE), 2),
                "forward_band": {"top": upper["top"], "bottom": upper["bottom"]},
                "reversed_band": {"top": lower["top"], "bottom": lower["bottom"]},
                "stripe_mean_abs_error": errors,
                "max_stripe_error": max(errors["forward"] + errors["reversed"]),
            }
    return None


def analyse(frames: bytes, width: int, height: int) -> dict:
    """Statistics over all decoded frames; the last frame must show the marker."""
    size = width * height * 3
    if size == 0 or len(frames) % size or not 1 <= len(frames) // size <= MAX_FRAMES:
        raise SessionError("decoder did not produce a bounded whole number of RGB frames")
    count = len(frames) // size
    found = [locate_marker(frames[index * size : (index + 1) * size], width, height)
             for index in range(count)]
    marker = found[-1]
    result = {"frames": count, "frames_with_marker": sum(item is not None for item in found),
              "marker_found": marker is not None, "marker": marker}
    if marker is None:
        raise SessionError(
            "fixture marker not found in the last mirrored frame (both stripe bands, in order); "
            "screen off, locked or covered? frame contents are not reported")
    if marker["max_stripe_error"] > MAX_COLOUR_ERROR:
        raise SessionError("fixture marker found but its stripe colours are out of tolerance")
    return result


def validate_report(session: dict) -> dict:
    if session["exit_code"] != 0 or not session["cleanup_removed"]:
        raise SessionError("display mirror probe failed or left its session directory")
    report = json.loads(session["stdout"])
    outcome, requested = report["outcome"], report["requested"]
    units = outcome["stream"]["nal_units"]
    if report["source"] != "display_mirror" or report["screen_capture"] is not True:
        raise SessionError("probe must declare a completed display mirror capture")
    if report["video_available"] is not False:
        raise SessionError("mirror proof must not announce a video capability")
    if not (
        outcome["error"] is None
        and outcome["release_error"] is None
        and outcome["display_created"] is True
        and outcome["display_released"] is True
        and units["idr"] + units["non_idr"] >= 1
        and units["sps"] >= 1
        and units["pps"] >= 1
        and units["idr"] >= 1
        and outcome["codec_name"]
        and (outcome["output_width"], outcome["output_height"])
        == (requested["width"], requested["height"])
    ):
        raise SessionError("mirror report does not prove a created, released, decodable capture")
    return report


def decode(path: Path, requested: dict, coded_frames: int) -> tuple:
    """Decode the stream on the host: (ffprobe facts, RGB frames kept in memory)."""
    probe = subprocess.run(
        ["ffprobe", "-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries",
         "stream=codec_name,profile,width,height,pix_fmt,color_range,color_space,nb_read_frames",
         "-of", "json", str(path)],
        capture_output=True, text=True, timeout=60, check=True,
    )
    stream = json.loads(probe.stdout)["streams"][0]
    if not (
        stream["codec_name"] == "h264"
        and (stream["width"], stream["height"]) == (requested["width"], requested["height"])
        and int(stream["nb_read_frames"]) == coded_frames
    ):
        raise SessionError("host decoder disagrees with codec, size or coded frame count")
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-xerror", "-i", str(path), "-f", "rawvideo",
         "-pix_fmt", "rgb24", "-"],
        capture_output=True, timeout=120, check=True,
    )
    return stream, raw.stdout


def capture(adb, manifest: Path, physical: bool, run=run_session, decoder=decode) -> dict:
    """Probe, dump, decode and locate; the stream never outlives this call."""
    report = validate_report(run(adb, manifest, "display-mirror-probe", 40, physical))
    requested = report["requested"]
    scratch = Path(tempfile.mkdtemp(prefix="carlo-mirror-"))
    try:
        stream_path = scratch / "mirror.h264"
        dump = run(adb, manifest, "display-mirror-dump", 40, physical, stream_path)
        if dump["exit_code"] != 0 or not dump["cleanup_removed"] or dump["stderr"]:
            raise SessionError("stream dump failed, wrote diagnostics or left its directory")
        counts = nal_units(stream_path.read_bytes())
        coded = counts["idr"] + counts["non_idr"]
        if not (counts["sps"] >= 1 and counts["pps"] >= 1 and counts["idr"] >= 1
                and 1 <= coded <= MAX_FRAMES):
            raise SessionError("dumped stream lacks parameter sets, an IDR or a bounded frame count")
        ffprobe, frames = decoder(stream_path, requested, coded)
        located = analyse(frames, requested["width"], requested["height"])
        del frames
        return {
            "emulator": dump["emulator"], "android_api": dump["android_api"], "abi": dump["abi"],
            "probe_report": report,
            "dump": {"stdout_bytes": dump["stdout_bytes"], "cleanup_removed": True},
            "host_nal_units": counts,
            "host_decode": {"ffprobe": ffprobe, **located},
        }
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
        if scratch.exists():
            raise SessionError("captured stream directory was not removed: " + str(scratch))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--manifest", type=Path, default=ROOT / "dist/manifest.json")
    parser.add_argument("--fixture", type=Path, default=ROOT / "dist/node-fixture/fixture.apk")
    parser.add_argument("--output", type=Path, default=ROOT / "results/display-mirror")
    parser.add_argument("--allow-physical", action="store_true",
                        help="the owner authorized this physical device for this run")
    args = parser.parse_args()
    if shutil.which("ffprobe") is None or shutil.which("ffmpeg") is None:
        parser.error("host decode check requires ffmpeg and ffprobe")
    adb = Adb(args.serial)
    emulator = adb.shell(["getprop", "ro.kernel.qemu"]).stdout.strip() == "1"
    if not emulator and not args.allow_physical:
        parser.error("display mirror fixture smoke requires a disposable emulator "
                     "or explicit --allow-physical authorization")
    if adb.shell(["pm", "path", PACKAGE], check=False).stdout.strip():
        parser.error("fixture package already exists; refusing to replace it")
    fixture_manifest = json.loads(args.fixture.with_name("manifest.json").read_text())
    digest = hashlib.sha256(args.fixture.read_bytes()).hexdigest()
    if fixture_manifest["package"] != PACKAGE or fixture_manifest["sha256"] != digest:
        parser.error("fixture manifest/checksum mismatch")
    manifest = args.manifest.resolve()
    summary = {"serial": args.serial, "source": "display_mirror", "screen_capture": True,
               "fixture_sha256": digest, "agent_manifest": json.loads(manifest.read_text()),
               "ui_contents_recorded": False, "stream_and_pixels_deleted": True}
    try:
        adb.run(["install", "--no-streaming", str(args.fixture.resolve())], timeout=60)
        # A display that is off composes no frames; the fixture then keeps it on.
        adb.shell(["input", "keyevent", "KEYCODE_WAKEUP"])
        adb.shell(["am", "start", "-W", "-n", PACKAGE + "/.FixtureActivity"], timeout=30)
        time.sleep(1)  # let the launch transition settle before mirroring
        summary.update(capture(adb, manifest, args.allow_physical))
    finally:
        adb.run(["uninstall", PACKAGE], check=False)
        if adb.shell(["pm", "path", PACKAGE], check=False).stdout.strip():
            raise SessionError("owned fixture package was not removed")
        summary["fixture_removed"] = True
    args.output.mkdir(parents=True, exist_ok=True)
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print("Mirrored display decoded on the host and fixture marker located; "
          "stream deleted, fixture and sessions cleaned up.")


if __name__ == "__main__":
    try:
        main()
    except (SessionError, subprocess.SubprocessError, OSError, ValueError, KeyError) as error:
        raise SystemExit(f"carlo-mirror-smoke: {error}") from error
