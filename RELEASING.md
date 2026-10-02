# Releasing tuxctl

A release is a merge of `dev` into `main`, an annotated tag on that merge, and
a published GitHub release. Publishing the release starts everything else:
binaries, checksums, attestations, crates.io and the documentation site.

Tags are immutable (a ruleset forbids moving or deleting `v*`), and crates.io
versions can only be yanked. A mistake after tagging is fixed with a new patch
version, never by reusing a number.

## 1. Prepare on `dev`

- [ ] Every workflow on `dev` is green (CI, Security, Docs), and the tree is
      clean.
- [ ] `Cargo.toml`: bump `version`; `cargo check` updates `Cargo.lock`.
- [ ] `CHANGELOG.md`:
  - rename `[Unreleased]` to `[X.Y.Z] - YYYY-MM-DD`, with a short summary
    paragraph, and add a new empty `[Unreleased]` above it;
  - update the compare links at the bottom (`[Unreleased]` from `vX.Y.Z`, and
    a new `[X.Y.Z]` line).
- [ ] `docs/installation.md`: the `--tag vX.Y.Z` example.
- [ ] Performance-sensitive release: measure it against the previous release
      (see `docs/performance.md`) and update that page and the README.

## 2. Check locally

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo clippy --all-targets --all-features --target x86_64-unknown-linux-musl -- -D warnings
cargo test && cargo test --target x86_64-unknown-linux-musl
cargo +1.88 check --all-targets --locked
cargo build --release --locked && scripts/smoke.py target/release/tuxctl
cargo build --release --locked --features redraw-counter --target-dir target/counter
scripts/measure.py target/counter/release/tuxctl --check
scripts/measure.py target/counter/release/tuxctl --check -- --interval 250ms
cargo publish --dry-run --locked
mdbook build docs
```

Commit as `chore: prepare vX.Y.Z` and push `dev`; wait for its workflows.

## 3. Merge into `main`

- [ ] Open a pull request `dev` → `main`, titled
      `Release vX.Y.Z: <theme>`, with a summary, measurements and the checks
      run.
- [ ] Wait for the required checks: `fmt, clippy, test`, `MSRV (1.88)`,
      `pty smoke and redraw guards`, `cargo-deny`, `zizmor`, `mdBook`.
- [ ] Merge with a **merge commit** (`Release vX.Y.Z (#N)`).

## 4. Tag and publish

```sh
git fetch origin
git tag -a vX.Y.Z -m "tuxctl vX.Y.Z" origin/main
git push origin vX.Y.Z
gh release create vX.Y.Z --verify-tag --title "tuxctl vX.Y.Z" --notes-file notes.md
```

The notes follow earlier releases: a summary, New / Changed / Fixes / Under
the hood, performance, verification, and installation.

## 5. Verify what publishing started

- [ ] **Release workflow** (`release.yml`) succeeded: both binaries, checksums
      and upload, crates.io.
- [ ] **Binaries:** download one with the README commands;
      `sha256sum -c` passes and `tuxctl --version` prints the new version. The
      assets can take a minute or two to show up in `gh release view`.
- [ ] **Attestation:**
      `gh attestation verify tuxctl-x86_64-unknown-linux-musl.tar.gz --repo Seqat/tuxctl`.
- [ ] **crates.io:** `https://crates.io/api/v1/crates/tuxctl` reports the new
      `max_version`; `cargo install tuxctl --locked --root /tmp/tuxctl-check`
      builds and runs.
- [ ] **AUR:** update `tuxctl` and `tuxctl-bin` as described in
      `packaging/aur/README.md`, and commit the new PKGBUILDs on `dev`.
- [ ] **Documentation** (`docs.yml`) deployed; the site describes the new
      release.

## 6. Bring `dev` level with `main`

```sh
git checkout dev
git merge --ff-only origin/main
git push origin dev
```

## When something fails

- **crates.io publish failed:** from a clean checkout of the tag, publish once
  by hand with a short-lived token (scope `publish-update`, crate `tuxctl`),
  then revoke it. Fix the workflow on `dev` for the next release.
- **A binary did not build:** fix it on `dev` and release a new patch version;
  do not move the tag.
- **Pages did not deploy:** run the Docs workflow by hand from `main`.

## Things that silently break releases

- **Renaming a CI job** listed above: `main` keeps waiting for the old check
  name. Update the `main` ruleset in the repository settings too.
- **Renaming `release.yml` or the `crates-io` environment:** crates.io Trusted
  Publishing is tied to both. Update the trusted publisher on crates.io
  first.
- **The `github-pages` environment** only deploys from `main`, `dev` and `v*`
  tags.
