# Security policy

kasane-pdf parses untrusted input by design. A crash, a hang, an
unbounded allocation, or a case where the engine reports success on
content it did not actually process (a silent skip presented as a
result) is treated as a security bug.

## Reporting a vulnerability

Please do **not** open a public issue for a security problem.

Report it privately through GitHub's *Report a vulnerability* form
on the repository's **Security** tab (private vulnerability
reporting). If that form is not available on the repository, open an
issue that only says you have found a security problem and ask for a
private channel — do not include details in the public issue.

Include, where possible:

- the kasane-pdf commit or version,
- a minimal input that reproduces the problem (only files you are
  allowed to share),
- the command or API call and the observed behaviour (panic, timeout,
  memory use, wrong-but-successful output).

You will receive an acknowledgement, and a fix or a mitigation will
be discussed with you before anything is published. There is no bug
bounty.

## How fixes are published

A fix for an undisclosed vulnerability is prepared privately (a draft
GitHub Security Advisory and, when needed, its temporary private fork)
and released so that the details become public only once the fixed
version can be installed:

1. the fixed version is published to crates.io from a tagged commit
   whose CHANGELOG says, in general terms, that it fixes security
   issues and that advisories follow;
2. as soon as it can be downloaded, the advisories are published
   (affected and patched versions, conditions, severity, workarounds);
3. the GitHub Release links them, and a following commit adds the full
   *Security* section to the CHANGELOG and the regression tests.

Reproduction files and exploit details are not published.
`docs/releasing.md` (*B. Security release*) is the full procedure.

## Scope and guarantees

- Every unbounded size, count and depth is capped by `Limits`
  ([`docs/security.md`](docs/security.md)); exceeding a limit is a
  typed `Error::LimitExceeded`, never a panic.
- Processing **time** is not bounded. The limits bound how much is
  parsed, decoded, allocated and executed, but not how long one page
  takes to render: content within every limit can still take minutes.
  Applications rendering untrusted documents must enforce their own
  time limit — render in a separate process they can terminate (a
  rendering call cannot be cancelled, and abandoning a thread leaves it
  running) — and may tighten `Limits` and the DPI.
- The engine never executes JavaScript, follows external references,
  or performs network or filesystem access from the library.
- **Not** in scope: the engine is not a redaction tool or a sanitizer
  (`docs/security.md` § What the engine is not), and it does not
  verify digital signatures cryptographically — it only detects them
  and refuses edits the signer did not permit.
