# Privacy policy

**Last updated:** October 23, 2026

> [!WARNING]
> JustEli5 is in development and not ready for use. This page describes what
> the code does today; it will grow as the project does, and never in the
> direction of collecting more.

## The short version

JustEli5 has no account, no telemetry and no server of ours. Your meetings are
captured and written on your own computer. The only thing that leaves it is the
transcript you send to the AI agent you chose, and with a local model nothing
leaves at all.

## What is kept, and where

- **Captions, audio, transcripts, notes and minutes** are written to your
  user's data folder, before any AI touches them. The JSONL files are the
  source of truth; the search index can always be rebuilt from them.
- **Audio is pruned when you ask.** `eli5 prune --audio` deletes old recordings
  once they are transcribed; captions, transcripts and minutes stay.
- **Your settings** — your name, your role, the agent you chose and your
  meeting app — stay in the same folder.

## What it asks before it captures

Capturing captions and recording audio are asked for separately, every
meeting. On macOS the system also asks for Accessibility (to read the captions)
and for Screen & System Audio Recording (to record the meeting audio).

## What goes over the network

- **The AI agent you configured.** The transcript of a meeting goes to it so it
  can write the minutes: Claude Code, GitHub Copilot CLI or the Anthropic API
  send it to their provider under that provider's terms; Ollama and LM Studio
  run on your computer and nothing leaves. Meeting text always reaches the model
  marked as untrusted data, never as instructions.
- **Speech recognition is local.** Whisper runs on your machine and the audio
  never leaves it. Turning it on downloads its models once, from Hugging Face.
- **Updates.** JustEli5 reads a small version file from this project on GitHub
  to see whether a newer version exists. It sends nothing. The new version is
  downloaded and installed only when you press the button that offers it.
- **Tisty.** Minutes go to Tisty only after you confirm, through the Tisty app
  on your own computer.

Nothing else: no analytics, no crash reporting, no cookies.

## Contact

<github@apirest.cl>
