"""Host-side validation of the display mirror smoke without a device.

Frames are synthetic renderings of the fixture marker; no screen content is used.
"""

import json
import random
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from android_session import SessionError
from smoke_display_mirror import (
    NOMINAL, REVERSED, SEQUENCE, analyse, capture, decode, locate_marker, validate_report,
)

REQUESTED = {"width": 360, "height": 640, "frames": 30}
WHITE, BLACK = (255, 255, 255), (0, 0, 0)


def stripes(width, sequence):
    """One row of equal stripes, with the same edges as the fixture drawable."""
    row = bytearray()
    for index, hue in enumerate(sequence):
        row += bytes(NOMINAL[hue]) * (width * (index + 1) // len(sequence)
                                      - width * index // len(sequence))
    return bytes(row)


def screen(width, height, upper=SEQUENCE, lower=REVERSED):
    """Row spans of a panel: status bar, both bands (None omits one), text, white."""
    status = height // 30
    band, gap = (height - status) // 10, (height - status) // 40
    white = bytes(WHITE) * width
    text = white[: 3 * (width // 4)] + bytes((40, 40, 40)) * (width // 2)
    text += white[: 3 * width - len(text)]
    spans = [(status, bytes((90, 90, 96)) * width),
             (band, stripes(width, upper) if upper else white), (gap, white),
             (band, stripes(width, lower) if lower else white)]
    used = sum(rows for rows, _ in spans)
    middle = height // 2 - used
    spans += [(middle, white), (height // 40, text)]
    spans.append((height - sum(rows for rows, _ in spans), white))
    return spans


def render(width, height, **bands):
    return b"".join(row * rows for rows, row in screen(width, height, **bands))


def box_row(row, source, target):
    """Box-filter one RGB row from `source` to `target` pixels."""
    out = bytearray()
    for x in range(target):
        first, last = x * source // target, max((x + 1) * source // target, x * source // target + 1)
        for channel in range(3):
            values = row[3 * first + channel : 3 * last : 3]
            out.append(sum(values) // len(values))
    return bytes(out)


def mirrored(panel=(1080, 2340), target=(360, 640), **bands):
    """The panel box-downscaled to fit `target`, centred on black bars."""
    scale = min(target[0] / panel[0], target[1] / panel[1])
    width, height = round(panel[0] * scale), round(panel[1] * scale)
    left, top = (target[0] - width) // 2, (target[1] - height) // 2
    kinds, owners = [], []
    for rows, row in screen(*panel, **bands):
        kinds.append(box_row(row, panel[0], width))
        owners += [len(kinds) - 1] * rows
    black = bytes(3 * target[0])
    pad_left, pad_right = bytes(3 * left), bytes(3 * (target[0] - width - left))
    blended, out = {}, bytearray(black * top)
    for y in range(height):
        covered = owners[y * panel[1] // height : max((y + 1) * panel[1] // height,
                                                      y * panel[1] // height + 1)]
        key = tuple(covered.count(kind) for kind in range(len(kinds)))
        if key not in blended:
            total = sum(key)
            blended[key] = bytes(
                sum(weight * kinds[kind][i] for kind, weight in enumerate(key) if weight) // total
                for i in range(3 * width))
        out += pad_left + blended[key] + pad_right
    out += black * (target[1] - height - top)
    return bytes(out)


def degrade(frame, seed=7, noise=12):
    """Limited-range look, a warm tint and uniform noise, as a lossy path would add."""
    rng = random.Random(seed)
    gains = (0.86, 0.82, 0.78)
    table = [bytes(min(255, int(16 + gain * value)) for value in range(256)) for gain in gains]
    out = bytearray(frame)
    for channel in range(3):
        out[channel::3] = frame[channel::3].translate(table[channel])
    return bytes(max(0, min(255, value + rng.randint(-noise, noise))) for value in out)


def session(**changes):
    outcome = {
        "display_created": True, "display_id": 7, "display_released": True,
        "callbacks": {"paused": 0, "resumed": 1, "stopped": 1, "rejected": 0},
        "codec_name": "c2.test.avc.encoder", "output_width": 360, "output_height": 640,
        "elapsed_ms": 5300, "error": None, "release_error": None,
        "stream": {"end_of_stream": True, "config_packets": 1, "key_frames": 1,
                   "pts_non_decreasing": True,
                   "nal_units": {"sps": 1, "pps": 1, "idr": 1, "non_idr": 2, "other": 0}},
    }
    report = {"source": "display_mirror", "screen_capture": True, "video_available": False,
              "requested": REQUESTED, "outcome": outcome}
    for path, value in changes.items():
        target = report
        *parents, leaf = path.split("__")
        for key in parents:
            target = target[key]
        target[leaf] = value
    return {"exit_code": 0, "cleanup_removed": True, "stdout": json.dumps(report)}


STREAM = b"\x00\x00\x00\x01\x67\x42" b"\x00\x00\x00\x01\x68\xce" b"\x00\x00\x01\x65\x88"


class MarkerTests(unittest.TestCase):
    def test_found_at_native_size(self):
        marker = locate_marker(render(540, 1170), 540, 1170)
        self.assertEqual((marker["left"], marker["right"]), (0, 540))
        self.assertEqual(marker["stripe_width"], 67.5)
        self.assertLess(marker["forward_band"]["bottom"], marker["reversed_band"]["top"] + 1)
        self.assertEqual(marker["max_stripe_error"], 0)

    def test_found_when_downscaled_and_pillarboxed(self):
        marker = locate_marker(mirrored(), 360, 640)
        self.assertTrue(30 <= marker["left"] <= 34 and 326 <= marker["right"] <= 330, marker)
        self.assertTrue(55 <= marker["forward_band"]["bottom"] - marker["forward_band"]["top"] <= 63)
        self.assertLess(marker["max_stripe_error"], 4)
        # A landscape panel is letterboxed instead: bars above and below.
        marker = locate_marker(mirrored(panel=(2340, 1080)), 360, 640)
        self.assertEqual((marker["left"], marker["right"]), (0, 360))
        self.assertGreater(marker["forward_band"]["top"], 230)

    def test_found_with_noise_and_colour_shift(self):
        result = analyse(degrade(mirrored()), 360, 640)
        self.assertTrue(result["marker_found"])
        self.assertTrue(20 < result["marker"]["max_stripe_error"] < 72, result["marker"])

    def test_not_found_without_the_exact_pattern(self):
        swapped = list(SEQUENCE)
        swapped[2], swapped[3] = swapped[3], swapped[2]
        cases = {
            "blank": bytes(360 * 640 * 3),
            "white": bytes(WHITE) * (360 * 640),
            "wrong order": mirrored(upper=tuple(swapped)),
            "bands exchanged": mirrored(upper=REVERSED, lower=SEQUENCE),
            "same band twice": mirrored(lower=SEQUENCE),
            "only forward band": mirrored(lower=None),
            "only reversed band": mirrored(upper=None),
        }
        for name, frame in cases.items():
            self.assertIsNone(locate_marker(frame, 360, 640), name)
            with self.assertRaises(SessionError, msg=name) as error:
                analyse(frame, 360, 640)
            self.assertIn("not found", str(error.exception))

    def test_last_frame_decides_and_sizes_are_checked(self):
        good, blank = mirrored(), bytes(360 * 640 * 3)
        result = analyse(blank + good, 360, 640)
        self.assertEqual((result["frames"], result["frames_with_marker"]), (2, 1))
        for bad in [good + blank, good[:-3], b""]:
            with self.assertRaises(SessionError):
                analyse(bad, 360, 640)
        # Right hues, but too desaturated to be our pure colours: rejected by the
        # colour error when still classifiable, and not classified at all when paler.
        washed = bytes(128 + (value - 128) // 3 for value in good)
        with self.assertRaises(SessionError) as error:
            analyse(washed, 360, 640)
        self.assertIn("out of tolerance", str(error.exception))
        pale = bytes(128 + (value - 128) // 5 for value in good)
        self.assertIsNone(locate_marker(pale, 360, 640))


class ReportTests(unittest.TestCase):
    def test_complete_report_passes_and_each_missing_proof_fails(self):
        self.assertEqual(validate_report(session())["requested"], REQUESTED)
        for change in [
            {"screen_capture": False}, {"video_available": True},
            {"source": "synthetic_egl_gles"},
            {"outcome__display_released": False}, {"outcome__display_created": False},
            {"outcome__release_error": {"kind": "binder"}},
            {"outcome__error": {"kind": "timeout"}},
            {"outcome__stream__nal_units": {"sps": 1, "pps": 1, "idr": 0, "non_idr": 0}},
            {"outcome__stream__nal_units": {"sps": 0, "pps": 1, "idr": 1, "non_idr": 0}},
            {"outcome__codec_name": None}, {"outcome__output_width": 320},
        ]:
            with self.assertRaises(SessionError, msg=change):
                validate_report(session(**change))
        for key, value in [("cleanup_removed", False), ("exit_code", 69)]:
            failed = session()
            failed[key] = value
            with self.assertRaises(SessionError):
                validate_report(failed)


class CleanupTests(unittest.TestCase):
    def run_capture(self, frame, dump_changes=None, stream=STREAM, decoder_error=None):
        seen = {"commands": []}

        def run(adb, manifest, command, timeout, physical, binary_output=None):
            seen["commands"].append((command, physical))
            if command == "display-mirror-probe":
                return session()
            seen["stream"] = binary_output
            binary_output.write_bytes(stream)
            return {"exit_code": 0, "cleanup_removed": True, "stderr": "", "emulator": True,
                    "android_api": 34, "abi": "x86_64", "stdout_bytes": len(stream),
                    "stdout_sha256": "0" * 64, **(dump_changes or {})}

        def decoder(path, requested, coded):
            seen["coded"] = coded
            # A decoder that spills pixels beside the stream must not leave them either.
            path.with_name("frames.rgb").write_bytes(frame)
            if decoder_error:
                raise decoder_error
            return {"codec_name": "h264", "nb_read_frames": "1"}, frame

        try:
            return seen, capture(object(), Path("unused"), False, run, decoder)
        except Exception as error:  # noqa: BLE001 - returned for assertions
            return seen, error

    def assert_removed(self, seen):
        self.assertFalse(seen["stream"].exists())
        self.assertFalse(seen["stream"].parent.exists())

    def test_stream_and_pixels_removed_on_success(self):
        seen, summary = self.run_capture(mirrored())
        self.assert_removed(seen)
        self.assertEqual(seen["commands"],
                         [("display-mirror-probe", False), ("display-mirror-dump", False)])
        self.assertEqual(seen["coded"], 1)
        self.assertTrue(summary["host_decode"]["marker_found"])
        text = json.dumps(summary)
        self.assertNotIn("stdout_sha256", text)
        self.assertNotIn(str(seen["stream"].parent), text)
        self.assertLess(len(text), 4000)  # statistics only, never frame bytes

    def test_stream_and_pixels_removed_on_every_failure(self):
        failures = [
            {"frame": bytes(360 * 640 * 3)},
            {"frame": mirrored(), "decoder_error": subprocess.CalledProcessError(1, ["ffmpeg"])},
            {"frame": mirrored(), "dump_changes": {"exit_code": 69}},
            {"frame": mirrored(), "dump_changes": {"stderr": "diagnostic"}},
            {"frame": mirrored(), "stream": b"\x00\x00\x01\x67\x42"},
        ]
        for failure in failures:
            seen, error = self.run_capture(**failure)
            self.assertIsInstance(error, (SessionError, subprocess.CalledProcessError), failure.keys())
            self.assert_removed(seen)

    def test_rejected_probe_never_dumps(self):
        commands = []

        def run(adb, manifest, command, timeout, physical, binary_output=None):
            commands.append(command)
            return session(screen_capture=False)

        with self.assertRaises(SessionError):
            capture(object(), Path("unused"), True, run, None)
        self.assertEqual(commands, ["display-mirror-probe"])


def software_encoder():
    """A software H.264 encoder of the host ffmpeg, as a stand-in for the device codec."""
    if shutil.which("ffmpeg") is None or shutil.which("ffprobe") is None:
        return None
    encoders = subprocess.run(["ffmpeg", "-v", "error", "-encoders"], capture_output=True,
                              text=True, timeout=30, check=False).stdout
    return next((name for name in ("libx264", "libopenh264") if name in encoders), None)


ENCODER = software_encoder()


@unittest.skipUnless(ENCODER, "needs ffmpeg/ffprobe with libx264 or libopenh264")
class RoundTripTests(unittest.TestCase):
    def test_marker_survives_scaling_and_h264_at_the_preset_bitrate(self):
        panel = render(1080, 2340)
        with tempfile.TemporaryDirectory() as directory:
            stream = Path(directory) / "synthetic.h264"
            subprocess.run(
                ["ffmpeg", "-v", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", "1080x2340",
                 "-r", "15", "-i", "-", "-vf",
                 "scale=360:640:force_original_aspect_ratio=decrease,"
                 "pad=360:640:(ow-iw)/2:(oh-ih)/2:black",
                 "-c:v", ENCODER, "-pix_fmt", "yuv420p", "-b:v", "1500k",
                 "-maxrate", "1500k", "-bufsize", "1500k", "-f", "h264", str(stream)],
                input=panel * 3, capture_output=True, timeout=120, check=True)
            facts, frames = decode(stream, REQUESTED, 3)
        result = analyse(frames, 360, 640)
        self.assertEqual((facts["codec_name"], result["frames"], result["frames_with_marker"]),
                         ("h264", 3, 3))
        self.assertTrue(30 <= result["marker"]["left"] <= 34, result["marker"])
        self.assertLess(result["marker"]["max_stripe_error"], 40)


if __name__ == "__main__":
    unittest.main()
