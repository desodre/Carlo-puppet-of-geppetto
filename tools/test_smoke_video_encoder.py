"""Host-side validation of the synthetic encoder evidence without a device."""

import json
import unittest

from android_session import SessionError
from smoke_video_encoder import bar_left, nal_units, validate_pixels, validate_report, validate_stream

REQUESTED = {"width": 16, "height": 4, "frames": 4}


def session(**changes):
    outcome = {
        "codec_name": "c2.test.avc.encoder", "output_width": 16, "output_height": 4,
        "frames_submitted": 4, "error": None,
        "stream": {"end_of_stream": True, "config_packets": 1, "key_frames": 1,
                   "pts_non_decreasing": True,
                   "nal_units": {"sps": 1, "pps": 1, "idr": 1, "non_idr": 3, "other": 0}},
    }
    report = {"source": "synthetic_egl_gles", "screen_capture": False, "video_available": False,
              "requested": REQUESTED, "outcome": outcome}
    for path, value in changes.items():
        target = report
        *parents, leaf = path.split("__")
        for key in parents:
            target = target[key]
        target[leaf] = value
    return {"exit_code": 0, "cleanup_removed": True, "stdout": json.dumps(report)}


def frames():
    data = bytearray()
    for index in range(REQUESTED["frames"]):
        phase = index / REQUESTED["frames"]
        left = bar_left(REQUESTED, index)
        row = bytearray()
        for x in range(REQUESTED["width"]):
            row += bytes([255] * 3 if left <= x < left + 2
                         else [int(255 * phase), 64, int(255 * (1 - phase))])
        data += row * REQUESTED["height"]
    return bytes(data)


class VideoSmokeTests(unittest.TestCase):
    def test_complete_report_passes_and_each_missing_proof_fails(self):
        self.assertEqual(validate_report(session())["requested"], REQUESTED)
        for change in [
            {"screen_capture": True}, {"video_available": True}, {"source": "display"},
            {"outcome__error": {"kind": "timeout"}}, {"outcome__frames_submitted": 3},
            {"outcome__codec_name": None}, {"outcome__output_width": 8},
            {"outcome__stream__end_of_stream": False},
            {"outcome__stream__nal_units": {"sps": 0, "pps": 1, "idr": 1, "non_idr": 3}},
            {"outcome__stream__nal_units": {"sps": 1, "pps": 1, "idr": 1, "non_idr": 2}},
        ]:
            with self.assertRaises(SessionError, msg=change):
                validate_report(session(**change))
        failed = session()
        failed["cleanup_removed"] = False
        with self.assertRaises(SessionError):
            validate_report(failed)

    def test_host_nal_scan_is_independent_and_rejects_malformed_streams(self):
        stream = (b"\x00\x00\x00\x01\x67\x42" b"\x00\x00\x00\x01\x68\xce" b"\x00\x00\x01\x65\x88"
                  + b"\x00\x00\x01\x41\x9a" * 3)
        self.assertEqual(validate_stream(stream, REQUESTED)["non_idr"], 3)
        self.assertEqual(nal_units(b"\x00\x00\x01\x06")["other"], 1)
        for bad in [b"\x65\x88", b"\x00\x00\x01", b"\x00\x00\x01\xe5", b""]:
            with self.assertRaises(SessionError):
                nal_units(bad)
        with self.assertRaises(SessionError):
            validate_stream(stream[:-5], REQUESTED)

    def test_decoded_pixels_must_show_bar_and_color_ramp(self):
        good = frames()
        self.assertTrue(validate_pixels(good, REQUESTED)["background_ramp"])
        with self.assertRaises(SessionError):
            validate_pixels(good[:-3], REQUESTED)
        with self.assertRaises(SessionError):
            validate_pixels(bytes(len(good)), REQUESTED)


if __name__ == "__main__":
    unittest.main()
