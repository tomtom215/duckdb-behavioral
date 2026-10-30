#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
"""Draw the ClickBench `hits` benchmark results as SVG. Offline, stdlib only.

Usage:
    scripts/bench-hits-charts.py [--results benches/hits/results] [--out docs/src/images]

Reads the JSON files written by `scripts/bench-hits.py` (the newest run for each
partition count) and writes three self-contained SVG cards:

  hits-throughput.svg  rows per second of each query at the largest scale, with the
                       min-max of its runs as a whisker and the plain-scan control
                       as the floor DuckDB itself costs
  hits-scaling.svg     nanoseconds per row at 1M, 10M and 100M rows: a flat line
                       means time grows in step with the data
  hits-vs-sql.svg      how much slower the same question is as hand-written SQL

Every figure on a chart is read from the JSON; nothing is typed in. The cards
draw their own dark surface so they read the same on a light or a dark page. The
colors are the validated dark steps of the dataviz reference palette
(blue `#3987e5`, orange `#d95926`, aqua `#199e70`), text in its text tokens.
"""

import argparse
import glob
import json
import math
import os
import pathlib
import sys
from xml.sax.saxutils import escape

ROOT = pathlib.Path(__file__).resolve().parent.parent

SURFACE, INK, INK_2, INK_3 = "#1a1a19", "#f0efec", "#c3c2b7", "#8c8b84"
GRID, TRACK, GRAY = "#2f2f2d", "#52514e", "#6b6a64"
BLUE, ORANGE, AQUA = "#3987e5", "#d95926", "#199e70"
FONT = "system-ui, -apple-system, 'Segoe UI', Roboto, 'Helvetica Neue', sans-serif"
MONO = "ui-monospace, 'SF Mono', Menlo, Consolas, monospace"
WIDTH = 920


def fail(message: str) -> None:
    sys.exit(f"bench-hits-charts: {message}")


def load_runs(results_dir: pathlib.Path) -> dict[int, dict]:
    """The newest verified run for each partition count."""
    newest: dict[int, dict] = {}
    for path in sorted(glob.glob(str(results_dir / "hits-*.json"))):
        doc = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
        if not doc.get("verified"):
            continue
        n = doc["settings"]["partitions"]
        if n not in newest or doc["environment"]["utc"] > newest[n]["environment"]["utc"]:
            newest[n] = doc
    return newest


def nice_step(top: float, ticks: int = 5) -> float:
    raw = top / ticks
    magnitude = 10 ** math.floor(math.log10(raw))
    for factor in (1, 2, 2.5, 5, 10):
        if raw <= factor * magnitude:
            return factor * magnitude
    return 10 * magnitude


def card(height: int, title: str, subtitle: str, body: str, footer: str, desc: str) -> str:
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {height}" width="{WIDTH}" height="{height}" role="img" aria-labelledby="t d">
<title id="t">{escape(title)}</title>
<desc id="d">{escape(desc)}</desc>
<rect width="{WIDTH}" height="{height}" rx="16" fill="{SURFACE}"/>
<text x="36" y="48" font-family="{FONT}" font-size="22" font-weight="650" fill="{INK}">{escape(title)}</text>
<text x="36" y="74" font-family="{FONT}" font-size="13.5" fill="{INK_2}">{escape(subtitle)}</text>
{body}
<text x="36" y="{height - 22}" font-family="{FONT}" font-size="11" fill="{INK_3}">{escape(footer)}</text>
</svg>
"""


def footer_for(doc: dict, extra: str) -> str:
    env, cfg = doc["environment"], doc["settings"]
    cpu = env["cpu"].replace("Intel(R) Xeon(R) Processor", "Xeon").replace("(R)", "")
    return (f"ClickBench hits, {doc['rows']:,} rows · {cpu}, {env['logical_cpus']} vCPU · {env['duckdb'].split()[0]} · "
            f"extension {env['extension_version']} · {extra}")


def throughput_chart(doc: dict) -> str:
    rows = doc["rows"]
    by_id = {q["id"]: q for q in doc["queries"]}
    control = by_id["scan"]
    behavioral = sorted((q for q in doc["queries"] if q["function"]), key=lambda q: -rows / q["time"]["median_s"])
    ordered = [control] + behavioral
    top = max(rows / q["time"]["min_s"] for q in ordered) / 1e6
    step = nice_step(top * 1.12)
    axis_max = math.ceil(top * 1.12 / step) * step
    x0, plot_w, row_h, bar_h, y0 = 300, 520, 36, 16, 112
    scale = plot_w / axis_max
    height = y0 + row_h * len(ordered) + 96
    out = []
    for i in range(int(axis_max / step) + 1):
        value = i * step
        x = x0 + value * scale
        out.append(f'<line x1="{x:.1f}" y1="{y0 - 14}" x2="{x:.1f}" y2="{y0 + row_h * len(ordered) - 6}" stroke="{GRID}" stroke-width="1"/>')
        out.append(f'<text x="{x:.1f}" y="{y0 + row_h * len(ordered) + 14}" text-anchor="middle" font-family="{FONT}" font-size="11" fill="{INK_3}">{value:g}</text>')
    out.append(f'<text x="{x0 + plot_w / 2}" y="{y0 + row_h * len(ordered) + 34}" text-anchor="middle" font-family="{FONT}" font-size="11.5" fill="{INK_2}">million rows per second (median of {doc["settings"]["reps"]} runs; whisker = fastest to slowest run)</text>')
    for i, q in enumerate(ordered):
        cy = y0 + i * row_h + row_h / 2 - 6
        median = rows / q["time"]["median_s"] / 1e6
        fast, slow = rows / q["time"]["min_s"] / 1e6, rows / q["time"]["max_s"] / 1e6
        is_control = q["id"] == "scan"
        color = GRAY if is_control else BLUE
        label = "scan (control)" if is_control else q["id"]
        sub = "plain per-user aggregate" if is_control else ("all rows, one group" if q["shape"] == "one group" else "one group per user")
        out.append(f'<text x="{x0 - 16}" y="{cy - 1:.1f}" text-anchor="end" font-family="{MONO}" font-size="12.5" fill="{INK}">{escape(label)}</text>')
        out.append(f'<text x="{x0 - 16}" y="{cy + 13:.1f}" text-anchor="end" font-family="{FONT}" font-size="10.5" fill="{INK_3}">{sub}</text>')
        width = median * scale
        out.append(f'<path d="M{x0},{cy - bar_h / 2 + 4:.1f} h{max(width - 4, 0):.1f} a4,4 0 0 1 4,4 v{bar_h - 8} a4,4 0 0 1 -4,4 h-{max(width - 4, 0):.1f} z" fill="{color}">'
                   f'<title>{escape(q["id"])}: {median:.1f} M rows/s, median {q["time"]["median_s"]:.3f} s (runs {q["time"]["min_s"]:.3f} to {q["time"]["max_s"]:.3f} s)</title></path>')
        xa, xb = x0 + slow * scale, x0 + fast * scale
        out.append(f'<path d="M{xa:.1f},{cy - 5:.1f} v10 M{xa:.1f},{cy:.1f} H{xb:.1f} M{xb:.1f},{cy - 5:.1f} v10" stroke="{INK}" stroke-width="1.4" fill="none" opacity="0.85"/>')
        out.append(f'<text x="{xb + 10:.1f}" y="{cy + 4.5:.1f}" font-family="{FONT}" font-size="12.5" font-weight="600" fill="{INK}">{median:.1f}'
                   f'<tspan font-weight="400" fill="{INK_2}"> · {q["time"]["median_s"]:.2f} s</tspan></text>')
    sessionize = by_id["sessionize"]
    desc = (f"Horizontal bars of throughput in million rows per second for {len(ordered)} queries over {rows:,} rows; "
            f"scan control {rows / control['time']['median_s'] / 1e6:.1f}, sessionize {rows / sessionize['time']['median_s'] / 1e6:.1f}.")
    return card(height, f"Throughput at {doc['settings']['partitions']}M rows",
                "Each behavioral query beside a plain scan of the same columns: the gap is what the function adds.",
                "\n".join(out), footer_for(doc, f"commit {doc['environment']['git_commit'][:7]}"), desc)


def scaling_chart(runs: dict[int, dict]) -> str:
    scales = sorted(runs)
    ids = [q["id"] for q in runs[scales[0]]["queries"] if q["function"]]
    def ns_per_row(scale: int, qid: str) -> float:
        q = next(q for q in runs[scale]["queries"] if q["id"] == qid)
        return q["time"]["median_s"] / runs[scale]["rows"] * 1e9
    highlight = [("sessionize", BLUE), ("window_funnel", ORANGE), ("sequence_next_node", AQUA)]
    top = max(ns_per_row(s, i) for s in scales for i in ids)
    step = nice_step(top * 1.1)
    axis_max = math.ceil(top * 1.1 / step) * step
    x0, plot_w, y0, plot_h = 90, 640, 130, 300
    xs = {s: x0 + plot_w * (math.log10(runs[s]["rows"]) - math.log10(runs[scales[0]]["rows"])) /
          (math.log10(runs[scales[-1]]["rows"]) - math.log10(runs[scales[0]]["rows"])) for s in scales}
    y = lambda v: y0 + plot_h - v / axis_max * plot_h
    out = []
    for i in range(int(axis_max / step) + 1):
        value = i * step
        out.append(f'<line x1="{x0}" y1="{y(value):.1f}" x2="{x0 + plot_w}" y2="{y(value):.1f}" stroke="{GRID}" stroke-width="1"/>')
        out.append(f'<text x="{x0 - 12}" y="{y(value) + 4:.1f}" text-anchor="end" font-family="{FONT}" font-size="11" fill="{INK_3}">{value:g}</text>')
    out.append(f'<text x="{x0 - 54}" y="{y0 - 6}" font-family="{FONT}" font-size="11.5" fill="{INK_2}">nanoseconds per row</text>')
    for s in scales:
        out.append(f'<text x="{xs[s]:.1f}" y="{y0 + plot_h + 24}" text-anchor="middle" font-family="{FONT}" font-size="12" fill="{INK_2}">{runs[s]["rows"] / 1e6:,.0f}M rows</text>')
    others = [i for i in ids if i not in dict(highlight)]
    for qid in others:
        pts = " ".join(f"{xs[s]:.1f},{y(ns_per_row(s, qid)):.1f}" for s in scales)
        out.append(f'<polyline points="{pts}" fill="none" stroke="{TRACK}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"><title>{escape(qid)}</title></polyline>')
    labels = []
    for qid, color in highlight:
        pts = " ".join(f"{xs[s]:.1f},{y(ns_per_row(s, qid)):.1f}" for s in scales)
        out.append(f'<polyline points="{pts}" fill="none" stroke="{color}" stroke-width="2.5" stroke-linejoin="round" stroke-linecap="round"><title>{escape(qid)}</title></polyline>')
        for s in scales:
            out.append(f'<circle cx="{xs[s]:.1f}" cy="{y(ns_per_row(s, qid)):.1f}" r="4.5" fill="{color}" stroke="{SURFACE}" stroke-width="2"><title>{escape(qid)} at {runs[s]["rows"] / 1e6:,.0f}M rows: {ns_per_row(s, qid):.1f} ns per row</title></circle>')
        labels.append([y(ns_per_row(scales[-1], qid)), qid, color, ns_per_row(scales[-1], qid)])
    labels.sort()
    for k in range(1, len(labels)):
        labels[k][0] = max(labels[k][0], labels[k - 1][0] + 18)
    for ly, qid, color, value in labels:
        out.append(f'<text x="{x0 + plot_w + 16}" y="{ly + 4:.1f}" font-family="{MONO}" font-size="12" fill="{INK}">{escape(qid)}'
                   f'<tspan font-family="{FONT}" fill="{INK_2}"> {value:.1f}</tspan></text>')
    lx = 36
    for name, color in [(q, c) for q, c in highlight] + [("the other seven", TRACK)]:
        out.append(f'<line x1="{lx}" y1="92" x2="{lx + 18}" y2="92" stroke="{color}" stroke-width="3" stroke-linecap="round"/>')
        out.append(f'<text x="{lx + 26}" y="96" font-family="{FONT}" font-size="12" fill="{INK_2}">{escape(name)}</text>')
        lx += 26 + 8 * len(name) + 30
    last = scales[-1]
    desc = ("Lines of nanoseconds per row against scale for every behavioral query; "
            + ", ".join(f"{q} {ns_per_row(last, q):.1f} ns at {runs[last]['rows'] / 1e6:,.0f}M rows" for q, _ in highlight) + ".")
    return card(plot_h + 230, "Cost per row as the data grows",
                "A flat line means time grows in step with the rows. Small scales carry the fixed cost of opening the files and loading the extension.",
                "\n".join(out), footer_for(runs[last], f"median of {runs[last]['settings']['reps']} runs at {last}M rows"), desc)


def vs_sql_chart(runs: dict[int, dict]) -> str:
    scales = sorted(runs)
    panels = [("sessionize", "sessions per user, 30-minute gap"), ("retention", "seven-day retention")]
    def pair(scale: int, qid: str) -> tuple[float, float]:
        q = next(q for q in runs[scale]["queries"] if q["id"] == qid)
        return q["time"]["median_s"], q["baseline_time"]["median_s"]
    ratios = [pair(s, q)[1] / pair(s, q)[0] for s in scales for q, _ in panels]
    top = max(max(ratios) * 1.15, 1.6)
    step = nice_step(top, 4)
    axis_max = math.ceil(top / step) * step
    plot_h, y0, panel_w, col_w = 250, 140, 390, 44
    plot_end = panel_w - 96
    out, xl = [], 36
    for qid, caption in panels:
        x0 = xl + 44
        y = lambda v: y0 + plot_h - v / axis_max * plot_h
        out.append(f'<text x="{xl}" y="{y0 - 34}" font-family="{MONO}" font-size="13.5" fill="{INK}">{qid}</text>')
        out.append(f'<text x="{xl + 8 * len(qid) + 12}" y="{y0 - 34}" font-family="{FONT}" font-size="12" fill="{INK_3}">{escape(caption)}</text>')
        for i in range(int(axis_max / step) + 1):
            value = i * step
            out.append(f'<line x1="{x0}" y1="{y(value):.1f}" x2="{x0 + plot_end}" y2="{y(value):.1f}" stroke="{GRID}" stroke-width="1"/>')
            out.append(f'<text x="{x0 - 8}" y="{y(value) + 4:.1f}" text-anchor="end" font-family="{FONT}" font-size="11" fill="{INK_3}">{value:g}×</text>')
        out.append(f'<line x1="{x0}" y1="{y(1):.1f}" x2="{x0 + plot_end}" y2="{y(1):.1f}" stroke="{INK_3}" stroke-width="1.4" stroke-dasharray="5 4"/>')
        out.append(f'<text x="{x0 + plot_end + 8}" y="{y(1) + 4:.1f}" font-family="{FONT}" font-size="10.5" fill="{INK_3}">same speed</text>')
        gap = (plot_end - col_w * len(scales)) / (len(scales) + 1)
        for k, s in enumerate(scales):
            ext, sql = pair(s, qid)
            ratio = sql / ext
            bx = x0 + gap * (k + 1) + col_w * k
            h = ratio / axis_max * plot_h
            out.append(f'<path d="M{bx:.1f},{y0 + plot_h} v-{max(h - 4, 0):.1f} a4,4 0 0 1 4,-4 h{col_w - 8} a4,4 0 0 1 4,4 v{max(h - 4, 0):.1f} z" fill="{BLUE}">'
                       f'<title>{qid} at {runs[s]["rows"] / 1e6:,.0f}M rows: extension {ext:.3f} s, pure SQL {sql:.3f} s, ratio {ratio:.2f}</title></path>')
            out.append(f'<text x="{bx + col_w / 2:.1f}" y="{y0 + plot_h - h - 9:.1f}" text-anchor="middle" font-family="{FONT}" font-size="13" font-weight="650" fill="{INK}">{ratio:.2f}×</text>')
            out.append(f'<text x="{bx + col_w / 2:.1f}" y="{y0 + plot_h + 20}" text-anchor="middle" font-family="{FONT}" font-size="11.5" fill="{INK_2}">{runs[s]["rows"] / 1e6:,.0f}M</text>')
            out.append(f'<text x="{bx + col_w / 2:.1f}" y="{y0 + plot_h + 37}" text-anchor="middle" font-family="{FONT}" font-size="10.5" fill="{INK_3}">ext {ext:.2f} s</text>')
            out.append(f'<text x="{bx + col_w / 2:.1f}" y="{y0 + plot_h + 51}" text-anchor="middle" font-family="{FONT}" font-size="10.5" fill="{INK_3}">SQL {sql:.2f} s</text>')
        xl += panel_w + 40
    last = scales[-1]
    desc = "Columns of how many times longer the same question takes as hand-written SQL than as the extension function: " + "; ".join(
        f"{q} {pair(s, q)[1] / pair(s, q)[0]:.2f}x at {runs[s]['rows'] / 1e6:,.0f}M rows" for q, _ in panels for s in scales) + "."
    return card(plot_h + 250, "The same question, written as SQL",
                "Time of the equivalent SQL divided by the extension's, on the same engine and data. The answers are identical.",
                "\n".join(out), footer_for(runs[last], "extension vs pure-SQL equivalent, medians"), desc)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--results", default=str(ROOT / "benches/hits/results"))
    parser.add_argument("--out", default=str(ROOT / "docs/src/images"))
    args = parser.parse_args()
    runs = load_runs(pathlib.Path(args.results))
    if not runs:
        fail(f"no verified result JSON in {args.results}")
    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    files = {"hits-throughput.svg": throughput_chart(runs[max(runs)])}
    if len(runs) >= 2:
        files["hits-scaling.svg"] = scaling_chart(runs)
        files["hits-vs-sql.svg"] = vs_sql_chart(runs)
    for name, svg in files.items():
        (out / name).write_text(svg, encoding="utf-8")
        print(f"wrote {os.path.relpath(out / name, ROOT)} ({len(svg):,} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
