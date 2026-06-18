#!/usr/bin/env bash
# install.sh - Install solana-arb-executor skill into Claude Code / Codex skill directories.
# Usage: bash install.sh [-y|--yes] [-h|--help]
set -euo pipefail

SKILL_NAME="solana-arb-executor"
SKILLS_DIR="${HOME}/.claude/skills"
SOLANA_DEV_SKILL_DIR="${SKILLS_DIR}/solana-dev"
SOLANA_DEV_REPO="https://github.com/solana-foundation/solana-dev-skill"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

YES=false

usage() {
  cat <<EOF
Usage: bash install.sh [-y|--yes] [-h|--help]

Install the ${SKILL_NAME} skill into ~/.claude/skills/ and optionally
mirror to ~/.codex/skills/.

Options:
  -y, --yes    Non-interactive; skip all confirmation prompts.
  -h, --help   Show this help and exit.
EOF
}

for arg in "$@"; do
  case "$arg" in
    -y|--yes) YES=true ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $arg" >&2; usage; exit 1 ;;
  esac
done

confirm() {
  local prompt="$1"
  if [ "$YES" = true ]; then
    return 0
  fi
  printf "%s [y/N] " "$prompt"
  read -r reply
  case "$reply" in
    [Yy]*) return 0 ;;
    *) return 1 ;;
  esac
}

echo "==> Installing ${SKILL_NAME}"

# ---------------------------------------------------------------------------
# Step 1: Ensure solana-dev core skill is present (dependency for delegation).
# ---------------------------------------------------------------------------
if [ ! -f "${SOLANA_DEV_SKILL_DIR}/SKILL.md" ]; then
  echo "--> solana-dev skill not found at ${SOLANA_DEV_SKILL_DIR}"
  echo "--> Fetching solana-dev skill from ${SOLANA_DEV_REPO} ..."
  TMP_CLONE="$(mktemp -d)"
  trap 'rm -rf "$TMP_CLONE"' EXIT
  git clone --depth 1 "${SOLANA_DEV_REPO}" "${TMP_CLONE}/solana-dev-skill"
  mkdir -p "${SOLANA_DEV_SKILL_DIR}"
  cp -r "${TMP_CLONE}/solana-dev-skill/skill/." "${SOLANA_DEV_SKILL_DIR}/"
  echo "--> solana-dev skill installed at ${SOLANA_DEV_SKILL_DIR}"
else
  echo "--> solana-dev skill already present at ${SOLANA_DEV_SKILL_DIR} (skipping)"
fi

# ---------------------------------------------------------------------------
# Step 2: Install this skill into ~/.claude/skills/<name>/ (idempotent).
# ---------------------------------------------------------------------------
DEST_SKILL_DIR="${SKILLS_DIR}/${SKILL_NAME}"
echo "--> Installing skill files to ${DEST_SKILL_DIR} ..."
rm -rf "${DEST_SKILL_DIR}"
mkdir -p "${DEST_SKILL_DIR}"
cp -r "${SCRIPT_DIR}/skill/." "${DEST_SKILL_DIR}/"
echo "--> Skill files installed."

# ---------------------------------------------------------------------------
# Step 3: Install agents, commands, and rules into ~/.claude/.
# ---------------------------------------------------------------------------
echo "--> Installing agents, commands, and rules ..."
mkdir -p "${HOME}/.claude/agents"
mkdir -p "${HOME}/.claude/commands"
mkdir -p "${HOME}/.claude/rules"

if [ -d "${SCRIPT_DIR}/agents" ] && [ "$(ls -A "${SCRIPT_DIR}/agents" 2>/dev/null)" ]; then
  cp "${SCRIPT_DIR}/agents/"*.md "${HOME}/.claude/agents/" 2>/dev/null || true
fi

if [ -d "${SCRIPT_DIR}/commands" ] && [ "$(ls -A "${SCRIPT_DIR}/commands" 2>/dev/null)" ]; then
  cp "${SCRIPT_DIR}/commands/"*.md "${HOME}/.claude/commands/" 2>/dev/null || true
fi

if [ -d "${SCRIPT_DIR}/rules" ] && [ "$(ls -A "${SCRIPT_DIR}/rules" 2>/dev/null)" ]; then
  cp "${SCRIPT_DIR}/rules/"*.md "${HOME}/.claude/rules/" 2>/dev/null || true
fi

echo "--> Agents, commands, and rules installed."

# ---------------------------------------------------------------------------
# Step 4: Mirror to ~/.codex/skills/<name>/ if Codex is present.
# ---------------------------------------------------------------------------
if [ -d "${HOME}/.codex" ]; then
  CODEX_DEST="${HOME}/.codex/skills/${SKILL_NAME}"
  if confirm "Codex directory detected at ~/.codex. Mirror skill there?"; then
    echo "--> Mirroring to ${CODEX_DEST} ..."
    rm -rf "${CODEX_DEST}"
    mkdir -p "${CODEX_DEST}"
    cp -r "${SCRIPT_DIR}/skill/." "${CODEX_DEST}/"
    echo "--> Codex mirror installed at ${CODEX_DEST}"
  else
    echo "--> Skipping Codex mirror."
  fi
fi

# ---------------------------------------------------------------------------
# Done.
# ---------------------------------------------------------------------------
echo ""
echo "==> ${SKILL_NAME} installed successfully."
echo "    Skill dir : ${DEST_SKILL_DIR}"
echo "    Agents    : ~/.claude/agents/"
echo "    Commands  : ~/.claude/commands/"
echo "    Rules     : ~/.claude/rules/"
echo ""
echo "    Restart Claude Code / Codex for changes to take effect."
echo "    Invoke the skill by asking Claude about Solana arbitrage execution,"
echo "    Jito bundles, or tx landing."
