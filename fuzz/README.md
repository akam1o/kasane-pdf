# Fuzzing

libFuzzer targets (cargo-fuzz, nightly toolchain) for the encrypted-
document paths. Not part of the published crate.

| target | input | oracle |
|---|---|---|
| `open_any` | arbitrary bytes, opened strict and in recovery, without a password and with the fixtures' user and owner passwords | no panic, hang or runaway allocation; an encrypted document opened without a password must report the empty user password |
| `encrypt_dict` | the `/Encrypt` dictionary of an encrypted fixture — or, with bit 7 of the first byte, an extra object of that fixture with its trailer and table cut off | as above, and a fixture whose user password is not empty never opens without it (strict or recovery, trailer or not) |
| `text_sources` | an arbitrary font program as a simple TrueType, CFF built-in-encoding or Type 0 (TrueType / CFF / OpenType, Identity and Japan1) font, or (byte 0 ≥ 128) an arbitrary annotation dictionary of a form page | no panic, hang or runaway allocation in text extraction with annotations and in rendering |
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

`encrypt_dict` and `ciphertext` start from an empty corpus or from the
fixtures' own dictionaries and streams (first byte: template index).
