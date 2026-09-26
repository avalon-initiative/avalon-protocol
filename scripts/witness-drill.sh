#!/usr/bin/env bash
# Witness-cosigning scenario drill: grows a network from one real
# avalon-server process to several, removes nodes (including the original),
# drops and refills a known-list witness, floods a victim with same-prefix
# candidates, and reconnects a long-offline node — all against real
# processes and a real Postgres, never in-process function calls.
#
# usage: scripts/witness-drill.sh [scenario ...]     (default: all scenarios)
#   scenarios: lifecycle witness-loss eclipse long-offline fork rollout cosigned
#
# See docs/projects/backend-server/for-maintainers/witness-drill.md for what
# each scenario proves, which ones run in CI, and how to repeat this as a
# live drill on the dev fleet.
#
# env: AVALON_ENV_FILE   .env to read DATABASE_URL and keys from
#      DRILL_PORT_BASE   first local port used (default 19700)
#      DRILL_ECLIPSE_FLOOD  candidate count for the eclipse scenario (default 6)
#      DRILL_KEEP_SCHEMAS=1    keep the test schemas after the run
#      DRILL_KEEP_DATA_DIRS=1  keep known-list data dirs after the run

set -uo pipefail

command -v jq >/dev/null 2>&1 || { echo "witness-drill.sh needs jq" >&2; exit 2; }

HARNESS_LOG_PREFIX=avalon-witness-drill
BASE_PORT="${DRILL_PORT_BASE:-19700}"
KEEP_SCHEMAS="${DRILL_KEEP_SCHEMAS:-}"
KEEP_DATA_DIRS="${DRILL_KEEP_DATA_DIRS:-}"
# shellcheck source=lib/harness.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib/harness.sh"

DATA_ROOT="$LOG_DIR/data"
mkdir -p "$DATA_ROOT"

PORT_OFFSET=0
next_port() { PORT_OFFSET=$((PORT_OFFSET + 1)); NEXT_PORT=$((BASE_PORT + PORT_OFFSET)); }

# Fast-forwarded known-list tuning: real defaults (10 min freshness, 30 min
# probation, 2 min refill) would make every scenario here take the better
# part of an hour. These env values only change *how fast* the same
# admission/refill/probation rules run, never the rules themselves.
FAST_KNOWN_LIST=(
  AVALON_KNOWN_LIST_REFILL_INTERVAL_SECS=2
  AVALON_KNOWN_LIST_FRESHNESS_SECS=6
  AVALON_KNOWN_LIST_PROBATION_SECS=3
  AVALON_ANNOUNCE_INTERVAL_SECS=2
  AVALON_MIRROR_POLL_INTERVAL_SECS=3
)

# node <name> <bootstrap-csv> [VAR=value ...]: starts one node with its own
# schema and known-list data dir, bootstrapped at the given peer URLs (may
# be empty). Sets LAST_PID / LAST_PORT / LAST_DATA_DIR.
node() {
  local name="$1" bootstrap="$2"
  shift 2
  local port schema="live_drill_${name//-/_}" data_dir="$DATA_ROOT/$name"
  next_port; port="$NEXT_PORT"
  new_schema "$schema" || return 1
  mkdir -p "$data_dir"
  # Every node gets its own witness key: a known list only admits peers that
  # prove one, so a keyless node could never be a slot. Callers may override.
  LAST_SEED="$(openssl rand -hex 32)"
  LAST_KEY_ID="$( (printf '\x30\x2e\x02\x01\x00\x30\x05\x06\x03\x2b\x65\x70\x04\x22\x04\x20'; echo "$LAST_SEED" | xxd -r -p) \
    | openssl pkey -inform DER -pubout -outform DER | tail -c 32 | xxd -p -c 64)"
  start_node "$name" "$schema" "$port" "${FAST_KNOWN_LIST[@]}" \
    AVALON_WITNESS_SIGNING_KEY="$LAST_SEED" \
    AVALON_DATA_DIR="$data_dir" AVALON_BOOTSTRAP_PEERS="$bootstrap" \
    AVALON_ALLOW_PRIVATE_PEERS=true AVALON_ANNOUNCE_VERIFY_REACHABILITY=false "$@" || return 1
  LAST_PORT="$port"
  LAST_DATA_DIR="$data_dir"
}

known_list_file() { echo "$1/known_list.json"; }

known_list_size() {
  local f; f="$(known_list_file "$1")"
  [ -f "$f" ] && jq '.slots | length' "$f" 2>/dev/null || echo 0
}

known_list_ids_sorted() {
  local f; f="$(known_list_file "$1")"
  [ -f "$f" ] && jq -r '.slots[].witness_key_id' "$f" 2>/dev/null | sort || true
}

known_list_prefix_count() {
  local f; f="$(known_list_file "$1")"
  [ -f "$f" ] && jq --arg p "$2" '[.slots[] | select(.prefix == $p)] | length' "$f" 2>/dev/null || echo 0
}

known_list_first_prefix() {
  local f; f="$(known_list_file "$1")"
  [ -f "$f" ] && jq -r '.slots[0].prefix // empty' "$f" 2>/dev/null || true
}

peer_count() { curl -sf "http://127.0.0.1:$1/nodes/peers" 2>/dev/null | jq 'length' 2>/dev/null || echo 0; }

alive() { curl -sf "http://127.0.0.1:$1/nodes/status" >/dev/null 2>&1; }


# register_integrator <port> <slug>: writes one ledger-backed event through a node; prints the HTTP status.
register_integrator() {
  curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$1/integrations" \
    -H 'content-type: application/json' \
    -d "{\"slug\":\"$2\",\"name\":\"Drill\",\"owner_name\":\"Drill\",\"requested_capabilities\":[],\"initial_key\":{\"algorithm\":\"ed25519\",\"public_key\":\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"}}"
}
is_2xx() { [[ "$1" == 2* ]]; }

# --- predicates used by wait_until / check, each a plain shell function
# taking the same args the scenario passes it, exit 0 = condition met.
known_list_has_at_least() { [ "$(known_list_size "$1")" -ge "$2" ]; }
peer_table_has_at_least() { [ "$(peer_count "$1")" -ge "$2" ]; }
proc_gone() { ! kill -0 "$1" 2>/dev/null; }
known_list_contains() { known_list_ids_sorted "$1" | grep -qx "$2"; }
known_list_lacks() { ! known_list_contains "$1" "$2"; }
# ids_only_removed <data-dir> <ids-before> <removed-id>: the list now holds
# exactly the ids it held before, minus <removed-id> (plus nothing that was
# not there), i.e. the pruned slot is the one named and no other.
ids_only_removed() {
  [ "$(known_list_ids_sorted "$1")" = "$(printf '%s\n' "$2" | grep -vx "$3")" ]
}
known_list_ids_differ_from() { [ "$(known_list_ids_sorted "$1")" != "$2" ]; }
log_contains() { grep -q "$2" "$1" 2>/dev/null; }
log_lacks() { ! grep -q "$2" "$1" 2>/dev/null; }
prefix_count_at_least() { [ "$(known_list_prefix_count "$1" "$2")" -ge "$3" ]; }
prefix_count_at_most() { [ "$(known_list_prefix_count "$1" "$2")" -le "$3" ]; }

# wait_until <description> <timeout-secs> <predicate-fn> [args...]: polls
# a predicate function every second until it exits 0 or the timeout
# elapses. Records PASS/FAIL into the shared RESULTS array (declared by
# lib/harness.sh).
wait_until() {
  local desc="$1" timeout="$2"
  shift 2
  local waited=0
  while [ "$waited" -lt "$timeout" ]; do
    if "$@"; then
      RESULTS+=("PASS  $desc")
      return 0
    fi
    sleep 1
    waited=$((waited + 1))
  done
  RESULTS+=("FAIL  $desc (timed out after ${timeout}s)")
  FAILED=1
  return 1
}

check() {
  local desc="$1"
  shift
  if "$@"; then
    RESULTS+=("PASS  $desc")
  else
    RESULTS+=("FAIL  $desc")
    FAILED=1
  fi
}

# ---------------------------------------------------------------------------
# Scenario: growth from one node to several, then loss of the original node
# (including A itself), then continued growth without it. Exactly the
# ticket's A -> A,B,C -> B,C -> B,C,D,E sequence. Runs in CI (trimmed).
# ---------------------------------------------------------------------------
scenario_lifecycle() {
  node a "" AVALON_KNOWN_LIST_MAX_PER_PREFIX=10 || return 1
  local port_a="$LAST_PORT" data_a="$LAST_DATA_DIR" pid_a="$LAST_PID" key_a="$LAST_KEY_ID"

  # A alone: single-signer-equivalent, known list empty or self only.
  check "A alone answers /nodes/status" alive "$port_a"

  node b "http://127.0.0.1:$port_a" AVALON_KNOWN_LIST_MAX_PER_PREFIX=10 || return 1
  local port_b="$LAST_PORT" data_b="$LAST_DATA_DIR"
  node c "http://127.0.0.1:$port_a" AVALON_KNOWN_LIST_MAX_PER_PREFIX=10 || return 1
  local port_c="$LAST_PORT" key_c="$LAST_KEY_ID"

  # B and C join and get admitted to A's known list, with no restart of A.
  wait_until "A's known list admits B and C without a restart" 30 \
    known_list_has_at_least "$data_a" 2
  wait_until "A's own peer table lists both B and C" 30 \
    peer_table_has_at_least "$port_a" 2
  check "A is still the same process throughout (no restart)" kill -0 "$pid_a"

  # Remove A, including its own process, and confirm B and C still verify
  # and author with no special-casing for "node A was first."
  wait_until "B's known list holds A's witness key before A leaves" 30 \
    known_list_contains "$data_b" "$key_a"
  wait_until "B's known list holds C's witness key before A leaves" 30 \
    known_list_contains "$data_b" "$key_c"
  local before_b; before_b="$(known_list_ids_sorted "$data_b")"
  kill "$pid_a" 2>/dev/null
  wait_until "A's process actually exited" 15 proc_gone "$pid_a"
  check "B still answers after A is gone" alive "$port_b"
  check "C still answers after A is gone" alive "$port_c"
  wait_until "A's slot (its witness key) is pruned from B's known list past the freshness window" 40 \
    known_list_lacks "$data_b" "$key_a"
  check "only A's slot left B's list: every other slot it held before is still there" \
    ids_only_removed "$data_b" "$before_b" "$key_a"
  check "C's slot in B's list is untouched by A's departure" known_list_contains "$data_b" "$key_c"

  # Growth continues from B and C without A — D and E join and get
  # admitted, proving nothing distinguishes "the original node" from any
  # other witness leaving or a later one joining.
  node d "http://127.0.0.1:$port_b" AVALON_KNOWN_LIST_MAX_PER_PREFIX=10 || return 1
  local port_d="$LAST_PORT"
  node e "http://127.0.0.1:$port_c" AVALON_KNOWN_LIST_MAX_PER_PREFIX=10 || return 1
  local port_e="$LAST_PORT"

  check "D answers" alive "$port_d"
  check "E answers" alive "$port_e"
  wait_until "B's known list admits D and E post-A, same as it admitted B/C pre-A" 30 \
    known_list_has_at_least "$data_b" 3
  return 0
}

# ---------------------------------------------------------------------------
# Scenario: a known-list witness drops and the list detects staleness and
# refills automatically, with no manual intervention. Not run in CI (needs
# the fast freshness window to actually elapse, tens of seconds of real
# wall time on top of node startup).
# ---------------------------------------------------------------------------
scenario_witness_loss() {
  node a "" AVALON_KNOWN_LIST_MAX_PER_PREFIX=10 || return 1
  local port_a="$LAST_PORT" data_a="$LAST_DATA_DIR"
  node b "http://127.0.0.1:$port_a" AVALON_KNOWN_LIST_MAX_PER_PREFIX=10 || return 1
  local port_b="$LAST_PORT" pid_b="$LAST_PID" key_b="$LAST_KEY_ID"
  node c "http://127.0.0.1:$port_a" AVALON_KNOWN_LIST_MAX_PER_PREFIX=10 || return 1

  wait_until "A admits B and C" 30 known_list_has_at_least "$data_a" 2
  check "B is in A's known list before it drops" known_list_contains "$data_a" "$key_b"

  # B stops responding without ever being removed from anyone's peer list:
  # a dropped, unresponsive witness rather than a graceful departure.
  kill -STOP "$pid_b" 2>/dev/null

  node d "http://127.0.0.1:$port_a" AVALON_KNOWN_LIST_MAX_PER_PREFIX=10 || return 1
  local port_d="$LAST_PORT" key_d="$LAST_KEY_ID"

  wait_until "A's known list drops the unresponsive B past the freshness window" 40 \
    known_list_lacks "$data_a" "$key_b"
  wait_until "A's known list refills the freed slot with the newly joined D" 40 \
    known_list_contains "$data_a" "$key_d"

  kill -CONT "$pid_b" 2>/dev/null
}

# ---------------------------------------------------------------------------
# Scenario: a flood of same-network-prefix candidates cannot buy more than
# the diversity cap's share of a victim's known list — the real,
# multi-process counterpart to KnownList's own unit-level diversity test.
# Every loopback address here is 127.0.0.0/24, i.e. one prefix, so a real
# flood of real processes IS a same-prefix flood without any IP spoofing.
# ---------------------------------------------------------------------------
scenario_eclipse() {
  local cap="${1:-2}"
  node victim "" AVALON_KNOWN_LIST_MAX_PER_PREFIX="$cap" || return 1
  local port_v="$LAST_PORT" data_v="$LAST_DATA_DIR"

  local flood_size="${DRILL_ECLIPSE_FLOOD:-6}"
  local i bootstrap="http://127.0.0.1:$port_v"
  for i in $(seq 1 "$flood_size"); do
    node "flood$i" "$bootstrap" AVALON_KNOWN_LIST_MAX_PER_PREFIX="$cap" || return 1
  done

  wait_until "victim's peer table sees the whole flood" 30 \
    peer_table_has_at_least "$port_v" "$flood_size"
  # A few extra refill ticks, to give a broken cap a chance to (wrongly)
  # keep admitting past it.
  sleep 6
  local prefix; prefix="$(known_list_first_prefix "$data_v")"
  if [ -z "$prefix" ]; then
    check "victim admitted at least one flood member to compute a prefix cap against" false
    return 0
  fi
  check "victim did admit up to the cap from the flood (the check is not vacuous)" \
    prefix_count_at_least "$data_v" "$prefix" "$cap"
  check "diversity cap ($cap) holds against a $flood_size-node same-prefix flood" \
    prefix_count_at_most "$data_v" "$prefix" "$cap"
}

# ---------------------------------------------------------------------------
# Scenario: a node/client offline for far longer than any freshness or
# probation window reconnects and catches up with no special handling —
# same mirror-watcher backfill and same known-list refill path a node that
# was never offline goes through.
# ---------------------------------------------------------------------------
scenario_long_offline() {
  node a "" AVALON_KNOWN_LIST_MAX_PER_PREFIX=10 || return 1
  local port_a="$LAST_PORT"
  node b "http://127.0.0.1:$port_a" AVALON_KNOWN_LIST_MAX_PER_PREFIX=10 \
    AVALON_MIRROR_PEERS="http://127.0.0.1:$port_a" || return 1
  local port_b="$LAST_PORT" schema_b="live_drill_b" pid_b="$LAST_PID" seed_b="$LAST_SEED"

  check "wrote something through A before B goes offline" is_2xx "$(register_integrator "$port_a" drill-long-offline)"
  wait_until "A has a signed tree head" 40 sth_available "$port_a" || return 1
  local size_before; size_before="$(sth_json "$port_a" | jq .tree_size)"
  wait_until "B mirrors A's head at size $size_before before going offline" 40 \
    head_at_least "$port_b" "$size_before"

  kill "$pid_b" 2>/dev/null
  wait_until "B's process actually exited" 15 proc_gone "$pid_b"
  check "A takes a further write while B is offline" is_2xx "$(register_integrator "$port_a" drill-long-offline-2)"
  wait_until "A's head moves past what B last mirrored" 40 head_at_least "$port_a" $((size_before + 1))
  local size_after; size_after="$(sth_json "$port_a" | jq .tree_size)"
  sleep 10 # well past the fast freshness/probation windows above

  # Reconnect: start the same node identity again on the same port/schema,
  # same as restarting a long-dead process rather than reinstalling one.
  start_node b "$schema_b" "$port_b" "${FAST_KNOWN_LIST[@]}" \
    AVALON_DATA_DIR="$DATA_ROOT/b" AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$port_a" \
    AVALON_MIRROR_PEERS="http://127.0.0.1:$port_a" AVALON_ALLOW_PRIVATE_PEERS=true \
    AVALON_ANNOUNCE_VERIFY_REACHABILITY=false AVALON_WITNESS_SIGNING_KEY="$seed_b" || return 1
  check "B answers again after a long offline stretch" alive "$port_b"
  wait_until "B rejoins A's peer table with no special-casing for the gap" 30 \
    peer_table_has_at_least "$port_a" 1
  wait_until "B's mirrored head catches up to A's current size ($size_after), past the $size_before it had when it left" 60 \
    head_at_least "$port_b" "$size_after"
  check "B's caught-up head is A's head (same root), not merely the same size" \
    same_root_at "$port_a" "$port_b" "$size_after"
}

# ---------------------------------------------------------------------------
# Scenario: a forked log shown to disjoint sources. Two nodes independently
# author the SAME shard with the SAME settlement key (the historical
# duplicate-authority misconfiguration this repo's own fleet notes
# describe) so each accumulates its own local writes and diverges from the
# other at the same tree_size — a real fork, not a fabricated one. A third
# node mirrors both and must record the disagreement.
#
# Part one exercises the source-based equivocation detection
# (mirror_watcher::check_equivocation -> equivocation_findings), which does
# not depend on witness cosigning. Part two adds a gossip-only node that
# mirrors nothing and only learns both heads through announce gossip; both
# authors are bare (no mirror, no cosignatures), so it must confirm the fork
# from the two author signatures alone
# (crate::equivocation::confirm_and_record) and store author-level evidence.
# ---------------------------------------------------------------------------
scenario_fork() {
  cargo build -q -p avalon-server --example witness_evidence || return 1
  node fork-a "" || return 1
  local port_a="$LAST_PORT" log_a="$LOG_DIR/fork-a.log"
  node fork-b "" || return 1
  local port_b="$LAST_PORT"

  check "fork-a accepts a write" is_2xx "$(register_integrator "$port_a" drill-fork-a)"
  check "fork-b accepts a different write" is_2xx "$(register_integrator "$port_b" drill-fork-b)"
  sleep 2 # let each node's own outbox settle its write into a signed tree head

  node fork-watcher "" \
    AVALON_MIRROR_PEERS="http://127.0.0.1:$port_a,http://127.0.0.1:$port_b" || return 1
  local log_watcher="$LOG_DIR/fork-watcher.log"

  wait_until "the mirror observes fork-a's and fork-b's disagreeing roots and records it" 40 \
    log_contains "$log_watcher" equivocation_detected
  check "fork-a itself logs nothing (equivocation is the mirror's finding, not either author's)" \
    log_lacks "$log_a" equivocation_detected

  node fork-gossip "http://127.0.0.1:$port_a,http://127.0.0.1:$port_b" || return 1
  local port_g="$LAST_PORT" log_g="$LOG_DIR/fork-gossip.log" schema_g="live_drill_fork_gossip"
  wait_until "the gossip-only node confirms the fork from two author signatures" 60 \
    log_contains "$log_g" equivocation_confirmed
  wait_until "the gossip-only node stores author-level evidence naming no witness" 30 \
    author_evidence_stored "$schema_g" core
  check "the gossip-only node logged the confirmation as author-level" \
    log_contains "$log_g" 'evidence_kind.*Author'
}

evidence_json() {
  DATABASE_URL="$(schema_url "$1")" "$ROOT/target/debug/examples/witness_evidence" \
    "$AVALON_NETWORK_ID" "$2" 2>/dev/null
}
author_evidence_stored() {
  evidence_json "$1" "$2" | jq -es 'length >= 1 and all(.[]; .kind == "author" and (.equivocating_witnesses | length) == 0)' >/dev/null 2>&1
}

# ---------------------------------------------------------------------------
# Scenario: rolling a running single-key network onto witness cosigning
# without a reset, and rolling back. A authors alone (a single-key network,
# exactly what runs today); B and C then start as cosigning mirrors of A.
# Asserts: A's history is untouched; an old client (author signature only)
# verifies before, during and after; a new client accepts A's head only with
# a majority of the witness keys; cosigning can be turned off on one node
# again with nothing else breaking.
# ---------------------------------------------------------------------------
VERIFY_STH="$ROOT/target/debug/examples/verify_sth"

sth_json() { curl -sf "http://127.0.0.1:$1/ledger/sth/latest${2:-}"; }
sth_available() { sth_json "$1" | jq -e '.tree_size > 0' >/dev/null 2>&1; }
head_at_least() { sth_json "$1" "?shard_id=core" | jq -e --argjson s "$2" '.tree_size >= $s' >/dev/null 2>&1; }
old_client_accepts() { printf '%s' "$1" | "$VERIFY_STH" old; }
cosigned_by() {
  sth_json "$1" "?shard_id=core&witnesses=1" | jq -e --arg k "$2" --argjson s "$3" \
    '.tree_size == $s and ([.cosignatures[].witness_key_id] | index($k)) != null' >/dev/null 2>&1
}
not_cosigned_by() {
  sth_json "$1" "?shard_id=core&witnesses=1" | jq -e --arg k "$2" \
    '([(.cosignatures // [])[].witness_key_id] | index($k)) == null' >/dev/null 2>&1
}
has_no_cosignatures_field() { printf '%s' "$1" | jq -e 'has("cosignatures") | not' >/dev/null 2>&1; }
history_unchanged() {
  curl -sf "http://127.0.0.1:$1/ledger/sth/$2" | jq -e --arg r "$3" --arg g "$4" \
    '.root_hash == $r and .signature == $g' >/dev/null 2>&1
}
consistency_proof_served() {
  curl -sf "http://127.0.0.1:$1/ledger/proof/consistency?first=$2&second=$3" >/dev/null 2>&1
}
majority_accepts() { printf '[%s,%s]' "$1" "$2" | "$VERIFY_STH" cosigned "$3" "$4"; }
majority_rejects() { ! printf '[%s]' "$1" | "$VERIFY_STH" cosigned "$2" "$3"; }
same_root_at() {
  [ "$(curl -sf "http://127.0.0.1:$1/ledger/sth/$3?shard_id=core" | jq -r .root_hash)" = \
    "$(curl -sf "http://127.0.0.1:$2/ledger/sth/$3?shard_id=core" | jq -r .root_hash)" ]
}
same_size_different_root() {
  local a b
  a="$(sth_json "$1" "?shard_id=core")"; b="$(sth_json "$2" "?shard_id=core")"
  [ "$(printf '%s' "$a" | jq .tree_size)" = "$(printf '%s' "$b" | jq .tree_size)" ] &&
    [ "$(printf '%s' "$a" | jq -r .root_hash)" != "$(printf '%s' "$b" | jq -r .root_hash)" ]
}
# cosigner_ids <port>: the witness key ids on the head a node serves, one per line.
cosigner_ids() { sth_json "$1" "?shard_id=core&witnesses=1" | jq -r '(.cosignatures // [])[].witness_key_id' | sort; }
# head_cosigned_only_by <port> <size> <key>...: the served head is at <size> and
# every cosignature on it is by one of the given keys, at least two of them.
head_cosigned_only_by() {
  local port="$1" size="$2" ids
  shift 2
  ids="$(printf '%s\n' "$@" | jq -R . | jq -sc .)"
  sth_json "$port" "?shard_id=core&witnesses=1" | jq -e --argjson s "$size" --argjson ids "$ids" \
    '.tree_size == $s and ((.cosignatures // []) | map(.witness_key_id)) as $c
     | ($c | length) >= 2 and all($c[]; IN($ids[]))' >/dev/null 2>&1
}
# witness_client_accepts <ports-csv> <key>...: a witness-aware client that
# collects the head from each listed node and accepts by a majority of <key>...
witness_client_accepts() {
  local ports="$1" p heads=""
  shift
  for p in ${ports//,/ }; do heads+="${heads:+,}$(sth_json "$p" "?shard_id=core&witnesses=1")"; done
  printf '[%s]' "$heads" | "$VERIFY_STH" cosigned "$@"
}
known_list_confirmed_at_least() {
  local f; f="$(known_list_file "$1")"
  [ -f "$f" ] && [ "$(jq '[.slots[] | select(.status == "confirmed")] | length' "$f" 2>/dev/null || echo 0)" -ge "$2" ]
}
known_list_ids_equal() { [ "$(known_list_ids_sorted "$1")" = "$(printf '%s\n' "${@:2}" | sort)" ]; }
scenario_rollout() {
  cargo build -q -p avalon-server --example verify_sth || return 1

  node ra "" || return 1
  local port_a="$LAST_PORT"
  local slug
  for slug in drill-rollout-1 drill-rollout-2 drill-rollout-3; do
    check "A accepts a write ($slug) as a single-key network" is_2xx "$(register_integrator "$port_a" "$slug")"
  done
  wait_until "A has a signed tree head" 40 sth_available "$port_a" || return 1

  local pre pre_size pre_root pre_sig
  pre="$(sth_json "$port_a")"
  pre_size="$(printf '%s' "$pre" | jq .tree_size)"
  pre_root="$(printf '%s' "$pre" | jq -r .root_hash)"
  pre_sig="$(printf '%s' "$pre" | jq -r .signature)"
  check "before: an old single-key client verifies A's head" old_client_accepts "$pre"

  local seed_b seed_c key_b key_c
  seed_b="$(openssl rand -hex 32)"; seed_c="$(openssl rand -hex 32)"
  key_b="$("$VERIFY_STH" pubkey "$seed_b")"; key_c="$("$VERIFY_STH" pubkey "$seed_c")"

  node rb "http://127.0.0.1:$port_a" AVALON_MIRROR_PEERS="http://127.0.0.1:$port_a" \
    AVALON_WITNESS_SIGNING_KEY="$seed_b" || return 1
  local port_b="$LAST_PORT" pid_b="$LAST_PID"
  node rc "http://127.0.0.1:$port_a" AVALON_MIRROR_PEERS="http://127.0.0.1:$port_a" \
    AVALON_WITNESS_SIGNING_KEY="$seed_c" || return 1
  local port_c="$LAST_PORT"

  wait_until "B cosigns A's head" 60 cosigned_by "$port_b" "$key_b" "$pre_size"
  wait_until "C cosigns A's head" 60 cosigned_by "$port_c" "$key_c" "$pre_size"

  check "history unchanged: A still serves the same root and signature at the pre-rollout size" \
    history_unchanged "$port_a" "$pre_size" "$pre_root" "$pre_sig"
  check "A's default response is the same shape as before (no cosignatures field)" \
    has_no_cosignatures_field "$(sth_json "$port_a")"
  check "during: an old single-key client still verifies A's head" old_client_accepts "$(sth_json "$port_a")"
  check "during: an old single-key client verifies the head B serves" \
    old_client_accepts "$(sth_json "$port_b" "?shard_id=core")"

  local from_b from_c
  from_b="$(sth_json "$port_b" "?shard_id=core&witnesses=1")"
  from_c="$(sth_json "$port_c" "?shard_id=core&witnesses=1")"
  check "a witness-aware client accepts the head cosigned by both known witnesses" \
    majority_accepts "$from_b" "$from_c" "$key_b" "$key_c"
  check "a witness-aware client rejects the same head with only one of two cosignatures" \
    majority_rejects "$from_b" "$key_b" "$key_c"

  # The network keeps growing with cosigning on, and the log stays continuous.
  check "A accepts a write after the rollout" is_2xx "$(register_integrator "$port_a" drill-rollout-4)"
  wait_until "A's head grows past the pre-rollout size" 40 head_at_least "$port_a" $((pre_size + 1))
  local grown_size; grown_size="$(sth_json "$port_a" | jq .tree_size)"
  check "consistency proof from the pre-rollout head to the new head is served" \
    consistency_proof_served "$port_a" "$pre_size" "$grown_size"
  check "after: an old single-key client verifies A's new head" old_client_accepts "$(sth_json "$port_a")"

  # Rollback: B stops cosigning (restart with the flag off); nothing else moves.
  kill "$pid_b" 2>/dev/null
  wait_until "B's process actually exited" 15 proc_gone "$pid_b"
  start_node rb "live_drill_rb" "$port_b" "${FAST_KNOWN_LIST[@]}" \
    AVALON_DATA_DIR="$DATA_ROOT/rb" AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$port_a" \
    AVALON_MIRROR_PEERS="http://127.0.0.1:$port_a" AVALON_ALLOW_PRIVATE_PEERS=true \
    AVALON_ANNOUNCE_VERIFY_REACHABILITY=false AVALON_WITNESS_SIGNING_KEY="$seed_b" \
    AVALON_WITNESS_COSIGNING_ENABLED=false || return 1
  check "A accepts a write after B's rollback" is_2xx "$(register_integrator "$port_a" drill-rollout-5)"
  local final_size
  wait_until "A's head grows again" 40 head_at_least "$port_a" $((grown_size + 1))
  final_size="$(sth_json "$port_a" | jq .tree_size)"
  wait_until "B (cosigning off) still mirrors the new head" 60 head_at_least "$port_b" "$final_size"
  wait_until "C (cosigning on) cosigns the new head" 60 cosigned_by "$port_c" "$key_c" "$final_size"
  check "after rollback: B produced no cosignature for the new head" not_cosigned_by "$port_b" "$key_b"
  check "after rollback: an old single-key client verifies the head B serves" \
    old_client_accepts "$(sth_json "$port_b" "?shard_id=core")"
  check "after rollback: history from before the rollout is still intact on A" \
    history_unchanged "$port_a" "$pre_size" "$pre_root" "$pre_sig"
}

# ---------------------------------------------------------------------------
# Scenario: cosigned heads accepted by a majority of a real known list, then
# loss of one witness. cs-a authors with cosigning off (so it never becomes a
# witness slot). cs-w1..3 are cosigning mirrors of the author. cs-m is a
# non-cosigning mirror of the author that keeps all three witnesses in its
# known list, so it accepts a head only when two of its three confirmed
# slots cosigned it. Every node keeps its default-sized known list, so once
# slots confirm (seconds, with the fast probation) each witness also
# requires a majority of its own list. Then one witness is killed and another
# write lands: verification continues on the remaining two, the dead
# witness's slot is dropped and a newly started witness takes its place.
# ---------------------------------------------------------------------------
scenario_cosigned() {
  cargo build -q -p avalon-server --example verify_sth || return 1
  local u="http://127.0.0.1" base="$PORT_OFFSET"
  local pa=$((BASE_PORT + base + 1)) p1=$((BASE_PORT + base + 2)) p2=$((BASE_PORT + base + 3))
  local p3=$((BASE_PORT + base + 4)) pm=$((BASE_PORT + base + 5))
  local cap=AVALON_KNOWN_LIST_MAX_PER_PREFIX=10

  node cs-a "" $cap AVALON_WITNESS_COSIGNING_ENABLED=false || return 1
  local slug
  for slug in drill-cosigned-1 drill-cosigned-2; do
    check "the author accepts a write ($slug)" is_2xx "$(register_integrator "$pa" "$slug")"
  done
  wait_until "the author has a signed tree head" 40 sth_available "$pa" || return 1

  node cs-w1 "$u:$pa" $cap AVALON_MIRROR_PEERS="$u:$pa" || return 1
  local k1="$LAST_KEY_ID"
  node cs-w2 "$u:$pa" $cap AVALON_MIRROR_PEERS="$u:$pa" || return 1
  local pid2="$LAST_PID" k2="$LAST_KEY_ID"
  node cs-w3 "$u:$pa" $cap AVALON_MIRROR_PEERS="$u:$pa" || return 1
  local k3="$LAST_KEY_ID"
  node cs-m "$u:$p1,$u:$p2,$u:$p3" $cap AVALON_WITNESS_COSIGNING_ENABLED=false \
    AVALON_MIRROR_PEERS="$u:$pa" || return 1
  local data_m="$LAST_DATA_DIR"

  wait_until "the mirror's known list holds exactly the three witnesses' own keys" 40 \
    known_list_ids_equal "$data_m" "$k1" "$k2" "$k3"
  check "the mirror's known list has more than one slot" known_list_has_at_least "$data_m" 2
  wait_until "all three of the mirror's slots are confirmed (past probation)" 40 \
    known_list_confirmed_at_least "$data_m" 3

  # A head written now can only reach the mirror through cosignatures.
  check "the author accepts a write with the mirror's list at three confirmed witnesses" \
    is_2xx "$(register_integrator "$pa" drill-cosigned-3)"
  local size; size=0
  wait_until "the author's head moves to include the new write" 40 head_at_least "$pa" 3
  size="$(sth_json "$pa" | jq .tree_size)"
  wait_until "the mirror serves the new head cosigned by at least two of the witnesses' keys" 60 \
    head_cosigned_only_by "$pm" "$size" "$k1" "$k2" "$k3"
  check "a witness-aware client accepts the mirror's head by a majority of the three witness keys" \
    witness_client_accepts "$pm" "$k1" "$k2" "$k3"
  local wp
  for wp in "$p1" "$p2" "$p3"; do
    wait_until "witness on port $wp serves the new head" 60 head_at_least "$wp" "$size"
  done
  check "the same client accepts it when it collects the head from the witnesses instead" \
    witness_client_accepts "$p1,$p2,$p3" "$k1" "$k2" "$k3"

  # Drop one witness: the two others keep the head verifiable.
  kill "$pid2" 2>/dev/null
  wait_until "witness two's process actually exited" 15 proc_gone "$pid2"
  check "the author accepts a write after a witness is gone" is_2xx "$(register_integrator "$pa" drill-cosigned-4)"
  wait_until "the author's head grows past the loss" 40 head_at_least "$pa" $((size + 1))
  local grown; grown="$(sth_json "$pa" | jq .tree_size)"
  wait_until "the mirror still verifies and serves the new head with the two live witnesses' cosignatures" 90 \
    head_cosigned_only_by "$pm" "$grown" "$k1" "$k3"
  check "the dead witness did not cosign the new head" not_cosigned_by "$pm" "$k2"
  check "a client with all three keys still accepts the new head by a majority" \
    witness_client_accepts "$pm" "$k1" "$k2" "$k3"

  wait_until "the dead witness's slot is dropped from the mirror's known list" 40 known_list_lacks "$data_m" "$k2"
  check "the surviving witnesses' slots are still in the mirror's list" \
    known_list_ids_equal "$data_m" "$k1" "$k3"

  node cs-w4 "$u:$pa,$u:$pm" $cap AVALON_MIRROR_PEERS="$u:$pa" || return 1
  local k4="$LAST_KEY_ID"
  wait_until "a newly started witness takes the freed slot in the mirror's list" 60 \
    known_list_ids_equal "$data_m" "$k1" "$k3" "$k4"
  check "the author accepts a write once the slot is refilled" is_2xx "$(register_integrator "$pa" drill-cosigned-5)"
  wait_until "the author's head grows again" 40 head_at_least "$pa" $((grown + 1))
  local final; final="$(sth_json "$pa" | jq .tree_size)"
  wait_until "the mirror serves the newest head cosigned by the refilled witness set" 90 \
    head_cosigned_only_by "$pm" "$final" "$k1" "$k3" "$k4"
  return 0
}

ALL=(lifecycle witness-loss eclipse long-offline fork rollout cosigned)
SELECTED=("$@")
[ "${#SELECTED[@]}" -eq 0 ] && SELECTED=("${ALL[@]}")

for s in "${SELECTED[@]}"; do
  echo "=== scenario: $s ==="
  case "$s" in
    lifecycle) scenario_lifecycle ;;
    witness-loss) scenario_witness_loss ;;
    eclipse) scenario_eclipse ;;
    long-offline) scenario_long_offline ;;
    fork) scenario_fork ;;
    rollout) scenario_rollout ;;
    cosigned) scenario_cosigned ;;
    *) echo "unknown scenario: $s" >&2; false ;;
  esac || { RESULTS+=("FAIL  scenario $s aborted before finishing its checks"); FAILED=1; }
  # Free every node's connections, ports and schema before the next scenario
  # so a full run never holds all scenarios' nodes at once.
  stop_all
done

echo ""
echo "=== results ==="
printf '%s\n' "${RESULTS[@]:-}"
[ -n "$KEEP_DATA_DIRS" ] || rm -rf "$DATA_ROOT" 2>/dev/null || true
exit "$FAILED"
