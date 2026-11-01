"""Guard against premature parent completion and stale acceptance labels."""

import unittest
from unittest.mock import patch

from issue_lifecycle import parent_action, reconcile


class LifecycleTests(unittest.TestCase):
    def test_child_reopened_reopens_closed_parent(self):
        self.assertEqual(parent_action("closed", ["closed", "open"]), (True, False))

    def test_all_children_closed_requests_acceptance_instead_of_closing_parent(self):
        self.assertEqual(parent_action("open", ["closed", "closed"]), (False, True))
        self.assertEqual(parent_action("closed", ["closed"]), (False, False))

    def test_leaf_issue_is_not_reopened_or_marked_for_parent_acceptance(self):
        self.assertEqual(parent_action("closed", []), (False, False))

    def test_unknown_child_state_cannot_satisfy_parent(self):
        with self.assertRaises(ValueError):
            parent_action("open", ["unknown"])

    def test_reconcile_clears_stale_acceptance_after_pending_child_returns(self):
        def fake_api(method, endpoint, fields=None, **kwargs):
            if method == "GET" and "sub_issues?" in endpoint:
                return [{"state": "open"}]
            if method == "GET":
                return {"state": "closed", "labels": [{"name": "needs:acceptance"}]}
            return None

        with patch("issue_lifecycle.api", side_effect=fake_api) as api:
            reconcile("owner/repo", 1)
        writes = [call for call in api.call_args_list if call.args[0] != "GET"]
        self.assertEqual(writes[0].args, ("PATCH", "repos/owner/repo/issues/1", {"state": "open"}))
        self.assertEqual(
            writes[1].args[:2], ("DELETE", "repos/owner/repo/issues/1/labels/needs:acceptance")
        )


if __name__ == "__main__":
    unittest.main()
