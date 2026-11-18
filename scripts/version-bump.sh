#!/usr/bin/env bash
# version-bump.sh — Single source of truth for version bumping.
#
# VERSIONING SCHEME (X.Y.Z):
#   - X (Major) can be as large as needed (unbounded integer).
#   - Y (Minor) and Z (Patch) are STRICTLY single digits: 0..9.
#   - If no release has been made within the last 24 hours:
#     Increment Y and reset Z to 0.
#     If Y exceeds 9 (rolls over from 9 to 10), Y becomes 0 and X increments by 1.
#     Example: 5.9.0 -> 6.0.0,  6.1.2 -> 6.2.0,  6.9.5 -> 7.0.0
#   - If a release already exists from within the 24-hour window:
#     Increment Z (hotfix).
#     If Z exceeds 9 (rolls over from 9 to 10), Z becomes 0 and Y increments by 1.
#     If Y also exceeds 9, Y becomes 0 and X increments by 1.
#     Example: 7.8.9 -> 7.9.0,  7.9.9 -> 8.0.0,  6.0.0 -> 6.0.1
#
# Usage:
#   ./scripts/version-bump.sh            # computes & prints the next version
#   ./scripts/version-bump.sh --apply    # writes new version into Cargo.toml & syncs Cargo.lock
#   ./scripts/version-bump.sh --test     # runs built-in self-tests for version calculation
#
# Requires: git, sed, date (GNU coreutils)
set -euo pipefail

CARGO_TOML="Cargo.toml"
SCRAPER_RS="src/scraper.rs"

# ── Function: Pure version calculation logic ─────────────────────────
calc_next_version() {
  local maj=$1
  local min=$2
  local pat=$3
  local hotfix=$4

  if [[ "$hotfix" == "true" ]]; then
    # Intraday hotfix: increment Z
    pat=$((pat + 1))
    if [[ $pat -gt 9 ]]; then
      pat=0
      min=$((min + 1))
      if [[ $min -gt 9 ]]; then
        min=0
        maj=$((maj + 1))
      fi
    fi
  else
    # New release window (>= 24h): increment Y, reset Z to 0
    min=$((min + 1))
    pat=0
    if [[ $min -gt 9 ]]; then
      min=0
      maj=$((maj + 1))
    fi
  fi
  echo "${maj}.${min}.${pat}"
}

# ── Built-in self-test mode ──────────────────────────────────────────
if [[ "${1:-}" == "--test" ]]; then
  echo "Running version calculation self-tests..."
  
  run_test() {
    local input_maj=$1
    local input_min=$2
    local input_pat=$3
    local is_hotfix=$4
    local expected=$5
    local actual
    actual=$(calc_next_version "$input_maj" "$input_min" "$input_pat" "$is_hotfix")
    if [[ "$actual" != "$expected" ]]; then
      echo "FAIL: ($input_maj.$input_min.$input_pat, hotfix=$is_hotfix) -> expected $expected, got $actual" >&2
      exit 1
    fi
    echo "  ✓ $input_maj.$input_min.$input_pat (hotfix=$is_hotfix) -> $actual"
  }

  run_test 5 9 0 false "6.0.0"   # Baseline 5.9 -> 6.0.0
  run_test 7 8 9 true  "7.9.0"   # Z overflow (single-digit constraint)
  run_test 7 9 9 true  "8.0.0"   # Z and Y double overflow
  run_test 6 0 0 true  "6.0.1"   # Normal intraday hotfix
  run_test 6 0 1 false "6.1.0"   # Normal >= 24h bump
  run_test 6 9 5 false "7.0.0"   # Y overflow on >= 24h bump
  run_test 12 9 0 false "13.0.0" # Large major version
  run_test 0 3 0 false "0.4.0"   # Normal minor bump

  echo "All version calculation tests passed successfully!"
  exit 0
fi

# ── Determine current version from Cargo.toml ───────────────────────
current_version=$(grep -oP '^version\s*=\s*"\K[0-9]+\.[0-9]+\.[0-9]+' "$CARGO_TOML" | head -1)
if [[ -z "$current_version" ]]; then
  echo "ERROR: Could not read version from $CARGO_TOML" >&2
  exit 1
fi

IFS='.' read -r C_MAJOR C_MINOR C_PATCH <<< "$current_version"

# ── Scan Git tags to find the highest numerical version ──────────────
BEST_TAG=""
TAG_MAJOR=0
TAG_MINOR=0
TAG_PATCH=0

while read -r raw_tag; do
  [[ -z "$raw_tag" ]] && continue
  clean_tag="${raw_tag#v}" # Strip optional leading 'v'

  if [[ "$clean_tag" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    t_maj="${BASH_REMATCH[1]}"
    t_min="${BASH_REMATCH[2]}"
    t_pat="${BASH_REMATCH[3]}"
  elif [[ "$clean_tag" =~ ^([0-9]+)\.([0-9]+)$ ]]; then
    t_maj="${BASH_REMATCH[1]}"
    t_min="${BASH_REMATCH[2]}"
    t_pat=0
  else
    continue
  fi

  # Numerical comparison: is this tag greater than BEST?
  if [[ "$t_maj" -gt "$TAG_MAJOR" ]] || \
     [[ "$t_maj" -eq "$TAG_MAJOR" && "$t_min" -gt "$TAG_MINOR" ]] || \
     [[ "$t_maj" -eq "$TAG_MAJOR" && "$t_min" -eq "$TAG_MINOR" && "$t_pat" -gt "$TAG_PATCH" ]]; then
    TAG_MAJOR=$t_maj
    TAG_MINOR=$t_min
    TAG_PATCH=$t_pat
    BEST_TAG="$raw_tag"
  fi
done < <(git tag -l)

# ── Determine baseline: max(Cargo.toml, highest git tag) ─────────────
BASE_MAJOR=$C_MAJOR
BASE_MINOR=$C_MINOR
BASE_PATCH=$C_PATCH

if [[ -n "$BEST_TAG" ]]; then
  if [[ "$TAG_MAJOR" -gt "$BASE_MAJOR" ]] || \
     [[ "$TAG_MAJOR" -eq "$BASE_MAJOR" && "$TAG_MINOR" -gt "$BASE_MINOR" ]] || \
     [[ "$TAG_MAJOR" -eq "$BASE_MAJOR" && "$TAG_MINOR" -eq "$BASE_MINOR" && "$TAG_PATCH" -gt "$BASE_PATCH" ]]; then
    BASE_MAJOR=$TAG_MAJOR
    BASE_MINOR=$TAG_MINOR
    BASE_PATCH=$TAG_PATCH
  fi
fi

# ── Get the timestamp of the latest release tag ──────────────────────
LATEST_TAG_DATE=""
if [[ -n "$BEST_TAG" ]]; then
  if git rev-parse --verify "refs/tags/$BEST_TAG" &>/dev/null; then
    LATEST_TAG_DATE=$(git log -1 --format=%aI "refs/tags/$BEST_TAG" 2>/dev/null || true)
  fi
fi

# ── Determine if within the 24h rolling window ───────────────────────
within_24h=false
if [[ -n "$LATEST_TAG_DATE" ]]; then
  tag_epoch=$(date -d "$LATEST_TAG_DATE" +%s 2>/dev/null || date -j -f "%Y-%m-%dT%H:%M:%S" "${LATEST_TAG_DATE%%[+-]*}" +%s 2>/dev/null || echo 0)
  now_epoch=$(date +%s)
  diff_seconds=$((now_epoch - tag_epoch))
  if [[ $diff_seconds -lt 86400 && $diff_seconds -ge 0 ]]; then
    within_24h=true
  fi
fi

# ── Calculate next version with single-digit constraint ──────────────
NEW_VERSION=$(calc_next_version "$BASE_MAJOR" "$BASE_MINOR" "$BASE_PATCH" "$within_24h")

echo "Current Cargo.toml: $current_version"
echo "Highest Git tag:    ${BEST_TAG:-<none>} (${TAG_MAJOR}.${TAG_MINOR}.${TAG_PATCH})"
echo "Baseline version:   ${BASE_MAJOR}.${BASE_MINOR}.${BASE_PATCH}"
echo "Tag timestamp:      ${LATEST_TAG_DATE:-<none>}"
echo "Within 24h window:  $within_24h"
echo "Calculated version: $NEW_VERSION"

# ── Apply flag: update Cargo.toml and sync Cargo.lock ────────────────
if [[ "${1:-}" == "--apply" ]]; then
  echo ""
  echo "Applying version $NEW_VERSION..."

  # Update Cargo.toml
  sed -i "s/^version = \"[0-9]*\.[0-9]*\.[0-9]*\"/version = \"${NEW_VERSION}\"/" "$CARGO_TOML"
  echo "  ✓ Updated $CARGO_TOML"

  # Sync Cargo.lock
  if command -v cargo &>/dev/null; then
    cargo check --quiet || true
    echo "  ✓ Synchronized Cargo.lock"
  fi

  # Verify consistency
  echo ""
  echo "Verifying version consistency..."
  cargo_ver=$(grep -oP '^version\s*=\s*"\K[0-9]+\.[0-9]+\.[0-9]+' "$CARGO_TOML" | head -1)

  if [[ "$cargo_ver" != "$NEW_VERSION" ]]; then
    echo "ERROR: Cargo.toml version ($cargo_ver) != expected ($NEW_VERSION)" >&2
    exit 1
  fi

  if ! grep -q 'env!("CARGO_PKG_VERSION")' "$SCRAPER_RS"; then
    echo "ERROR: $SCRAPER_RS does not use compile-time env!(\"CARGO_PKG_VERSION\")" >&2
    exit 1
  fi
  echo "  ✓ Cargo.toml is $NEW_VERSION and User-Agent is compile-time bound via env!(\"CARGO_PKG_VERSION\")"
fi

# Output for GitHub Actions
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  echo "version=${NEW_VERSION}" >> "$GITHUB_OUTPUT"
  echo "within_24h=${within_24h}" >> "$GITHUB_OUTPUT"
  echo "previous_version=${BASE_MAJOR}.${BASE_MINOR}.${BASE_PATCH}" >> "$GITHUB_OUTPUT"
  echo "previous_tag=${BEST_TAG}" >> "$GITHUB_OUTPUT"
fi
