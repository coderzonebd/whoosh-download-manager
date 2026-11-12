#!/usr/bin/env python3
"""Summarise tools/daemon-ab.sh output: medians per (scenario, variant), and
each variant's change against a baseline paired within rounds (the runs of
one round are consecutive lines, one per variant).

    python3 -I tools/ab-summary.py <output> <variants per round> [baseline]
"""
import collections
import statistics as st
import sys

path, nvar = sys.argv[1], int(sys.argv[2])
base = sys.argv[3] if len(sys.argv) > 3 else "old"
runs = [l.split() for l in open(path) if " wall " in l]
by_sc = collections.defaultdict(list)
for f in runs:
    by_sc[f[1]].append(f)
for sc, fs in by_sc.items():
    per = collections.defaultdict(lambda: ([], [], []))
    for f in fs:
        cpu, wall, flt = per[f[0]]
        cpu.append(float(f[9])); wall.append(float(f[3])); flt.append(int(f[13]))
    for v, (cpu, wall, flt) in sorted(per.items()):
        print(f"{sc:18} {v:30} n={len(cpu):2}  CPU-s/GiB median {st.median(cpu):.2f}"
              f" ({min(cpu):.2f}-{max(cpu):.2f})  wall median {st.median(wall):.2f} s"
              f"  page faults median {int(st.median(flt))}")
    diffs = collections.defaultdict(list)
    for i in range(0, len(fs), nvar):
        cpu = {f[0]: float(f[9]) for f in fs[i:i + nvar]}
        if base in cpu:
            for v, c in cpu.items():
                if v != base:
                    diffs[v].append(c / cpu[base] - 1)
    for v, d in diffs.items():
        lower = sum(1 for x in d if x < 0)
        print(f"{sc:18} {v:30} vs {base}: median {100 * st.median(d):+.0f}% "
              f"({lower}/{len(d)} rounds lower)")
