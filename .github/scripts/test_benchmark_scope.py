"""Exercise benchmark migration detection without running Cargo or Criterion."""

from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import tempfile
import unittest

import benchmark_scope


class BenchmarkScopeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def suite(self, name, *, criterion=True, path="benches/pool.rs", alias=None, source=None):
        directory = self.root / name
        directory.mkdir()
        manifest = '[package]\nname = "pool"\nversion = "0.1.0"\n\n[[bench]]\nname = "pool"\nharness = false\n'
        if path != "benches/pool.rs":
            manifest += f'path = "{path}"\n'
        if criterion:
            manifest += '\n[dev-dependencies]\n'
            manifest += f'{alias} = {{ package = "criterion", version = "0.7" }}\n' if alias else 'criterion = "0.7"\n'
        (directory / "Cargo.toml").write_text(manifest)
        bench = directory / path
        bench.parent.mkdir(parents=True)
        bench.write_text(source if source is not None else "criterion::criterion_main!(benches);" if criterion else "fn main() {}")
        return directory

    def scope(self, before, after):
        output = self.root / "comparison.json"
        summary = self.root / "comparison.md"
        github_output = self.root / "github-output"
        step_summary = self.root / "step-summary"
        with redirect_stdout(io.StringIO()):
            status = benchmark_scope.main([
                "--base-directory", str(before), "--head-directory", str(after),
                "--base-sha", "base-sha", "--head-sha", "head-sha",
                "--json-output", str(output), "--summary-output", str(summary),
                "--github-output", str(github_output), "--step-summary", str(step_summary),
            ])
        return status, json.loads(output.read_text()), github_output, summary, step_summary

    def test_first_migration_pr_is_explicitly_skipped(self):
        status, report, github_output, _, _ = self.scope(
            self.suite("base", criterion=False), self.suite("head")
        )
        self.assertEqual(status, 0)
        self.assertEqual(report["status"], "skipped")
        self.assertIn("previous custom harness", report["reason"])
        self.assertEqual(github_output.read_text(), "required=false\n")

    def test_compatible_revisions_enable_measurements_and_preserve_reports(self):
        status, report, github_output, summary, step_summary = self.scope(
            self.suite("base"), self.suite("head")
        )
        self.assertEqual(status, 0)
        self.assertEqual(report["status"], "ready")
        self.assertEqual(report["base_sha"], "base-sha")
        self.assertEqual(report["head_sha"], "head-sha")
        self.assertEqual(github_output.read_text(), "required=true\n")
        self.assertIn("**Ready**", summary.read_text())
        self.assertEqual(summary.read_text(), step_summary.read_text())

    def test_incompatible_head_does_not_silently_skip_gate(self):
        status, report, _, summary, _ = self.scope(
            self.suite("base"), self.suite("head", criterion=False)
        )
        self.assertEqual(status, 2)
        self.assertEqual(report["status"], "error")
        self.assertIn("PR benchmark suite is incompatible", report["reason"])
        self.assertIn("**Error**", summary.read_text())

    def test_custom_bench_path_and_aliased_criterion_are_compatible(self):
        after = self.suite(
            "head", path="performance/pool.rs", alias="timing",
            source="timing::criterion_main!(benches);",
        )
        status, report, _, _, _ = self.scope(self.suite("base"), after)
        self.assertEqual(status, 0)
        self.assertEqual(report["status"], "ready")

    def test_aliased_criterion_can_use_manual_command_line_configuration(self):
        after = self.suite(
            "head", alias="timing",
            source="use timing::Criterion; fn main() { Criterion::default().configure_from_args(); }",
        )
        self.assertIsNone(benchmark_scope.criterion_suite(after))

    def test_missing_source_and_missing_command_line_options_are_incompatible(self):
        missing = self.suite("missing")
        (missing / "benches/pool.rs").unlink()
        self.assertIn("source is missing", benchmark_scope.criterion_suite(missing))
        no_options = self.suite("no-options", source="fn main() {}")
        self.assertIn("command-line options", benchmark_scope.criterion_suite(no_options))

    def test_harness_and_required_features_are_checked(self):
        for label, replacement, reason in (
            ("harness", "harness = true", "harness = false"),
            ("features", 'harness = false\nrequired-features = ["bench"]', "requires feature flags"),
        ):
            with self.subTest(label=label):
                directory = self.suite(label)
                manifest = directory / "Cargo.toml"
                manifest.write_text(manifest.read_text().replace("harness = false", replacement))
                self.assertIn(reason, benchmark_scope.criterion_suite(directory))

    def test_missing_base_manifest_is_reported_as_skipped(self):
        before = self.root / "base"
        before.mkdir()
        status, report, github_output, _, _ = self.scope(before, self.suite("head"))
        self.assertEqual(status, 0)
        self.assertEqual(report["status"], "skipped")
        self.assertIn("no Cargo.toml", report["reason"])
        self.assertEqual(github_output.read_text(), "required=false\n")

    def test_malformed_manifest_is_reported_as_an_error(self):
        after = self.suite("head")
        (after / "Cargo.toml").write_text("[invalid")
        status, report, _, _, _ = self.scope(self.suite("base"), after)
        self.assertEqual(status, 2)
        self.assertEqual(report["status"], "error")


if __name__ == "__main__":
    unittest.main()
