#!/usr/bin/env python3
"""Times a command until it has printed its result and exited, and until every process it started has exited.

A program can pass its result on and exit while a worker process it started is still finishing, as the fastest
Java entries do with the unmapping of the file. Benchmarks only see the first time; this reports both.

usage: lifetime.py <label> <runs> <command...>, printing a markdown table row after a warmup run
"""
import ctypes
import os
import subprocess
import sys
import time

PR_SET_CHILD_SUBREAPER = 36


def mean_and_stddev(values):
    mean = sum(values) / len(values)
    return mean, (sum((v - mean) ** 2 for v in values) / len(values)) ** 0.5


def main():
    label, runs, command = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
    # orphaned descendants are handed to this process instead of init, so that it can wait for them
    if ctypes.CDLL(None, use_errno=True).prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
        sys.exit(f"prctl failed: {os.strerror(ctypes.get_errno())}")
    until_result, until_all = [], []
    for run in range(runs + 1):
        start = time.perf_counter()
        subprocess.run(command, stdout=subprocess.DEVNULL, check=True)
        result_time = time.perf_counter() - start
        while True:
            try:
                os.wait()
            except ChildProcessError:
                break
        if run > 0:
            until_result.append(result_time)
            until_all.append(time.perf_counter() - start)
    (result_mean, result_dev), (all_mean, all_dev) = map(mean_and_stddev, (until_result, until_all))
    print(f"| {label} | {result_mean:.3f} ± {result_dev:.3f} s | {all_mean:.3f} ± {all_dev:.3f} s "
          f"| {all_mean - result_mean:.3f} s |")


if __name__ == "__main__":
    main()
