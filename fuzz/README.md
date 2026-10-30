# Fuzzing

libFuzzer targets (cargo-fuzz, nightly toolchain) for the encrypted-
document paths. Not part of the published crate.

| target | input | oracle |
|---|---|---|
| `open_any` | arbitrary bytes, opened strict and in recovery, without a password and with the fixtures' user and owner passwords | no panic, hang or runaway allocation; an encrypted document opened without a password must report the empty user password |
| `encrypt_dict` | the `/Encrypt` dictionary of an encrypted fixture — or, with bit 7 of the first byte, an extra object of that fixture with its trailer and table cut off | as above, and a fixture whose user password is not empty never opens without it (strict or recovery, trailer or not) |
| `ciphertext` | the content-stream data and the `/Info` title string of an encrypted fixture that authenticates, under RC4, AES-128 and AES-256 | no panic; arbitrary ciphertext is refused or decoded within the limits |

Every iteration also reads the content, extracts text and renders the
first pages at 9 dpi with tight limits (`src/lib.rs`).

```console
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked
mkdir -p fuzz/corpus/open_any && cp tests/fixtures/encrypted/*.pdf fuzz/corpus/open_any/
cargo +nightly fuzz run -O open_any fuzz/corpus/open_any -- -max_total_time=600 -rss_limit_mb=2048 -timeout=10
```

`encrypt_dict` and `ciphertext` start from an empty corpus or from the
fixtures' own dictionaries and streams (first byte: template index).
