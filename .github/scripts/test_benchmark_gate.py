"""Check the gate against critcmp 0.1.8 list-output fixtures."""

from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("benchmark_gate.awk")


def group(name, labels=("base", "pr"), ranks=("1.00", "1.20"), unit="ns", separator="  "):
    lines = [name, "-" * len(name)]
    for label, rank in zip(labels, ranks):
        lines.append(separator.join([label, rank, f"100.0±2.00{unit}", "1.2", "GElem/sec"]))
    return "\n".join(lines) + "\n"


class BenchmarkGateTests(unittest.TestCase):
    def run_gate(self, complete, significant=""):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            all_path = directory / "all.txt"
            filtered_path = directory / "significant.txt"
            all_path.write_text(complete)
            filtered_path.write_text(significant)
            return subprocess.run(
                ["awk", "-f", str(SCRIPT), str(all_path), str(filtered_path)],
                capture_output=True, text=True, check=False,
            )

    def test_empty_filtered_output_passes_with_a_valid_paired_comparison(self):
        result = self.run_gate(group("local/reuse/small"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Paired cases: 1", result.stdout)
        self.assertIn("regressions: 0", result.stdout)

    def test_significant_regression_fails(self):
        comparison = group("local/reuse/small")
        result = self.run_gate(comparison, comparison)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("Regression above threshold: local/reuse/small", result.stdout)

    def test_significant_improvement_passes(self):
        comparison = group("remote/return/4", labels=("pr", "base"))
        result = self.run_gate(comparison, comparison)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("regressions: 0", result.stdout)

    def test_gate_uses_order_when_ranks_round_to_the_same_number(self):
        comparison = group("local/reuse/small", ranks=("1.00", "1.00"))
        result = self.run_gate(comparison, comparison)
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_regression_rounded_to_threshold_still_fails(self):
        comparison = group("local/reuse/small", ranks=("1.00", "1.15"))
        self.assertEqual(self.run_gate(comparison, comparison).returncode, 1)

    def test_exact_threshold_absent_from_filtered_output_passes(self):
        comparison = group("local/reuse/small", ranks=("1.00", "1.15"))
        self.assertEqual(self.run_gate(comparison).returncode, 0)

    def test_added_and_removed_cases_are_reported(self):
        complete = "\n".join([
            group("common"),
            group("added", labels=("pr",), ranks=("1.00",)),
            group("removed", labels=("base",), ranks=("1.00",)),
        ])
        result = self.run_gate(complete)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Added case: added", result.stdout)
        self.assertIn("Removed case: removed", result.stdout)
        self.assertIn("added: 1; removed: 1", result.stdout)

    def test_no_common_benchmarks_fails(self):
        complete = "\n".join([
            group("added", labels=("pr",), ranks=("1.00",)),
            group("removed", labels=("base",), ranks=("1.00",)),
        ])
        result = self.run_gate(complete)
        self.assertEqual(result.returncode, 2)
        self.assertIn("no benchmark IDs", result.stderr)

    def test_empty_complete_report_fails(self):
        self.assertEqual(self.run_gate("").returncode, 2)

    def test_two_regressions_and_an_improvement_are_classified_separately(self):
        first = group("first")
        second = group("second", labels=("pr", "base"))
        third = group("third")
        report = "\n".join([first, second, third])
        result = self.run_gate(report, report)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("Paired cases: 3", result.stdout)
        self.assertIn("regressions: 2", result.stdout)

    def test_tabs_and_all_time_units_are_supported(self):
        for unit in ("ns", "µs", "ms", "s"):
            with self.subTest(unit=unit):
                comparison = group("case", unit=unit, separator="\t")
                self.assertEqual(self.run_gate(comparison).returncode, 0)

    def test_windows_line_endings_are_supported(self):
        comparison = group("case").replace("\n", "\r\n")
        self.assertEqual(self.run_gate(comparison).returncode, 0)

    def test_benchmark_names_can_contain_spaces(self):
        comparison = group("local reuse small")
        self.assertEqual(self.run_gate(comparison).returncode, 0)

    def test_malformed_output_fails(self):
        for malformed in (
            "group base pr\n-----\ncase 1.00 1.20\n",
            group("case", labels=("base", "base")),
            group("case", labels=("base", "unexpected")),
            group("case", ranks=("NaN", "1.20")),
            group("case", ranks=("1.00", "inf")),
            group("case").replace("100.0±2.00ns", "not-a-duration"),
        ):
            with self.subTest(malformed=malformed):
                self.assertEqual(self.run_gate(malformed).returncode, 2)

    def test_unpaired_case_cannot_appear_in_threshold_report(self):
        complete = "\n".join([group("common"), group("added", labels=("pr",), ranks=("1.00",))])
        result = self.run_gate(complete, group("added", labels=("pr",), ranks=("1.00",)))
        self.assertEqual(result.returncode, 2)

    def test_threshold_report_cannot_introduce_an_unknown_case(self):
        self.assertEqual(self.run_gate(group("common"), group("unknown")).returncode, 2)

    def test_threshold_report_must_preserve_baseline_order(self):
        result = self.run_gate(group("case"), group("case", labels=("pr", "base")))
        self.assertEqual(result.returncode, 2)

    def test_duplicate_comparison_blocks_fail(self):
        duplicated = "\n".join([group("case"), group("case")])
        self.assertEqual(self.run_gate(duplicated).returncode, 2)
        self.assertEqual(self.run_gate(group("case"), duplicated).returncode, 2)


if __name__ == "__main__":
    unittest.main()
