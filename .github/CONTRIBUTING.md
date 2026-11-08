# Contributing to kemana-proxy

Thank you for contributing. Keep changes focused, tested and easy to review.

## Before you start

1. Search existing issues and pull requests.
2. For a new feature or an architectural change, open an issue first so the approach can be agreed.
3. Run `mise install`. It installs every pinned tool, including the Rust toolchain, and enables the git hooks (`pre-commit`, `commit-msg` and `pre-push`). Hooks need mise's experimental flag, which `mise.toml` already sets.

## Branch flow

```text
feature/*, fix/*  →  develop  →  release/*  →  main
hotfix/*          →  main
```

| Branch | Created from | Merged into | Merge method |
|---|---|---|---|
| `feature/*` | `develop` | `develop` | Squash |
| `fix/*` | `develop` | `develop` | Squash |
| `release/*` | `develop` | `main`, then back into `develop` | Merge commit into `main` |
| `hotfix/*` | `main` | `main`, then back into `develop` | Merge commit into `main` |

`develop` and `main` are protected: changes go through pull requests, and force pushes and deletions are blocked. Merged branches are deleted automatically.

## Releases

[release-please](https://github.com/googleapis/release-please) keeps a release pull request open against `develop`. It reads the Conventional Commit headers merged since the last release, picks the next version of the `kemana-proxy` crate (while the version is below 1.0, a breaking change bumps the minor version and a `feat` also bumps the minor), and updates `Cargo.toml`, `Cargo.lock` and `CHANGELOG.md`.

- Its title is `chore(release): release <version>`, which is a valid commit header, so it squashes into `develop` like any other pull request.
- Merging it tags `v<version>` and creates the GitHub release.
- A GitHub App token opens the pull request, so the required `rust` check runs on it. The workflow needs the repository secrets `RELEASE_APP_ID` and `RELEASE_APP_PRIVATE_KEY`.
- Do not edit the crate version or `CHANGELOG.md` by hand.

## Commit messages

Commits follow [Conventional Commits](https://www.conventionalcommits.org) and are checked by the `commit-msg` hook:

```text
type(scope): summary
```

- **Types:** `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `ci`, `chore`, `revert`. Add `!` after the scope for a breaking change.
- **Scope:** a Cargo package or workspace member name from `Cargo.toml` such as `kemana-proxy`, or one of the areas `build`, `ci`, `deps`, `tooling`, `docs`, `release`. The scope is optional for `ci` and `docs`.
- **Summary:** imperative, starts with a lowercase letter, no trailing period. The whole header is at most 72 characters.

Examples:

```text
feat(kemana-proxy): add stop lookup
fix(kemana-proxy): keep the previous feed when a download fails
docs: describe the release flow
```

## Pull requests

- Use the pull request template.
- The pull request title must be a valid commit header: it becomes the squash commit on `develop`.
- One pull request is one complete change: logic, wiring and tests together. Keep unrelated refactors out.
- Resolve all review conversations before merging.

## Code style

- Rust is formatted by rustfmt and linted by clippy with warnings denied. Run `mise run format` to format manually.
- Prefer clear names and small functions over comments. Comment only on a non-obvious reason.
- Write tests first.

## Local verification

```sh
mise run check
mise run test
```

The `pre-push` hook runs both. On every pull request CI runs:

- **Rust**: rustfmt check, clippy, the script tests, and `cargo llvm-cov` with the report uploaded to Codecov
- **CodeQL** for Rust, and **Dependency review** for new dependencies

The `Rust` job must pass before a pull request can merge. A pull request that only changes Markdown files or `docs/` skips it; the skipped job still satisfies the required check.

## Secrets

Never commit tokens, keys or personal data. Anything private belongs in an untracked `.env` file or CI secrets.
