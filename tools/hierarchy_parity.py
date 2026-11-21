#!/usr/bin/env python3
"""Validate Carlo's hierarchy XML and compare it with a `uiautomator dump` reference.

Both documents are UI contents. They are read, compared in memory and never
echoed: results hold counters, attribute NAMES and stable error codes only.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import xml.etree.ElementTree as ElementTree
from pathlib import Path

MAX_BYTES = 8 * 1024 * 1024
MAX_NODES = 20_000
MAX_DEPTH = 150           # keeps the recursive alignment far from the interpreter limit
MAX_PAIRS = 2_000_000     # node pairs the alignment may evaluate

BOOLEANS = ("checkable", "checked", "clickable", "enabled", "focusable", "focused",
            "scrollable", "long-clickable", "password", "selected")
# Order of every Carlo <node>; window roots append WINDOW_ATTRIBUTES.
NODE_ATTRIBUTES = ("index", "text", "resource-id", "class", "package", "content-desc",
                   *BOOLEANS, "bounds")
WINDOW_ATTRIBUTES = ("window-id", "display-id", "window-type", "window-layer")
HIERARCHY_ATTRIBUTES = ("rotation", "snapshot-id", "start-ms", "end-ms", "generation",
                        "windows", "nodes")
# Index is compared apart: the reference skips invisible children.
COMPARED = NODE_ATTRIBUTES[1:]
# A reference node pairs only with a Carlo node of the same kind.
KIND = ("class", "package")
WINDOW_KEY = ("package", "class", "resource-id", "bounds")

BOUNDS = re.compile(r"^\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]$")
ATTRIBUTE = r"""\s+([^\s=<>"'/]+)\s*=\s*(?:"[^"<]*"|'[^'<]*')"""
START_TAG = re.compile(r"<(hierarchy|node)((?:" + ATTRIBUTE + r")*)\s*/?>")
ATTRIBUTE_NAME = re.compile(ATTRIBUTE)
SAFE_NAME = re.compile(r"^[A-Za-z][A-Za-z0-9-]{0,31}$")
FORBIDDEN = re.compile(rb"<!\s*(DOCTYPE|ENTITY)", re.IGNORECASE)


class ParityError(ValueError):
    """A refused document; the message is a stable code, never document content."""

    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


def parse(data: bytes, role: str) -> tuple:
    """Safely parse one bounded document: (root element, decoded text)."""
    if not isinstance(data, (bytes, bytearray)):
        raise ParityError(role + "_not_bytes")
    if len(data) > MAX_BYTES:
        raise ParityError(role + "_too_large")
    if FORBIDDEN.search(data):
        raise ParityError(role + "_dtd_or_entity_refused")
    if data.count(b"<node") > MAX_NODES:
        raise ParityError(role + "_too_many_nodes")
    try:
        text = bytes(data).decode("utf-8-sig")
    except UnicodeDecodeError:
        raise ParityError(role + "_not_utf8") from None
    try:
        root = ElementTree.fromstring(text.encode("utf-8"))
    except ElementTree.ParseError:
        raise ParityError(role + "_malformed_xml") from None
    count, deepest = 0, 0
    stack = [(root, 0)]
    while stack:
        element, depth = stack.pop()
        count += 1
        deepest = max(deepest, depth)
        stack.extend((child, depth + 1) for child in element)
    if count - 1 > MAX_NODES:
        raise ParityError(role + "_too_many_nodes")
    if deepest > MAX_DEPTH:
        raise ParityError(role + "_too_deep")
    return root, text


def valid_bounds(value) -> bool:
    match = BOUNDS.match(value or "")
    if match is None:
        return False
    left, top, right, bottom = map(int, match.groups())
    return right >= left and bottom >= top


def validate_carlo(root, text: str) -> dict:
    """Check Carlo's document against its own contract; codes mapped to counts."""
    errors: dict = {}

    def fail(code):
        errors[code] = errors.get(code, 0) + 1

    if root.tag != "hierarchy":
        fail("root_not_hierarchy")
        return errors
    if "<!--" in text or "<![CDATA[" in text or "<?" in text[1:]:
        fail("unexpected_markup")
    for name in HIERARCHY_ATTRIBUTES:
        if name not in root.attrib:
            fail("hierarchy_attribute_missing")
    elements = list(root.iter())
    if any(element.tag != "node" for element in elements[1:]):
        fail("unexpected_element")
    if any((element.text or "").strip() or (element.tail or "").strip() for element in elements):
        fail("unexpected_text")
    windows = [child for child in root if child.tag == "node"]
    nodes = [element for element in elements[1:] if element.tag == "node"]
    if root.attrib.get("windows") != str(len(windows)):
        fail("windows_count_mismatch")
    if root.attrib.get("nodes") != str(len(nodes)):
        fail("nodes_count_mismatch")

    # ElementTree loses attribute order: read it from each start tag, in document order.
    tags = [(match.group(1), [name.group(1) for name in ATTRIBUTE_NAME.finditer(match.group(2))])
            for match in START_TAG.finditer(text)]
    scanned = len(tags) == len(elements) and all(
        tag == element.tag and sorted(names) == sorted(element.attrib)
        for (tag, names), element in zip(tags, elements))
    if not scanned:
        fail("tag_scan_mismatch")
    roots = {id(window) for window in windows}
    for position, element in enumerate(elements):
        if element.tag != "node":
            continue
        is_window = id(element) in roots
        expected = NODE_ATTRIBUTES + (WINDOW_ATTRIBUTES if is_window else ())
        present = element.attrib
        if any(name not in present for name in NODE_ATTRIBUTES):
            fail("missing_attribute")
        if is_window and any(name not in present for name in WINDOW_ATTRIBUTES):
            fail("window_attribute_missing")
        if any(name not in expected for name in present):
            fail("unexpected_attribute")
        if scanned:
            order = [name for name in tags[position][1] if name in expected]
            if order != [name for name in expected if name in present]:
                fail("attribute_order")
        if any(name in present and present[name] not in ("true", "false") for name in BOOLEANS):
            fail("invalid_boolean")
        if "bounds" in present and not valid_bounds(present["bounds"]):
            fail("invalid_bounds")
    stack = [root]
    while stack:
        parent = stack.pop()
        # A window root has index 0. Elsewhere the index is the position in the
        # parent's child list; a deduplicated child leaves a gap, so siblings
        # must be strictly increasing rather than consecutive.
        previous = -1
        for child in parent:
            if child.tag == "node" and "index" in child.attrib:
                value = child.attrib["index"]
                number = int(value) if value.isascii() and value.isdigit() else -1
                if (parent is root and number != 0) or number <= (-1 if parent is root else previous):
                    fail("index_mismatch")
                previous = number
            stack.append(child)
    return dict(sorted(errors.items()))


def reference_root(root):
    """The single node tree of a plain `uiautomator dump`; other forms are refused."""
    if root.tag != "hierarchy":
        raise ParityError("reference_root_not_hierarchy")
    children = list(root)
    if any(child.tag in ("display", "window", "hierarchy") for child in root.iter()
           if child is not root):
        raise ParityError("reference_windows_form_unsupported")
    if any(element.tag != "node" for element in root.iter() if element is not root):
        raise ParityError("reference_unexpected_element")
    if not children:
        raise ParityError("reference_empty")
    if len(children) != 1:
        raise ParityError("reference_multiple_roots")
    return children[0]


class _Aligner:
    """Best ordered-subsequence alignment of reference children onto Carlo children."""

    def __init__(self):
        self.memo: dict = {}
        self.pairs = 0

    def pair(self, reference, carlo) -> tuple:
        """(matched nodes, attribute mismatches, index differences, children pairing)."""
        key = (id(reference), id(carlo))
        found = self.memo.get(key)
        if found is not None:
            return found
        self.pairs += 1
        if self.pairs > MAX_PAIRS:
            raise ParityError("alignment_budget_exceeded")
        mismatches = sum(reference.attrib.get(name, "") != carlo.attrib.get(name, "")
                         for name in COMPARED)
        index = int(reference.attrib.get("index") != carlo.attrib.get("index"))
        ours, theirs = list(reference), list(carlo)
        rows, columns = len(ours), len(theirs)
        # best[i][j]: best total for ours[i:] against theirs[j:]; maximise matched
        # nodes, then minimise attribute mismatches, then index differences.
        best = [[(0, 0, 0)] * (columns + 1) for _ in range(rows + 1)]
        take = [[0] * (columns + 1) for _ in range(rows + 1)]
        for i in range(rows - 1, -1, -1):
            kind = tuple(ours[i].attrib.get(name, "") for name in KIND)
            for j in range(columns - 1, -1, -1):
                choice, how = best[i + 1][j], 1          # reference child unmatched
                if _better(best[i][j + 1], choice):
                    choice, how = best[i][j + 1], 2      # Carlo-only child
                if kind == tuple(theirs[j].attrib.get(name, "") for name in KIND):
                    below, rest = self.pair(ours[i], theirs[j]), best[i + 1][j + 1]
                    both = (below[0] + rest[0], below[1] + rest[1], below[2] + rest[2])
                    if not _better(choice, both):
                        choice, how = both, 3
                best[i][j], take[i][j] = choice, how
        chosen, i, j = [], 0, 0
        while i < rows and j < columns:
            how = take[i][j]
            if how == 3:
                chosen.append((ours[i], theirs[j]))
                i, j = i + 1, j + 1
            elif how == 2:
                j += 1
            else:
                i += 1
        total = best[0][0] if rows and columns else (0, 0, 0)
        found = (1 + total[0], mismatches + total[1], index + total[2], chosen)
        self.memo[key] = found
        return found

    def collect(self, reference, carlo, names: dict) -> None:
        """Count mismatching attribute names over the chosen alignment."""
        stack = [(reference, carlo)]
        while stack:
            ours, theirs = stack.pop()
            for name in COMPARED:
                if ours.attrib.get(name, "") != theirs.attrib.get(name, ""):
                    names[name] = names.get(name, 0) + 1
            stack.extend(self.pair(ours, theirs)[3])


def _better(left: tuple, right: tuple) -> bool:
    """True if `left` beats `right`: more matches, then fewer mismatches/differences."""
    return (left[0], -left[1], -left[2]) > (right[0], -right[1], -right[2])


def size(element) -> int:
    return sum(1 for _ in element.iter())


def compare(carlo_root, reference_tree) -> dict:
    """Align the reference tree onto the best matching Carlo window tree."""
    reference_nodes = size(reference_tree)
    result = {
        "reference_nodes": reference_nodes, "matched_nodes": 0,
        "unmatched_reference_nodes": reference_nodes, "carlo_window_nodes": 0,
        "carlo_only_nodes": 0, "attribute_mismatches": {}, "index_differences": 0,
        "candidate_windows": 0, "window_matched": False, "parity": False,
    }
    key = tuple(reference_tree.attrib.get(name, "") for name in WINDOW_KEY)
    candidates = [window for window in carlo_root if window.tag == "node"
                  and tuple(window.attrib.get(name, "") for name in WINDOW_KEY) == key]
    result["candidate_windows"] = len(candidates)
    aligner = _Aligner()
    chosen, score = None, None
    for window in candidates:
        found = aligner.pair(reference_tree, window)
        if score is None or _better(found[:3], score[:3]):
            chosen, score = window, found
    if chosen is None:
        return result
    names: dict = {}
    aligner.collect(reference_tree, chosen, names)
    window_nodes = size(chosen)
    result.update({
        "window_matched": True, "matched_nodes": score[0],
        "unmatched_reference_nodes": reference_nodes - score[0],
        "carlo_window_nodes": window_nodes, "carlo_only_nodes": window_nodes - score[0],
        "attribute_mismatches": dict(sorted(names.items())), "index_differences": score[2],
        "parity": score[0] == reference_nodes and not names,
    })
    return result


def extra_attributes(reference_tree) -> dict:
    """Reference attribute names Carlo does not emit (NAF, drawing-order, ...), counted."""
    extras: dict = {}
    for element in reference_tree.iter():
        for name in element.attrib:
            if name not in NODE_ATTRIBUTES:
                label = name if SAFE_NAME.match(name) else "other"
                extras[label] = extras.get(label, 0) + 1
    return dict(sorted(extras.items()))


def evaluate(carlo: bytes, reference: bytes) -> dict:
    """Full result for two documents; raises ParityError(code) on a refused input."""
    carlo_root, carlo_text = parse(carlo, "carlo")
    reference_document, _ = parse(reference, "reference")
    errors = validate_carlo(carlo_root, carlo_text)
    tree = reference_root(reference_document)
    result = compare(carlo_root, tree) if carlo_root.tag == "hierarchy" else compare(
        ElementTree.Element("hierarchy"), tree)
    rotation = reference_document.attrib.get("rotation", "")
    result.update({
        "carlo_valid": not errors,
        "validation_errors": list(errors),
        "validation_error_counts": errors,
        "carlo_windows": sum(1 for child in carlo_root if child.tag == "node"),
        "carlo_nodes": sum(1 for element in carlo_root.iter() if element.tag == "node"),
        "reference_extra_attributes": extra_attributes(tree),
        "reference_rotation_numeric": rotation.isdigit(),
        "ui_contents_recorded": False,
    })
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--carlo", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="summary JSON (counters only)")
    args = parser.parse_args()
    try:
        documents = []
        for path, role in ((args.carlo, "carlo"), (args.reference, "reference")):
            if path.stat().st_size > MAX_BYTES:
                raise ParityError(role + "_too_large")
            documents.append(path.read_bytes())
        result = evaluate(*documents)
    except ParityError as error:
        print("carlo-hierarchy-parity: refused: " + error.code, file=sys.stderr)
        return 2
    except OSError as error:
        print("carlo-hierarchy-parity: cannot read input: " + type(error).__name__,
              file=sys.stderr)
        return 2
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    passed = result["parity"] and result["carlo_valid"]
    print(f"parity={str(result['parity']).lower()} "
          f"carlo_valid={str(result['carlo_valid']).lower()} "
          f"matched={result['matched_nodes']}/{result['reference_nodes']} "
          f"carlo_only={result['carlo_only_nodes']}")
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
