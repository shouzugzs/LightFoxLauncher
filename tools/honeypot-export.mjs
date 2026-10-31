// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Export honeypot events from the HP KV namespace for network-security use.
//
// Usage:
//   npx wrangler kv key list --binding=HP --prefix=hit: --remote > keys.json
//   npx wrangler kv key get  --binding=HP --remote "<key>" >> hits.ndjson
//
// This script automates both steps into one NDJSON file (one JSON event per
// line, the format most SIAM/SIEM pipelines ingest directly), with optional
// filtering. It is a maintainer tool: it runs with the account's wrangler
// credentials and is never reachable over HTTP.
//
//   node export.mjs [--since 2025-10-31] [--out hits.ndjson]
//
// OWNER: lead.

import { execFileSync } from "node:child_process";
import { writeFileSync, readFileSync } from "node:fs";

function arg(name, fallback) {
  const i = process.argv.indexOf(name);
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : fallback;
}

const since = arg("--since", null); // ISO date; keep events at/after this time
const out = arg("--out", "hits.ndjson");

function wrangler(args) {
  return execFileSync("npx", ["wrangler", ...args], {
    encoding: "utf8",
    shell: process.platform === "win32",
    maxBuffer: 64 * 1024 * 1024,
  });
}

// 1. List keys. `wrangler kv key list` prints JSON, but with banners on some
//    versions, so extract the outermost JSON array defensively.
const rawList = wrangler([
  "kv", "key", "list", "--binding=HP", "--prefix=hit:", "--remote",
]);
const start = rawList.indexOf("[");
const end = rawList.lastIndexOf("]");
if (start < 0 || end < 0) {
  console.error("could not parse wrangler key list output:\n" + rawList.slice(0, 500));
  process.exit(1);
}
const keys = JSON.parse(rawList.slice(start, end + 1)).map((k) => k.name);
console.error(`${keys.length} event key(s) found`);

// 2. Fetch each value and keep it as one JSON line. A missing key (expired
//    between list and get) is skipped, not fatal.
const lines = [];
let dropped = 0;
for (const key of keys) {
  try {
    const value = wrangler([
      "kv", "key", "get", "--binding=HP", "--remote", key,
    ]).trim();
    const event = JSON.parse(value);
    if (since && event.time && event.time < since) continue;
    lines.push(JSON.stringify(event));
  } catch {
    dropped += 1;
  }
}

writeFileSync(out, lines.join("\n") + (lines.length ? "\n" : ""));
console.error(`wrote ${lines.length} event(s) to ${out}` + (dropped ? ` (${dropped} skipped)` : ""));

// 3. Optionally delete what was exported so the namespace does not grow
//    forever. Opt-in: the store also self-expires after 30 days.
if (process.argv.includes("--delete")) {
  for (const key of keys) {
    try {
      wrangler(["kv", "key", "delete", "--binding=HP", "--remote", key]);
    } catch {
      // already gone
    }
  }
  console.error(`deleted ${keys.length} exported key(s)`);
}
