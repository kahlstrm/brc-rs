#!/usr/bin/env bash
# Renders a markdown comparison of the "base" and "PR" results of hyperfine JSON exports, one table per dataset.
# usage: bench-comment.sh <base ref> <outputs identical: true|false> (<dataset description> <hyperfine.json>)...
set -euo pipefail

base_ref=$1
identical=$2
shift 2

echo "<!-- bench-compare -->"
echo "### Benchmark against \`$base_ref\`"
while (($#)); do
  jq -r --arg dataset "$1" '
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
  | "",
    "\($dataset) on `ubuntu-latest`, \(.results[0].measurements | length) runs each.",
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
        + "| \(if $change > 0 then "+" else "" end)\($change)% |")
' "$2"
  shift 2
done
echo
if [ "$identical" = true ]; then echo "Output identical to base ✅"; else echo "**Output differs from base** ❌"; fi
