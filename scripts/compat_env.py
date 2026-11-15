"""The measurement environment of a compatibility run: the commit, the
platform, and the versions of the reference renderers and tools
(docs/compat-evaluation.md § Environments). Imported by
scripts/compat-attribute.py and scripts/ci-regression.py.

A baseline records the environment it was measured in; a comparison
in another environment is refused (renders and reference renders
differ between platforms and renderer versions — measured: 230 of
1147 pages differ in SSIM between the Linux nightly and a macOS run of
the same commit).
"""
import os
import platform
import re
import subprocess

# The keys that must agree between a run and its baseline for the
# numeric comparisons to be meaningful.
COMPARED = ("os", "arch", "poppler", "mupdf", "features")


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


def environment(commit=None, features="", **tools):
    env = {
        "commit": commit or os.environ.get("GITHUB_SHA") or _git_head(),
        "os": os_name(),
        "arch": platform.machine(),
        "features": features,
    }
    env.update(tool_versions(**tools))
    return env


def _git_head():
    try:
        r = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True, timeout=30,
                           cwd=os.path.dirname(os.path.abspath(__file__)))
        return r.stdout.strip() or None
    except (OSError, subprocess.TimeoutExpired):
        return None


def differences(base, now, keys=COMPARED):
    """The compared keys whose values differ. `update-baseline` refuses
    to record an environment without `os`, `arch` and `poppler`, so a
    value both sides lack is one neither measured with (MuPDF on a run
    without attribution)."""
    return [k for k in keys if base.get(k) != now.get(k)]


def incomplete(env, need_mupdf):
    """Keys a recorded environment must have but lacks."""
    need = ["commit", "os", "arch", "poppler"] + (["mupdf"] if need_mupdf else [])
    return [k for k in need if not env.get(k)]
