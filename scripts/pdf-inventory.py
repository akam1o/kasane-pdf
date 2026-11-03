#!/usr/bin/env python3
"""Action inventory read with pypdf — the third implementation the
runner's inventory (`kasane-compat inventory FILE`, semantic/v4.rs) and,
later, the library's D2 report are checked against
(docs/plans/rewrite-policy.md §3.5).

  pdf-inventory.py FILE
      One JSON object per line: class, kind, owner, position,
      shared_with, script_bytes — the same fields, owners and order as
      the runner. Content-free: no script text, no names.

  pdf-inventory.py --compare FILE RUNNER.jsonl
      Compare with the runner's lines for FILE; exit 1 on a difference.

  pdf-inventory.py --corpus DIR --runner BIN [--details OUT.json]
      Both inventories for every *.pdf in DIR. Prints counts only;
      `--details` writes the per-file differences (file ids: keep the
      output in a private, git-ignored place).

Owners: catalog/OpenAction (an action dictionary; an array is a
destination), catalog/AA/<trigger>, page<p>/AA/<trigger>,
annot<p>.<k>/A, annot<p>.<k>/AA/<trigger>, field<path>/AA/<trigger>
(field nodes that are not a page's annotation), outline<path>/A,
names/JavaScript#<i>. Triggers in byte order. `/Next` chains depth
first; an action object met before is listed once more with
`shared_with` (the first item) and its chain is not followed again; a
cycle ends the chain.
"""
import json
import os
import subprocess
import sys

from pypdf import PdfReader
from pypdf.generic import ArrayObject, DictionaryObject, IndirectObject, NullObject, StreamObject

SCRIPT = {"JavaScript"}
EXTERNAL = {"Launch", "URI", "GoToR", "GoToE", "SubmitForm", "ImportData"}
STATE = {"ResetForm", "Hide", "SetOCGState", "Trans"}
NAVIGATION = {"GoTo", "GoToDp", "Thread", "Named", "GoTo3DView"}
MULTIMEDIA = {"Rendition", "Movie", "Sound", "RichMediaExecute"}
MAX_ACTION_DEPTH = 32
MAX_DEPTH = 64


def klass(kind):
    for name, s in (("script", SCRIPT), ("external", EXTERNAL), ("state", STATE),
                    ("navigation", NAVIGATION), ("multimedia", MULTIMEDIA)):
        if kind in s:
            return name
    return "unknown"


def ident(o):
    return (o.idnum, o.generation) if isinstance(o, IndirectObject) else None


def get(o):
    return o.get_object() if isinstance(o, IndirectObject) else o


def entry(d, key):
    """A dictionary entry, None when absent or null (direct or through a
    reference)."""
    if key not in d:
        return None
    v = d.raw_get(key)
    return None if isinstance(get(v), NullObject) or get(v) is None else v


def is_dict(o):
    return isinstance(get(o), DictionaryObject)


def name(o):
    o = get(o)
    return str(o)[1:] if o is not None and str(o).startswith("/") else None


class Inventory:
    def __init__(self, reader):
        self.r = reader
        self.root = reader.trailer["/Root"].get_object()
        self.annots = set()
        for p in reader.pages:
            for a in get(p.get("/Annots")) or []:
                if ident(a):
                    self.annots.add(ident(a))

    def aa(self, head, d, out):
        a = entry(d, "/AA")
        if a is None or not is_dict(a):
            return
        a = get(a)
        for k in sorted(a.keys(), key=lambda k: k[1:].encode("latin-1")):
            out.append((f"{head}/AA/{k[1:]}", a.raw_get(k)))

    def roots(self):
        out = []
        cat = self.root
        oa = entry(cat, "/OpenAction")
        if oa is not None and is_dict(oa):
            out.append(("catalog/OpenAction", oa))
        self.aa("catalog", cat, out)
        for i, p in enumerate(self.r.pages):
            self.aa(f"page{i}", p, out)
            for k, a in enumerate(get(p.get("/Annots")) or []):
                if not is_dict(a):
                    continue
                ad = get(a)
                x = entry(ad, "/A")
                if x is not None:
                    out.append((f"annot{i}.{k}/A", x))
                self.aa(f"annot{i}.{k}", ad, out)
        af = entry(cat, "/AcroForm")
        if af is not None and is_dict(af):
            seen = set()
            for i, f in enumerate(get(get(af).get("/Fields")) or []):
                self.fields(f, str(i), 0, seen, out)
        ol = entry(cat, "/Outlines")
        if ol is not None and is_dict(ol):
            self.outlines(get(ol), "", 0, set(), out)
        names = entry(cat, "/Names")
        if names is not None and is_dict(names):
            js = entry(get(names), "/JavaScript")
            if js is not None:
                for i, v in enumerate(self.tree(js)):
                    out.append((f"names/JavaScript#{i}", v))
        return out

    def fields(self, o, at, depth, seen, out):
        if depth > MAX_DEPTH:
            raise ValueError("field tree deeper than the limit")
        r = ident(o)
        if r:
            if r in seen:
                raise ValueError("cycle in the field tree")
            seen.add(r)
        if not is_dict(o):
            return
        d = get(o)
        if r not in self.annots:
            self.aa(f"field{at}", d, out)
        for i, k in enumerate(get(d.get("/Kids")) or []):
            self.fields(k, f"{at}.{i}", depth + 1, seen, out)

    def outlines(self, parent, at, depth, seen, out):
        if depth > MAX_DEPTH:
            raise ValueError("outline tree deeper than the limit")
        cur = parent.raw_get("/First") if "/First" in parent else None
        i = 0
        while cur is not None:
            r = ident(cur)
            if r:
                if r in seen:
                    raise ValueError("cycle in the outline tree")
                seen.add(r)
            if not is_dict(cur):
                break
            d = get(cur)
            p = str(i) if not at else f"{at}.{i}"
            a = entry(d, "/A")
            if a is not None:
                out.append((f"outline{p}/A", a))
            self.outlines(d, p, depth + 1, seen, out)
            cur = d.raw_get("/Next") if "/Next" in d else None
            i += 1

    def tree(self, root):
        out, seen, stack = [], set(), [(root, 0)]
        while stack:
            node, depth = stack.pop()
            if depth > MAX_DEPTH:
                raise ValueError("tree deeper than the limit")
            r = ident(node)
            if r:
                if r in seen:
                    raise ValueError("cycle in a name tree")
                seen.add(r)
            d = get(node)
            if not isinstance(d, DictionaryObject):
                raise ValueError("tree node is not a dictionary")
            if "/Names" in d:
                kv = get(d.raw_get("/Names"))
                for j in range(0, len(kv) - 1, 2):
                    out.append(kv[j + 1])
            for k in reversed(get(d.get("/Kids")) or []):
                stack.append((k, depth + 1))
        return out

    def items(self):
        items, first = [], {}
        for owner, o in self.roots():
            position = 0
            stack = [(o, 0, ())]
            while stack:
                o, depth, path = stack.pop()
                if depth > MAX_ACTION_DEPTH:
                    raise ValueError("action chain deeper than the limit")
                r = ident(o)
                if r and r in path:
                    continue
                d = get(o)
                if d is None or isinstance(d, NullObject):
                    continue  # undefined: no action
                if not isinstance(d, DictionaryObject):
                    items.append({"class": "unknown", "kind": "#malformed", "owner": owner, "position": position,
                                  "shared_with": None, "script_bytes": None})
                    position += 1
                    continue
                kind = name(d.get("/S")) or "-"
                shared = first.get(r) if r else None
                script = None
                if kind == "JavaScript" and "/JS" in d:
                    js = get(d.raw_get("/JS"))
                    if isinstance(js, StreamObject):
                        script = len(js.get_data())
                    elif isinstance(js, (str, bytes)):
                        script = len(js.original_bytes if hasattr(js, "original_bytes") else js)
                if r and shared is None:
                    first[r] = len(items)
                items.append({"class": klass(kind), "kind": kind, "owner": owner, "position": position,
                              "shared_with": shared, "script_bytes": script})
                position += 1
                if shared is not None:
                    continue
                n = entry(d, "/Next")
                if n is not None:
                    nx = get(n)
                    nx = list(nx) if isinstance(nx, ArrayObject) else [n]
                    p = path + ((r,) if r else ())
                    for x in reversed(nx):
                        stack.append((x, depth + 1, p))
        return items


def pypdf_items(path):
    return Inventory(PdfReader(path, strict=False)).items()


def labelled(items):
    """`shared_with` as the first item's owner@position (independent of
    list indices)."""
    out = []
    for it in items:
        it = dict(it)
        s = it.get("shared_with")
        if s is not None:
            it["shared_with"] = f"{items[s]['owner']}@{items[s]['position']}"
        out.append(it)
    return out


def diff(a, b):
    a, b = labelled(a), labelled(b)
    if a == b:
        return []
    out = []
    if len(a) != len(b):
        out.append(f"items {len(a)} vs {len(b)}")
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y:
            keys = sorted(k for k in x if x.get(k) != y.get(k))
            out.append(f"item {i}: {','.join(keys)}")
            break
    return out


def runner_items(runner, path):
    p = subprocess.run([runner, "inventory", path], capture_output=True, text=True, timeout=120)
    if p.returncode != 0:
        raise ValueError(p.stderr.strip().splitlines()[-1] if p.stderr.strip() else "runner failed")
    return [json.loads(l) for l in p.stdout.splitlines() if l.strip()]


def main(argv):
    if len(argv) == 2:
        for it in pypdf_items(argv[1]):
            print(json.dumps(it))
        return 0
    if len(argv) == 4 and argv[1] == "--compare":
        with open(argv[3]) as fh:
            theirs = [json.loads(l) for l in fh if l.strip()]
        d = diff(pypdf_items(argv[2]), theirs)
        for line in d:
            print(line)
        return 1 if d else 0
    if len(argv) >= 5 and argv[1] == "--corpus" and argv[3] == "--runner":
        corpus, runner = argv[2], argv[4]
        details_path = argv[6] if len(argv) >= 7 and argv[5] == "--details" else None
        counts = {"files": 0, "both_read": 0, "agree": 0, "differ": 0, "runner_error": 0, "pypdf_error": 0,
                  "both_error": 0, "files_with_actions": 0, "items": 0}
        by_class = {}
        details = {}
        for fn in sorted(os.listdir(corpus)):
            if not fn.endswith(".pdf"):
                continue
            counts["files"] += 1
            path = os.path.join(corpus, fn)
            ours = theirs = None
            try:
                theirs = runner_items(runner, path)
            except Exception as e:  # noqa: BLE001 - recorded, not raised
                details.setdefault(fn[:-4], []).append(f"runner: {type(e).__name__}")
            try:
                ours = pypdf_items(path)
            except Exception as e:  # noqa: BLE001
                details.setdefault(fn[:-4], []).append(f"pypdf: {type(e).__name__}")
            if ours is None and theirs is None:
                counts["both_error"] += 1
                continue
            if theirs is None:
                counts["runner_error"] += 1
                continue
            if ours is None:
                counts["pypdf_error"] += 1
                continue
            counts["both_read"] += 1
            d = diff(ours, theirs)
            if d:
                counts["differ"] += 1
                details.setdefault(fn[:-4], []).extend(d)
            else:
                counts["agree"] += 1
                if theirs:
                    counts["files_with_actions"] += 1
                counts["items"] += len(theirs)
                for it in theirs:
                    by_class[it["class"]] = by_class.get(it["class"], 0) + 1
        print(json.dumps({"counts": counts, "agreed_items_by_class": by_class}, indent=1, sort_keys=True))
        if details_path:
            with open(details_path, "w") as fh:
                json.dump(details, fh, indent=1, sort_keys=True)
        return 1 if counts["differ"] else 0
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
