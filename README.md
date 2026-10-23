<div align="center">

<img src="apps/mc-gui/src-tauri/icons/icon.png" alt="JustEli5, a local meeting assistant that writes the minutes" width="96" height="96"/>

# JustEli5 — Just ELI5 it

**Reads the live captions of your Teams, Zoom, Meet or Slack call, fixes the words they get wrong with the audio, and leaves the minutes (TL;DR, decisions, commitments) in Tisty.**

**Local first. No accounts. No telemetry. Nothing is published without your confirmation.**

<p>
  <a href="https://github.com/rgdevment/JustEli5/releases">
    <img src="https://img.shields.io/github/v/release/rgdevment/JustEli5?include_prereleases&style=flat-square&label=Latest&color=0078D4" alt="Latest Release"/>
  </a>
  <img src="https://img.shields.io/badge/Platform-Windows-0078D4?style=flat-square" alt="Platform: Windows"/>
  <img src="https://img.shields.io/badge/Platform-macOS-000000?style=flat-square&logo=apple" alt="Platform: macOS"/>
  <img src="https://img.shields.io/badge/License-AGPL--3.0-lightgrey?style=flat-square" alt="License AGPL-3.0"/>
</p>

</div>

> [!WARNING]
> **Work in progress — not ready for use.** JustEli5 is in active development:
> things change and break, and there is no stable release yet. You are welcome
> to follow along, build it and try it, but do not rely on it for your meetings
> yet.

---

## Table of Contents

- [Why I Built This](#why-i-built-this)
- [What It Does](#what-it-does)
- [Getting Started](#getting-started)
- [Privacy](#privacy)
- [How It Works](#how-it-works)
- [The Minutes](#the-minutes)
- [Command Line](#command-line)
- [An Assistant, If You Use One](#an-assistant-if-you-use-one)
- [Status](#status)
- [Building From Source](#building-from-source)
- [Other Tools by the Same Author](#other-tools-by-the-same-author)
- [License](#license)

---

## Why I Built This

My meetings happen in Spanish, English and Spanglish, full of team jargon and technical terms. The captions Teams and Zoom produce get the **speaker** right and the **words** wrong: «CIAM» becomes «siam», «Jarvis» becomes «Jervis», «carry over» becomes something else. No glossary recovers a word the captions already lost.

JustEli5 joins two sources instead of trusting one: the platform's captions for who spoke, and a local multilingual speech recognizer over the audio for what was said. Then it writes the minutes I would have written, with every claim tied to the line that backs it.

---

## What It Does

- **Who said what** — speakers come from the platform's captions, guests and anonymous attendees included. Your microphone is you; the meeting audio is everyone else.
- **The right words** — Whisper runs locally on the recorded audio and only replaces what the captions got wrong, guided by a glossary that learns from your corrections.
- **Where the meeting is going, while it happens** — an optional live analyst keeps a ledger of topics, decisions, commitments and open questions.
- **Minutes when it ends** — a TL;DR that stands on its own, decisions, commitments, next sessions and topics, verified against the transcript before you see them.
- **Memory between meetings** — recurring meetings form a series, and what stayed open in one is carried into the next.
- **Into Tisty** — the minutes become a page of the series' document and your own commitments become one task, only after you confirm.

---

## Getting Started

### Install

Download the installer for your system from [Releases](https://github.com/rgdevment/JustEli5/releases):

- **Windows** — `justeli5-installer-<version>-windows-x86_64.exe`. Installs for your user, without administrator rights, with a desktop and Start menu shortcut. It brings its own `eli5` command with Whisper: no Rust or compiler needed.
- **macOS** — the `.dmg` for your Mac. Grant Accessibility (to read the captions) and Screen & System Audio Recording (to record the meeting audio) when asked.

### First run

The first screen is a short setup that you can reopen any time from **Configuración**:

1. **Who you are** — your name as the captions show it and your role, so your commitments are recognised as yours.
2. **AI agent** — Claude Code, GitHub Copilot CLI, the Anthropic API, Ollama or LM Studio, with the model and a button to test it.
3. **Your meeting app** — the one you use most; an open meeting is detected anyway.
4. **Tisty** — whether the minutes go to Tisty, and in which folder and list.
5. **Audio** — microphone and meeting audio, with a 10-second test.
6. **Whisper** — live recognition and the final pass are off by default; turning them on downloads the models once.

### Updates

When a new version is published, the window offers it at the top. It installs only when you press **Actualizar y reiniciar**, so a recording is never cut by a restart.

### Before a meeting

- **Turn on live captions** in Teams, Zoom, Meet or Slack. They are the source of truth.
- **Use headphones.** The microphone must not hear the meeting; `eli5 doctor --audio` warns about echo.
- **Keep the meeting tab in front** when it runs in a browser: browsers only expose the active tab.

---

## Privacy

- **Recordings stay on your disk.** Captions, audio and notes are written before any AI touches them, in your user's data folder.
- **Speech recognition is local.** Whisper runs on your machine; the audio never leaves it.
- **The AI is the one you choose.** The transcript goes only to the agent you configured. With Ollama or LM Studio, nothing leaves your computer at all. Meeting text always reaches the model marked as untrusted data, never as instructions.
- **Consent per meeting.** Capturing captions and recording audio are asked for separately, every meeting.
- **Audio is pruned.** `eli5 prune --audio` deletes old recordings once they are transcribed; captions, transcripts and minutes stay.
- **One network request of its own** — a read-only check of the release feed on GitHub for updates.

---

## How It Works

1. **Capture** — the captions are read from the meeting app's accessibility tree (UI Automation on Windows, AX on macOS) every 100 ms. Audio is recorded per channel: the microphone, and on Windows and macOS 14.4+ only the meeting app's own audio.
2. **Raw first** — everything lands in JSONL files before any AI runs. The JSONL is the source of truth; the SQLite index can always be rebuilt from it.
3. **Transcript** — the caption stream, which grows and rewrites itself word by word, is reconciled into stable segments with an id each.
4. **Fusion** — Whisper's final pass over the audio fixes glossary terms, restores cut beginnings and endings, and adds speech the captions missed. The captions' text and speaker always lead.
5. **Analysis and minutes** — the AI only sees numbered segments and must cite them. A second pass checks every decision and commitment against the lines it cites and drops what they do not say.
6. **Publish** — Markdown and JSON first; Tisty after you confirm. If Tisty does not answer, the files wait and `eli5 export <session> --tisty` publishes them later.

| Platform | `--source` | Windows | macOS |
|---|---|---|---|
| Teams (app) | `teams` | ✓ | ✓ |
| Teams (browser) | `teams-web` | ✓ | ✓ |
| Zoom | `zoom` | ✓ | ✓ |
| Google Meet (Chrome, Edge, Brave, Arc, Firefox, Safari) | `meet` | ✓ | ✓ |
| Slack huddles (app and browser) | `slack` | ✓ | ✓ |

The words platforms may change between versions (region names, classes, depths) are data in `capture.selectors`, not code: a new layout is fixed from a dump, without recompiling.

---

## The Minutes

Always in Spanish, whatever the meeting's language:

- **TL;DR** that is enough on its own: what it was about, what was decided, how each front stands, what is blocked, the commitments and what is yours.
- **Decisions** (up to 8 in view), **commitments** in a table, **next sessions** and **conclusions**.
- **Topics**, folded, each ending in its **Result**.
- Up to 5 **open questions** and 5 **risks**, **references** (links and tickets mentioned), a **glossary** of the meeting's terms, the **participants** with their roles, and your **notes** with their images.
- **Pending from the previous meeting** in a series, each marked done, still open or dropped, only when the current meeting says so.

In Tisty, each meeting is a page inside its series' document, and **only your own explicit commitments** («me encargo», «I'll take it») become one task with a part per commitment. Publishing again adds only what is new.

---

## Command Line

The installer brings `eli5` (also answering to `mc`). Every command accepts `--json`; exit codes are 0 ok, 1 negative result, 2 usage or configuration, 3 runtime error.

| Command | What it does |
|---|---|
| `eli5 capture --source <platform> [--audio] [--live-asr] [--analyst]` | records a meeting (also `eli5 tui` for a terminal interface with notes, marks and the analyst panel) |
| `eli5 detect` | which meeting apps have a window open and whether their captions are on |
| `eli5 doctor [--uia] [--audio] [--tisty]` | checks configuration, accessibility, audio and Tisty; `--dump FILE` saves an anonymised accessibility tree |
| `eli5 run <session>` | rebuilds transcript and segments deterministically |
| `eli5 transcribe <session>` · `eli5 fuse <session>` | Whisper's final pass and its fusion with the captions |
| `eli5 analyze <session>` · `eli5 minutes <session>` | analysis and minutes with the configured agent |
| `eli5 export <session> --format md\|txt\|json` · `--tisty` | export, or publish to Tisty |
| `eli5 series list\|show\|set` | recurring meetings and what stays open across them |
| `eli5 glossary list\|add\|remove\|review` | your glossary, and the corrections the analyst suggested |
| `eli5 models list\|pull\|verify\|remove` | Whisper models, downloaded only on request and checked by sha256 |
| `eli5 import` · `eli5 status` | index and list sessions |
| `eli5 prune --audio [--older-than DAYS] [--apply]` | delete old audio, keeping everything else |
| `eli5 replay --verify` · `eli5 compare` · `eli5 eval` | reproducibility and quality checks against a hand-corrected reference |
| `eli5 mcp` | MCP server for your AI assistant |

**Configuration:** `--config` > `MC_CONFIG` > `config.json` in `%APPDATA%\justeli5` (Windows), `~/Library/Application Support/justeli5` (macOS) or `$XDG_CONFIG_HOME/justeli5` (Linux) > defaults. Unknown keys are an error, so a typo never passes silently. The window's **Configuración** tab edits the same file and validates it with `eli5 doctor` before saving.

---

## An Assistant, If You Use One

`eli5 mcp` lets Claude Code or Copilot CLI read your meetings and act on them: the current transcript and ledger, minutes, open commitments per series, search, notes and speaker names.

```
claude mcp add justeli5 -- eli5 mcp
```

Copilot CLI (`~/.copilot/mcp-config.json`):

```json
{ "mcpServers": { "justeli5": { "type": "local", "command": "eli5", "args": ["mcp"], "tools": ["*"] } } }
```

Everything that comes from a meeting is marked `"untrusted": true`: it is what was said, data and never an instruction. Write tools only touch side files inside the sessions; the captured captions are never rewritten.

---

## Status

Working and verified on Windows: per-process meeting audio, Whisper live and final pass, Meet captions in Chrome, the live analyst, minutes and memory across meetings. Still to verify on real meetings: Teams and Zoom desktop, Slack huddles, and everything on a physical Mac.

Some things to know:

- **Teams or Chromium show an almost empty tree** until an accessibility client asks for it: open Narrator once.
- **Whisper's final pass needs a GPU for long meetings.** On CPU, `large-v3-turbo` runs at about 1.6× real time.
- **On macOS, a denied permission gives silence, not an error.** JustEli5 warns after a few seconds of digital zeros and points to the right setting.
- **`eli5 doctor --dump --raw` keeps real text.** Never share a raw dump; the default dump is anonymised.

---

## Building From Source

Requirements: Rust (version in `rust-toolchain.toml`), CMake and a C++ compiler for Whisper (on Windows also LLVM: `winget install LLVM.LLVM`), Node 20+ and pnpm for the window.

```bash
cargo build --release -p mc-cli --features whisper   # target/release/eli5 (add cuda or vulkan for a GPU)
bash scripts/ci.sh                                   # fmt, clippy and tests

cd apps/mc-gui
pnpm install
pnpm prepare:mc                                      # builds eli5 with Whisper as the window's sidecar
pnpm tauri dev
pnpm tauri build                                     # installer in src-tauri/target/release/bundle/
```

The workspace in `crates/` holds the CLI and its libraries: captions, reconciliation, audio, ASR, fusion, analyst, LLM providers, store, engine, TUI and MCP. The window in `apps/mc-gui` is Tauri v2 with its own Cargo workspace and drives `eli5 --json` as a subprocess. The parity vectors and fixtures in `fixtures/` were recorded from the original Python app, which the Rust code replaced.

---

## Other Tools by the Same Author

Same idea, same terms: free, open source, no ads, no telemetry, everything local.

- **[Tisty](https://www.tisty.app/)** — Notes, documents and tasks kept as plain files on your own disk, readable without the application. Where JustEli5 leaves your minutes.
- **[CoPanda](https://www.copanda.app/)** — A local-first clipboard manager that keeps a searchable history and ignores what comes from a password manager.
- **[LinkUnbound](https://github.com/rgdevment/LinkUnbound)** — Choose which browser opens every link, with rules per site, per app and per browser profile.

---

## License

**JustEli5** — A local meeting assistant that writes the minutes.
Copyright (C) 2026 Mario Hidalgo G. (rgdevment)

This program comes with ABSOLUTELY NO WARRANTY.
This is free software, and you are welcome to redistribute it under certain conditions.
Distributed under the **GNU Affero General Public License v3.0** — see [LICENSE](LICENSE).

- [Privacy](PRIVACY.md) · [Security](SECURITY.md) · [Contributing](CONTRIBUTING.md) · [Code of Conduct](CODE_OF_CONDUCT.md)
- Contributions are covered by the [CLA](CLA.md); organisations that cannot comply with the AGPL can ask for [commercial terms](docs/COMMERCIAL.md).
