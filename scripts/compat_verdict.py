"""The second witness's verdict from its measures
(docs/compat-evaluation.md § 2). Shared by scripts/compat-attribute.py,
which computes it, and scripts/ci-regression.py, which recomputes it
instead of trusting a recorded label."""
import math

# Within tolerance of a witness at 300 dpi (compared on the 72 dpi grid).
# Set in the v0.11.0 analysis (docs/compatibility-report.md § 6.2).
TOLERANCE = {"dpx": 0.005, "mad": 2.0}
AGREEMENTS = ("both", "poppler", "mupdf", "neither", "not_comparable")
KEYS = ("dpx_kp", "dpx_km", "dpx_pm", "mad_kp", "mad_km", "mad_pm")


def finite(*xs):
    return all(isinstance(x, (int, float)) and not isinstance(x, bool) and math.isfinite(x) for x in xs)


def in_range(r300):
    """Every measure present, finite and possible: dpx a share (0–1),
    mad a grey difference (0–255)."""
    return bool(r300) and all(finite(r300.get(k)) for k in KEYS) and \
        all(0 <= r300[k] <= 1 for k in KEYS if k.startswith("dpx")) and \
        all(0 <= r300[k] <= 255 for k in KEYS if k.startswith("mad"))


def within(dpx, mad):
    return finite(dpx, mad) and dpx < TOLERANCE["dpx"] and mad < TOLERANCE["mad"]


def verdict(r300):
    """(agreement, references_agree); not comparable unless every measure
    is present, finite and in range."""
    if not in_range(r300):
        return "not_comparable", None
    wp, wm = within(r300["dpx_kp"], r300["mad_kp"]), within(r300["dpx_km"], r300["mad_km"])
    refs = within(r300["dpx_pm"], r300["mad_pm"])
    return ("both" if wp and wm else "poppler" if wp else "mupdf" if wm else "neither"), refs
