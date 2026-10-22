#!/bin/sh
# One-command compatibility run: sync corpus from R2, build the
# runner, execute against the corpus, write reports.
#
# Requires: the R2 credentials in ~/.cloudflare (or AWS_* env),
#           .venv-corpus with boto3+pyyaml (see docs/compat-corpus.md),
#           cargo, and (for --with-poppler) pdfinfo/pdftoppm/pdftotext.
# Usage:  scripts/compat-run.sh [--with-poppler] [--only ID] [--dpi N]
set -eu
cd "$(dirname "$0")/.."

VENV=.venv-corpus
if [ ! -x "$VENV/bin/python" ]; then
    echo "creating $VENV"; python3 -m venv "$VENV"
    "$VENV/bin/pip" install -q boto3 pyyaml pillow
fi

echo "==> syncing corpus from R2"
"$VENV/bin/python" compat/fetch-corpus.py download

echo "==> building runner"
cargo build --manifest-path compat/runner/Cargo.toml --release

echo "==> running compat suite"
compat/runner/target/release/kasane-compat run \
    --corpus compat/corpus --out compat/reports "$@"

echo "==> done — reports in compat/reports/ (latest.md / latest.json)"
