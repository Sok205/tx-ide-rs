# Branches and releases

How a change reaches a release. This page is a summary; the rules themselves live in
[`CONTRIBUTING.md`](https://github.com/Sok205/tx-ide-rs/blob/main/CONTRIBUTING.md), and where the
two ever disagree, that file wins.

## Branches

- `main` holds released code only. Each release is a merge commit from `develop`, tagged
  `vMAJOR.MINOR.PATCH`.
- `develop` is the default branch and where pull requests land.
- Work happens on `<type>/<short-description>` branches off `develop` (`feat/…`, `fix/…`,
  `docs/…`, `chore/…`).

Both long-lived branches are protected: pull requests only, no direct or force pushes, no deletion.

## Pull requests

Every pull request starts from an issue and names it in its body: `Closes #N` when it finishes the
issue, `Refs #N` when it is one part of it. The required `issue-link` check fails a PR whose body
does not reference an existing issue of this repository.

## Versions

There is one version for the workspace, `version` under `[workspace.package]` in the root
`Cargo.toml`, which every crate inherits. It follows Semantic Versioning; before `1.0.0`, a breaking
change bumps the minor number and everything else the patch number.

## A release

1. A PR into `develop` bumps the workspace version.
2. A PR from `develop` into `main` is merged with a merge commit, once its `test` (the whole
   suite, on GitHub's macOS runner) and `issue-link` checks are green.
3. That commit is tagged and the tag pushed:
   `git tag -a vX.Y.Z -m "tx-ide X.Y.Z"` then `git push origin vX.Y.Z`.

A hotfix branches off `main`, merges back into `main` by PR, is tagged, and `main` is then merged
into `develop`. Its issue is closed by hand: GitHub auto-closes issues only for PRs
merged into the default branch, `develop`.

This site is built and deployed by `docs-site.yml` on every push to `main`, so what you are reading
is republished with each release.
