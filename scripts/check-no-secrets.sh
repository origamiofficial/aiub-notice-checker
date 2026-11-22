#!/bin/sh
# H4: fail if a live-secrets file was ever tracked in git.
# This script itself is a plain repo file (not a workflow), so it can be
# pushed normally. To wire it into CI, add one step to
# .github/workflows/test.yml (a workflow file — that edit has to be made
# by a human with workflows permission, see PR #44 for why the GitHub App
# cannot push workflow files):
#
#   - name: Check no secrets files are tracked
#     run: sh scripts/check-no-secrets.sh
#
# It checks tracked files only: an untracked local .dev.vars (the normal
# dev setup) is fine and must NOT fail this check.
set -eu
if git ls-files | grep -E '(^|/)\.dev\.vars$|(^|/)\.env(\..*)?$|(^|/)[^/]*\.vars$' ; then
    echo "ERROR: a secrets file is tracked in git: see above. Remove it with" >&2
    echo "  git rm --cached <file>" >&2
    echo "and rotate the affected tokens before pushing." >&2
    exit 1
fi
echo "OK: no secrets files tracked."
