"""Verify the release tag and write the matching, nonempty changelog section."""

import re
import sys
import tomllib
from pathlib import Path


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: release_notes.py vX.Y.Z output.md")
    tag, destination = sys.argv[1:]
    version = tomllib.loads(Path("Cargo.toml").read_text())["package"]["version"]
    if tag != f"v{version}" or not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
        raise SystemExit(f"tag {tag!r} does not match Cargo.toml version v{version}")

    match = re.search(
        rf"^## \[{re.escape(version)}\][^\n]*\n(.*?)(?=^## \[|\Z)",
        Path("CHANGELOG.md").read_text(),
        flags=re.MULTILINE | re.DOTALL,
    )
    if match is None or not match.group(1).strip():
        raise SystemExit(f"CHANGELOG.md has no release notes for {version}")

    output = Path(destination)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(match.group(1).strip() + "\n")
    print(f"Release {tag}; notes written to {output}")


if __name__ == "__main__":
    main()
