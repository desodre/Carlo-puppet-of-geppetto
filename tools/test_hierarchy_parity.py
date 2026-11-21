"""Host-side tests of the hierarchy contract and parity checks on hand-written XML.

Every document here is synthetic; no screen content is used.
"""

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock
from xml.sax.saxutils import quoteattr

import hierarchy_parity
from hierarchy_parity import (
    BOOLEANS, MAX_BYTES, NODE_ATTRIBUTES, WINDOW_ATTRIBUTES, ParityError, evaluate, parse,
    validate_carlo,
)

SENTINEL = "ZQX_SECRET_SENTINEL"
HEADER = "<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>"
DEFAULTS = {"text": "", "resource-id": "", "class": "android.view.View", "package": "pkg.app",
            "content-desc": "", **{name: "false" for name in BOOLEANS}, "enabled": "true",
            "bounds": "[0,0][100,200]"}


def node(children=(), order=None, drop=(), extra=None, **fields):
    """A node spec; attribute names use `_` for `-`. Index is assigned by its parent."""
    values = dict(DEFAULTS)
    values.update({name.replace("_", "-"): value for name, value in fields.items()})
    return {"values": values, "children": list(children), "order": order, "drop": drop,
            "extra": extra or {}}


def render(spec, index, window=None):
    values = {"index": str(index), **spec["values"]}
    if "index" in spec["values"]:
        values["index"] = spec["values"]["index"]
    names = list(spec["order"] or NODE_ATTRIBUTES)
    if window is not None:
        values.update(window)
        names += [name for name in WINDOW_ATTRIBUTES if name in window]
    values.update(spec["extra"])
    names += list(spec["extra"])
    attributes = " ".join(f"{name}={quoteattr(values[name])}" for name in names
                          if name not in spec["drop"])
    inner = "".join(render(child, position) for position, child in enumerate(spec["children"]))
    return f"<node {attributes}>{inner}</node>" if inner else f"<node {attributes} />"


def count(spec):
    return 1 + sum(count(child) for child in spec["children"])


def carlo(*windows, nodes=None, window_count=None, window_fields=None):
    body = ""
    for position, window in enumerate(windows):
        fields = {"window-id": str(40 + position), "display-id": "0",
                  "window-type": "application", "window-layer": str(len(windows) - position)}
        fields.update(window_fields or {})
        body += render(window, 0, fields)  # every window root has index 0
    total = sum(count(window) for window in windows) if nodes is None else nodes
    shown = len(windows) if window_count is None else window_count
    return (f'{HEADER}<hierarchy rotation="unknown" snapshot-id="7" start-ms="10" end-ms="12" '
            f'generation="1" windows="{shown}" nodes="{total}">{body}</hierarchy>').encode()


def reference(root, rotation="0"):
    return (f"{HEADER}<hierarchy rotation=\"{rotation}\">{render(root, 0)}</hierarchy>").encode()


def app(**root_fields):
    """A small application window: root > [toolbar, list > [row, row]]."""
    return node([
        node(class_="android.widget.Toolbar", text="Title", bounds="[0,0][100,20]"),
        node([node(class_="android.widget.TextView", text="one", bounds="[0,20][100,40]"),
              node(class_="android.widget.TextView", text="two", bounds="[0,40][100,60]")],
             class_="android.widget.ListView", scrollable="true", bounds="[0,20][100,200]"),
    ], class_="android.widget.FrameLayout", resource_id="pkg.app:id/root", **root_fields)


def codes(document):
    return list(validate_carlo(*parse(document, "carlo")))


class Parity(unittest.TestCase):
    def test_identical_trees(self):
        result = evaluate(carlo(app()), reference(app()))
        self.assertTrue(result["parity"])
        self.assertTrue(result["carlo_valid"])
        self.assertEqual(result["validation_errors"], [])
        self.assertEqual((result["reference_nodes"], result["matched_nodes"]), (5, 5))
        self.assertEqual((result["carlo_window_nodes"], result["carlo_only_nodes"]), (5, 0))
        self.assertEqual(result["unmatched_reference_nodes"], 0)
        self.assertEqual(result["attribute_mismatches"], {})
        self.assertEqual(result["index_differences"], 0)

    def test_reference_pruned_of_invisible_children(self):
        full = app()
        full["children"].insert(0, node(class_="android.view.ViewStub", bounds="[0,0][0,0]"))
        full["children"][2]["children"].insert(
            1, node(class_="android.widget.TextView", text="hidden", bounds="[0,60][100,60]"))
        result = evaluate(carlo(full), reference(app()))
        self.assertTrue(result["parity"])
        self.assertEqual(result["matched_nodes"], 5)
        self.assertEqual((result["carlo_window_nodes"], result["carlo_only_nodes"]), (7, 2))
        # Pruning shifted three reference positions; reported, not a failure.
        self.assertEqual(result["index_differences"], 3)

    def test_reference_keeping_original_indexes_has_no_index_difference(self):
        full = app()
        full["children"].insert(0, node(class_="android.view.ViewStub", bounds="[0,0][0,0]"))
        pruned = app()
        pruned["children"][0]["values"]["index"] = "1"
        pruned["children"][1]["values"]["index"] = "2"
        result = evaluate(carlo(full), reference(pruned))
        self.assertTrue(result["parity"])
        self.assertEqual(result["index_differences"], 0)

    def test_attribute_mismatch_counted_by_name_without_values(self):
        ours, theirs = app(), app()
        ours["children"][0]["values"]["text"] = SENTINEL + "_carlo"
        theirs["children"][0]["values"]["text"] = SENTINEL + "_reference"
        theirs["children"][1]["values"]["scrollable"] = "false"
        theirs["children"][1]["children"][0]["values"]["content-desc"] = SENTINEL
        theirs["children"][1]["children"][1]["values"]["bounds"] = "[0,40][99,60]"
        result = evaluate(carlo(ours), reference(theirs))
        self.assertFalse(result["parity"])
        self.assertEqual(result["matched_nodes"], 5)
        self.assertEqual(result["attribute_mismatches"],
                         {"bounds": 1, "content-desc": 1, "scrollable": 1, "text": 1})
        dumped = json.dumps(result)
        for secret in (SENTINEL, "android.widget", "pkg.app", "[0,40]", "Title"):
            self.assertNotIn(secret, dumped)

    def test_reference_node_absent_from_carlo(self):
        theirs = app()
        theirs["children"].append(node(class_="android.widget.Button", bounds="[0,0][5,5]"))
        result = evaluate(carlo(app()), reference(theirs))
        self.assertFalse(result["parity"])
        self.assertEqual((result["reference_nodes"], result["matched_nodes"]), (6, 5))
        self.assertEqual(result["unmatched_reference_nodes"], 1)

    def test_reordered_siblings_are_not_a_subsequence(self):
        theirs = app()
        theirs["children"].reverse()
        result = evaluate(carlo(app()), reference(theirs))
        self.assertFalse(result["parity"])
        self.assertGreaterEqual(result["unmatched_reference_nodes"], 1)

    def test_multi_window_document_picks_the_matching_window(self):
        status = node([node(class_="android.widget.TextView", text="12:00")],
                      class_="android.widget.FrameLayout", package="com.android.systemui",
                      bounds="[0,0][100,10]")
        # Same root key as the reference, but a different subtree: a worse match.
        decoy = node([node(class_="android.widget.Button")],
                     class_="android.widget.FrameLayout", resource_id="pkg.app:id/root")
        result = evaluate(carlo(status, decoy, app()), reference(app()))
        self.assertTrue(result["parity"])
        self.assertEqual(result["candidate_windows"], 2)
        self.assertEqual((result["carlo_windows"], result["carlo_nodes"]), (3, 9))
        self.assertEqual((result["carlo_window_nodes"], result["carlo_only_nodes"]), (5, 0))

    def test_no_window_matches_the_reference_root(self):
        result = evaluate(carlo(app()), reference(app(bounds="[0,0][50,50]")))
        self.assertFalse(result["parity"])
        self.assertFalse(result["window_matched"])
        self.assertEqual((result["matched_nodes"], result["unmatched_reference_nodes"]), (0, 5))

    def test_reference_extra_attributes_are_ignored_and_counted_by_name(self):
        theirs = app()
        theirs["children"][0]["extra"] = {"NAF": "true", "drawing-order": "1"}
        theirs["extra"] = {"hint": SENTINEL, "display-id": "0", "sécret_" + SENTINEL: "x"}
        result = evaluate(carlo(app()), reference(theirs))
        self.assertTrue(result["parity"])
        self.assertEqual(result["reference_extra_attributes"]["NAF"], 1)
        self.assertEqual(result["reference_extra_attributes"]["drawing-order"], 1)
        self.assertEqual(result["reference_extra_attributes"]["other"], 1)
        self.assertNotIn(SENTINEL, json.dumps(result))

    def test_unicode_and_escaped_characters_round_trip(self):
        tricky = "Carlo ação 🪵 <&> \"quoted\" 'single' é中\n\ttab"
        ours, theirs = app(), app()
        for tree in (ours, theirs):
            tree["children"][0]["values"]["text"] = tricky
            tree["values"]["content-desc"] = tricky
        document = carlo(ours)
        self.assertIn(b"&lt;&amp;&gt;", document)
        root, _ = parse(document, "carlo")
        self.assertEqual(root[0].attrib["content-desc"].split(" \"")[0], "Carlo ação 🪵 <&>")
        result = evaluate(document, reference(theirs))
        self.assertTrue(result["parity"] and result["carlo_valid"])
        theirs["children"][0]["values"]["text"] = tricky.replace("🪵", "🪓")
        self.assertEqual(evaluate(document, reference(theirs))["attribute_mismatches"],
                         {"text": 1})

    def test_literal_greater_than_in_attribute_does_not_break_the_tag_scan(self):
        ours = app()
        document = carlo(ours).replace(b'text="Title"', b'text="a > b /> c"')
        self.assertEqual(codes(document), [])


class Validation(unittest.TestCase):
    def test_valid_document(self):
        self.assertEqual(codes(carlo(app(), app())), [])

    def test_wrong_attribute_order(self):
        order = list(NODE_ATTRIBUTES)
        order[1], order[2] = order[2], order[1]
        tree = app()
        tree["children"][0]["order"] = order
        self.assertEqual(codes(carlo(tree)), ["attribute_order"])

    def test_window_attributes_must_follow_bounds(self):
        document = carlo(app()).replace(b'<node index="0"', b'<node window-id="40" index="0"', 1)
        document = document.replace(b' window-id="40" display-id', b" display-id", 1)
        self.assertEqual(codes(document), ["attribute_order"])

    def test_missing_attribute(self):
        tree = app()
        tree["children"][1]["drop"] = ("password",)
        self.assertEqual(codes(carlo(tree)), ["missing_attribute"])

    def test_unexpected_attribute(self):
        tree = app()
        tree["children"][1]["extra"] = {"NAF": "true"}
        self.assertEqual(codes(carlo(tree)), ["unexpected_attribute"])

    def test_bad_boolean(self):
        for value in ("True", "1", ""):
            tree = app()
            tree["children"][0]["values"]["checked"] = value
            self.assertEqual(codes(carlo(tree)), ["invalid_boolean"], value)

    def test_bad_bounds(self):
        for value in ("", "[0,0][10]", "[0,0] [1,1]", "[5,0][4,9]", "[0,9][4,8]", "[a,0][1,1]"):
            tree = app()
            tree["children"][0]["values"]["bounds"] = value
            self.assertEqual(codes(carlo(tree)), ["invalid_bounds"], value)
        tree = app(bounds="[-5,-7][0,0]")
        self.assertEqual(codes(carlo(tree)), [])

    def test_wrong_index(self):
        tree = app()
        tree["children"][1]["values"]["index"] = "0"  # repeats its sibling's index
        self.assertEqual(codes(carlo(tree)), ["index_mismatch"])
        gap = app()
        gap["children"][1]["values"]["index"] = "5"  # a deduplicated sibling leaves a gap
        self.assertEqual(codes(carlo(gap)), [])
        self.assertEqual(codes(carlo(app()).replace(b'<node index="0"', b'<node index="1"', 1)),
                         ["index_mismatch"])

    def test_wrong_counters(self):
        self.assertEqual(codes(carlo(app(), nodes=9)), ["nodes_count_mismatch"])
        self.assertEqual(codes(carlo(app(), window_count=2)), ["windows_count_mismatch"])

    def test_window_root_without_window_attributes(self):
        document = carlo(app(), window_fields={}).replace(b' window-layer="1"', b"")
        self.assertEqual(codes(document), ["window_attribute_missing"])

    def test_hierarchy_attributes_and_root(self):
        self.assertEqual(codes(carlo(app()).replace(b' generation="1"', b"")),
                         ["hierarchy_attribute_missing"])
        self.assertEqual(codes(b"<nodes/>"), ["root_not_hierarchy"])

    def test_unexpected_element_text_and_comment(self):
        document = carlo(app())
        self.assertIn("unexpected_element",
                      codes(document.replace(b"</hierarchy>", b"<extra/></hierarchy>")))
        self.assertEqual(codes(document.replace(b"</hierarchy>", b"stray</hierarchy>")),
                         ["unexpected_text"])
        self.assertIn("unexpected_markup",
                      codes(document.replace(b"</hierarchy>", b"<!-- x --></hierarchy>")))

    def test_invalid_carlo_is_reported_with_codes_only(self):
        tree = app()
        tree["children"][0]["values"]["checked"] = SENTINEL
        result = evaluate(carlo(tree, nodes=1), reference(app()))
        self.assertFalse(result["carlo_valid"])
        self.assertEqual(result["validation_errors"], ["invalid_boolean", "nodes_count_mismatch"])
        self.assertEqual(result["validation_error_counts"]["invalid_boolean"], 1)
        self.assertNotIn(SENTINEL, json.dumps(result))


class Refusals(unittest.TestCase):
    def refused(self, carlo_document, reference_document):
        with self.assertRaises(ParityError) as raised:
            evaluate(carlo_document, reference_document)
        self.assertNotIn(SENTINEL, str(raised.exception))
        return raised.exception.code

    def test_doctype_and_entity_rejected(self):
        bomb = (b'<?xml version="1.0"?><!DOCTYPE hierarchy [<!ENTITY a "' + SENTINEL.encode()
                + b'">]><hierarchy rotation="0"><node text="&a;"/></hierarchy>')
        self.assertEqual(self.refused(carlo(app()), bomb), "reference_dtd_or_entity_refused")
        self.assertEqual(self.refused(bomb, reference(app())), "carlo_dtd_or_entity_refused")
        for marker in (b"<!ENTITY x 'y'>", b"<!doctype hierarchy>", b"<! DOCTYPE x>"):
            document = reference(app()).replace(b"<hierarchy", marker + b"<hierarchy", 1)
            self.assertEqual(self.refused(carlo(app()), document),
                             "reference_dtd_or_entity_refused")

    def test_oversize_rejected(self):
        padding = b" " * (MAX_BYTES + 1)
        self.assertEqual(self.refused(carlo(app()) + padding, reference(app())),
                         "carlo_too_large")
        self.assertEqual(self.refused(carlo(app()), reference(app()) + padding),
                         "reference_too_large")

    def test_node_count_bounded(self):
        with mock.patch.object(hierarchy_parity, "MAX_NODES", 3):
            self.assertEqual(self.refused(carlo(app()), reference(app())),
                             "carlo_too_many_nodes")

    def test_depth_bounded(self):
        deep = node()
        for _ in range(6):
            deep = node([deep])
        with mock.patch.object(hierarchy_parity, "MAX_DEPTH", 5):
            self.assertEqual(self.refused(carlo(deep), reference(deep)), "carlo_too_deep")

    def test_alignment_budget_bounded(self):
        with mock.patch.object(hierarchy_parity, "MAX_PAIRS", 2):
            self.assertEqual(self.refused(carlo(app()), reference(app())),
                             "alignment_budget_exceeded")

    def test_malformed_and_non_utf8_rejected(self):
        self.assertEqual(self.refused(b"<hierarchy><node></hierarchy>", reference(app())),
                         "carlo_malformed_xml")
        self.assertEqual(self.refused(carlo(app()), b"\xff\xfe<\x00h\x00"), "reference_not_utf8")

    def test_reference_forms(self):
        ours = carlo(app())
        windows = (b'<hierarchy rotation="0"><display id="0"><window index="0"><hierarchy>'
                   b"<node/></hierarchy></window></display></hierarchy>")
        self.assertEqual(self.refused(ours, windows), "reference_windows_form_unsupported")
        self.assertEqual(self.refused(ours, b'<hierarchy rotation="0"></hierarchy>'),
                         "reference_empty")
        self.assertEqual(self.refused(ours, b'<hierarchy rotation="0"><node/><node/></hierarchy>'),
                         "reference_multiple_roots")
        self.assertEqual(self.refused(ours, b"<dump/>"), "reference_root_not_hierarchy")


class CommandLine(unittest.TestCase):
    def invoke(self, carlo_document, reference_document):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            (base / "carlo.xml").write_bytes(carlo_document)
            (base / "reference.xml").write_bytes(reference_document)
            arguments = ["hierarchy_parity.py", "--carlo", str(base / "carlo.xml"), "--reference",
                         str(base / "reference.xml"), "--output", str(base / "out/summary.json")]
            stdout, stderr = io.StringIO(), io.StringIO()
            with mock.patch.object(sys, "argv", arguments), \
                    contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                status = hierarchy_parity.main()
            output = base / "out/summary.json"
            summary = output.read_text() if output.exists() else None
            return status, stdout.getvalue() + stderr.getvalue(), summary

    def test_success_writes_counters_only(self):
        ours, theirs = app(), app()
        for tree in (ours, theirs):
            tree["children"][0]["values"]["text"] = SENTINEL
        status, printed, summary = self.invoke(carlo(ours), reference(theirs))
        self.assertEqual(status, 0)
        self.assertTrue(json.loads(summary)["parity"])
        self.assertFalse(json.loads(summary)["ui_contents_recorded"])
        self.assertNotIn(SENTINEL, printed + summary)

    def test_mismatch_exits_one_without_echoing_inputs(self):
        ours, theirs = app(), app()
        ours["children"][0]["values"]["text"] = SENTINEL
        status, printed, summary = self.invoke(carlo(ours), reference(theirs))
        self.assertEqual(status, 1)
        self.assertEqual(json.loads(summary)["attribute_mismatches"], {"text": 1})
        self.assertNotIn(SENTINEL, printed + summary)

    def test_refused_input_exits_two_with_a_code(self):
        bomb = b"<!DOCTYPE x><hierarchy>" + SENTINEL.encode() + b"</hierarchy>"
        status, printed, summary = self.invoke(carlo(app()), bomb)
        self.assertEqual(status, 2)
        self.assertIsNone(summary)
        self.assertIn("reference_dtd_or_entity_refused", printed)
        self.assertNotIn(SENTINEL, printed)


if __name__ == "__main__":
    unittest.main()
