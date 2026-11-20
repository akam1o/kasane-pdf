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
lists (by content), and the fontconfig and FreeType versions. Paths
inside a reference directory are recorded relative to it, so the same
setup in another temporary directory has the same fingerprint.
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
REFERENCE_FIELDS = ("profile", "version", "config_sha256", "fonts_sha256", "font_files", "fontconfig")


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


def _wrapped_config(pdftoppm):
    """The FONTCONFIG_FILE a wrapper script (setup-e1.sh's bin/) runs the
    real tool with, or None for a binary."""
    try:
        with open(pdftoppm, "rb") as fh:
            head = fh.read(4096)
    except OSError:
        return None
    if not head.startswith(b"#!"):
        return None
    m = re.search(rb"FONTCONFIG_FILE=(\S+)", head)
    return m.group(1).decode() if m else None


def _freetype():
    """FreeType's version as the loaded library names it (Linux), or
    pkg-config's; None when neither tells."""
    for argv in (["sh", "-c", "ldconfig -p 2>/dev/null | grep -m1 'libfreetype.so.6 '"],):
        out = _version(argv, r"=>\s*(\S+)")
        if out:
            return os.path.basename(os.path.realpath(out))
    return _version(["pkg-config", "--modversion", "freetype2"], r"(\S+)")


def reference(pdftoppm="pdftoppm"):
    """The reference renderer's font environment (see the module
    docstring); fields are None where a tool is missing."""
    path = shutil.which(pdftoppm) or pdftoppm
    env = {k: v for k, v in os.environ.items() if k != "FC_DEBUG"}
    conf = _wrapped_config(path) or env.get("FONTCONFIG_FILE")
    root = None
    profile = {"profile": "system", "version": 1}
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
    top = conf or "/etc/fonts/fonts.conf"
    files = [top]
    try:
        r = subprocess.run(["fc-conflist"], capture_output=True, text=True, timeout=60, env=env)
        listed = [l[2:].split(": ", 1)[0] for l in r.stdout.splitlines() if l.startswith("+ ")] if r.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired):
        listed = None
    config = None
    if listed is not None:
        h = hashlib.sha256()
        for f in files + listed:
            try:
                with open(f, "rb") as fh:
                    body = fh.read()
            except OSError:
                body = b"<unreadable>"
            body = rel(body.decode("utf-8", "replace")).encode()
            h.update(rel(f).encode() + b"\0" + _sha(body).encode() + b"\n")
        config = h.hexdigest()
    fonts, count = None, None
    try:
        r = subprocess.run(["fc-list", "--format", "%{file}\\n"], capture_output=True, text=True, timeout=120, env=env)
        if r.returncode == 0:
            rows = []
            for f in sorted(set(r.stdout.splitlines())):
                try:
                    with open(f, "rb") as fh:
                        rows.append(f"{rel(f)}\0{_sha(fh.read())}")
                except OSError:
                    rows.append(f"{rel(f)}\0<unreadable>")
            fonts, count = _sha("\n".join(rows).encode()), len(rows)
    except (OSError, subprocess.TimeoutExpired):
        pass
    return dict(profile, config_sha256=config, fonts_sha256=fonts, font_files=count,
                fontconfig=_version(["fc-list", "--version"], r"version (\S+)"), freetype=_freetype())


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


def reference_complete(ref):
    return isinstance(ref, dict) and all(ref.get(k) not in (None, "") for k in REFERENCE_FIELDS)


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
