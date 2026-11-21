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
the binary loads (name and content). Paths inside a reference directory
are recorded relative to it, so the same setup in another temporary directory has the
same fingerprint. It fails closed: a tool that is neither a binary nor
exactly setup-e1.sh's wrapper of one, a binary run with FONTCONFIG_PATH
or FONTCONFIG_SYSROOT set, a listed file that cannot be read, or an empty
listing leaves the reference incomplete — never equal to any.

Schema 2 adds the reference's isolation — `system` (the machine's
fontconfig), `inherits-system` (a reference directory that includes it:
E1), `isolated` (scripts/courier-reference/setup-e2.py's E2, nothing of
the machine's fontconfig) — the lock an isolated reference was built
from (its SHA-256, checked against the directory's inputs.json), and the
fontconfig library by name and content beside FreeType's, and Poppler's
build (the pdftoppm binary and the libpoppler it loads) by content (ldd on Linux;
on macOS otool -L followed through the libraries it names). An isolated
reference whose fontconfig loads a configuration file or lists a font
outside its directory is not one: incomplete. A reference of another
schema is incomplete, never upgraded by guessing what it lacked.
"""
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys

# The keys that must agree between a run and its baseline for the
# numeric comparisons to be meaningful.
COMPARED = ("os", "arch", "poppler", "mupdf", "features", "reference")
# The fingerprint fields a reference must have to be compared at all.
REFERENCE_FIELDS = ("schema", "profile", "version", "isolation", "config_sha256", "fonts_sha256", "font_files",
                    "fontconfig", "fontconfig_lib", "fontconfig_lib_sha256", "freetype", "freetype_sha256",
                    "poppler_build_sha256")
# 2: the isolation, the lock of an isolated reference, the fontconfig
# library by content. A reference of another schema (an older report) is
# incomplete: never compared, never upgraded by guessing.
SCHEMA = 2
ISOLATION = ("system", "inherits-system", "isolated")


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
# Paths of characters the shell takes literally, unquoted (and no "=",
# which env(1) would take for an assignment).
WRAPPER = re.compile(rb'#!/bin/sh\nexec /usr/bin/env -u FONTCONFIG_PATH -u FONTCONFIG_SYSROOT FONTCONFIG_FILE=(/[A-Za-z0-9._/+@%,:-]+) '
                     rb'(/[A-Za-z0-9._/+@%,:-]+) "\$@"\n')
# setup-e2.py's wrapper, exactly: it also clears the XDG variables and
# gives the process its own empty HOME.
CLEARED = ("FONTCONFIG_PATH", "FONTCONFIG_SYSROOT", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME",
           "XDG_CONFIG_DIRS", "XDG_DATA_DIRS")
_P = rb"(/[A-Za-z0-9._/+@%,:-]+)"
WRAPPER_ISOLATED = re.compile(rb"#!/bin/sh\nexec /usr/bin/env " + b" ".join(b"-u " + v.encode() for v in CLEARED)
                              + rb" HOME=" + _P + rb" FONTCONFIG_FILE=" + _P + rb" " + _P + rb' "\$@"\n')
BINARY = (b"\x7fELF", b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe", b"\xca\xfe\xba\xbe")


def _head(path, n=4096):
    try:
        with open(path, "rb") as fh:
            return fh.read(n)
    except OSError:
        return None


def _identify(pdftoppm):
    """("binary", None, None, path); ("wrapper", fonts.conf, home, binary)
    for setup-e1.sh's (home None) or setup-e2.py's wrapper of a binary; or
    None: a tool that is missing, unreadable or anything else is not
    identified, and its reference is incomplete."""
    head = _head(pdftoppm)
    if head is None:
        return None
    if head.startswith(BINARY):
        return ("binary", None, None, os.path.realpath(pdftoppm))
    m = WRAPPER.fullmatch(head)
    if m:
        (conf, real), home = (x.decode() for x in m.groups()), None
    else:
        m = WRAPPER_ISOLATED.fullmatch(head)
        if not m:
            return None
        home, conf, real = (x.decode() for x in m.groups())
    target = _head(real)
    if target is not None and target.startswith(BINARY):
        return ("wrapper", conf, home, os.path.realpath(real))
    return None


def _linked(binary):
    """{library basename: path} of what the binary loads: ldd on Linux;
    on macOS otool -L, followed through each library it names (absolute
    paths only — @rpath and the like are not guessed)."""
    out = {}
    if shutil.which("ldd"):
        try:
            r = subprocess.run(["ldd", binary], capture_output=True, text=True, timeout=30)
        except (OSError, subprocess.TimeoutExpired):
            return out
        for m in re.finditer(r"^\s*(\S+) => (/\S+)", r.stdout, re.M):
            out[m.group(1)] = m.group(2)
        return out
    if not shutil.which("otool"):
        return out

    def rpaths(image):
        try:
            r = subprocess.run(["otool", "-l", image], capture_output=True, text=True, timeout=30)
        except (OSError, subprocess.TimeoutExpired):
            return []
        return re.findall(r"cmd LC_RPATH\n\s+cmdsize \d+\n\s+path (\S+)", r.stdout)

    def resolve(lib, loader, stack):
        """dyld's resolution of @loader_path, @executable_path and
        @rpath (the run paths of the loading images, innermost first);
        None when no candidate exists."""
        subst = lambda p: (p.replace("@loader_path", os.path.dirname(loader))
                           .replace("@executable_path", os.path.dirname(binary)))
        if lib.startswith("@rpath/"):
            for image in [loader] + stack[::-1]:
                for rp in rpaths(image):
                    rp = rp.replace("@loader_path", os.path.dirname(image)).replace(
                        "@executable_path", os.path.dirname(binary))
                    cand = os.path.normpath(os.path.join(rp, lib[len("@rpath/"):]))
                    if os.path.exists(cand):
                        return cand
            return None
        cand = os.path.normpath(subst(lib))
        return cand if os.path.exists(cand) else None

    seen, todo = set(), [(os.path.realpath(binary), [])]
    while todo:
        b, stack = todo.pop()
        if b in seen:
            continue
        seen.add(b)
        try:
            r = subprocess.run(["otool", "-L", b], capture_output=True, text=True, timeout=30)
        except (OSError, subprocess.TimeoutExpired):
            continue
        for line in r.stdout.splitlines()[1:]:
            lib = line.strip().split(" (", 1)[0]
            if lib.startswith(("/usr/lib/", "/System/")):
                continue
            path = resolve(lib, b, stack)
            if path:
                out.setdefault(os.path.basename(path), path)
                todo.append((os.path.realpath(path), stack + [b]))
    return out


def _library(binary, stem):
    """(file name, content hash) of the `stem` library (libfreetype,
    libfontconfig) the binary loads; None for both when that cannot be
    told or read."""
    libs = _linked(binary)
    path = next((p for n, p in sorted(libs.items()) if re.match(re.escape(stem) + r"[.\-]", n)), None)
    body = _read_all([path]) if path else None
    if not body:
        return None, None
    return os.path.basename(os.path.realpath(path)), _sha(body[0][1])


def _freetype(binary):
    return _library(binary, "libfreetype")


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


INCOMPLETE = {"schema": SCHEMA, "isolation": None, "inputs_sha256": None, "config_sha256": None, "fonts_sha256": None,
              "font_files": None, "fontconfig": None, "fontconfig_lib": None, "fontconfig_lib_sha256": None,
              "freetype": None, "freetype_sha256": None, "poppler_build_sha256": None}


def reference(pdftoppm="pdftoppm"):
    """The reference renderer's font environment (see the module
    docstring). Fails closed: a field is None (the reference incomplete,
    so never equal to any) where a tool is missing or unidentified, a
    listed file cannot be read, or the output is empty."""
    tool = _identify(shutil.which(pdftoppm) or pdftoppm)
    if tool is None:
        return dict(INCOMPLETE, profile="unidentified", version=0)
    env = {k: v for k, v in os.environ.items() if k != "FC_DEBUG"}
    profile = {"profile": "system", "version": 1, "isolation": "system", "inputs_sha256": None}
    unidentified = dict(INCOMPLETE, profile="unidentified", version=0)
    conf = None
    if tool[0] == "wrapper":
        conf, home = tool[1], tool[2]
        for k in (CLEARED if home else FONTCONFIG_VARS):
            env.pop(k, None)
        if home:
            env["HOME"] = home
    else:
        if any(env.get(k) for k in FONTCONFIG_VARS):
            return unidentified
        conf = env.get("FONTCONFIG_FILE")
    root = None
    if conf:
        env["FONTCONFIG_FILE"] = conf
        root = os.path.dirname(os.path.abspath(conf))
        try:
            with open(os.path.join(root, "profile.json")) as fh:
                p = json.load(fh)
            profile = {"profile": str(p["profile"]), "version": int(p["version"]),
                       "isolation": str(p.get("isolation", "inherits-system")), "inputs_sha256": None}
        except (OSError, ValueError, KeyError, TypeError):
            # A configuration that does not say what it is: fingerprinted,
            # but never the system's.
            profile = {"profile": "undeclared", "version": 0, "isolation": "inherits-system", "inputs_sha256": None}
        if profile["isolation"] not in ISOLATION[1:]:
            return unidentified
        if profile["isolation"] == "isolated":
            # The lock it says it was built from, by content; its own HOME.
            body = _read_all([os.path.join(root, "inputs.json")])
            if not body or _sha(body[0][1]) != p.get("inputs_sha256") or tool[2] != os.path.join(root, "home"):
                return unidentified
            profile["inputs_sha256"] = p["inputs_sha256"]
        elif tool[0] == "wrapper" and tool[2]:
            return unidentified  # an isolated wrapper on a configuration that is not
    rel = (lambda s: s.replace(root, "<reference>")) if root else (lambda s: s)
    inside = lambda f: root is not None and os.path.realpath(f).startswith(os.path.realpath(root) + os.sep)
    out = dict(INCOMPLETE, **profile)
    try:
        r = subprocess.run(["fc-conflist"], capture_output=True, text=True, timeout=60, env=env)
        listed = [l[2:].split(": ", 1)[0] for l in r.stdout.splitlines() if l.startswith("+ ")] if r.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired):
        listed = None
    if listed and profile["isolation"] == "isolated" and not all(inside(f) for f in [conf] + listed):
        return unidentified  # isolated, it says; fontconfig loads a file outside
    # The top-level file: the reference's, else the system's — which
    # fontconfig lists itself where it is not /etc/fonts (Homebrew).
    top = [conf] if conf else ["/etc/fonts/fonts.conf"] if os.path.exists("/etc/fonts/fonts.conf") else []
    bodies = _read_all(top + listed) if listed else None
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
    if listed and profile["isolation"] == "isolated" and not all(inside(f) for f in listed):
        return unidentified  # isolated, it says; fontconfig lists a font outside
    bodies = _read_all(listed) if listed else None
    if bodies:
        out["fonts_sha256"] = _sha("\n".join(f"{rel(f)}\0{_sha(b)}" for f, b in bodies).encode())
        out["font_files"] = len(bodies)
    out["fontconfig"] = _version(["fc-list", "--version"], r"version (\S+)")
    out["freetype"], out["freetype_sha256"] = _library(tool[-1], "libfreetype")
    out["fontconfig_lib"], out["fontconfig_lib_sha256"] = _library(tool[-1], "libfontconfig")
    # Poppler itself by content — the pdftoppm binary and the libpoppler
    # it loads: two builds of one version string are two references.
    poppler = _library(tool[-1], "libpoppler")[1]
    exe = _read_all([tool[-1]])
    if poppler and exe:
        out["poppler_build_sha256"] = _sha((_sha(exe[0][1]) + poppler).encode())
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
    sha = lambda k: isinstance(ref.get(k), str) and bool(_SHA256.fullmatch(ref[k]))
    return (type(ref.get("schema")) is int and ref["schema"] == SCHEMA
            and ok(ref.get("profile")) and ref["profile"] not in ("unidentified",)
            and type(ref.get("version")) is int and ref["version"] >= 1
            and ref.get("isolation") in ISOLATION
            and (sha("inputs_sha256") if ref["isolation"] == "isolated" else ref.get("inputs_sha256") is None)
            and all(sha(k) for k in ("config_sha256", "fonts_sha256", "freetype_sha256", "fontconfig_lib_sha256",
                                     "poppler_build_sha256"))
            and type(ref.get("font_files")) is int and ref["font_files"] > 0
            and ok(ref.get("fontconfig")) and ok(ref.get("freetype")) and ok(ref.get("fontconfig_lib")))


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
