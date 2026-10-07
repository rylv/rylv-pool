"""Determine whether both revisions expose a comparable Criterion pool suite."""

import argparse
import json
from pathlib import Path
import sys
import tomllib


def criterion_suite(directory):
    """Return an incompatibility reason, or None for a comparable pool suite."""
    manifest_path = directory / "Cargo.toml"
    if not manifest_path.is_file():
        return "there is no Cargo.toml"
    manifest = tomllib.loads(manifest_path.read_text())
    dependencies = manifest.get("dev-dependencies", {})
    if not any(
        name == "criterion" or isinstance(value, dict) and value.get("package") == "criterion"
        for name, value in dependencies.items()
    ):
        return "the suite does not depend on Criterion (the previous custom harness is unsupported)"
    bench = next((value for value in manifest.get("bench", []) if value.get("name") == "pool"), None)
    if not bench or bench.get("harness") is not False:
        return "there is no pool benchmark with harness = false"
    if bench.get("required-features"):
        return "the pool benchmark requires feature flags not used by this comparison"
    source = directory / bench.get("path", "benches/pool.rs")
    if not source.is_file():
        return "the pool benchmark source is missing"
    if not any(token in source.read_text() for token in ("criterion_main!", "configure_from_args")):
        return "the pool benchmark does not expose Criterion command-line options"
    return None


def write_report(report, args):
    summary = "\n".join([
        "## Benchmark comparison",
        "",
        f"Base: `{report['base_sha']}` · PR: `{report['head_sha']}`",
        "",
        f"**{report['status'].capitalize()}**: {report['reason']}",
        "",
    ])
    for path, content in (
        (args.json_output, json.dumps(report, indent=2, allow_nan=False) + "\n"),
        (args.summary_output, summary),
    ):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
    if args.step_summary:
        with args.step_summary.open("a") as output:
            output.write(summary)
    print(summary)


def arguments(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-directory", type=Path, required=True)
    parser.add_argument("--head-directory", type=Path, required=True)
    parser.add_argument("--base-sha", default="unknown")
    parser.add_argument("--head-sha", default="unknown")
    parser.add_argument("--json-output", type=Path, default=Path("target/benchmark-comparison.json"))
    parser.add_argument("--summary-output", type=Path, default=Path("target/benchmark-comparison.md"))
    parser.add_argument("--github-output", type=Path)
    parser.add_argument("--step-summary", type=Path)
    return parser.parse_args(argv)


def main(argv=None):
    args = arguments(argv)
    try:
        head_reason = criterion_suite(args.head_directory)
        if head_reason:
            raise ValueError(f"PR benchmark suite is incompatible: {head_reason}")
        base_reason = criterion_suite(args.base_directory)
        required = base_reason is None
        report = {
            "status": "ready" if required else "skipped",
            "reason": "Both revisions expose Criterion; measurements are pending" if required else f"Base revision is incompatible: {base_reason}. Comparison starts with the next PR after the migration is merged.",
        }
        if args.github_output:
            with args.github_output.open("a") as output:
                print(f"required={str(required).lower()}", file=output)
    except (OSError, ValueError) as error:
        report = {"status": "error", "reason": str(error)}
    report.update(base_sha=args.base_sha, head_sha=args.head_sha)
    write_report(report, args)
    return 2 if report["status"] == "error" else 0


if __name__ == "__main__":
    sys.exit(main())
