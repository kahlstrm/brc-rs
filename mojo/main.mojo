from std.os import getenv
from std.sys import argv, num_logical_cores

from brc import WORKER_ENV, calc, run_worker, thread_count


def main() raises:
    var job = getenv(WORKER_ENV)
    if job:
        run_worker(job)
        return
    var args = argv()
    var file_name = String(args[1]) if len(args) > 1 else String("measurements.txt")
    var processes = thread_count(getenv("BRC_THREADS"), num_logical_cores())
    print(calc(file_name, String(args[0]), processes), end="")
