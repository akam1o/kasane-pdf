---
name: Bug report
about: A crash, a hang, a wrong result, or a typed error on a valid PDF
labels: bug
---

<!-- For security problems (crash / hang / unbounded memory on hostile
input, success reported on unprocessed content) use SECURITY.md
instead of this public tracker. -->

**What happened**

<!-- The command or API call, and the output / error. `kasane-pdf`
errors carry a stable category, e.g. `error [unsupported feature]: …`;
please paste the whole line. -->

**What you expected**

**Input**

<!-- A minimal PDF that reproduces it, attached or linked — only files
you are allowed to redistribute. If you cannot share the file, the
output of `kasane-pdf inspect <file> <page>` and the producer string
from `kasane-pdf <file>` help. -->

**Environment**

- kasane-pdf commit / version:
- OS and Rust version (`rustc --version`):
- features enabled (e.g. `cjk-fallback`):
