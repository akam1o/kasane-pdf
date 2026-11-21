#!/usr/bin/env python3
"""Whether an isolated reference (setup-e2.py) is isolated, by what its
Poppler does — not by the variables its wrapper sets.

    scripts/courier-reference/isolation-probe.py DIR --json OUT [--no-strace]

With DIR's wrapper (DIR/bin/pdftoppm), for each probe font of probe.py
(the Standard 14 names, their aliases, other names; not embedded):
- the file fontconfig matched (FC_DEBUG trace) is one of DIR's, and
  Courier / Courier New are DIR's Nimbus Mono PS of their style;
- the page's pixels (SHA-256) — recorded, for comparing runs: a cold and a
  warm cache, another directory, a polluted machine must give the same;
- on Linux, strace of the process: no font, fontconfig configuration or
  cache outside DIR is opened (attempts that fail are listed, not
  failures). fontconfig reads, whatever its configuration, its
  compiled-in template directory and what those templates include (the
  machine's local.conf, say) to describe them in fc-conflist as not
  enabled (`-`): such reads are counted apart, not failures — read, not
  applied (what is applied is the `+` list below; isolation-test.py puts
  rules in those places and checks E2 does not change).
And, with the wrapper's environment: every configuration file fontconfig
loads is DIR's, and DIR/conf.d is by content exactly the lock's; the fonts it lists are DIR's, and by content exactly the
lock's (DIR/inputs.json) — none missing, none added.

Exit 1 on any of those; 2 when a tool is missing (strace is required on
Linux unless --no-strace, which is then recorded). The JSON holds probe
names, file basenames, counts and hashes.
"""
import argparse, hashlib, importlib.util, json, os, re, shutil, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("probe", os.path.join(HERE, "probe.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)

FONT_EXT = (".ttf", ".otf", ".ttc", ".pfb", ".pfa", ".afm", ".t1", ".pcf", ".pcf.gz", ".woff", ".woff2", ".dfont")
FONT_PLACES = ("/etc/fonts", "/usr/share/fonts", "/usr/local/share/fonts", "/usr/share/fontconfig",
               "/var/cache/fontconfig", "/.fonts", "/.config/fontconfig", "/.cache/fontconfig", "/.fontconfig",
               "/.local/share/fonts", "/usr/share/ghostscript", "/usr/share/X11/fonts", "/Library/Fonts",
               "/System/Library/Fonts", "/opt/homebrew/etc/fonts", "/usr/local/etc/fonts")


def sha(path):
    with open(path, "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def wrapper_env(root):
    """The environment DIR's wrapper gives its tool — for fc-list and
    fc-conflist, which have no wrapper."""
    sys.path.insert(0, os.path.dirname(HERE))
    import compat_env
    env = {k: v for k, v in os.environ.items() if k not in compat_env.CLEARED and k != "FC_DEBUG"}
    env.update(HOME=os.path.join(root, "home"), FONTCONFIG_FILE=os.path.join(root, "fonts.conf"))
    return env


# fontconfig's compiled-in template directory: it parses the files there
# for the descriptions fc-conflist shows as `-` (not enabled) — read, not
# applied. Recorded apart; the enabled ones are fc-conflist's `+`, checked
# below, and courier-reference.yml's pollution test puts a rule there.
TEMPLATES = ("/usr/share/fontconfig/conf.avail", "/opt/homebrew/share/fontconfig/conf.avail")


# Opened outside the reference and not about fonts: the process's own
# input and output, libraries, the C library's and Poppler's data.
NOT_FONT = ("/proc/", "/sys/", "/dev/", "/etc/ld.so", "/usr/lib/locale/", "/usr/share/locale/",
            "/usr/lib/x86_64-linux-gnu/gconv/", "/usr/lib/aarch64-linux-gnu/gconv/", "/etc/localtime",
            "/usr/share/poppler/", "/etc/nsswitch.conf", "/etc/passwd", "/etc/group", "/usr/share/zoneinfo/",
            "/etc/gnutls/", "/usr/share/coreutils/")


def outside_font_access(log, root, parse_only=(), cwd=None, own=()):
    """(opened, parsed, attempted): font, configuration and cache paths
    outside root the process opened (shared libraries aside); the
    configuration files it read only to describe them — fontconfig's
    template directory and what fc-conflist lists as not enabled (`-`,
    `parse_only`); and paths it tried and could not open."""
    opened, templates, attempted = set(), set(), set()
    for line in open(log, errors="replace"):
        # open("p", …), openat(AT_FDCWD|fd, "p", …), openat2(…) — after
        # strace's pid prefix.
        m = re.search(r'\b(?:open|openat|openat2)\((?:(?:AT_FDCWD|-?\d+), )?"([^"]+)"[^)]*\)\s*=\s*(-?\d+)', line)
        if not m:
            continue
        path, rc = m.group(1), int(m.group(2))
        if not os.path.isabs(path):
            path = os.path.join(cwd or os.getcwd(), path)
        real = os.path.realpath(path) if rc >= 0 else os.path.normpath(path)
        if real.startswith(root + os.sep) or re.search(r"\.so(\.\d+)*$", real) or any(real == o or real.startswith(o + os.sep) for o in own) \
                or any(x.startswith(p) or x == p.rstrip("/") for p in NOT_FONT for x in (path, real)):
            continue
        # Anything else outside: a font, a configuration (any .conf or
        # XML, wherever), a cache — or a file of unknown purpose, which
        # is reported as opened too.
        if rc < 0:
            attempted.add(path)
        elif any(real == t or real.startswith(t + "/") for t in TEMPLATES) or path in parse_only or real in parse_only:
            templates.add(path)
        else:
            opened.add(path)
    return sorted(opened), sorted(templates), sorted(attempted)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("dir")
    ap.add_argument("--json", required=True)
    ap.add_argument("--no-strace", action="store_true")
    a = ap.parse_args()
    root = os.path.realpath(a.dir)
    tool = os.path.join(root, "bin", "pdftoppm")
    linux = sys.platform.startswith("linux")
    strace = shutil.which("strace") if linux and not a.no_strace else None
    if not os.access(tool, os.X_OK) or not shutil.which("fc-list") or (linux and not a.no_strace and not strace):
        print("isolation-probe: the wrapper, fc-list or (on Linux) strace missing")
        return 2
    env = wrapper_env(root)
    problems, rows = [], {}
    conf = subprocess.run(["fc-conflist"], capture_output=True, text=True, env=env)
    loaded = [l[2:].split(": ", 1)[0] for l in conf.stdout.splitlines() if l.startswith("+ ")]
    # Read for its description, not applied (fc-conflist's `-`).
    described = {l[2:].split(": ", 1)[0] for l in conf.stdout.splitlines() if l.startswith("- ")}
    with tempfile.TemporaryDirectory() as tmp:
        for base in list(probe.COURIER) + probe.OTHERS:
            pdf = os.path.join(tmp, "p.pdf")
            open(pdf, "wb").write(probe.probe_pdf(base))
            out = os.path.join(tmp, "o")
            argv = [tool, "-r", "72", "-gray", "-singlefile", pdf, out]
            log = os.path.join(tmp, "strace.log")
            if strace:
                argv = [strace, "-f", "-qq", "-e", "trace=open,openat,openat2", "-o", log] + argv
            r = subprocess.run(argv, capture_output=True, text=True, env=dict(os.environ, FC_DEBUG="1"), timeout=120)
            if r.returncode:
                problems.append(f"{base}: pdftoppm failed ({r.returncode})")
                continue
            files = probe.matched(r.stdout + r.stderr)
            row = {"matched": [os.path.basename(f) for f in files], "pixels": sha(out + ".pgm")}
            if not files or any(not os.path.realpath(f).startswith(root + os.sep) for f in files):
                problems.append(f"{base}: matched {files}, not a file of the reference")
            if base in probe.COURIER and row["matched"] != [f"NimbusMonoPS-{probe.COURIER[base]}.otf"]:
                problems.append(f"{base}: drew {row['matched']}, not Nimbus Mono PS {probe.COURIER[base]}")
            if strace:
                opened, parsed, attempted = outside_font_access(log, root, described, own=(os.path.realpath(tmp),))
                row["opened_outside"], row["attempted_outside"] = opened, attempted
                row["parse_only_reads"] = len(parsed)
                if opened:
                    problems.append(f"{base}: opened outside the reference: {opened}")
            rows[base] = row
    stray = [f for f in loaded if not os.path.realpath(f).startswith(root + os.sep)]
    if conf.returncode or not loaded or stray:
        problems.append(f"configuration loaded outside the reference: {stray or 'none listed'}")
    fonts = sorted(set(subprocess.run(["fc-list", "--format", "%{file}\\n"], capture_output=True, text=True,
                                      env=env).stdout.split("\n")) - {""})
    stray = [f for f in fonts if not os.path.realpath(f).startswith(root + os.sep)]
    if stray:
        problems.append(f"fonts listed outside the reference: {stray}")
    lock = json.load(open(os.path.join(root, "inputs.json")))
    # The configuration files are the lock's, by content, and only those.
    conf_dir = os.path.join(root, "conf.d")
    have_conf = {n: sha(os.path.join(conf_dir, n)) for n in os.listdir(conf_dir)}
    want_conf = {c["name"]: c["sha256"] for c in lock["conf_d"]}
    if have_conf != want_conf:
        problems.append(f"conf.d is not the lock's: {sorted(set(have_conf) ^ set(want_conf)) or 'contents differ'}")
    want = sorted(f["sha256"] for f in lock["fonts"])
    have = sorted(sha(f) for f in fonts if os.path.isfile(f))
    if have != want:
        problems.append(f"the fonts are not the lock's: {len(have)} listed, {len(want)} locked, "
                        f"{len(set(want) - set(have))} missing, {len(set(have) - set(want))} not locked")
    doc = {"probes": rows, "config_files": len(loaded), "fonts": len(fonts), "strace": bool(strace),
           "problems": problems}
    json.dump(doc, open(a.json, "w"), indent=1, sort_keys=True)
    att = sorted({p for r in rows.values() for p in r.get("attempted_outside", [])})
    print(f"isolation-probe: {len(rows)} probes, {len(loaded)} configuration files, {len(fonts)} fonts, "
          f"strace {'on' if strace else 'off'}; attempted outside (not opened): {len(att)}")
    for p in problems:
        print("PROBLEM:", p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
