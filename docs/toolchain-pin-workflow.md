# Toolchain pin — workflow changes (apply via GitHub web UI)

## Status and scope

- **Branch:** `fix/v6.2.4-top10-rust-1.100` (off v6.2.4, commit `07dfdf6`).
- **Phase A (done, uncommitted in the working tree):**
  - `Cargo.toml`: `edition = "2024"`, `rust-version = "1.100"`, added
    `description` / `license = "MIT"` / `repository =
    "https://github.com/origamiofficial/aiub-notice-checker"`, and a
    `[lints.clippy]` table (`correctness = { level = "deny", priority = -1 }`,
    `suspicious/style/complexity/perf/must_use_candidate = "warn"`).
  - `rust-toolchain.toml` (new): pins `channel = "1.100.0"` with components
    `rustfmt`, `clippy` and target `wasm32-unknown-unknown`.
  - `cargo fix --edition` applied one required 2024 fix in `src/lib.rs`
    (`if let … else` → `match`, Rust 2024 tail-expression temporary scope);
    `cargo fmt` under rustfmt 1.100 / style edition 2024 reordered imports
    in 7 files (mechanical only).
  - Verified: `cargo test --all-features` **160 passed** (156 lib + 4
    benchmark-bin), `cargo fmt --check` pass,
    `cargo check --target wasm32-unknown-unknown` pass.
- **Why this document exists:** the Muse GitHub App cannot push
  `.github/workflows/*`, so the workflow edits below must be applied by a
  human through the GitHub web UI (repo → `.github/workflows/<file>` →
  pencil icon → paste the replacement → "Commit changes" directly to the
  phase-A branch, **not** to `main`). Do **not** commit these workflow
  edits from a local checkout — the push will be rejected.
- **Known state — read before expecting green CI:** after these workflow
  changes, `cargo clippy --all-targets --all-features -- -D warnings`
  will still fail with **60 errors** (29× `collapsible_if`, 31×
  `must_use_candidate`). Both are consequences of phase A itself, not of
  the workflow edits: the pristine v6.2.4 tree passes Clippy 1.100 clean,
  edition 2024 newly enables let-chains so Clippy now suggests collapsing
  nested `if`/`if let` (29 sites, mostly `src/scraper.rs`), and
  `must_use_candidate = "warn"` is new by design (31 pub fns/methods).
  Fixing them needs `src/*.rs` edits (phase B) or revisiting the lint
  table — the workflow changes alone do not fix Clippy.

## Design decisions (apply to all three files)

1. **Single source of truth: `rust-toolchain.toml`.** The stable install
   step uses `dtolnay/rust-toolchain` with **no `toolchain:` input**, so
   the action reads the committed `rust-toolchain.toml` (channel
   `1.100.0`, components `rustfmt` + `clippy`, target
   `wasm32-unknown-unknown`). Do not repeat the version/components/targets
   in the YAML — if the pin ever moves, only the file changes. Keep the
   action ref `dtolnay/rust-toolchain@stable` (that is the *action's*
   release line, not the Rust toolchain).
2. **Nightly stays, but pinned to a date.** `worker-build --panic-unwind`
   (see `wrangler.toml` `[build] command`) still needs nightly with the
   `rust-src` component, so `deploy.yml` and `release.yml` keep a nightly
   install step — but pinned to a dated nightly,
   `toolchain: nightly-2026-11-23`, instead of floating
   `dtolnay/rust-toolchain@nightly`. If that date fails to install when
   you apply this (e.g. it was never published), roll forward one day at a
   time until `rustup toolchain install nightly-YYYY-MM-DD` succeeds, and
   use that date. Revisit removing nightly entirely only if
   `worker-build --panic-unwind` is ever dropped.
3. **`rustup default stable` must go.** After the pin, no toolchain named
   `stable` is installed — the installed toolchains are `1.100.0` and the
   dated nightly — so `rustup default stable` would fail. Replace it with
   `rustup default 1.100.0` so `cargo check` / `cargo install worker-build`
   run on the pinned stable. (The `rust-toolchain.toml` override would
   already select 1.100.0 inside the checkout; the explicit default is
   belt-and-braces for steps that run outside it.)
4. **Step order is unchanged:** stable install → nightly install (where
   present) → set default → cache → build/test/deploy steps.

## 1. `.github/workflows/test.yml`

### Current (quoted verbatim, lines 42–46)

```yaml
      - name: Setup Rust stable toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
          targets: wasm32-unknown-unknown
```

### Replacement

```yaml
      - name: Setup Rust toolchain (from rust-toolchain.toml)
        uses: dtolnay/rust-toolchain@stable
```

No `with:` block — the action installs channel `1.100.0`, components
`rustfmt` + `clippy`, and target `wasm32-unknown-unknown` from the
committed `rust-toolchain.toml`. No nightly step exists in this file and
none is added (no `worker-build` here).

### Optional but recommended — trigger on toolchain-pin changes

`test.yml` `paths:` (push, lines 6–13, and `pull_request`, lines 15–22)
currently lists `Cargo.toml` / `Cargo.lock` but not the pin file, so a
pin-only change would not run Tests. Add one line to **both** lists:

```yaml
      - "rust-toolchain.toml"
```

(place it after the `"Cargo.lock"` line in each list).

## 2. `.github/workflows/deploy.yml`

### Current (quoted verbatim, lines 37–49)

```yaml
      - name: Setup Rust stable & wasm32 target
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: wasm32-unknown-unknown

      - name: Setup Rust nightly for worker-build --panic-unwind
        uses: dtolnay/rust-toolchain@nightly
        with:
          components: rust-src
          targets: wasm32-unknown-unknown

      - name: Set stable as default toolchain
        run: rustup default stable
```

### Replacement

```yaml
      - name: Setup Rust toolchain (from rust-toolchain.toml)
        uses: dtolnay/rust-toolchain@stable

      - name: Setup pinned nightly for worker-build --panic-unwind
        uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: nightly-2026-11-23
          components: rust-src
          targets: wasm32-unknown-unknown

      - name: Set pinned stable as default toolchain
        run: rustup default 1.100.0
```

Notes:

- The stable step drops its `with:` block for the reason in §1 above.
- The nightly step keeps the action ref `@stable` and pins the *Rust*
  toolchain with `toolchain: nightly-2026-11-23` (see decision 2 for the
  roll-forward rule if that date does not install).
- `rustup default stable` is replaced, not kept (see decision 3).

## 3. `.github/workflows/release.yml`

### Current (quoted verbatim, lines 152–168)

```yaml
      - name: Setup Rust stable toolchain
        if: steps.check_changes.outputs.has_worker_changes == 'true'
        uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
          targets: wasm32-unknown-unknown

      - name: Setup Rust nightly for worker-build --panic-unwind
        if: steps.check_changes.outputs.has_worker_changes == 'true'
        uses: dtolnay/rust-toolchain@nightly
        with:
          components: rust-src
          targets: wasm32-unknown-unknown

      - name: Set stable as default toolchain
        if: steps.check_changes.outputs.has_worker_changes == 'true'
        run: rustup default stable
```

### Replacement

```yaml
      - name: Setup Rust toolchain (from rust-toolchain.toml)
        if: steps.check_changes.outputs.has_worker_changes == 'true'
        uses: dtolnay/rust-toolchain@stable

      - name: Setup pinned nightly for worker-build --panic-unwind
        if: steps.check_changes.outputs.has_worker_changes == 'true'
        uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: nightly-2026-11-23
          components: rust-src
          targets: wasm32-unknown-unknown

      - name: Set pinned stable as default toolchain
        if: steps.check_changes.outputs.has_worker_changes == 'true'
        run: rustup default 1.100.0
```

Keep the `if: steps.check_changes.outputs.has_worker_changes == 'true'`
condition on all three steps exactly as before. Same notes as
`deploy.yml` apply (dated nightly, no `rustup default stable`).

## Verification after applying (in the Actions run on the PR)

1. In the "Setup Rust toolchain" step log, confirm the installed toolchain
   is `1.100.0` (not floating `stable`), with `rustfmt`, `clippy`, and
   `wasm32-unknown-unknown` present. A quick sanity step you can add
   temporarily (or run locally) is `rustup show`, which prints the active
   toolchain and the override from `rust-toolchain.toml`.
2. In the nightly step log, confirm `nightly-2026-11-23` (or the rolled
   date) installed with `rust-src` + `wasm32-unknown-unknown`.
3. Confirm `cargo fmt --check`, `cargo test --all-features`, and
   `cargo check --target wasm32-unknown-unknown` still behave as in
   phase A (tests 160 passed; fmt and wasm check pass).
4. Expect `cargo clippy … -- -D warnings` to fail with the 60 known
   errors until phase B (see "Known state" above) — that failure is
   pre-existing phase-A work, not a workflow regression.
5. Optional cache note: the `actions/cache` keys hash `Cargo.lock` /
   `Cargo.toml` only. If the pin ever changes, consider adding
   `rust-toolchain.toml` to those `hashFiles(...)` keys so a toolchain
   change busts the cache; not required for this change.

## Files deliberately not touched here

- `wrangler.toml` — still `worker-build --release --panic-unwind`, which
  is why nightly is kept at all.
- `Cargo.lock` — unchanged by phase A (no dependency changes).
- No version bump, tag, or release is part of this change.
