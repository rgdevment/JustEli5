# Security policy

JustEli5 records meetings: material that is often confidential. It is in
development and not ready for use, so no version is supported yet; reports are
still welcome and read.

## Reporting a vulnerability

**Do not open a public issue.** Use either private channel:

- **Email** <github@apirest.cl>, subject `[SECURITY] short description`.
- **GitHub Security Advisory** — the *Security* tab, *Report a vulnerability*.

Useful in a report: what an attacker could do and to whom, steps to reproduce,
the version or commit, and your operating system. A proof of concept helps but
is not required.

Good-faith security research will never be met with legal threats.

## What the design protects

- **Recordings stay on your disk.** There is no service of ours to breach,
  because we run none. What goes over the network is listed in
  [PRIVACY.md](PRIVACY.md).
- **Meeting text is data, never instructions.** What people say in a meeting
  reaches the AI marked as untrusted, so a sentence spoken in a call cannot
  steer what the agent does.
- **Nothing is published without you.** Minutes reach Tisty only after you
  confirm them.
- **Updates are signed.** An update is checked against a public key compiled
  into the copy you already have, and refused if the signature does not match.

## What it does not protect

- **Your data folder is not encrypted.** Its protection is your operating
  system's account. Anyone who can read your files can read your meetings.
- **The AI agent you choose sees the transcript.** Pick a local model (Ollama,
  LM Studio) if the meeting must not leave your computer.
- **A raw diagnostic dump keeps real text.** `eli5 doctor --dump --raw` is for
  your own eyes; the default dump is anonymised.
