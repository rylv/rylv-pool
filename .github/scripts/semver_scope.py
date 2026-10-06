"""Select the crates.io baseline without failing for the first publication."""

import json
import os
import tomllib
import urllib.error
import urllib.request
from pathlib import Path


def main():
    package = tomllib.loads(Path("Cargo.toml").read_text())["package"]
    name = package["name"]
    current = package["version"]
    request = urllib.request.Request(
        f"https://crates.io/api/v1/crates/{name}",
        headers={"User-Agent": f"{name}-ci"},
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            versions = json.load(response)["versions"]
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise
        versions = []

    stable = [
        version["num"]
        for version in versions
        if not version["yanked"]
        and "-" not in version["num"]
        and "+" not in version["num"]
    ]
    baseline = max(stable, key=lambda value: tuple(map(int, value.split("."))), default="")
    required = False
    if baseline:
        major, minor, _ = map(int, current.split("-", 1)[0].split("+", 1)[0].split("."))
        old_major, old_minor, _ = map(int, baseline.split("."))
        breaking = major > old_major or (major == old_major == 0 and minor > old_minor)
        required = not breaking
        reason = "breaking version increment" if breaking else "compatible version increment"
    else:
        reason = "no published stable version"

    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
        print(f"required={str(required).lower()}", file=output)
        print(f"baseline={baseline}", file=output)
    print(f"{name}: current={current}; baseline={baseline or 'none'}; {reason}")


if __name__ == "__main__":
    main()
