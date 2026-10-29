#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
"""Check that the documentation agrees with the repository. Offline, stdlib only.

Usage:
    python3 -I scripts/check-docs.py

The same facts are written in many files (test counts, the DuckDB releases CI
loads the build into, the demo page's results), and every release used to leave
some of them stale. This reads the facts from their source and fails on a
disagreement:

  * test counts: `#[test]` attributes in src/ and tests/extension_load.rs
    against every "N unit" / "N integration" mention, and the SQL logic test
    directive counts in test/sql;
  * the DuckDB releases in .github/workflows/e2e.yml against every doc line
    that lists the releases CI checks;
  * relative links and #anchors in the Markdown files, and that every mdBook
    page is in SUMMARY.md;
  * demo/index.html's result tables against test/sql/git_mining.test.

CHANGELOG.md, PERF.md and LESSONS.md describe the past and are not checked for
counts. External URLs are not fetched.
"""

import collections
import pathlib
import re
import sys
import urllib.parse

ROOT = pathlib.Path(__file__).resolve().parent.parent
SITE = "https://tomtom215.github.io/duckdb-behavioral/"
problems: list[str] = []


def problem(message: str) -> None:
    problems.append(message)


def read(path: pathlib.Path) -> str:
    return path.read_text(encoding="utf-8")


def rel(path: pathlib.Path) -> str:
    return str(path.relative_to(ROOT))


# Files whose current claims are checked.
CURRENT_DOCS = [
    ROOT / name
    for name in (
        "README.md",
        "CLAUDE.md",
        "CONTRIBUTING.md",
        "SECURITY.md",
        "RELEASING.md",
        "description.yml",
        ".github/PULL_REQUEST_TEMPLATE.md",
        "examples/README.md",
    )
] + sorted((ROOT / "docs/src").rglob("*.md"))


def numbered_lines(path: pathlib.Path):
    yield from enumerate(read(path).split("\n"), 1)


# ── Test counts ──────────────────────────────────────────────────────────

TEST_ATTRIBUTE = re.compile(r"^\s*#\[test\]", re.M)
unit = sum(len(TEST_ATTRIBUTE.findall(read(p))) for p in (ROOT / "src").rglob("*.rs"))
integration = len(TEST_ATTRIBUTE.findall(read(ROOT / "tests/extension_load.rs")))
queries = statements = 0
for test_file in (ROOT / "test/sql").glob("*.test"):
    text = read(test_file)
    queries += len(re.findall(r"^query", text, re.M))
    statements += len(re.findall(r"^statement", text, re.M))
directives = queries + statements

for doc in CURRENT_DOCS:
    for number, line in numbered_lines(doc):
        where = f"{rel(doc)}:{number}"
        for m in re.finditer(r"\b(\d{3})\s+unit", line):
            if int(m.group(1)) != unit:
                problem(f"{where}: says {m.group(1)} unit tests, the source has {unit}")
        for m in re.finditer(r"\b(\d+)\s+(?:in-process )?integration", line):
            if int(m.group(1)) != integration:
                problem(
                    f"{where}: says {m.group(1)} integration tests, "
                    f"the source has {integration}"
                )
        for m in re.finditer(r"\b(\d+)\s+directives", line):
            if int(m.group(1)) != directives:
                problem(f"{where}: says {m.group(1)} directives, test/sql has {directives}")
        for m in re.finditer(r"\b(\d+)\s+result-checked queries", line):
            if int(m.group(1)) != queries:
                problem(
                    f"{where}: says {m.group(1)} result-checked queries, "
                    f"test/sql has {queries}"
                )

# ── DuckDB releases CI loads the build into ──────────────────────────────

e2e = read(ROOT / ".github/workflows/e2e.yml")
matrix = re.findall(r"v(\d+\.\d+\.\d+)", re.search(r"duckdb:\s*\[(.*?)\]", e2e, re.S).group(1))
primary = re.search(r'DUCKDB_VERSION:\s*"v(\d+\.\d+\.\d+)"', e2e).group(1)


def version_key(version: str):
    return tuple(int(part) for part in version.split("."))


ci_versions = sorted(set(matrix + [primary]), key=version_key)
for doc in CURRENT_DOCS:
    # A sentence, not a line: prose wraps, and a list can break after any
    # release. Tables are skipped (the crash matrix lists releases for a
    # different reason).
    line_number = 1
    for paragraph in read(doc).split("\n\n"):
        if "|" not in paragraph:
            flat = " ".join(paragraph.split())
            for sentence in re.split(r"(?<=[.;:])\s+(?=[A-Z(`*])", flat):
                lists_ci_releases = (
                    "1.3.2" in sentence
                    and "1.5.6" in sentence
                    and any(word in sentence for word in ("CI", "checks", "loads"))
                )
                missing = [v for v in ci_versions if v not in sentence]
                if lists_ci_releases and missing:
                    problem(
                        f"{rel(doc)}:{line_number}: lists the DuckDB releases CI checks "
                        f"but omits {missing} (e2e.yml: {ci_versions})"
                    )
        line_number += paragraph.count("\n") + 2

# ── Links, anchors, mdBook coverage ──────────────────────────────────────


def slug_github(heading: str) -> str:
    heading = re.sub(r"`", "", heading)
    # `\<str\>` is escaped text, not an HTML tag: keep its letters.
    heading = heading.replace("\\<", "\x00").replace("\\>", "\x01")
    heading = re.sub(r"<[^>]+>", "", heading)
    heading = heading.replace("\x00", "<").replace("\x01", ">")
    heading = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", heading)
    return "".join(
        "-" if ch == " " else ch
        for ch in heading.strip().lower()
        if ch.isalnum() or ch in "_- "
    )


def slug_mdbook(heading: str) -> str:
    heading = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", heading)
    heading = re.sub(r"<[^>]+>", "", heading.replace("`", ""))
    return "".join(
        "-" if ch.isspace() else ch.lower()
        for ch in heading.strip()
        if ch.isalnum() or ch in "_-" or ch.isspace()
    )


def heading_ids(path: pathlib.Path, mdbook: bool) -> set[str]:
    ids: set[str] = set()
    seen: collections.Counter = collections.Counter()
    in_fence = False
    for line in read(path).split("\n"):
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
        if in_fence:
            continue
        m = re.match(r"^#{1,6}\s+(.*?)\s*#*\s*$", line)
        if m:
            slug = (slug_mdbook if mdbook else slug_github)(m.group(1))
            ids.add(slug if seen[slug] == 0 else f"{slug}-{seen[slug]}")
            seen[slug] += 1
        ids.update(re.findall(r'<a\s+(?:name|id)="([^"]+)"', line))
    return ids


LINK_FILES = [
    ROOT / name
    for name in (
        "README.md",
        "CONTRIBUTING.md",
        "SECURITY.md",
        "RELEASING.md",
        "CHANGELOG.md",
        "PERF.md",
        "examples/README.md",
    )
] + sorted((ROOT / "docs/src").rglob("*.md"))
links = 0
for doc in LINK_FILES:
    in_book = "docs/src" in str(doc)
    text = re.sub(r"```.*?```", lambda m: "\n" * m.group(0).count("\n"), read(doc), flags=re.S)
    for number, line in enumerate(text.split("\n"), 1):
        targets = re.findall(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)", line)
        targets += re.findall(r'(?:href|src)="([^"]+)"', line)
        targets += re.findall(r"^\[[^\]]+\]:\s*(\S+)", line)
        for target in targets:
            links += 1
            where = f"{rel(doc)}:{number}"
            if target.startswith(SITE):
                page, _, fragment = target[len(SITE):].partition("#")
                page = page or "index.html"
                source = ROOT / "docs/src" / (page[:-5] + ".md" if page.endswith(".html") else page)
                if page == "demo.html":
                    source = ROOT / "demo/index.html"
                if not source.exists():
                    problem(f"{where}: site link {target} has no source page")
                elif fragment and source.suffix == ".md" and fragment not in heading_ids(source, True):
                    problem(f"{where}: site link {target}: no heading #{fragment}")
            elif target.startswith(("http://", "https://", "mailto:", "javascript:", "data:")):
                continue
            else:
                path, _, fragment = target.partition("#")
                destination = doc if path == "" else (doc.parent / urllib.parse.unquote(path)).resolve()
                if not destination.exists():
                    problem(f"{where}: broken relative link {target}")
                elif fragment and destination.suffix == ".md":
                    if fragment not in heading_ids(destination, "docs/src" in str(destination)):
                        problem(f"{where}: no heading #{fragment} in {rel(destination)}")

summary = read(ROOT / "docs/src/SUMMARY.md")
listed = set(re.findall(r"\]\(\./([^)]+\.md)\)", summary))
on_disk = {str(p.relative_to(ROOT / "docs/src")) for p in (ROOT / "docs/src").rglob("*.md")}
on_disk.discard("SUMMARY.md")
for page in sorted(on_disk - listed):
    problem(f"docs/src/SUMMARY.md: {page} is not listed")
for page in sorted(listed - on_disk):
    problem(f"docs/src/SUMMARY.md: {page} does not exist")

# ── demo/index.html against the SQL logic test it was copied from ────────

demo = read(ROOT / "demo/index.html")
git_mining = read(ROOT / "test/sql/git_mining.test")


def expected_rows(tag: str) -> list[list[str]]:
    section = git_mining.split(tag, 1)[1].split("----\n", 1)[1].split("\n\n", 1)[0]
    return [row.split("\t") for row in section.strip().split("\n")]


funnel_html = demo[demo.index('id="funnel"'):demo.index('id="sequence"')]
for author, step in expected_rows("GIT-5"):
    m = re.search(rf'<tr><td>{author}</td><td class="val-num">(\d+)</td></tr>', funnel_html)
    if m and m.group(1) != step:
        problem(
            f"demo/index.html: funnel row {author} shows {m.group(1)}, "
            f"test/sql/git_mining.test (GIT-5) expects {step}"
        )
for pr_id, timestamps in expected_rows("GIT-6"):
    m = re.search(rf"<tr><td>{pr_id}</td><td>(\[.*?\])</td></tr>", demo)
    if m:
        shown = len(re.findall(r"\d\d-\d\d \d\d:\d\d", m.group(1)))
        expected = timestamps.count("'") // 2
        if shown != expected:
            problem(
                f"demo/index.html: PR {pr_id} shows {shown} timestamps, "
                f"test/sql/git_mining.test (GIT-6) expects {expected}"
            )

print(
    f"facts: {unit} unit tests, {integration} integration tests, {queries} queries + "
    f"{statements} statements in test/sql, CI loads DuckDB {', '.join(ci_versions)}; "
    f"{links} links in {len(LINK_FILES)} files"
)
for message in problems:
    print(f"PROBLEM {message}")
print(f"{len(problems)} problem(s)")
sys.exit(1 if problems else 0)
