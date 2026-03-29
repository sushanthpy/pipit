#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ORIGINAL_CWD="$(pwd)"

provider="${PIPIT_PROVIDER:-}"
base_url="${PIPIT_BASE_URL:-}"
api_key=""
has_provider=0
has_base_url=0
has_api_key=0
has_root=0

args=("$@")
index=0
while [[ $index -lt ${#args[@]} ]]; do
	arg="${args[$index]}"
	case "$arg" in
		-p|--provider)
			if [[ $((index + 1)) -lt ${#args[@]} ]]; then
				provider="${args[$((index + 1))]}"
				has_provider=1
			fi
			index=$((index + 2))
			;;
		--provider=*)
			provider="${arg#*=}"
			has_provider=1
			index=$((index + 1))
			;;
		--base-url)
			if [[ $((index + 1)) -lt ${#args[@]} ]]; then
				base_url="${args[$((index + 1))]}"
				has_base_url=1
			fi
			index=$((index + 2))
			;;
		--base-url=*)
			base_url="${arg#*=}"
			has_base_url=1
			index=$((index + 1))
			;;
		--api-key)
			if [[ $((index + 1)) -lt ${#args[@]} ]]; then
				api_key="${args[$((index + 1))]}"
				has_api_key=1
			fi
			index=$((index + 2))
			;;
		--api-key=*)
			api_key="${arg#*=}"
			has_api_key=1
			index=$((index + 1))
			;;
		--root|--root=*)
			has_root=1
			index=$((index + 1))
			;;
		*)
			index=$((index + 1))
			;;
	esac
done

launch_args=("$@")

if [[ -n "$base_url" && $has_base_url -eq 0 ]]; then
	launch_args+=(--base-url "$base_url")
fi

if [[ -n "$base_url" && -z "$provider" ]]; then
	provider="openai"
fi

if [[ -n "$provider" && $has_provider -eq 0 ]]; then
	launch_args=(--provider "$provider" "${launch_args[@]}")
fi

if [[ $has_api_key -eq 0 ]]; then
	if [[ -n "$base_url" ]]; then
		if [[ -z "${OPENAI_API_KEY:-}" ]]; then
			launch_args=(--api-key dummy "${launch_args[@]}")
		fi
	elif [[ -z "${ANTHROPIC_API_KEY:-}" && -z "${OPENAI_API_KEY:-}" && -z "${PIPIT_API_KEY:-}" ]]; then
		cat <<'EOF' >&2
No API key configured for Pipit.

Hosted provider examples:
  bash scripts/launch-pipit.sh --provider anthropic --api-key "$ANTHROPIC_API_KEY"
  bash scripts/launch-pipit.sh --provider openai --api-key "$OPENAI_API_KEY"

Local OpenAI-compatible example:
  PIPIT_BASE_URL=http://localhost:8000 bash scripts/launch-pipit.sh --provider openai --model grok-4-1-fast-non-reasoning
EOF
		exit 1
	fi
fi

cd "$REPO_ROOT"
if [[ $has_root -eq 0 ]]; then
	launch_args=(--root "$ORIGINAL_CWD" "${launch_args[@]}")
fi
exec cargo run -p pipit-cli --bin pipit -- "${launch_args[@]}"
