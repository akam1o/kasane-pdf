#!/bin/sh
# One-command compatibility run: sync the corpus (object store or a
# local dir), build the runner, execute against it, write reports.
#
# Requires: credentials via the standard AWS provider chain (env /
#           ~/.aws / IAM) plus an endpoint+bucket — see
#           compat/corpus.env.example — *or* a locally-populated
#           compat/corpus dir (no store needed).
#           .venv-corpus with boto3+pyyaml (see docs/compat-corpus.md),
#           cargo, and (for --with-poppler) pdfinfo/pdftoppm/pdftotext.
# Usage:  scripts/compat-run.sh [--with-poppler] [--only ID] [--dpi N]
set -eu
cd "$(dirname "$0")/.."

# Load local env config if present (.env is git-ignored).
if [ -f .env ]; then
    set -a
    # shellcheck disable=SC1091
    . ./.env
    set +a
fi

VENV=.venv-corpus
if [ ! -x "$VENV/bin/python" ]; then
    echo "creating $VENV"; python3 -m venv "$VENV"
    "$VENV/bin/pip" install -q boto3 pyyaml pillow
fi

echo "==> syncing corpus"
if [ -n "${KASANE_CORPUS_ENDPOINT:-}" ] && [ -n "${KASANE_CORPUS_BUCKET:-}" ]; then
    "$VENV/bin/python" compat/fetch-corpus.py download
elif [ -d compat/corpus ] && [ "$(find compat/corpus -name '*.pdf' | wc -l)" -gt 0 ]; then
    echo "    no store configured — using existing local compat/corpus"
else
    echo "    no store configured and no local corpus; skipping sync."
    echo "    Set KASANE_CORPUS_ENDPOINT/KASANE_CORPUS_BUCKET, or seed from"
    echo "    local PDFs:  fetch-corpus.py seed --from DIR"
fi

echo "==> building runner"
cargo build --manifest-path compat/runner/Cargo.toml --release

if [ ! -d compat/corpus ] || [ "$(find compat/corpus -name '*.pdf' | wc -l)" -eq 0 ]; then
    echo "==> no corpus files found — nothing to run."
    echo "    Fetch with fetch-corpus.py download, or seed from local PDFs."
    exit 0
fi

echo "==> running compat suite"
compat/runner/target/release/kasane-compat run \
    --corpus compat/corpus --out compat/reports "$@"

echo "==> done — reports in compat/reports/ (latest.md / latest.json)"
