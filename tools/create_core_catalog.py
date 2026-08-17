#!/usr/bin/env python3
"""Create an approved Hakoniwa Core catalog from verified release archives.

The caller must provide SHA-256 hashes produced by the release build. This script
never downloads or trusts binaries itself; it serializes already-approved facts.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

PLATFORM_MAP = {
    "windows-x64": ("windows", "x64", "bin/hako-cmd.exe"),
    "macos-x64": ("macos", "x64", "bin/hako-cmd"),
    "macos-arm64": ("macos", "arm64", "bin/hako-cmd"),
    "linux-x64": ("linux", "x64", "bin/hako-cmd"),
}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--release-base-url", required=True, help="HTTPS release download root, without a trailing slash")
    parser.add_argument("--artifact", action="append", default=[], metavar="PLATFORM=ARCHIVE", help="for example linux-x64=dist/hakoniwa-core-pro-v1.3.0-linux-x64.zip")
    parser.add_argument("--output", required=True)
    arguments = parser.parse_args()

    if not arguments.release_base_url.startswith("https://"):
        raise SystemExit("release-base-url must start with https://")
    artifacts = []
    for item in arguments.artifact:
        try:
            platform_id, archive_name = item.split("=", 1)
            platform, architecture, command_path = PLATFORM_MAP[platform_id]
        except (ValueError, KeyError) as error:
            raise SystemExit(f"invalid artifact declaration: {item}") from error
        archive = Path(archive_name)
        if not archive.is_file():
            raise SystemExit(f"archive not found: {archive}")
        filename = archive.name
        artifacts.append(
            {
                "platform": platform,
                "architecture": architecture,
                "url": f"{arguments.release_base_url.rstrip('/')}/{filename}",
                "sha256": sha256(archive),
                "archive_format": "zip",
                "hako_cmd_relative_path": command_path,
                "install_root": None,
                "provenance": {
                    "repository": "https://github.com/hakoniwalab/hakoniwa-core-pro",
                    "release_tag": arguments.version,
                    "build_workflow": "publish-core-artifacts.yml",
                },
            }
        )
    if not artifacts:
        raise SystemExit("at least one --artifact is required")
    catalog = {
        "schema_version": 1,
        "component": "hakoniwa-core-pro",
        "publisher": "Hakoniwa Desktop Manager maintainers",
        "releases": [{"version": arguments.version, "source_revision": arguments.revision, "artifacts": artifacts}],
    }
    output = Path(arguments.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(catalog, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
