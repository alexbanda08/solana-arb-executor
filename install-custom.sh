#!/usr/bin/env bash
# install-custom.sh - Interactive installer for solana-arb-executor.
# Lets you choose install scope (personal global, project-local, custom path)
# and which components (skill files, agents, commands, rules) to install.
set -euo pipefail

SKILL_NAME="solana-arb-executor"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
print_header() {
  echo ""
  echo "================================================"
  echo "  ${SKILL_NAME} - Custom Installer"
  echo "================================================"
  echo ""
}

ask_choice() {
  local prompt="$1"
  shift
  local options=("$@")
  echo "$prompt"
  local i=1
  for opt in "${options[@]}"; do
    echo "  $i) $opt"
    i=$((i + 1))
  done
  while true; do
    printf "Enter number [1-%d]: " "${#options[@]}"
    read -r choice
    if [[ "$choice" =~ ^[0-9]+$ ]] && [ "$choice" -ge 1 ] && [ "$choice" -le "${#options[@]}" ]; then
      CHOICE_RESULT="${options[$((choice - 1))]}"
      return 0
    fi
    echo "Invalid selection. Please enter a number between 1 and ${#options[@]}."
  done
}

ask_yes_no() {
  local prompt="$1"
  local default="${2:-n}"
  local hint
  if [ "$default" = "y" ]; then hint="[Y/n]"; else hint="[y/N]"; fi
  printf "%s %s " "$prompt" "$hint"
  read -r reply
  if [ -z "$reply" ]; then reply="$default"; fi
  case "$reply" in
    [Yy]*) return 0 ;;
    *) return 1 ;;
  esac
}

ask_path() {
  local prompt="$1"
  local default="$2"
  printf "%s [%s]: " "$prompt" "$default"
  read -r val
  if [ -z "$val" ]; then
    PATH_RESULT="$default"
  else
    PATH_RESULT="$val"
  fi
}

install_skill_files() {
  local dest="$1"
  echo "--> Installing skill files to ${dest} ..."
  rm -rf "${dest}"
  mkdir -p "${dest}"
  cp -r "${SCRIPT_DIR}/skill/." "${dest}/"
  echo "--> Skill files installed at ${dest}"
}

install_components() {
  local agents_dest="$1"
  local commands_dest="$2"
  local rules_dest="$3"
  local do_agents="$4"
  local do_commands="$5"
  local do_rules="$6"

  if [ "$do_agents" = true ]; then
    mkdir -p "$agents_dest"
    if [ -d "${SCRIPT_DIR}/agents" ] && [ "$(ls -A "${SCRIPT_DIR}/agents" 2>/dev/null)" ]; then
      cp "${SCRIPT_DIR}/agents/"*.md "$agents_dest/" 2>/dev/null || true
      echo "--> Agents installed at ${agents_dest}"
    fi
  fi

  if [ "$do_commands" = true ]; then
    mkdir -p "$commands_dest"
    if [ -d "${SCRIPT_DIR}/commands" ] && [ "$(ls -A "${SCRIPT_DIR}/commands" 2>/dev/null)" ]; then
      cp "${SCRIPT_DIR}/commands/"*.md "$commands_dest/" 2>/dev/null || true
      echo "--> Commands installed at ${commands_dest}"
    fi
  fi

  if [ "$do_rules" = true ]; then
    mkdir -p "$rules_dest"
    if [ -d "${SCRIPT_DIR}/rules" ] && [ "$(ls -A "${SCRIPT_DIR}/rules" 2>/dev/null)" ]; then
      cp "${SCRIPT_DIR}/rules/"*.md "$rules_dest/" 2>/dev/null || true
      echo "--> Rules installed at ${rules_dest}"
    fi
  fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
print_header

echo "This installer lets you choose WHERE and WHAT to install."
echo "Run bash install.sh --yes for a fully automated install."
echo ""

# --- Choose install scope ---
ask_choice "Where do you want to install the skill?" \
  "Personal global (~/.claude/skills/)" \
  "Project-local (./.claude/skills/ in the current directory)" \
  "Custom path (you specify)"

SCOPE="$CHOICE_RESULT"

case "$SCOPE" in
  "Personal global (~/.claude/skills/)")
    SKILL_DEST="${HOME}/.claude/skills/${SKILL_NAME}"
    AGENTS_DEST="${HOME}/.claude/agents"
    COMMANDS_DEST="${HOME}/.claude/commands"
    RULES_DEST="${HOME}/.claude/rules"
    ;;
  "Project-local (./.claude/skills/ in the current directory)")
    SKILL_DEST="${PWD}/.claude/skills/${SKILL_NAME}"
    AGENTS_DEST="${PWD}/.claude/agents"
    COMMANDS_DEST="${PWD}/.claude/commands"
    RULES_DEST="${PWD}/.claude/rules"
    ;;
  "Custom path (you specify)")
    ask_path "Base directory for skill files" "${HOME}/.claude/skills/${SKILL_NAME}"
    SKILL_DEST="$PATH_RESULT"
    ask_path "Agents directory" "${HOME}/.claude/agents"
    AGENTS_DEST="$PATH_RESULT"
    ask_path "Commands directory" "${HOME}/.claude/commands"
    COMMANDS_DEST="$PATH_RESULT"
    ask_path "Rules directory" "${HOME}/.claude/rules"
    RULES_DEST="$PATH_RESULT"
    ;;
esac

echo ""
echo "Skill will be installed to: ${SKILL_DEST}"
echo ""

# --- Choose components ---
echo "Which optional components do you want to install?"
echo ""

DO_AGENTS=false
DO_COMMANDS=false
DO_RULES=false

if ask_yes_no "Install agents (specialist agent definitions)?" "y"; then
  DO_AGENTS=true
fi

if ask_yes_no "Install commands (slash command definitions)?" "y"; then
  DO_COMMANDS=true
fi

if ask_yes_no "Install rules (auto-attached coding standards)?" "y"; then
  DO_RULES=true
fi

# --- Codex mirror ---
DO_CODEX=false
CODEX_DEST="${HOME}/.codex/skills/${SKILL_NAME}"
if [ -d "${HOME}/.codex" ]; then
  echo ""
  if ask_yes_no "Codex detected. Mirror skill to ~/.codex/skills/${SKILL_NAME}?" "y"; then
    DO_CODEX=true
  fi
fi

# --- Confirm ---
echo ""
echo "--- Installation plan ---"
echo "  Skill files : ${SKILL_DEST}"
[ "$DO_AGENTS"   = true ] && echo "  Agents      : ${AGENTS_DEST}"
[ "$DO_COMMANDS" = true ] && echo "  Commands    : ${COMMANDS_DEST}"
[ "$DO_RULES"    = true ] && echo "  Rules       : ${RULES_DEST}"
[ "$DO_CODEX"    = true ] && echo "  Codex mirror: ${CODEX_DEST}"
echo ""

if ! ask_yes_no "Proceed with installation?" "y"; then
  echo "Aborted by user."
  exit 0
fi

echo ""

# --- Execute ---
install_skill_files "${SKILL_DEST}"
install_components "${AGENTS_DEST}" "${COMMANDS_DEST}" "${RULES_DEST}" \
  "$DO_AGENTS" "$DO_COMMANDS" "$DO_RULES"

if [ "$DO_CODEX" = true ]; then
  echo "--> Mirroring skill to ${CODEX_DEST} ..."
  rm -rf "${CODEX_DEST}"
  mkdir -p "${CODEX_DEST}"
  cp -r "${SCRIPT_DIR}/skill/." "${CODEX_DEST}/"
  echo "--> Codex mirror installed."
fi

echo ""
echo "==> ${SKILL_NAME} installed successfully."
echo "    Restart Claude Code / Codex for changes to take effect."
