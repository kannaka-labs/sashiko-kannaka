#!/bin/bash
# Kannaka Labs addition: let Sashiko's claude-cli provider, running in WSL,
# use the signed-in Claude Code CLI on the Windows side.
#
# Installs /usr/local/bin/claude, which execs the Windows claude.exe with the
# arguments, stdin and exit code passed through. It also appends the isolation
# flags (no tools, no MCP servers, no settings sources), so a stock Sashiko
# binary run through this bridge is isolated the same way this fork is.
#
# Usage (as root in WSL):  scripts/kannaka/install-wsl-claude-bridge.sh [path-to-claude.exe]
set -euo pipefail

EXE="${1:-}"
if [ -z "$EXE" ]; then
  WINUSER="$(cmd.exe /c "echo %USERNAME%" 2>/dev/null | tr -d '\r')"
  EXE="/mnt/c/Users/${WINUSER}/AppData/Roaming/npm/node_modules/@anthropic-ai/claude-code/bin/claude.exe"
fi
[ -x "$EXE" ] || { echo "claude.exe not found or not executable: $EXE" >&2; exit 1; }

cat > /usr/local/bin/claude <<EOF
#!/bin/bash
exec "$EXE" "\$@" --tools "" --strict-mcp-config --setting-sources ""
EOF
chmod 755 /usr/local/bin/claude

echo "installed /usr/local/bin/claude -> $EXE"
cd "$HOME"
echo 'Reply with the single word OK.' | claude --print --output-format json --no-session-persistence --model haiku \
  | python3 -c 'import sys, json; d = json.load(sys.stdin); print("bridge check:", d.get("result"))'
