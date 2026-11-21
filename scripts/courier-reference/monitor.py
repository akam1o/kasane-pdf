#!/usr/bin/env python3
"""Resource samples of the machine a job runs on, appended and fsynced
every INTERVAL seconds until it is stopped (or the machine is lost).

    scripts/courier-reference/monitor.py OUT.jsonl [--interval 10] [--path DIR ...]

Each line: the time, memory available and total, swap in use, the load
average, free space on each --path, the number of processes, and the
three largest by resident memory — their command name and RSS only (no
arguments, no paths). Linux (/proc); elsewhere it writes what it can.
Diagnostics of a run, never part of its verdict.
"""
import argparse, json, os, shutil, signal, sys, time


def meminfo():
    out = {}
    try:
        with open("/proc/meminfo") as fh:
            for line in fh:
                k, v = line.split(":", 1)
                out[k] = int(v.split()[0])
    except OSError:
        pass
    return out


def processes():
    procs = []
    for pid in os.listdir("/proc") if os.path.isdir("/proc") else []:
        if not pid.isdigit():
            continue
        try:
            with open(f"/proc/{pid}/status") as fh:
                st = dict(l.split(":", 1) for l in fh if ":" in l)
            procs.append((int(st.get("VmRSS", "0 kB").split()[0]), st.get("Name", "?").strip()))
        except (OSError, ValueError):
            continue
    procs.sort(reverse=True)
    return len(procs), [{"name": n, "rss_kb": r} for r, n in procs[:3]]


def sample(paths):
    m = meminfo()
    n, top = processes()
    s = {"time": round(time.time(), 1), "mem_total_kb": m.get("MemTotal"), "mem_available_kb": m.get("MemAvailable"),
         "swap_used_kb": (m["SwapTotal"] - m["SwapFree"]) if "SwapTotal" in m else None,
         "processes": n, "top_rss": top}
    try:
        s["load"] = [float(x) for x in open("/proc/loadavg").read().split()[:3]]
    except OSError:
        s["load"] = list(os.getloadavg())
    s["disk_free_bytes"] = {}
    for p in paths:
        try:
            s["disk_free_bytes"][p] = shutil.disk_usage(p).free
        except OSError:
            s["disk_free_bytes"][p] = None
    return s


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("out")
    ap.add_argument("--interval", type=float, default=10)
    ap.add_argument("--path", action="append", default=[])
    a = ap.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    with open(a.out, "a") as fh:
        while True:
            fh.write(json.dumps(sample(a.path or ["/"]), sort_keys=True) + "\n")
            fh.flush()
            os.fsync(fh.fileno())
            time.sleep(a.interval)


if __name__ == "__main__":
    sys.exit(main())
