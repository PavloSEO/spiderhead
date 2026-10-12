"""Run one complete pytest collection in four disjoint whole-file processes."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import xml.etree.ElementTree as ET
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--collect-only", action="store_true")
    parser.add_argument("paths", nargs="*", default=["tests"])
    args = parser.parse_args()
    if any("::" in path for path in args.paths):
        parser.error("select whole files or directories, not individual test nodes")
    out = args.out_dir.resolve()
    if out.is_relative_to(ROOT):
        parser.error("output must be outside the checkout to preserve native provenance")
    out.mkdir(parents=True, exist_ok=False)
    env = {**os.environ, "PYTHONPATH": str(ROOT)}
    collected = subprocess.run(
        [sys.executable, "-m", "pytest", "--collect-only", "-q", "--color=no", *args.paths],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    (out / "collection.txt").write_text(collected.stdout + collected.stderr, encoding="utf-8")
    if collected.returncode:
        print(collected.stdout + collected.stderr, flush=True)
        return collected.returncode
    nodes = [
        line
        for line in collected.stdout.splitlines()
        if "::" in line and line.split("::", 1)[0].endswith(".py")
    ]
    if not nodes or len(nodes) != len(set(nodes)):
        raise ValueError("pytest collection must contain distinct test node IDs")
    counts = Counter(node.split("::", 1)[0] for node in nodes)
    groups: list[list[str]] = [[] for _ in range(4)]
    sizes = [0] * 4
    for path in sorted(counts, key=lambda path: (-counts[path], path)):
        index = min(range(4), key=lambda index: sizes[index])
        groups[index].append(path)
        sizes[index] += counts[path]
    files = [path for group in groups for path in group]
    grouped_nodes = [node for group in groups for node in nodes if node.split("::", 1)[0] in group]
    if len(files) != len(set(files)) or Counter(grouped_nodes) != Counter(nodes):
        raise ValueError("file groups must be disjoint and cover the exact pytest collection")
    (out / "groups.json").write_text(
        json.dumps({"nodes": nodes, "files": groups, "counts": sizes}, indent=2) + "\n",
        encoding="utf-8",
    )
    print(f"Collection: {len(nodes)} tests; group counts: {sizes}", flush=True)
    if args.collect_only:
        return 0

    jobs = []
    for index, group in enumerate(groups):
        if not group:
            continue
        log = (out / f"group-{index}.log").open("w", encoding="utf-8")
        child_env = {**env, "COVERAGE_FILE": str(out / f".coverage-group-{index}")}
        process = subprocess.Popen(
            [
                sys.executable,
                "-m",
                "pytest",
                "-q",
                "--cov=seohead",
                "--cov-report=",
                "--durations=10",
                f"--junitxml={out / f'group-{index}.xml'}",
                f"--basetemp={out / f'group-{index}-temp'}",
                *sorted(group),
            ],
            cwd=ROOT,
            env=child_env,
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        jobs.append((index, process, log))
    failed = False
    for index, process, log in jobs:
        code = process.wait()
        log.close()
        failed |= code != 0
        print(f"Group {index}: exit {code}", flush=True)
        print((out / f"group-{index}.log").read_text(encoding="utf-8"), flush=True)
    for index, _, _ in jobs:
        cases = ET.parse(out / f"group-{index}.xml").getroot().findall(".//testcase")
        if len(cases) != sizes[index]:
            raise ValueError(
                f"group {index} ran {len(cases)} tests; collection requires {sizes[index]}"
            )
    coverage_env = {**env, "COVERAGE_FILE": str(out / ".coverage")}
    subprocess.run(
        [
            sys.executable,
            "-m",
            "coverage",
            "combine",
            "--keep",
            *(str(out / f".coverage-group-{index}") for index, _, _ in jobs),
        ],
        cwd=ROOT,
        env=coverage_env,
        check=True,
    )
    subprocess.run(
        [sys.executable, "-m", "coverage", "report", "-m"],
        cwd=ROOT,
        env=coverage_env,
        check=True,
    )
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
