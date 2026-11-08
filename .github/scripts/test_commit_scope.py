import tempfile
import unittest
from pathlib import Path
from unittest import mock

from commit_scope import allowed_scopes, check_message, first_message_line

SINGLE_CRATE = '[package]\nname = "kemana-proxy"\nversion = "0.1.0"\n'
WORKSPACE = """
[workspace]
members = ["crates/gtfs", "crates/realtime", "kemana-proxy", "crates/*"]

[package]
name = "kemana-proxy"
"""


class AllowedScopesTest(unittest.TestCase):
    def test_package_name_and_areas_are_allowed(self):
        scopes = allowed_scopes(SINGLE_CRATE)
        for s in ["kemana-proxy", "build", "ci", "deps", "tooling", "docs", "release"]:
            self.assertIn(s, scopes)

    def test_workspace_members_are_allowed_by_directory_name(self):
        scopes = allowed_scopes(WORKSPACE)
        for s in ["gtfs", "realtime", "kemana-proxy"]:
            self.assertIn(s, scopes)

    def test_glob_members_are_not_scopes(self):
        self.assertNotIn("*", allowed_scopes(WORKSPACE))

    def test_manifest_without_package_or_workspace_yields_only_areas(self):
        self.assertEqual({"build", "ci", "deps", "tooling", "docs", "release"}, allowed_scopes(""))

    def test_unrelated_names_are_not_allowed(self):
        self.assertNotIn("gtfs", allowed_scopes(SINGLE_CRATE))


class CheckMessageTest(unittest.TestCase):
    scopes = allowed_scopes(WORKSPACE)

    def test_valid_scope_passes(self):
        self.assertIsNone(check_message("feat(gtfs): add stop parsing", self.scopes))

    def test_breaking_marker_passes(self):
        self.assertIsNone(check_message("feat(gtfs)!: drop v0 schema", self.scopes))

    def test_unknown_scope_fails_naming_it(self):
        self.assertIn("gtsf", check_message("fix(gtsf): typo", self.scopes))

    def test_missing_scope_fails(self):
        self.assertIsNotNone(check_message("fix: something", self.scopes))

    def test_ci_and_docs_may_omit_scope(self):
        for m in ["ci: add workflows", "docs: add adrs"]:
            self.assertIsNone(check_message(m, self.scopes))

    def test_area_scope_passes(self):
        self.assertIsNone(check_message("build(tooling): add service skeleton", self.scopes))

    def test_git_generated_messages_pass(self):
        for m in [
            "Merge branch 'develop' into feature/x",
            'Revert "feat(gtfs): x"',
            "fixup! feat(gtfs): x",
            "squash! feat(gtfs): x",
            "amend! feat(gtfs): x",
        ]:
            self.assertIsNone(check_message(m, self.scopes))

    def test_unknown_type_fails_naming_it(self):
        self.assertIn("feature", check_message("feature(gtfs): x", self.scopes))

    def test_uppercase_summary_fails(self):
        self.assertIsNotNone(check_message("feat(gtfs): Add stops", self.scopes))

    def test_trailing_period_fails(self):
        self.assertIsNotNone(check_message("feat(gtfs): add stops.", self.scopes))

    def test_missing_space_after_colon_fails(self):
        self.assertIsNotNone(check_message("feat(gtfs):add stops", self.scopes))

    def test_empty_summary_fails(self):
        self.assertIsNotNone(check_message("feat(gtfs): ", self.scopes))

    def test_header_length_limit_is_72(self):
        prefix = "feat(gtfs): "
        at_limit = prefix + "a" * (72 - len(prefix))
        self.assertEqual(72, len(at_limit))
        self.assertIsNone(check_message(at_limit, self.scopes))
        self.assertIsNotNone(check_message(at_limit + "a", self.scopes))


class FirstMessageLineTest(unittest.TestCase):
    def test_file_is_read_as_utf_8(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "COMMIT_EDITMSG"
            path.write_text("# café comment\nfeat(gtfs): add stops\n", encoding="utf-8")
            with mock.patch("io.text_encoding", side_effect=lambda encoding, stacklevel=2: encoding or "ascii"):
                self.assertEqual("feat(gtfs): add stops", first_message_line(str(path)))


if __name__ == "__main__":
    unittest.main()
