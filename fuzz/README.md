# Fuzzing

libFuzzer targets (cargo-fuzz, nightly toolchain) for the encrypted-
document paths, the cross-reference reader and the rewriter. Not part of the published crate.

| target | input | oracle |
|---|---|---|
| `open_any` | arbitrary bytes, opened strict and in recovery, without a password and with the fixtures' user and owner passwords | no panic, hang or runaway allocation; an encrypted document opened without a password must report the empty user password |
| `encrypt_dict` | the `/Encrypt` dictionary of an encrypted fixture — or, with bit 7 of the first byte, an extra object of that fixture with its trailer and table cut off | as above, and a fixture whose user password is not empty never opens without it (strict or recovery, trailer or not) |
| `text_sources` | an arbitrary font program as a simple TrueType, CFF built-in-encoding or Type 0 (TrueType / CFF / OpenType, Identity and Japan1) font, or (byte 0 ≥ 128) an arbitrary annotation dictionary of a form page | no panic, hang or runaway allocation in text extraction with annotations and in rendering |
| `xref_updates` | 1–6 update sections (classic table, xref stream or hybrid table + `/XRefStm`) built from the input, each defining, freeing, compressing or relisting objects at generations 0–2; with bit 7 of the first byte one byte is overwritten | every object resolves exactly as a model of the precedence rule predicts (the newest section that lists a number decides; in a hybrid section the table before its stream); corrupted input: no panic, hang or runaway allocation |
| `rewrite_graph` | up to 48 objects (dictionaries, arrays, bare references, strings, plain and Flate streams) whose references point at defined objects, undefined numbers, free entries or another generation — chains and cycles included; optionally an XFA packet array (duplicate or non-string names; undefined, non-stream, undecodable or XML-signed packets) and a `/Perms /UR3` signature; optionally declared PDF 2.0 (header or catalog `/Version`) with `/AF`, a DPart root and page `/Metadata` pointing into the graph; with bit 7 of the first byte one byte is overwritten | `rewrite` (strict and recovery) either succeeds — the output reparses strictly, has no dangling reference, and the catalog graph with references resolved by §7.3.10 and streams compared by raw bytes equals the input's, the declared version is kept and a 2.0 output (only) has a trailer `/ID` — or fails for a cause the input contains (signature, undecodable packet, reference cycle, limit); corrupted input: no panic, hang or runaway allocation |
| `ciphertext` | the content-stream data and the `/Info` title string of an encrypted fixture that authenticates, under RC4, AES-128 and AES-256 | no panic; arbitrary ciphertext is refused or decoded within the limits |

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
