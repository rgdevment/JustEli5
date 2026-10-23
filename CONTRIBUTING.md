# Contributing to JustEli5

JustEli5 is in development and not ready for use. Issues and ideas are welcome;
for code, open an issue first so we can agree on the shape before you spend
time on it, since much is still moving.

## The CLA

JustEli5 is released under the AGPL-3.0 and offered under separate
[commercial terms](docs/COMMERCIAL.md). That model needs the right to license
every contribution the same way, so merging code requires signing the
[Contributor License Agreement](CLA.md). You keep the copyright on your work.
A bot asks for the signature on your first pull request; you sign once.

## Development setup

The requirements and the commands are in the README, under
[Building From Source](README.md#building-from-source). Before a pull request:

```bash
bash scripts/ci.sh   # fmt, clippy and tests
```

## Commits

One line, in the conventional format: `type(scope): what changes`, for
example `fix(captions): a speaker who leaves keeps their last line`.
`git config core.hooksPath hooks` checks each message as you commit.

## Conduct and security

Everyone here follows the [Code of Conduct](CODE_OF_CONDUCT.md). Security
problems go privately, as [SECURITY.md](SECURITY.md) explains.
