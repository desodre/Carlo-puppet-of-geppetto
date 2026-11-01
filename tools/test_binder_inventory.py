"""Export parsing and target guards without requiring a device or NDK."""

import unittest
from pathlib import Path
from types import SimpleNamespace

from android_session import SessionError
from binder_inventory import collect, parse_exports


class FakeAdb:
    def __init__(self, properties):
        self.properties = properties
        self.calls = []

    def shell(self, args):
        self.calls.append(args)
        return SimpleNamespace(stdout=self.properties[args[-1]])

    def run(self, args):
        raise AssertionError("must refuse before pulling any device files")


class InventoryTests(unittest.TestCase):
    def test_preserves_versions_and_unversioned_exports(self):
        self.assertEqual(
            parse_exports("AIBinder_ping@@LIBBINDER_NDK T ffe0 73\nplain W a 1\n"),
            [{"name": "AIBinder_ping", "version": "LIBBINDER_NDK"},
             {"name": "plain", "version": None}],
        )

    def test_rejects_empty_or_malformed_output_instead_of_claiming_missing_exports(self):
        for text in ["", "llvm-nm: error", "name TYPE 10 20", "symbol T"]:
            with self.subTest(text=text), self.assertRaises(ValueError):
                parse_exports(text)

    def test_physical_device_requires_opt_in_before_library_pull(self):
        adb = FakeAdb({"ro.kernel.qemu": "0"})
        with self.assertRaises(SessionError):
            collect(adb, Path("/unused"))
        self.assertEqual(len(adb.calls), 1)

    def test_other_api_or_abi_refused_before_library_pull(self):
        for api, abi in [(35, "x86_64"), (34, "armeabi-v7a")]:
            adb = FakeAdb({"ro.kernel.qemu": "1", "ro.build.version.sdk": str(api),
                           "ro.product.cpu.abi": abi})
            with self.assertRaises(SessionError):
                collect(adb, Path("/unused"))


if __name__ == "__main__":
    unittest.main()
