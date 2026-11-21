#!/usr/bin/env python3
"""E2's isolation, tested on a machine: what must not change E2 does not,
what must change it is seen.

    scripts/courier-reference/isolation-test.py WORK --json OUT [--debs CACHE] [--pollute-host]

Each case builds E2 with setup-e2.py under WORK and runs
isolation-probe.py (the probes' matched files and pixels) and
scripts/compat_env.py's fingerprint:
- the same:      another directory; a cold cache (DIR/cache emptied) and
                 a warm one; a polluted user (HOME, XDG_CONFIG_HOME and
                 FONTCONFIG_PATH holding a fontconfig configuration that
                 sends Courier elsewhere, a font in ~/.fonts); and with
                 --pollute-host, the machine polluted too — a rule in
                 /etc/fonts/local.conf, one in fontconfig's template
                 directory, a font in /usr/share/fonts, the shared caches
                 rebuilt — while the machine's own Poppler (E0) and E1,
                 the positive controls, do change;
- detected:      a font missing, a font of other content, a configuration
                 file changed (isolation-probe fails; the fingerprint
                 differs), a package that is not its lock's (setup-e2.py
                 fails and leaves no profile.json).
--pollute-host writes system files (and removes them after): only on a
throwaway machine — a CI runner, a container. Exit 1 when any case does
not come out as stated.
"""
import argparse, json, os, shutil, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
import compat_env  # noqa: E402

PY = sys.executable
RULE = """<?xml version="1.0"?><!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd"><fontconfig>
<match target="pattern"><test name="family"><string>{fam}</string></test>
<edit name="family" mode="prepend" binding="strong"><string>{to}</string></edit></match></fontconfig>
"""


def run(argv, **kw):
    return subprocess.run(argv, capture_output=True, text=True, **kw)


def setup(d, debs, env=None):
    a = [PY, os.path.join(HERE, "setup-e2.py"), d] + (["--debs", debs] if debs else [])
    return run(a, env=env)


def probe(d, out, env=None):
    r = run([PY, os.path.join(HERE, "isolation-probe.py"), d, "--json", out], env=env)
    doc = json.load(open(out)) if os.path.exists(out) else {}
    return r.returncode, doc


def view(doc):
    """What must be the same: each probe's matched files and pixels."""
    return {k: (v["matched"], v["pixels"]) for k, v in (doc.get("probes") or {}).items()}


def fingerprint(d, env=None):
    saved = dict(os.environ)
    try:
        if env is not None:
            os.environ.clear()
            os.environ.update(env)
        return compat_env.reference(os.path.join(d, "bin", "pdftoppm"))
    finally:
        os.environ.clear()
        os.environ.update(saved)


def courier_pixels(tool, work, env=None):
    """The Courier probe drawn by `tool` (E0's or E1's pdftoppm)."""
    spec = __import__("importlib.util").util.spec_from_file_location("probe", os.path.join(HERE, "probe.py"))
    p = __import__("importlib.util").util.module_from_spec(spec)
    spec.loader.exec_module(p)
    pdf = os.path.join(work, "courier.pdf")
    open(pdf, "wb").write(p.probe_pdf("Courier"))
    out = os.path.join(work, "courier")
    r = run([tool, "-r", "72", "-gray", "-singlefile", pdf, out], env=dict(env or os.environ, FC_DEBUG="1"))
    return p.matched(r.stdout + r.stderr), open(out + ".pgm", "rb").read() if r.returncode == 0 else None


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("work")
    ap.add_argument("--json", required=True)
    ap.add_argument("--debs")
    ap.add_argument("--pollute-host", action="store_true")
    ap.add_argument("--e1", help="an E1 directory (setup-e1.sh), for the positive control")
    a = ap.parse_args()
    work = os.path.realpath(a.work)
    os.makedirs(work, exist_ok=True)
    a.debs = a.debs or os.path.join(work, "debs")  # downloaded once, then read
    cases, bad = {}, []

    def case(name, ok, **detail):
        cases[name] = dict(ok=bool(ok), **detail)
        if not ok:
            bad.append(name)

    base = os.path.join(work, "a")
    r = setup(base, a.debs)
    if r.returncode:
        print(r.stdout + r.stderr)
        raise SystemExit("isolation-test: setup-e2.py failed")
    rc, ref = probe(base, os.path.join(work, "a.json"))
    case("baseline probe passes", rc == 0, problems=ref.get("problems"))
    fp = fingerprint(base)
    case("baseline fingerprint complete", compat_env.reference_complete(fp))

    other = os.path.join(work, "deeper", "path", "b")
    setup(other, a.debs)
    rc, doc = probe(other, os.path.join(work, "b.json"))
    case("another directory: the same", rc == 0 and view(doc) == view(ref) and fingerprint(other) == fp)

    shutil.rmtree(os.path.join(base, "cache"))
    os.makedirs(os.path.join(base, "cache"))
    rc, cold = probe(base, os.path.join(work, "cold.json"))
    rc2, warm = probe(base, os.path.join(work, "warm.json"))
    case("cold and warm cache: the same", rc == rc2 == 0 and view(cold) == view(ref) == view(warm)
         and fingerprint(base) == fp)

    # A polluted user: its HOME, XDG_CONFIG_HOME and FONTCONFIG_PATH.
    user = os.path.join(work, "user")
    for sub in (".config/fontconfig", ".fonts", "xdg/fontconfig", "fcpath"):
        os.makedirs(os.path.join(user, sub), exist_ok=True)
    rule = RULE.format(fam="Courier", to="DejaVu Sans")
    for f in (".config/fontconfig/fonts.conf", "xdg/fontconfig/fonts.conf", "fcpath/fonts.conf"):
        open(os.path.join(user, f), "w").write(rule)
    lib = os.path.join(base, "fonts", "system", "truetype", "liberation", "LiberationSerif-Regular.ttf")
    shutil.copy(lib, os.path.join(user, ".fonts", "kasane-test-user.ttf"))
    env = dict(os.environ, HOME=user, XDG_CONFIG_HOME=os.path.join(user, "xdg"),
               FONTCONFIG_PATH=os.path.join(user, "fcpath"))
    rc, doc = probe(base, os.path.join(work, "user.json"), env=env)
    case("a polluted user: the same", rc == 0 and view(doc) == view(ref) and fingerprint(base, env) == fp)
    # ... where the machine's own Poppler does change (the pollution works).
    e0 = shutil.which("pdftoppm")
    clean = {k: v for k, v in os.environ.items() if k not in compat_env.CLEARED}
    case("positive control: E0 changes for that user",
         courier_pixels(e0, work, env)[0] != courier_pixels(e0, work, clean)[0],
         before=courier_pixels(e0, work, clean)[0], after=courier_pixels(e0, work, env)[0])

    created = []
    if a.pollute_host:
        sudo = [] if os.geteuid() == 0 else ["sudo"]
        e0_before = courier_pixels(shutil.which("pdftoppm"), work)
        e1_before = courier_pixels(os.path.join(a.e1, "bin", "pdftoppm"), work) if a.e1 else None
        # local.conf (enabled by the machine's 51-local.conf): Courier, and
        # the Nimbus Mono PS E1 asks for, sent elsewhere.
        local = RULE.format(fam="Courier", to="Liberation Serif").replace("</fontconfig>", "") + \
            RULE.format(fam="Nimbus Mono PS", to="DejaVu Sans").split("<fontconfig>", 1)[1]
        host = {"/etc/fonts/local.conf": local,
                "/usr/share/fontconfig/conf.avail/99-kasane-test.conf": RULE.format(fam="Courier", to="DejaVu Sans")}
        font_dir = "/usr/share/fonts/truetype/kasane-test"
        for path in list(host) + [font_dir]:
            if os.path.exists(path):
                raise SystemExit(f"isolation-test: {path} exists; not overwriting it")
        # Every file is recorded as soon as it exists, inside the try: a
        # failure part-way still removes what was added.
        try:
            for path, body in host.items():
                tmp = os.path.join(work, "host.conf")
                open(tmp, "w").write(body)
                created.append(path)
                subprocess.run(sudo + ["cp", tmp, path], check=True)
            created.append(font_dir)
            subprocess.run(sudo + ["mkdir", "-p", font_dir], check=True)
            subprocess.run(sudo + ["cp", lib, os.path.join(font_dir, "kasane-test-host.ttf")], check=True)
            if os.environ.get("KASANE_ISOLATION_TEST_FAIL_AFTER_FIRST"):  # the cleanup's own test
                raise subprocess.CalledProcessError(1, "injected")
            subprocess.run(sudo + ["fc-cache", "-f"], check=True, capture_output=True)
            subprocess.run(["fc-cache", "-f"], check=True, capture_output=True)
            rc, doc = probe(base, os.path.join(work, "host.json"), env=env)
            case("a polluted machine: the same", rc == 0 and view(doc) == view(ref) and fingerprint(base, env) == fp)
            e0_after = courier_pixels(shutil.which("pdftoppm"), work)
            case("positive control: E0 changes on that machine", e0_after != e0_before,
                 before=e0_before[0], after=e0_after[0])
            if a.e1:
                e1_after = courier_pixels(os.path.join(a.e1, "bin", "pdftoppm"), work)
                case("positive control: E1 changes on that machine", e1_after != e1_before,
                     before=e1_before[0], after=e1_after[0])
        finally:
            for path in reversed(created):
                subprocess.run(sudo + ["rm", "-rf", path], check=True)
            subprocess.run(sudo + ["fc-cache", "-f"], capture_output=True)

    # Detected: a font missing, a font of other content, a configuration changed.
    def broken(name, change):
        d = os.path.join(work, name)
        setup(d, a.debs)
        change(d)
        rc, doc = probe(d, os.path.join(work, name + ".json"))
        return rc, doc, fingerprint(d)
    nimbus = lambda d: os.path.join(d, "fonts", "e1", "NimbusMonoPS-Regular.otf")
    rc, doc, f = broken("missing-font", lambda d: os.remove(nimbus(d)))
    case("a font missing: detected", rc == 1 and f["fonts_sha256"] != fp["fonts_sha256"], problems=doc.get("problems"))
    rc, doc, f = broken("other-font", lambda d: open(nimbus(d), "ab").write(b"\0"))
    case("a font of other content: detected", rc == 1 and f["fonts_sha256"] != fp["fonts_sha256"],
         problems=doc.get("problems"))
    rc, doc, f = broken("changed-conf", lambda d: open(os.path.join(d, "conf.d", "30-metric-aliases.conf"), "a").write(
        "<!-- changed -->\n"))
    case("a configuration file changed: detected", rc == 1 and f["config_sha256"] != fp["config_sha256"],
         problems=doc.get("problems"))
    # A configuration outside E2 included — an arbitrary file, or one of
    # the machine's templates applied: detected.
    rogue = os.path.join(work, "rogue.conf")
    open(rogue, "w").write("<fontconfig></fontconfig>\n")
    tmpl = next((os.path.join(t, f) for t in ("/usr/share/fontconfig/conf.avail", "/opt/homebrew/share/fontconfig/conf.avail")
                 if os.path.isdir(t) for f in sorted(os.listdir(t)) if f.endswith(".conf")), None)
    for name, target in (("an outside configuration included", rogue), ("a machine template applied", tmpl)):
        if target is None:
            case(name + ": detected", False, why="no template directory on this machine")
            continue
        def inc(d, target=target):
            p = os.path.join(d, "fonts.conf")
            s = open(p).read()
            open(p, "w").write(s.replace("</fontconfig>", f'\t<include ignore_missing="no">{target}</include>\n</fontconfig>'))
        rc, doc, f = broken(name.replace(" ", "-"), inc)
        case(name + ": detected", rc == 1 and f["profile"] == "unidentified", problems=doc.get("problems"))
    # A package that is not its lock's.
    cache = os.path.join(work, "bad-debs")
    shutil.rmtree(cache, ignore_errors=True)
    shutil.copytree(a.debs, cache)
    lock = json.load(open(os.path.join(HERE, "e2-inputs.json")))
    victim = os.path.join(cache, os.path.basename(lock["packages"][0]["path"]))
    data = open(victim, "rb").read()
    open(victim, "wb").write(data[:-1] + bytes([data[-1] ^ 1]))
    d = os.path.join(work, "bad-package")
    r = setup(d, cache)
    case("a package not its lock's: refused", r.returncode != 0 and not os.path.exists(os.path.join(d, "profile.json")))

    doc = {"cases": cases, "failed": bad, "polluted_host": bool(a.pollute_host),
           "fingerprint": {k: v for k, v in fp.items()}}
    json.dump(doc, open(a.json, "w"), indent=1, sort_keys=True)
    for n, c in cases.items():
        print(f"{'ok ' if c['ok'] else 'NOT'} {n}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
