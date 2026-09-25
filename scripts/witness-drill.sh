#!/usr/bin/env bash
# Witness-cosigning scenario drill: grows a network from one real
# avalon-server process to several, removes nodes (including the original),
# drops and refills a known-list witness, floods a victim with same-prefix
# candidates, and reconnects a long-offline node — all against real
# processes and a real Postgres, never in-process function calls.
#
# usage: scripts/witness-drill.sh [scenario ...]     (default: all scenarios)
#   scenarios: lifecycle witness-loss eclipse long-offline fork rollout
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
  local port_a="$LAST_PORT" data_a="$LAST_DATA_DIR" pid_a="$LAST_PID"

  # A alone: single-signer-equivalent, known list empty or self only.
  check "A alone answers /nodes/status" alive "$port_a"

  node b "http://127.0.0.1:$port_a" AVALON_KNOWN_LIST_MAX_PER_PREFIX=10 || return 1
  local port_b="$LAST_PORT" data_b="$LAST_DATA_DIR"
  node c "http://127.0.0.1:$port_a" AVALON_KNOWN_LIST_MAX_PER_PREFIX=10 || return 1
  local port_c="$LAST_PORT"

  # B and C join and get admitted to A's known list, with no restart of A.
  wait_until "A's known list admits B and C without a restart" 30 \
    known_list_has_at_least "$data_a" 2
  wait_until "A's own peer table lists both B and C" 30 \
    peer_table_has_at_least "$port_a" 2
  check "A is still the same process throughout (no restart)" kill -0 "$pid_a"

  # Remove A, including its own process, and confirm B and C still verify
  # and author with no special-casing for "node A was first."
  local before_b; before_b="$(known_list_ids_sorted "$data_b")"
  kill "$pid_a" 2>/dev/null
  wait_until "A's process actually exited" 15 proc_gone "$pid_a"
  check "B still answers after A is gone" alive "$port_b"
  check "C still answers after A is gone" alive "$port_c"
  wait_until "B's known list changes membership after A's departure (stale slot pruned/refilled)" 30 \
    known_list_ids_differ_from "$data_b" "$before_b"

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

  kill "$pid_b" 2>/dev/null
  wait_until "B's process actually exited" 15 proc_gone "$pid_b"
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
}

# ---------------------------------------------------------------------------
# Scenario: a forked log shown to disjoint sources. Two nodes independently
# author the SAME shard with the SAME settlement key (the historical
# duplicate-authority misconfiguration this repo's own fleet notes
# describe) so each accumulates its own local writes and diverges from the
# other at the same tree_size — a real fork, not a fabricated one. A third
# node mirrors both and must record the disagreement.
#
# This exercises the source-based equivocation detection
# (mirror_watcher::check_equivocation -> equivocation_findings), which does
# not depend on witness cosigning. The gossip-driven cosigned confirmation
# (crate::equivocation::confirm_and_record) needs several cosigning nodes
# with proven witness keys and is covered by the follow-up drill work.
# ---------------------------------------------------------------------------
scenario_fork() {
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

ALL=(lifecycle witness-loss eclipse long-offline fork rollout)
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
    *) echo "unknown scenario: $s" >&2; FAILED=1 ;;
  esac
done

echo ""
echo "=== results ==="
printf '%s\n' "${RESULTS[@]:-}"
[ -n "$KEEP_DATA_DIRS" ] || rm -rf "$DATA_ROOT" 2>/dev/null || true
exit "$FAILED"
