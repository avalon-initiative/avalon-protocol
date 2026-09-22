#!/usr/bin/env node
// Generates src/generated.ts from ../../docs/generated/openapi.json (#723)
// via openapi-typescript's programmatic API rather than its CLI, because
// the real spec has real operationId collisions across tags
// (list_messages/send_message for chat vs. guild channels,
// register_start/register_finish for identity vs. passkeys — the same
// four collisions the Rust SDK's own build.rs (#724) found and resolved
// by generating one module per tag). openapi-typescript's `operations`
// namespace is flat and keyed only by operationId with no such
// namespacing, so feeding it the spec unmodified produces a generated.ts
// that doesn't even type-check (`error TS2300: Duplicate identifier`).
// Fixed the same way here: prefix every colliding operationId with its
// own first tag before generation — this only affects (unused, for now)
// `operations`/`paths` naming, never `components.schemas`, which has no
// collisions since every schema already has a globally unique name.
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import openapiTS, { astToString } from "openapi-typescript";

const SCHEMA_PATH = fileURLToPath(
  new URL("../../../docs/generated/openapi.json", import.meta.url),
);
const OUT_PATH = fileURLToPath(new URL("../src/generated.ts", import.meta.url));
const METHODS = ["get", "post", "put", "patch", "delete"];

function dedupeOperationIds(doc) {
  const seenAt = new Map();
  for (const methods of Object.values(doc.paths)) {
    for (const [method, op] of Object.entries(methods)) {
      if (!METHODS.includes(method) || !op.operationId) continue;
      const entries = seenAt.get(op.operationId) ?? [];
      entries.push(op);
      seenAt.set(op.operationId, entries);
    }
  }
  for (const [operationId, ops] of seenAt) {
    if (ops.length < 2) continue;
    for (const op of ops) {
      const tag = op.tags?.[0] ?? "untagged";
      op.operationId = `${tag}_${operationId}`;
    }
  }
}

async function main() {
  const doc = JSON.parse(readFileSync(SCHEMA_PATH, "utf8"));
  dedupeOperationIds(doc);
  const ast = await openapiTS(doc);
  const schemaVersion = doc.info.version;
  const output =
    astToString(ast) +
    `\n// Issue #735: the info.version this file's types were generated from.\n` +
    `export const OPENAPI_SCHEMA_VERSION = ${JSON.stringify(schemaVersion)} as const\n`;
  writeFileSync(OUT_PATH, output);
}

main();
