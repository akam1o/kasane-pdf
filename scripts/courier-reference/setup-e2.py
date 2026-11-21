#!/usr/bin/env python3
"""The isolated Poppler reference E2 (docs/courier-reference.md): E1's
fonts and rules — the runner's fonts with Courier and Courier New drawn
with Nimbus Mono PS — built only from the pinned packages of
scripts/courier-reference/e2-inputs.json, and nothing of the machine's
fontconfig: no /etc/fonts, no user configuration, no shared cache.

    scripts/courier-reference/setup-e2.py DIR [--debs CACHE_DIR]

Every package is downloaded from the lock's snapshot of the Ubuntu
archive (or read from CACHE_DIR) and checked against its SHA-256; every
file taken from it against its own. Nothing is installed; the machine's
fontconfig configuration, fonts and caches are not written to.

    DIR/fonts/e1/      Nimbus Mono PS (fonts-urw-base35), as E1 has it
    DIR/fonts/system/  the runner's fonts, laid out as under /usr/share/fonts
    DIR/conf.d/        the configuration files the runner's fontconfig
                       loads, from fontconfig-config and the font packages —
                       but 50-user.conf and 51-local.conf
    DIR/fonts.conf     fontconfig-config's fonts.conf with its directories,
                       include and caches replaced by those above and
                       DIR/cache, and E1's Courier rules first
    DIR/inputs.json    the lock it was built from
    DIR/profile.json   profile, version, isolation, the lock's SHA-256
                       (scripts/compat_env.py reads it, and checks it)
    DIR/home/          an empty HOME for the reference processes
    DIR/bin/           pdftoppm, pdftotext, pdftocairo, pdfinfo wrappers:
                       the system tools with FONTCONFIG_FILE=DIR/fonts.conf,
                       HOME=DIR/home, FONTCONFIG_PATH, FONTCONFIG_SYSROOT
                       and the XDG_* variables cleared — put DIR/bin first
                       on PATH for a reference process only

A package that cannot be had or is not its SHA-256, a file missing from
it or not its own: exit 1, nothing usable left behind (no profile.json).
"""
import argparse, hashlib, json, os, re, shutil, subprocess, sys, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from e2lib import deb_members  # noqa: E402

LOCK = os.path.join(HERE, "e2-inputs.json")
PROFILE = {"profile": "courier-nimbus-mono-ps-isolated", "version": 1, "isolation": "isolated"}
# E1's rules (setup-e1.sh): only these family names, as fontconfig
# compares them, tested on the pattern before anything else.
COURIER_FAMILIES = ("Courier", "Courier New", "Courier New PS", "Courier New PSMT", "CourierNewPSMT", "CourierNewPS")
TOOLS = ("pdftoppm", "pdftotext", "pdftocairo", "pdfinfo")
CLEARED = ("FONTCONFIG_PATH", "FONTCONFIG_SYSROOT", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME",
           "XDG_CONFIG_DIRS", "XDG_DATA_DIRS")
# Paths the wrapper carries unquoted: characters the shell and env(1)
# take literally (scripts/compat_env.py identifies the wrapper by its form).
SAFE = re.compile(r"/[A-Za-z0-9._/+@%,:-]+")


def sha(b):
    return hashlib.sha256(b).hexdigest()


def package(p, lock, cache):
    name = os.path.basename(p["path"])
    cached = os.path.join(cache, name) if cache else None
    if cached and os.path.exists(cached):
        data = open(cached, "rb").read()
    else:
        req = urllib.request.Request(f"{lock['snapshot']}/{p['path']}", headers={"User-Agent": "kasane-pdf setup-e2"})
        data = urllib.request.urlopen(req, timeout=300).read()
        if cached:
            os.makedirs(cache, exist_ok=True)
            open(cached, "wb").write(data)
    if sha(data) != p["sha256"]:
        raise SystemExit(f"setup-e2: {name} is not the lock's ({sha(data)})")
    return dict(deb_members(data))


def fonts_conf(template, root):
    t = template.decode()
    rules = "".join(f"""
	<match target="pattern">
		<test name="family" qual="first" compare="eq"><string>{fam}</string></test>
		<edit name="family" mode="prepend" binding="strong"><string>Nimbus Mono PS</string></edit>
	</match>""" for fam in COURIER_FAMILIES)
    subs = [
        # E1's order: its directory, its rules, then the configuration.
        (r"(<description>[^<]*</description>)", r"\1\n\t<!-- kasane-pdf E2: E1's Courier rules -->" + rules.replace("\\", "\\\\")),
        (r"\t<dir>/usr/share/fonts</dir>\n", f"\t<dir>{root}/fonts/e1</dir>\n\t<dir>{root}/fonts/system</dir>\n"),
        (r"\t<dir>/usr/local/share/fonts</dir>\n", ""),
        (r"\t<dir prefix=\"xdg\">fonts</dir>\n", ""),
        (r"\t<!-- the following element will be removed in the future -->\n\t<dir>~/.fonts</dir>\n", ""),
        (r"\t<dir>~/.fonts</dir>\n", ""),
        (r'<include ignore_missing="yes">conf.d</include>', f'<include ignore_missing="no">{root}/conf.d</include>'),
        (r"\t<cachedir>/var/cache/fontconfig</cachedir>\n", f"\t<cachedir>{root}/cache</cachedir>\n"),
        (r"\t<cachedir prefix=\"xdg\">fontconfig</cachedir>\n", ""),
        (r"\t<!--\n\t\t\tthe following element will be removed in the future\n\t\t-->\n\t<cachedir>~/.fontconfig</cachedir>\n", ""),
        (r"\t<cachedir>~/.fontconfig</cachedir>\n", ""),
    ]
    for pat, rep in subs:
        t = re.sub(pat, rep, t, count=1)
    # Nothing of the machine's or the user's is left.
    left = re.findall(r"<(?:dir|cachedir|include)[^>]*>[^<]*</", t)
    stray = [x for x in left if root not in x]
    if stray or "~" in "".join(left) or 'prefix="xdg"' in t:
        raise SystemExit(f"setup-e2: fonts.conf still names {stray}")
    return t.encode()


def wrapper(root, real):
    clear = " ".join(f"-u {v}" for v in CLEARED)
    return (f"#!/bin/sh\nexec /usr/bin/env {clear} HOME={root}/home FONTCONFIG_FILE={root}/fonts.conf "
            f'{real} "$@"\n')


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("dir")
    ap.add_argument("--debs")
    ap.add_argument("--lock", default=LOCK)
    a = ap.parse_args()
    lockbytes = open(a.lock, "rb").read()
    lock = json.loads(lockbytes)
    os.makedirs(a.dir, exist_ok=True)
    root = os.path.realpath(a.dir)
    if not SAFE.fullmatch(root):
        raise SystemExit(f"setup-e2: {root} has characters the wrapper cannot carry")
    for d in ("fonts", "conf.d", "cache", "home", "bin"):
        shutil.rmtree(os.path.join(root, d), ignore_errors=True)
    for f in ("profile.json", "fonts.conf", "inputs.json"):
        if os.path.exists(os.path.join(root, f)):
            os.remove(os.path.join(root, f))
    pkgs = {p["name"]: package(p, lock, a.debs) for p in lock["packages"]}

    def take(entry):
        body = pkgs[entry["package"]].get(entry["member"])
        if body is None or sha(body) != entry["sha256"]:
            raise SystemExit(f"setup-e2: {entry['package']}:{entry['member']} missing or not the lock's")
        return body
    for f in lock["fonts"]:
        rel = (os.path.join("e1", os.path.basename(f["member"])) if f["package"] == "fonts-urw-base35"
               else os.path.join("system", re.sub(r"^usr/share/fonts/", "", f["member"])))
        dest = os.path.join(root, "fonts", rel)
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        open(dest, "wb").write(take(f))
    os.makedirs(os.path.join(root, "conf.d"))
    for c in lock["conf_d"]:
        if c["name"] in lock["excluded_conf_d"]:
            raise SystemExit(f"setup-e2: the lock lists excluded {c['name']}")
        open(os.path.join(root, "conf.d", c["name"]), "wb").write(take(c))
    open(os.path.join(root, "fonts.conf"), "wb").write(fonts_conf(take(lock["fonts_conf"]), root))
    open(os.path.join(root, "inputs.json"), "wb").write(lockbytes)
    os.makedirs(os.path.join(root, "cache"))
    os.makedirs(os.path.join(root, "home"))
    os.makedirs(os.path.join(root, "bin"))
    for t in TOOLS:
        real = shutil.which(t, path="/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin")
        if not real:
            raise SystemExit(f"setup-e2: no {t}")
        real = os.path.realpath(real)
        if not SAFE.fullmatch(real):
            raise SystemExit(f"setup-e2: {real} has characters the wrapper cannot carry")
        w = os.path.join(root, "bin", t)
        open(w, "w").write(wrapper(root, real))
        os.chmod(w, 0o755)
    env = {k: v for k, v in os.environ.items() if k not in CLEARED}
    env.update(HOME=os.path.join(root, "home"), FONTCONFIG_FILE=os.path.join(root, "fonts.conf"))
    fc = shutil.which("fc-cache", path="/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin")
    if not fc or subprocess.run([fc, "-f"], env=env, capture_output=True).returncode:
        raise SystemExit("setup-e2: fc-cache failed")
    # Last: without it the directory is not a reference.
    with open(os.path.join(root, "profile.json"), "w") as fh:
        json.dump(dict(PROFILE, inputs_sha256=sha(lockbytes)), fh)
        fh.write("\n")
    print(f"E2 ready in {root}: {len(lock['fonts'])} fonts, {len(lock['conf_d'])} configuration files")


if __name__ == "__main__":
    main()
