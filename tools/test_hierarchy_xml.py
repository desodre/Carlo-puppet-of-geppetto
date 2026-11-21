"""The hierarchy document contract, checked with a real XML parser.

`tests/fixtures/hierarchy_golden.xml` is the exact output of the Rust writer
(a Rust unit test compares it byte for byte), so parsing it here proves that
what Carlo emits is well-formed and round-trips the documented values.
"""

import re
import unittest
import xml.etree.ElementTree as ElementTree
from pathlib import Path

GOLDEN = Path(__file__).resolve().parents[1] / "tests/fixtures/hierarchy_golden.xml"
NODE_ATTRIBUTES = [
    "index",
    "text",
    "resource-id",
    "class",
    "package",
    "content-desc",
    "checkable",
    "checked",
    "clickable",
    "enabled",
    "focusable",
    "focused",
    "scrollable",
    "long-clickable",
    "password",
    "selected",
    "bounds",
]
WINDOW_ATTRIBUTES = ["window-id", "display-id", "window-type", "window-layer"]
HIERARCHY_ATTRIBUTES = [
    "rotation",
    "snapshot-id",
    "start-ms",
    "end-ms",
    "generation",
    "windows",
    "empty-windows",
    "nodes",
    "displays",
]
BOOLEANS = NODE_ATTRIBUTES[6:16]


class HierarchyXmlTests(unittest.TestCase):
    def setUp(self):
        self.raw = GOLDEN.read_bytes()
        self.root = ElementTree.fromstring(self.raw)

    def test_declaration_root_and_metadata(self):
        self.assertTrue(
            self.raw.startswith(b"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy ")
        )
        self.assertTrue(self.raw.endswith(b"</hierarchy>"))
        self.assertEqual(self.root.tag, "hierarchy")
        self.assertEqual(list(self.root.attrib), HIERARCHY_ATTRIBUTES)
        # Rotation is not available; it is never reported as a fake zero.
        self.assertEqual(self.root.get("rotation"), "unknown")
        self.assertEqual(self.root.get("displays"), "0:1080x2340 2:1920x1080")
        self.assertEqual(int(self.root.get("nodes")), len(list(self.root.iter("node"))))
        self.assertEqual(int(self.root.get("windows")), len(list(self.root)))
        self.assertLessEqual(int(self.root.get("start-ms")), int(self.root.get("end-ms")))

    def test_every_node_has_the_attributes_in_the_documented_order(self):
        roots = set(self.root)
        for node in self.root.iter("node"):
            expected = NODE_ATTRIBUTES + (WINDOW_ATTRIBUTES if node in roots else [])
            self.assertEqual(list(node.attrib), expected)
            for name in BOOLEANS:
                self.assertIn(node.get(name), ("true", "false"))
            self.assertRegex(node.get("bounds"), r"^\[-?\d+,-?\d+\]\[-?\d+,-?\d+\]$")
        for parent in self.root.iter("node"):
            self.assertEqual([child.get("index") for child in parent], [str(i) for i in range(len(parent))])

    def test_window_order_is_display_then_topmost_layer(self):
        windows = [
            (int(node.get("display-id")), int(node.get("window-layer")), node.get("window-id"))
            for node in self.root
        ]
        self.assertEqual(windows, [(0, 9, "12"), (0, 4, "11"), (2, 1, "30")])
        self.assertEqual(windows, sorted(windows, key=lambda item: (item[0], -item[1])))

    def test_values_round_trip_through_the_parser(self):
        nodes = list(self.root.iter("node"))
        button = next(node for node in nodes if node.get("class") == "android.widget.Button")
        self.assertEqual(button.get("text"), "Ação <&> \"quoted\" 'single'")
        self.assertEqual(button.get("resource-id"), "org.geppetto.carlo.fixture:id/botão")
        # TAB, LF and CR survive attribute-value normalisation as references.
        self.assertEqual(button.get("content-desc"), "line one\nline two\ttab\rreturn")
        self.assertEqual(button.get("bounds"), "[40,200][520,320]")
        self.assertTrue(all(button.get(name) == "true" for name in BOOLEANS))
        texts = [node.get("text") for node in nodes]
        self.assertIn("emoji 🪵 👩‍👩‍👧‍👦 終 \U0010ffff", texts)
        # Characters XML 1.0 cannot carry were replaced, not dropped or escaped.
        self.assertIn("nul� bell� noncharacter�", texts)
        self.assertIsNone(re.search(rb"[\x00-\x08\x0b\x0c\x0e-\x1f]", self.raw))
        # Structural nodes without text or class are kept with empty strings.
        structural = [node for node in nodes if node.get("class") == ""]
        self.assertEqual(len(structural), 1)
        self.assertEqual(structural[0].get("text"), "")
        scroll = next(node for node in nodes if node.get("scrollable") == "true" and len(node))
        self.assertEqual(scroll.get("bounds"), "[-20,320][1080,2000]")
        self.assertEqual(len(scroll), 2)


if __name__ == "__main__":
    unittest.main()
