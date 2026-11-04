# Fuzzing

libFuzzer targets (cargo-fuzz, nightly toolchain) for the encrypted-
document paths, the cross-reference reader, the rewriter and the renderer. Not part of the published crate.

| target | input | oracle |
|---|---|---|
| `open_any` | arbitrary bytes, opened strict and in recovery, without a password and with the fixtures' user and owner passwords | no panic, hang or runaway allocation; an encrypted document opened without a password must report the empty user password |
| `encrypt_dict` | the `/Encrypt` dictionary of an encrypted fixture — or, with bit 7 of the first byte, an extra object of that fixture with its trailer and table cut off | as above, and a fixture whose user password is not empty never opens without it (strict or recovery, trailer or not) |
| `text_sources` | an arbitrary font program as a simple TrueType, CFF built-in-encoding or Type 0 (TrueType / CFF / OpenType, Identity and Japan1) font, or (byte 0 ≥ 128) an arbitrary annotation dictionary of a form page | no panic, hang or runaway allocation in text extraction with annotations and in rendering |
| `xref_updates` | 1–6 update sections (classic table, xref stream or hybrid table + `/XRefStm`) built from the input, each defining, freeing, compressing or relisting objects at generations 0–2; with bit 7 of the first byte one byte is overwritten | every object resolves exactly as a model of the precedence rule predicts (the newest section that lists a number decides; in a hybrid section the table before its stream); corrupted input: no panic, hang or runaway allocation |
| `rewrite_graph` | up to 48 objects (dictionaries, arrays, bare references, strings, plain and Flate streams) whose references point at defined objects, undefined numbers, free entries or another generation — chains and cycles included; optionally an XFA packet array (duplicate or non-string names; undefined, non-stream, undecodable or XML-signed packets) and a `/Perms /UR3` signature; optionally declared PDF 2.0 (header or catalog `/Version`) with `/AF`, a DPart root and page `/Metadata` pointing into the graph; with bit 7 of the first byte one byte is overwritten | `rewrite` (strict and recovery) either succeeds — the output reparses strictly, has no dangling reference, and the catalog graph with references resolved by §7.3.10 and streams compared by raw bytes equals the input's, the declared version is kept and a 2.0 output (only) has a trailer `/ID` — or fails for a cause the input contains (signature, undecodable packet, reference cycle, limit); `UsageRightsPolicy::Remove` gives `rewrite`'s bytes with nothing removed, or refuses where `rewrite` refuses; corrupted input: no panic, hang or runaway allocation |
| `rewrite_policy` | action graphs: every owner (`/OpenAction` as action, array or destination; catalog, page, annotation and field `/AA`; annotation and outline `/A`; `/Names /JavaScript`), action objects of every class, an unknown or missing `/S`, malformed values, `/Next` single or array with sharing, cycles and long chains, scripts as strings, Flate or undecodable streams, XFA packets with `script` elements (one split across packets); optionally a usage-rights signature, a signed field, `/RichMedia`, an outline cycle; with bit 7 of the first byte one byte is overwritten | `rewrite_with` under `Profile`, `RefuseScriptsAndExternal` and `Carry`: the default gives `rewrite`'s bytes or error; a strict success leaves no dictionary with a script or external `/S` or a `/JS` entry, and no XFA `script` element, reachable anywhere in the output; a `Carry` success keeps the catalog's keys and has nothing unclassified; signature and `/RichMedia` refusals hold under every policy, with `UsageRightsPolicy::Remove` too; with Remove and no removable signature each policy gives its own bytes or refusal; reports and errors hold no document content; output objects resolve; corrupted input: no panic, hang or runaway allocation |
| `signature_markers` | `UsageRightsPolicy::Remove` against a ground truth: an optional `/UR3` usage-rights signature (direct or indirect, valid or not, shared, holding a shared object) in a direct or indirect `/Perms` (with another entry or `/DocMDP`), `/SigFlags` of every bit pattern (direct or an integer object), and decoys of every signature marker (`/ByteRange`; `/Type /Sig` or `/DocTimeStamp`, through a reference too; `/Contents` with `/Filter` or `/SubFilter`; `/Cert` with `/Contents`; `/Reference` with `/TransformMethod` alone, in an array, nested, through a reference; a stream) or near misses, placed in the catalog, a page, an annotation, a field `/V`, a `/Sig` widget outside `/Fields`, only behind `/P` or `/Parent` (with cycles), in `/Info`, below a seed value, behind a chain or an object-stream alias, at the end of a deep chain, or unreachable; field locks, a seed value's own lock, `/DSS`, `/Legal`, an XFA XML signature, a page script, `/RichMedia`, a large dictionary, objects packed in an object stream, tight limits; with bit 7 of the first byte one byte is overwritten | the default gives `rewrite`'s bytes or error; under every executable-content policy, Remove refuses (a signature refusal) whenever the generator put a reachable marker, a lock, a store, an XFA signature, a bad or shared target or `/DocMDP` in, and succeeds otherwise (no script under the strict policy, no `/RichMedia`); a success has no signature-like dictionary reachable (the target's own walker), no `/ByteRange` in its bytes, no `/Perms /UR`, `/UR3`, `/SigFlags` with bit 1 cleared and every other bit kept, and reports `Removed` exactly when a target was there; reports and errors hold no signature value or certificate; corrupted input: no panic, hang or runaway allocation |
| `ciphertext` | the content-stream data and the `/Info` title string of an encrypted fixture that authenticates, under RC4, AES-128 and AES-256 | no panic; arbitrary ciphertext is refused or decoded within the limits |
| `render_content` | a fixed, valid page whose resources hold nested and cyclic Form XObjects, a knockout transparency group, alpha and luminosity soft masks, RGB / gray / 1-bit / soft-masked images, axial and radial shadings, a tiling and a shading pattern, a Type 3 and a standard font, an annotation appearance; its content stream is built from the input: operators with the right operand count or one too few or too many, extreme numbers (huge, tiny, negative, overflowing, malformed), defined and undefined resource names, inline images, unbalanced `q`/`Q`, text objects, marked content, and well-formed snippets (text in each font, pattern fills, masked and blended drawing, shadings under a clip) | no panic, stack overflow, hang or runaway allocation; the page is the size its box and DPI give; rendering is deterministic; when the content renders, wrapping it in `q … Q` or prepending an identity `cm` changes no pixel, and a shared form drawn twice equals two equal copies |
| `render_graph` | a page tree with nested `/Pages`, inherited `/Resources` and `/MediaBox`, a shared page and a `/Kids` cycle; up to 24 Form XObjects referencing each other (chains, cycles, sharing) directly or through a soft mask or a tiling pattern, with or without a transparency group; undefined and other-generation references; deep direct dictionaries and arrays; the objects packed in an object stream or not | rendering and text extraction of every page, strict and in recovery mode: no panic, stack overflow, hang or runaway allocation; a page within `max_render_total_pixels`; deterministic results (pixels or error) |

Every iteration also reads the content, extracts text and renders the
first pages at 9 dpi with tight limits (`src/lib.rs`).

```console
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked
mkdir -p fuzz/corpus/open_any && cp tests/fixtures/encrypted/*.pdf fuzz/corpus/open_any/
cargo +nightly fuzz run -O open_any fuzz/corpus/open_any -- -max_total_time=600 -rss_limit_mb=2048 -timeout=10
```

`text_sources` starts from `fuzz/seeds/text_sources/` (byte 0: how
the PDF uses the font; `hostile-cmap12-full-range` hung extraction
before 0.4.0 — 2 000 format 12 groups over all of Unicode):

```console
mkdir -p fuzz/corpus/text_sources && cp fuzz/seeds/text_sources/* fuzz/corpus/text_sources/
cargo +nightly fuzz run -O text_sources fuzz/corpus/text_sources -- -max_total_time=600 -rss_limit_mb=2048 -timeout=10
```

`xref_updates` starts from `fuzz/seeds/xref_updates/`; `open_any` also
takes the synthetic precedence cases `fuzz/seeds/open_any/xref-*`:

```console
mkdir -p fuzz/corpus/xref_updates && cp fuzz/seeds/xref_updates/* fuzz/corpus/xref_updates/
cargo +nightly fuzz run -O xref_updates fuzz/corpus/xref_updates -- -max_total_time=600 -rss_limit_mb=2048 -timeout=10
```

`rewrite_graph` starts from `fuzz/seeds/rewrite_graph/` (undefined and
free references, a reference cycle, XFA with duplicate and undefined
packets, an XML-signed packet, an undecodable packet with a usage-rights
signature, a corrupted file):

```console
mkdir -p fuzz/corpus/rewrite_graph && cp fuzz/seeds/rewrite_graph/* fuzz/corpus/rewrite_graph/
cargo +nightly fuzz run -O rewrite_graph fuzz/corpus/rewrite_graph -- -max_total_time=600 -rss_limit_mb=2048 -timeout=10
```

`encrypt_dict` and `ciphertext` start from an empty corpus or from the
fixtures' own dictionaries and streams (first byte: template index).

`render_content` and `render_graph` (0.5.1) start from
`fuzz/seeds/render_content/` (one seed per operator, well-formed
snippet and inline image; `regression-*` are the two inputs that
panicked the rasteriser before 0.5.1) and `fuzz/seeds/render_graph/`
(each page-tree flag × nesting style as a chain of forward references,
some forms sharing the next; and four graphs broken on purpose: a
cycle, a soft-mask cycle, an undefined reference, another generation).

CI runs every target through `fuzz/ci-run.sh`, which fails on
libFuzzer's exit status *and* on any file left in
`fuzz/artifacts/<target>/`, an error marker in the log, or a missing
final `Done … runs` line; limits 2 GiB RSS and malloc, 10 s per input.
Reproduce the PR smoke exactly as CI runs it, then fuzz longer
(`FUZZ_TOOLCHAIN` overrides the pinned `nightly-2026-10-29`):

```console
for t in render_content render_graph; do
  KASANE_FUZZ_REACH="/tmp/reach-$t" fuzz/ci-run.sh "$t" "fuzz/seeds/$t" -- -runs=0
  sort "/tmp/reach-$t" | uniq -c          # ok painted / ok blank / err <kind>, per page
  mkdir -p "/tmp/corpus-$t" && cp "fuzz/seeds/$t"/* "/tmp/corpus-$t/"
  fuzz/ci-run.sh "$t" "/tmp/corpus-$t" -- -seed=1 -runs=3000 -max_total_time=180
done
mkdir -p fuzz/corpus/render_content
fuzz/ci-run.sh render_content fuzz/corpus/render_content fuzz/seeds/render_content -- -max_total_time=900
```

`KASANE_FUZZ_REACH` (a file name) makes the render targets append one
line per rendered page: CI replays the seeds with it and requires
painted pages, so a generator that stops reaching the rasteriser fails
the job. `ci_selftest` is not a fuzz target: with
`KASANE_FUZZ_SELFTEST=crash|timeout|oom` every input panics, hangs or
exhausts memory, and `fuzz-nightly.yml`'s `selftest` job checks that
`ci-run.sh` reports each as a failure with its input saved; unset, it
does nothing.

The seed replay gives the same inputs every run; the 3000-input phase
is bounded but not bit-reproducible (libFuzzer's coverage feedback
follows the library's per-process hash order). A failing input is
deterministic: `cargo +nightly fuzz run -O <target> <artifact>` replays
it, and CI uploads it (`render-fuzz-smoke-artifacts`, or
`render-fuzz-<target>-artifacts` from `fuzz-nightly.yml`). Inputs are
generator bytes that become synthetic PDFs; they hold no document
content.

Not covered by these two targets: TrueType / CFF / CID fonts (see
`text_sources`), function-based and mesh shadings, images behind
filters, optional content.
