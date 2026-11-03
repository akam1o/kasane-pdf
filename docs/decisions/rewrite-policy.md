# Decision — rewrite policy: executable content and usage-rights removal

Status: accepted for implementation in v0.5.0 phase C, pending the
open decisions of the plan. The design is
[`../plans/rewrite-policy.md`](../plans/rewrite-policy.md).

- **The default stays the default.** `rewrite` keeps its bytes and its
  refusals. A new `rewrite_with(&Document, &RewriteOptions) ->
  Result<RewriteOutput, Error>` takes explicit policies, and with
  default options it produces exactly `rewrite`'s output. No feature
  flag, no relaxed default.
- **The crate's option style.** `#[non_exhaustive]` option structs with
  `Default` and `with_*` methods, and `#[non_exhaustive]` enums, as
  `Limits` and `OpenOptions`, so every later policy is an additive
  change. Plain public fields and exhaustive two-variant enums were
  rejected, because each new policy would break callers.
- **Classified by effect, not by "is it JavaScript".** Scripts,
  external actions (Launch, URI, GoToR, GoToE, SubmitForm, ImportData),
  state changes, navigation, multimedia and unknown actions are
  separate classes. The default profile is 0.4's: document-level
  script entry points refused, everything else carried. That is now
  *reported*, because 0.4 carried page-, annotation-, field- and
  outline-level actions silently.
- **Carry is not sanitizing; multimedia is separate.** `Carry` copies
  actions byte for byte, executes nothing and promises nothing about
  the output's safety. `/RichMedia` has its own policy and stays
  refused in v0.5.
- **Removing usage rights, not signatures in general.** `Remove`
  deletes only `/Perms /UR` and `/UR3` usage-rights signature
  dictionaries:
  - they must be recognised by their `/TransformMethod`;
  - they must be reachable only from `/Perms`;
  - no other signature-like dictionary may exist anywhere in the
    reachable graph (`/ByteRange`, `/Type /Sig` or `/DocTimeStamp`,
    `/Contents` with any `/Filter` or `/SubFilter`, transform
    references; `/FT /Sig` is not required, and no handler-name list
    decides).

  DocMDP, approval and timestamp signatures, XFA XML signatures,
  `/DSS` and `/Legal` refuse. The engine never verifies, never
  re-signs, and never edits signature evidence.
- **Checked on the output.** The written bytes are reparsed and walked
  by an independent reader for any signature marker; a residual one
  is an error and no bytes are returned.
- **The type says what happened.** `RewriteOutput` is `#[must_use]`
  and holds a report. A usage-rights removal is
  `UsageRightsOutcome::Removed(..)`, not a warning. Reports and errors
  hold positions, object numbers, kinds and lengths, never document
  content, field names or digests.
- **The semantic round trip accepts only declared transformations.**
  Snapshots are unchanged. A table of rules in the runner, each
  declared by the run's options or by the library's report and
  confirmed by the runner's own reading of the input, classifies a
  matching difference `transformed`, which is never `pass`. A near
  miss stays `fail`. The version floor (W2) is the first rule, usage
  rights the second.
- **Measured before designed.** A spike (not merged) measured:
  - D2 alone +6 Full PASS;
  - D1 alone +0 (every usage-rights form also has a JavaScript tree);
  - both together +33, to 378 / 425.

  So D2 comes first, and D1 is only worth shipping with it.
