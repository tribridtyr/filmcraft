#!/usr/bin/env bash
# mcp-smoke.sh: a quick end-to-end check of the FilmCraft MCP server, run through the official
# MCP Inspector CLI (https://github.com/modelcontextprotocol/inspector).
#
#   bash scripts/mcp-smoke.sh                  # uses target/debug/filmcraft-cli (builds it if missing)
#   FILMCRAFT_CLI=/path/to/filmcraft-cli bash scripts/mcp-smoke.sh
#
# Needs Node >= 22.19 and jq. Every Inspector call starts a fresh `filmcraft-cli mcp --demo`
# (headless, demo project), so nothing here touches a real project. It checks, in order:
#   1. the MCP handshake works and the server sends its instructions
#   2. the tools an agent depends on exist, and every schema is free of portability errors
#   3. `sequence.verify` passes on the demo sequence
#   4. edit -> verify -> undo -> verify in one batch: still `ok`, same length, undo restores it
#   5. a bad command id is refused as a tool error (exit 5), not accepted and not a crash
# It stops at the first failed check and exits non-zero. CI pins the Inspector (see INSPECTOR_VERSION).

set -euo pipefail

INSPECTOR_VERSION="${INSPECTOR_VERSION:-2.11.0}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLI="${FILMCRAFT_CLI:-$ROOT/target/debug/filmcraft-cli}"

for tool in node npx jq; do
  command -v "$tool" > /dev/null || { echo "FAIL: $tool is required" >&2; exit 1; }
done
if [ ! -x "$CLI" ]; then
  echo "building filmcraft-cli ..."
  (cd "$ROOT" && cargo build -p filmcraft-cli)
fi

# A scratch store: the smoke run must never read or write real Inspector tokens or catalogs.
# MCP_INSPECTOR_OAUTH_STATE_PATH wins over MCP_STORAGE_DIR, so set both.
SCRATCH="$(mktemp -d)"
export MCP_STORAGE_DIR="$SCRATCH"
export MCP_INSPECTOR_OAUTH_STATE_PATH="$SCRATCH/oauth.json"
trap 'rm -rf "$SCRATCH"' EXIT

# Run one Inspector method against a fresh demo server. stdout is the JSON result, stderr goes to a
# file so a failure can show it. The caller captures the exit status itself (see `|| status=$?`).
inspect() {
  npx --yes "@modelcontextprotocol/inspector@$INSPECTOR_VERSION" --cli \
    "$CLI" mcp --demo \
    --connect-timeout 30000 --format json "$@" 2> "$SCRATCH/stderr"
}

# `call TOOL JSON` runs a tool, fails the script on any non-zero exit and prints the first text block.
call() {
  local status=0 out
  out="$(inspect --method tools/call --tool-name "$1" --tool-args-json "$2")" || status=$?
  if [ "$status" -ne 0 ]; then
    echo "FAIL: $1 exited $status" >&2
    tail -3 "$SCRATCH/stderr" >&2
    exit "$status"
  fi
  jq -r '.result.content[0].text' <<< "$out"
}

pass() { echo "ok: $1"; }
fail() { echo "FAIL: $1" >&2; exit 1; }

# 1. Handshake.
status=0
init="$(inspect --method initialize)" || status=$?
[ "$status" -eq 0 ] || { tail -3 "$SCRATCH/stderr" >&2; fail "handshake exited $status (is $CLI a current build?)"; }
jq -e '.result.serverInfo.name == "filmcraft"' > /dev/null <<< "$init" || fail "unexpected serverInfo"
jq -e '.result.instructions | contains("sequence.verify")' > /dev/null <<< "$init" \
  || fail "server instructions no longer tell agents to run sequence.verify"
pass "handshake (protocol $(jq -r '.result.protocolVersion' <<< "$init"))"

# 2. Tools exist and their schemas are portable (--strict exits 6 on error-severity findings).
status=0
tools="$(inspect --method tools/list --strict)" || status=$?
[ "$status" -eq 0 ] || { tail -5 "$SCRATCH/stderr" >&2; fail "tools/list --strict exited $status"; }
want='["command_list","command_run","command_batch","doc_inspect","sequence_inspect","render_frame","ui_map"]'
jq -e --argjson want "$want" '[.result.tools[].name] as $have | $want - $have | length == 0' > /dev/null <<< "$tools" \
  || fail "a required tool is missing (wanted $want)"
# Warnings do not fail --strict, but the schemas are clean today (nullable types and untyped fields
# are rewritten by `portable_schema` in crates/automation/src/server.rs), so keep them that way.
findings="$(jq '[.schemaFindings[]?.findings[]?] | length' <<< "$tools")"
[ "$findings" -eq 0 ] || fail "$findings schema portability findings: $(jq -c '[.schemaFindings[] | {toolName, rules: [.findings[].rule]}]' <<< "$tools") (add \`#[schemars(transform = portable_schema)]\` to the new params struct)"
pass "$(jq '.result.tools | length' <<< "$tools") tools, schemas clean"

# 3. The demo sequence verifies.
verify="$(call command_run '{"id":"sequence.verify","params":{}}')"
jq -e '.ok == true and .errors == 0 and .durationFrames > 0' > /dev/null <<< "$verify" \
  || { jq -c '{ok,errors,issues}' <<< "$verify" >&2; fail "demo sequence does not verify"; }
frames="$(jq '.durationFrames' <<< "$verify")"
pass "sequence.verify ok ($frames frames)"

# 4. Edit, verify, undo, verify, in one connection (the demo state lives only that long).
batch="$(call command_batch '{"steps":[
  {"id":"sequence.verify"},
  {"id":"sequence.addEdit","params":{"seconds":1.5}},
  {"id":"sequence.verify"},
  {"id":"edit.undo"},
  {"id":"sequence.verify"}]}')"
jq -e '.failed == 0 and .completed == 5' > /dev/null <<< "$batch" || fail "a batch step failed: $(jq -c '.results' <<< "$batch")"
jq -e --argjson n "$frames" '[.results[] | .result | select(type == "object" and has("durationFrames"))]
    | length == 3 and all(.ok == true and .errors == 0 and .durationFrames == $n)' > /dev/null <<< "$batch" \
  || fail "edit/undo changed the sequence length or broke verification"
pass "edit + undo keep the sequence ok and $frames frames long"

# 5. A bad command id is a refusal (exit 5, tool_is_error), not success and not a crash.
status=0
inspect --method tools/call --tool-name command_run --tool-args-json '{"id":"no.such.command"}' > /dev/null || status=$?
[ "$status" -eq 5 ] || fail "bad command id exited $status, expected 5"
pass "unknown command refused"

echo "mcp smoke OK"
