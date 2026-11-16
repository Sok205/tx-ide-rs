# Contributing

This file is the source of truth for how changes reach tx-ide: branches, pull requests, checks and
releases. The guide's [Releasing](docs-site/guide/src/releasing.md) page summarises it and links
back here.

## Branches

| Branch | Holds | Who writes to it |
|---|---|---|
| `main` | released code only; every commit on it is a release or a hotfix | merges from `develop` (releases) and hotfix PRs |
| `develop` | the integration branch, the default branch on GitHub | merged pull requests |
| `<type>/<short-description>` | one change | you |

Both `main` and `develop` are protected: changes land only through a pull request, direct pushes
are refused, and neither branch can be force-pushed or deleted. The `issue-link` check (below) is
required before a PR can merge.

## Making a change

1. **Open an issue first**, or pick an existing one. Every PR is tied to an issue so the reason
   for a change outlives the PR. Use the bug report or feature request form.
2. **Branch off `develop`** as `<type>/<short-description>`, where `<type>` is one of `feat`,
   `fix`, `docs`, `chore`, `refactor`, `test`:

   ```sh
   git fetch origin
   git switch -c fix/kill-plain-session origin/develop
   ```

3. **Commit** in small, atomic commits. Before pushing, run what CI and reviewers expect
   (see [Developing](docs-site/guide/src/developing.md)):

   ```sh
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   ```

4. **Open a pull request into `develop`.** The PR template asks for a summary, the linked issue and
   testing notes. The body must reference the issue:
   - `Closes #N` (or `Fixes #N`, `Resolves #N`) when the PR finishes the issue; GitHub closes it
     on merge.
   - `Refs #N` when the PR is one part of a larger issue that stays open.

### The `issue-link` check

The `issue-link` workflow runs on every PR into `develop` or `main` (on open, edit, reopen and new
pushes). It fails unless the PR body contains one of `close`, `closes`, `closed`, `fix`, `fixes`,
`fixed`, `resolve`, `resolves`, `resolved`, `ref` or `refs` (any case, optional `:`) followed by
`#N`, `Sok205/tx-ide-rs#N` or `https://github.com/Sok205/tx-ide-rs/issues/N`, and `N` is an existing
**issue** in this repository (a pull request number does not count). If it fails, edit the PR
description to add the line; the check reruns on the edit.

## Versioning

tx-ide follows [Semantic Versioning](https://semver.org): `MAJOR.MINOR.PATCH`. There is one
version for the whole workspace, `version` under `[workspace.package]` in the root `Cargo.toml`;
every crate inherits it with `version.workspace = true`.

- **PATCH** (`0.1.0` → `0.1.1`): bug fixes only, nothing a user must change.
- **MINOR** (`0.1.1` → `0.2.0`): new features that keep existing use working.
- **MAJOR** (`1.4.2` → `2.0.0`): a breaking change to the command line, the on-disk records,
  configuration, definitions or the plugin API.

**Before 1.0** the public surface is not yet stable. While the version is `0.y.z`, a breaking
change bumps **MINOR** (`0.1.4` → `0.2.0`), and everything else, features included, bumps
**PATCH**. `1.0.0` is the first release that promises the rules above.

## Releasing

A release is a merge of `develop` into `main`, tagged.

1. **Bump the version.** On a branch off `develop` (for example `chore/release-0.2.0`), change
   `version` under `[workspace.package]` in the root `Cargo.toml`, run `cargo check --workspace`
   so `Cargo.lock` follows, and open a PR into `develop` that `Refs` the release issue.
2. **Open a PR from `develop` into `main`** and merge it with a **merge commit** (not squash or
   rebase), so `main`'s history shows each release as one merge.
3. **Tag the merge commit on `main`** with an annotated tag and push it:

   ```sh
   git fetch origin
   git switch main && git pull --ff-only
   git tag -a v0.2.0 -m "tx-ide 0.2.0"
   git push origin v0.2.0
   ```

   Tags are `vMAJOR.MINOR.PATCH` and match the `Cargo.toml` version.

The docs site (`.github/workflows/docs-site.yml`) builds and deploys on every push to `main`, so
it is republished with each release, not with each merge into `develop`.

### Hotfixes

A fix that cannot wait for the next release:

1. Branch off `main` as `fix/<short-description>`, fix it and bump **PATCH** in the root
   `Cargo.toml`.
2. Open a PR into `main` (with `Closes #N`) and merge it.
3. Tag the result on `main` as above (`v0.2.1`).
4. Merge `main` back into `develop` through a PR, so `develop` has the fix and the new version.
