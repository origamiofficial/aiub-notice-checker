import tempfile
import unittest
import re
from pathlib import Path

import release_version


class ReleaseVersionTests(unittest.TestCase):
    def script(self, directory: str, version: str = "5.4") -> Path:
        path = Path(directory) / "main.py"
        path.write_text(f'SCRIPT_VERSION = "{version}"\nprint("ok")\n', encoding="utf-8")
        return path

    def test_increments_latest_release_by_one_tenth(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.script(directory)
            self.assertEqual(release_version.next_version("5.4", path), "5.5")
            self.assertEqual(release_version.next_version("5.9", path), "6.0")

    def test_never_moves_backwards_from_playground_version(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.script(directory, "7.0")
            self.assertEqual(release_version.next_version("5.4", path), "7.1")

    def test_sets_exactly_one_version_and_rejects_invalid_tags(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.script(directory)
            release_version.write_version("5.5", path)
            self.assertEqual(release_version.read_version(path), "5.5")
            self.assertNotIn("\r", path.read_text(encoding="utf-8"))
            with self.assertRaisesRegex(ValueError, "not numeric"):
                release_version.next_version("v5.5", path)

    def test_workflows_enforce_stable_release_contract(self):
        root = Path(__file__).parent
        production = (root / ".github/workflows/aiub-notice-checker.yml").read_text(encoding="utf-8")
        tests = (root / ".github/workflows/tests.yml").read_text(encoding="utf-8")
        self.assertIn("- cron: '*/5 * * * *'", production)
        self.assertIn("run: python .stable-release/main.py", production)
        self.assertNotIn("run: python main.py", production)
        self.assertIn("gh release download", production)
        self.assertIn("sha256sum --check --strict SHA256SUMS", production)
        self.assertNotIn("force: true", production)
        self.assertIn("needs: tests", tests)
        self.assertIn("python live_smoke.py", tests)
        self.assertIn("gh release create", tests)
        self.assertIn("--verify-tag --latest", tests)
        for workflow in (root / ".github/workflows").glob("*.yml"):
            for reference in re.findall(r"^\s*uses:\s*\S+@([^\s#]+)", workflow.read_text(encoding="utf-8"), re.MULTILINE):
                self.assertRegex(reference, r"^[0-9a-f]{40}$", workflow)


if __name__ == "__main__":
    unittest.main()
