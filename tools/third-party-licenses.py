#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Generates THIRD_PARTY_LICENSES from Cargo.toml and Cargo.lock.

Walks the normal (linked) dependency graph of the binary for the image's target platform, as
`cargo metadata --locked` resolves it, and writes a summary table and every crate's license files
from the cargo registry (top-level LICENSE*, LICENCE*, COPYING*, COPYRIGHT*, NOTICE* files). A
crate that publishes none is recorded with its declared license. A crate whose declared license is
not on the allowlist below fails the run, so nothing incompatible with MIT distribution slips in.

    python3 tools/third-party-licenses.py           # rewrite THIRD_PARTY_LICENSES
    python3 tools/third-party-licenses.py --check   # exit 1 if it is out of date

Run `cargo fetch --locked` first so every crate's sources are in the registry.
"""

import json
import re
import subprocess
import sys
from pathlib import Path

TARGET = "x86_64-unknown-linux-musl"
OUTPUT = "THIRD_PARTY_LICENSES"
# SPDX identifiers that may appear in a declared license expression.
ALLOWED = {
    "MIT", "MIT-0", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause",
    "0BSD", "ISC", "Zlib", "BSL-1.0", "Unicode-3.0", "Unicode-DFS-2016", "CC0-1.0", "Unlicense",
}
# C libraries a -sys crate bundles and compiles in: their license files, below the crate root. Only
# the library is compiled (lz4's GPL-2.0 command-line programs are not; zstd is dual BSD/GPLv2 and
# used under BSD; mimalloc v3 is a feature that is not enabled).
BUNDLED = {
    "libmimalloc-sys": ["c_src/mimalloc/v2/LICENSE"],
    "lz4-sys": ["liblz4/lib/LICENSE"],
    "zstd-sys": ["zstd/LICENSE"],
}
LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|copyright|notice)", re.IGNORECASE)

ROOT = Path(__file__).resolve().parent.parent

TOOLCHAIN = """\
Rust toolchain and container base image

The binary is compiled with the Rust toolchain (rust:1.95-alpine, pinned by digest, used only to
build) for x86_64-unknown-linux-musl and linked statically: the Rust standard library (MIT OR
Apache-2.0), musl libc (MIT, https://musl.libc.org) and LLVM's libunwind (Apache-2.0 WITH
LLVM-exception) are linked into it. The image is FROM scratch: it holds that one binary and two
empty directories, and no other software. The builder image is not shipped and is not part of this
repository's source."""


def metadata() -> dict:
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", TARGET],
        cwd=ROOT, check=True, capture_output=True, text=True,
    ).stdout
    return json.loads(out)


def linked(meta: dict) -> list[dict]:
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = meta["resolve"]["root"]
    seen, todo = set(), [root]
    while todo:
        node = nodes[todo.pop()]
        for dep in node["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]) and dep["pkg"] not in seen:
                seen.add(dep["pkg"])
                todo.append(dep["pkg"])
    seen.discard(root)
    return sorted((packages[i] for i in seen), key=lambda p: (p["name"], version_key(p["version"])))


def version_key(v: str):
    return [int(x) if x.isdigit() else x for x in re.split(r"[.+-]", v)]


def check_license(pkg: dict) -> None:
    expr = (pkg.get("license") or "").replace("/", " OR ")
    terms = [t.strip(" ()") for t in re.split(r"\bOR\b|\bAND\b", expr)]
    bad = [t for t in terms if t not in ALLOWED]
    if not expr or bad:
        raise SystemExit(f"{pkg['name']} {pkg['version']}: license {pkg.get('license')!r} is not allowed")


def source(pkg: dict) -> str:
    repo = (pkg.get("repository") or pkg.get("homepage") or "").strip()
    return re.sub(r"\.git/?$", "", repo).rstrip("/")


def table(rows: list[list[str]]) -> str:
    widths = [max(len(r[i]) for r in rows) + 2 for i in range(len(rows[0]) - 1)]
    lines = []
    for r in rows:
        cells = [c.ljust(w) for c, w in zip(r, widths)] + [r[-1]]
        lines.append("".join(cells).rstrip())
    return "\n".join(lines)


def render() -> str:
    packages = linked(metadata())
    for p in packages:
        check_license(p)
    rows = [["Component", "Version", "License", "How used", "Source"]]
    rows += [[p["name"], p["version"], p["license"], "Rust crate (linked)", source(p)] for p in packages]
    parts = [
        "Third-party software in lake-polars\n\n"
        "lake-polars is licensed under the MIT License (see LICENSE). It includes or uses the components\n"
        "below, each under its own license. This file is generated; do not edit it by hand.\n"
        f"Generated with: python3 tools/third-party-licenses.py (cargo metadata --locked --filter-platform\n"
        f"{TARGET}, walking the normal (linked) dependency graph, with each crate's license files from\n"
        "the cargo registry)\n"
        "From: Cargo.toml and Cargo.lock\n\n"
        "Summary\n\n" + table(rows) + "\n\n" + TOOLCHAIN + "\n"
    ]
    for p in packages:
        crate_dir = Path(p["manifest_path"]).parent
        files = sorted(f for f in crate_dir.iterdir() if f.is_file() and LICENSE_FILE.match(f.name))
        files += [crate_dir / rel for rel in BUNDLED.get(p["name"], [])]
        block = [
            "=" * 80,
            f"{p['name']} {p['version']} — {p['license']}",
            source(p),
            "-" * 80,
        ]
        if files:
            for f in files:
                block.append(f"[{f.relative_to(crate_dir)}]")
                block.append(f.read_text(encoding="utf-8", errors="replace").strip("\n").lstrip())
                block.append("")
        else:
            block.append(f"No license file in the published crate; its declared license is {p['license']}.")
            block.append("")
        parts.append("\n".join(block))
    return "\n".join(parts)


def main() -> int:
    text = render()
    path = ROOT / OUTPUT
    if "--check" in sys.argv[1:]:
        if path.read_text(encoding="utf-8") != text:
            print(f"{OUTPUT} is out of date: run python3 tools/third-party-licenses.py", file=sys.stderr)
            return 1
        return 0
    path.write_text(text, encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
