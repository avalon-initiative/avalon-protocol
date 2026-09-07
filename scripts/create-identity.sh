#!/usr/bin/env bash
# `make create-identity` — prompts for username/password (and optionally a
# server), then registers via POST /identities. Prints the equivalent curl
# command first so you can copy/paste and run it yourself next time, or
# adjust it (different server, scripting it, etc.).
set -euo pipefail

read -rp "Username: " username
read -rsp "Password: " password
echo
read -rp "Server [localhost:8080]: " server
server="${server:-localhost:8080}"

if command -v jq >/dev/null 2>&1; then
    body=$(jq -nc --arg u "$username" --arg p "$password" '{username: $u, password: $p}')
else
    echo "warning: jq not found — falling back to naive JSON, special characters in username/password may break it" >&2
    body="{\"username\":\"$username\",\"password\":\"$password\"}"
fi

echo
echo "Equivalent curl command:"
echo "curl -s -X POST http://$server/identities -H \"Content-Type: application/json\" -d '$body'"
echo

curl -s -X POST "http://$server/identities" -H "Content-Type: application/json" -d "$body"
echo
