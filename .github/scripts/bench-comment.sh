#!/usr/bin/env bash
# Renders a markdown comparison of the "base" and "PR" results of a hyperfine JSON export.
# usage: bench-comment.sh <hyperfine.json> <base ref> <outputs identical: true|false>
set -euo pipefail

jq -r --arg base_ref "$2" --argjson identical "$3" '
  def round_to($digits): (. * pow(10; $digits) | round) / pow(10; $digits);
  def metrics: [
    ["time_wall_clock", "Wall time", 1, "s", 3],
    ["time_cpu", "CPU time", 1, "s", 2],
    ["instructions", "Instructions", 1e-9, "B", 2],
    ["cpu_cycles", "Cycles", 1e-9, "B", 2],
    ["branch_misses", "Branch misses", 1e-6, "M", 1],
    ["memory_peak_resident", "Peak memory", (1 / 1048576), "MiB", 1]
  ];
  (.results | map({key: .name, value: .summary}) | from_entries) as $results
  | "<!-- bench-compare -->",
    "### Benchmark against `\($base_ref)`",
    "",
    "1B rows on `ubuntu-latest`, \(.results[0].measurements | length) runs each.",
    "",
    "| Metric | Base | PR | Change |",
    "|---|---:|---:|---:|",
    (metrics[] as [$key, $label, $scale, $unit, $digits]
      | select($results.base[$key] and $results.PR[$key])
      | [$results.base[$key], $results.PR[$key]] as [$base, $pr]
      | (($pr.mean - $base.mean) / $base.mean * 100 | round_to(1) + 0) as $change
      | "| \($label) "
        + "| \($base.mean * $scale | round_to($digits)) ± \($base.stddev * $scale | round_to($digits)) \($unit) "
        + "| \($pr.mean * $scale | round_to($digits)) ± \($pr.stddev * $scale | round_to($digits)) \($unit) "
        + "| \(if $change > 0 then "+" else "" end)\($change)% |"),
    "",
    if $identical then "Output identical to base ✅" else "**Output differs from base** ❌" end
' "$1"
