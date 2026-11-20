"""The measurement environment of a compatibility run: the commit, the
platform, and the versions of the reference renderers and tools
(docs/compat-evaluation.md § Environments). Imported by
scripts/compat-attribute.py and scripts/ci-regression.py.

A baseline records the environment it was measured in; a comparison
in another environment is refused (renders and reference renders
differ between platforms and renderer versions — measured: 230 of
1147 pages differ in SSIM between the Linux nightly and a macOS run of
the same commit).

The reference renderer's fonts are part of the environment
(`reference`): Poppler draws a non-embedded font with what fontconfig
gives it, so the same Poppler with other fonts or another configuration
is another reference. `reference()` fingerprints what the `pdftoppm` in
use sees — its profile (`system`, or the profile a reference directory
such as scripts/courier-reference/setup-e1.sh's declares in its
profile.json), the effective fontconfig configuration (the top-level
file and every file it loads, in order, by content), the font files it
lists (by content), the fontconfig version and the FreeType library
the binary loads. Paths inside a reference directory are recorded
relative to it, so the same setup in another temporary directory has the
same fingerprint. It fails closed: a tool that is neither a binary nor
exactly setup-e1.sh's wrapper of one, a binary run with FONTCONFIG_PATH
or FONTCONFIG_SYSROOT set, a listed file that cannot be read, or an empty
listing leaves the reference incomplete — never equal to any.
"""
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess

# The keys that must agree between a run and its baseline for the
# numeric comparisons to be meaningful.
COMPARED = ("os", "arch", "poppler", "mupdf", "features", "reference")
# The fingerprint fields a reference must have to be compared at all.
REFERENCE_FIELDS = ("profile", "version", "config_sha256", "fonts_sha256", "font_files", "fontconfig", "freetype")


def _version(argv, pattern):
    try:
        r = subprocess.run(argv, capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return None
    m = re.search(pattern, (r.stdout or "") + (r.stderr or ""))
    return m.group(1) if m else None


def tool_versions(pdftoppm="pdftoppm", mutool="mutool", qpdf="qpdf"):
    return {
        "poppler": _version([pdftoppm, "-v"], r"pdftoppm version (\S+)"),
        "mupdf": _version([mutool, "-v"], r"mutool version (\S+)"),
        "qpdf": _version([qpdf, "--version"], r"qpdf version (\S+)"),
    }


def os_name():
    if platform.system() == "Linux":
        try:
            rel = platform.freedesktop_os_release()
            return f"linux-{rel.get('ID', '?')}-{rel.get('VERSION_ID', '?')}"
        except OSError:
            return "linux"
    if platform.system() == "Darwin":
        return f"macos-{platform.mac_ver()[0].split('.')[0]}"
    return platform.system().lower()


def _sha(data):
    return hashlib.sha256(data).hexdigest()


# Variables that change what fontconfig loads without showing in
# FONTCONFIG_FILE: the wrappers clear them; a binary run with them set is
# not fingerprinted.
FONTCONFIG_VARS = ("FONTCONFIG_PATH", "FONTCONFIG_SYSROOT")
# setup-e1.sh's wrapper, exactly: anything else is not identified.
WRAPPER = re.compile(rb'#!/bin/sh\nexec /usr/bin/env -u FONTCONFIG_PATH -u FONTCONFIG_SYSROOT FONTCONFIG_FILE=(/[^\s"$`\\\\]+) '
                     rb'(/[^\s"$`\\\\]+) "\$@"\n')
BINARY = (b"\x7fELF", b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe", b"\xca\xfe\xba\xbe")


def _head(path, n=4096):
    try:
        with open(path, "rb") as fh:
            return fh.read(n)
    except OSError:
        return None


def _identify(pdftoppm):
    """("binary", path), ("wrapper", fonts.conf, binary) for setup-e1.sh's
    wrapper of a binary, or None: a tool that is missing, unreadable or
    anything else is not identified, and its reference is incomplete."""
    head = _head(pdftoppm)
    if head is None:
        return None
    if head.startswith(BINARY):
        return ("binary", os.path.realpath(pdftoppm))
    m = WRAPPER.fullmatch(head)
    if m:
        conf, real = (x.decode() for x in m.groups())
        target = _head(real)
        if target is not None and target.startswith(BINARY):
            return ("wrapper", conf, os.path.realpath(real))
    return None


def _freetype(binary):
    """The FreeType library the binary loads, by file name (Linux, ldd);
    None when that cannot be told."""
    out = _version(["ldd", binary], r"libfreetype\.so\.\d+ => (/\S+)")
    return os.path.basename(os.path.realpath(out)) if out else None


def _read_all(paths):
    """The bodies of every file, or None if one cannot be read."""
    out = []
    for f in paths:
        try:
            with open(f, "rb") as fh:
                out.append((f, fh.read()))
        except OSError:
            return None
    return out


INCOMPLETE = {"config_sha256": None, "fonts_sha256": None, "font_files": None, "fontconfig": None, "freetype": None}


def reference(pdftoppm="pdftoppm"):
    """The reference renderer's font environment (see the module
    docstring). Fails closed: a field is None (the reference incomplete,
    so never equal to any) where a tool is missing or unidentified, a
    listed file cannot be read, or the output is empty."""
    tool = _identify(shutil.which(pdftoppm) or pdftoppm)
    if tool is None:
        return dict(INCOMPLETE, profile="unidentified", version=0)
    env = {k: v for k, v in os.environ.items() if k != "FC_DEBUG"}
    profile = {"profile": "system", "version": 1}
    conf = None
    if tool[0] == "wrapper":
        conf = tool[1]
        for k in FONTCONFIG_VARS:
            env.pop(k, None)
    else:
        if any(env.get(k) for k in FONTCONFIG_VARS):
            return dict(INCOMPLETE, profile="unidentified", version=0)
        conf = env.get("FONTCONFIG_FILE")
    root = None
    if conf:
        env["FONTCONFIG_FILE"] = conf
        root = os.path.dirname(os.path.abspath(conf))
        try:
            with open(os.path.join(root, "profile.json")) as fh:
                p = json.load(fh)
            profile = {"profile": str(p["profile"]), "version": int(p["version"])}
        except (OSError, ValueError, KeyError, TypeError):
            # A configuration that does not say what it is: fingerprinted,
            # but never the system's.
            profile = {"profile": "undeclared", "version": 0}
    rel = (lambda s: s.replace(root, "<reference>")) if root else (lambda s: s)
    out = dict(profile, **INCOMPLETE)
    try:
        r = subprocess.run(["fc-conflist"], capture_output=True, text=True, timeout=60, env=env)
        listed = [l[2:].split(": ", 1)[0] for l in r.stdout.splitlines() if l.startswith("+ ")] if r.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired):
        listed = None
    bodies = _read_all([conf or "/etc/fonts/fonts.conf"] + listed) if listed else None
    if bodies:
        h = hashlib.sha256()
        for f, body in bodies:
            body = rel(body.decode("utf-8", "replace")).encode()
            h.update(rel(f).encode() + b"\0" + _sha(body).encode() + b"\n")
        out["config_sha256"] = h.hexdigest()
    try:
        r = subprocess.run(["fc-list", "--format", "%{file}\\n"], capture_output=True, text=True, timeout=120, env=env)
        listed = sorted(set(l for l in r.stdout.splitlines() if l)) if r.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired):
        listed = None
    bodies = _read_all(listed) if listed else None
    if bodies:
        out["fonts_sha256"] = _sha("\n".join(f"{rel(f)}\0{_sha(b)}" for f, b in bodies).encode())
        out["font_files"] = len(bodies)
    out["fontconfig"] = _version(["fc-list", "--version"], r"version (\S+)")
    out["freetype"] = _freetype(tool[-1])
    return out


def environment(commit=None, features="", **tools):
    env = {
        "commit": commit or os.environ.get("GITHUB_SHA") or _git_head(),
        "os": os_name(),
        "arch": platform.machine(),
        "features": features,
    }
    env.update(tool_versions(**tools))
    env["reference"] = reference(tools.get("pdftoppm", "pdftoppm"))
    return env


def _git_head():
    try:
        r = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True, timeout=30,
                           cwd=os.path.dirname(os.path.abspath(__file__)))
        return r.stdout.strip() or None
    except (OSError, subprocess.TimeoutExpired):
        return None


_SHA256 = re.compile(r"[0-9a-f]{64}")


def reference_complete(ref):
    """Whether a recorded reference has every field, well formed: a
    named, identified profile, both hashes, at least one font."""
    if not isinstance(ref, dict):
        return False
    ok = lambda v: isinstance(v, str) and v != ""
    return (ok(ref.get("profile")) and ref["profile"] not in ("unidentified",)
            and type(ref.get("version")) is int and ref["version"] >= 1
            and all(isinstance(ref.get(k), str) and _SHA256.fullmatch(ref[k]) for k in ("config_sha256", "fonts_sha256"))
            and type(ref.get("font_files")) is int and ref["font_files"] > 0
            and ok(ref.get("fontconfig")) and ok(ref.get("freetype")))


def differences(base, now, keys=COMPARED):
    """The compared keys whose values differ. `update-baseline` refuses
    to record an environment without `os`, `arch` and `poppler`, so a
    value both sides lack is one neither measured with (MuPDF on a run
    without attribution). The reference is different unless both sides
    have a complete one and they are equal: a side without one (an older
    report) is never the same environment."""
    out = []
    for k in keys:
        if k == "reference":
            a, b = base.get(k), now.get(k)
            if not (reference_complete(a) and reference_complete(b) and a == b):
                out.append(k)
        elif base.get(k) != now.get(k):
            out.append(k)
    return out


def incomplete(env, need_mupdf):
    """Keys a recorded environment must have but lacks."""
    need = ["commit", "os", "arch", "poppler"] + (["mupdf"] if need_mupdf else [])
    missing = [k for k in need if not env.get(k)]
    if not reference_complete(env.get("reference")):
        missing.append("reference")
    return missing
