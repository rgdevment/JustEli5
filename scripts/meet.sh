#!/usr/bin/env bash
# Records a meeting with captions, audio, live ASR and the analyst, then transcribes, fuses and writes the minutes.
# Usage: scripts/meet.sh [--source meet|teams-web|teams|zoom|slack] [--name "Daily"] [--build] [--tisty] | --config
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
built="$repo/target/release/eli5"
support="$HOME/Library/Application Support/justeli5"
[[ "$(uname)" == Darwin ]] || support="${XDG_CONFIG_HOME:-$HOME/.config}/justeli5"
config_file="$support/config.json"

source=meet name="" build=0 tisty=0 config=0
while (($#)); do
  case "$1" in
    --source) source="$2"; shift ;;
    --name) name="$2"; shift ;;
    --build) build=1 ;;
    --tisty) tisty=1 ;;
    --config) config=1 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

if ((config)); then
  [[ -f "$config_file" ]] || { mkdir -p "$support"; cp "$repo/scripts/config.example.json" "$config_file"; }
  if [[ "$(uname)" == Darwin ]]; then open -t "$config_file"; else "${EDITOR:-vi}" "$config_file"; fi
  exit 0
fi

mc="$(command -v eli5 || true)"
if ((build)) || [[ -z "$mc" ]]; then
  cargo build --release -p mc-cli --features whisper --manifest-path "$repo/Cargo.toml"
  mc="$built"
fi

data="$HOME/Library/Application Support/justeli5"
[[ "$(uname)" == Darwin ]] || data="${XDG_DATA_HOME:-$HOME/.local/share}/justeli5"
stamp="$(mktemp)"
trap 'rm -f "$stamp"' EXIT

args=(tui --source "$source" --consent --audio --consent-audio --live-asr --analyst)
[[ -n "$name" ]] && args+=(--name "$name")
"$mc" "${args[@]}" || true

dir=""
while IFS= read -r d; do
  [[ -f "$d/manifest.json" && ( -z "$dir" || "$d" -nt "$dir" ) ]] && dir="$d"
done < <(find "$data/corpus" -mindepth 1 -maxdepth 1 -type d -newer "$stamp" 2>/dev/null)
if [[ -z "$dir" ]]; then
  echo "No meeting was recorded."
  exit 1
fi

"$mc" transcribe "$dir"
"$mc" fuse "$dir"
"$mc" minutes "$dir"
"$mc" export "$dir" --format md --save
((tisty)) && "$mc" export "$dir" --tisty
echo "Session: $dir"
