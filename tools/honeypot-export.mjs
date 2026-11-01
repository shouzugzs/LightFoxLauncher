// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Export honeypot events from the HP KV namespace for network-security use.
//
// Two capture sources share that one namespace (same binding, same `hit:`
// prefix), and their events are told apart by shape alone:
//
//   * decoy pages (private-site/functions/api/login.js): the event has NO
//     `source` field at all. Events written before the field existed are
//     therefore still decoy events — a missing `source` means "decoy".
//   * room endpoint (site/functions/api/room.js): the event carries
//     `"source":"room"` plus a `"reason"` naming what tripped the filter
//     (e.g. injection_chars, path_traversal, control_chars).
//
// Usage:
//   npx wrangler kv key list --binding=HP --prefix=hit: --remote > keys.json
//   npx wrangler kv key get  --binding=HP --remote "<key>" >> hits.ndjson
//
// This script automates both steps into one NDJSON file (one JSON event per
// line, the format most SIAM/SIEM pipelines ingest directly), with optional
// filtering by source / reason / time. It is a maintainer tool: it runs with
// the account's wrangler credentials and is never reachable over HTTP.
//
//   node tools/honeypot-export.mjs [options]
//
//     --since <iso>     keep events at/after this time (default: no cutoff)
//     --source <kind>   decoy | room (default: both)
//     --reason <name>   keep only events whose `reason` equals <name>
//     --out <path>      NDJSON output file (default: hits.ndjson)
//     --stats-only      print the summary only; write no file
//     --delete          delete the keys that were exported (opt-in)
//
// The summary goes to stderr so stdout/NDJSON stays machine-clean.
//
// OWNER: lead.

import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";

// The two source labels. "decoy" is also the label for legacy events that
// predate the `source` field, which is why it is the fallback everywhere.
const SOURCE_DECOY = "decoy";
const SOURCE_ROOM = "room";
const SOURCES = [SOURCE_DECOY, SOURCE_ROOM];

const USAGE = `usage: node tools/honeypot-export.mjs [options]

  --since <iso>     keep events at/after this time (default: no cutoff)
  --source <kind>   decoy | room (default: both)
  --reason <name>   keep only events with this reason (room events)
  --out <path>      NDJSON output file (default: hits.ndjson)
  --stats-only      print the summary only; write no file
  --delete          delete the keys that were exported (opt-in)
`;

// ---------------------------------------------------------------------------
// Pure helpers. Exported so tools/export-selftest.mjs can exercise them with
// fabricated events, without wrangler or Cloudflare.
// ---------------------------------------------------------------------------

// Which of the two capture sources produced this event. Anything that is not
// an object, or whose `source` is not exactly "room", is a decoy event: that
// covers both the modern decoy shape (no `source`) and every pre-`source`
// event still inside the 30-day TTL.
export function eventSource(event) {
  return event && typeof event === "object" && event.source === SOURCE_ROOM
    ? SOURCE_ROOM
    : SOURCE_DECOY;
}

// Does one event survive the requested filters? `since` keeps the original
// semantics: an event with no `time` is never dropped by the cutoff, because
// a missing timestamp is a gap in the capture, not evidence of age.
export function matchesFilters(event, opts = {}) {
  const { source = null, reason = null, since = null } = opts || {};
  if (!event || typeof event !== "object" || Array.isArray(event)) return false;
  if (since && event.time && event.time < since) return false;
  if (source && eventSource(event) !== source) return false;
  if (reason && event.reason !== reason) return false;
  return true;
}

// Keep the events that pass every filter. Non-object entries are dropped
// here as well as in the caller, so a caller that hands over a mixed bag
// still gets only usable events back.
export function filterEvents(events, opts = {}) {
  if (!Array.isArray(events)) return [];
  return events.filter((event) => matchesFilters(event, opts));
}

// Counts for the stderr summary: total, a decoy/room split, and a breakdown
// by `reason`. Events without a reason simply do not appear in `byReason`.
// The reason map has a null prototype: `reason` is attacker-influenced data,
// so a literal "__proto__" must land as an ordinary key.
export function summarize(events) {
  const bySource = { [SOURCE_DECOY]: 0, [SOURCE_ROOM]: 0 };
  const byReason = Object.create(null);
  let total = 0;
  for (const event of Array.isArray(events) ? events : []) {
    if (!event || typeof event !== "object" || Array.isArray(event)) continue;
    total += 1;
    bySource[eventSource(event)] += 1;
    if (typeof event.reason === "string" && event.reason) {
      byReason[event.reason] = (byReason[event.reason] || 0) + 1;
    }
  }
  return { total, bySource, byReason };
}

// Render the summary block. Most-frequent reason first, ties broken by name,
// so two runs over the same data print byte-identical output.
export function formatSummary(stats, opts = {}) {
  const { out = "hits.ndjson", statsOnly = false, skipped = 0 } = opts || {};
  const skippedNote = skipped ? ` (${skipped} skipped)` : "";
  const lines = [
    statsOnly
      ? `${stats.total} event(s) matched, stats only (no file written)${skippedNote}`
      : `exported ${stats.total} event(s) to ${out}${skippedNote}`,
    `  by source: ${SOURCE_DECOY}=${stats.bySource[SOURCE_DECOY]} ` +
      `${SOURCE_ROOM}=${stats.bySource[SOURCE_ROOM]}`,
  ];
  const reasons = Object.keys(stats.byReason)
    .map((name) => [name, stats.byReason[name]])
    .sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  if (reasons.length) {
    lines.push(`  by reason: ${reasons.map(([n, c]) => `${n}=${c}`).join(" ")}`);
  }
  return lines.join("\n");
}

// ---------------------------------------------------------------------------
// Argument parsing.
// ---------------------------------------------------------------------------

class UsageError extends Error {}

// Accepts both `--flag value` and `--flag=value`; bare switches stand alone.
// Unknown options are an error rather than silently ignored, because a typo
// in `--source` would otherwise export the whole namespace by accident.
export function parseArgs(argv) {
  const opts = {
    since: null,
    out: "hits.ndjson",
    source: null,
    reason: null,
    statsOnly: false,
    del: false,
  };
  const valueFlags = {
    "--since": "since",
    "--out": "out",
    "--source": "source",
    "--reason": "reason",
  };
  for (let i = 0; i < argv.length; i += 1) {
    let name = argv[i];
    let inline = null;
    const eq = name.indexOf("=");
    if (name.startsWith("--") && eq > 2) {
      inline = name.slice(eq + 1);
      name = name.slice(0, eq);
    }
    if (name === "--stats-only") {
      opts.statsOnly = true;
      continue;
    }
    if (name === "--delete") {
      opts.del = true;
      continue;
    }
    if (name in valueFlags) {
      let value = inline;
      if (value === null) {
        value = argv[i + 1];
        if (value === undefined || value.startsWith("--")) {
          throw new UsageError(`${name} needs a value`);
        }
        i += 1;
      }
      if (value === "") throw new UsageError(`${name} needs a non-empty value`);
      opts[valueFlags[name]] = value;
      continue;
    }
    if (name.startsWith("--")) throw new UsageError(`unknown option ${name}`);
  }
  if (opts.source !== null && !SOURCES.includes(opts.source)) {
    throw new UsageError(
      `--source must be one of ${SOURCES.join("|")} (got "${opts.source}")`,
    );
  }
  return opts;
}

// ---------------------------------------------------------------------------
// wrangler plumbing.
// ---------------------------------------------------------------------------

function wrangler(args) {
  return execFileSync("npx", ["wrangler", ...args], {
    encoding: "utf8",
    shell: process.platform === "win32",
    maxBuffer: 64 * 1024 * 1024,
  });
}

// `wrangler kv key list` prints JSON, but with banners on some versions, so
// extract the outermost JSON array defensively.
function listKeys() {
  const raw = wrangler([
    "kv", "key", "list", "--binding=HP", "--prefix=hit:", "--remote",
  ]);
  const start = raw.indexOf("[");
  const end = raw.lastIndexOf("]");
  if (start < 0 || end < 0) {
    throw new Error(
      "could not parse wrangler key list output:\n" + raw.slice(0, 500),
    );
  }
  return JSON.parse(raw.slice(start, end + 1)).map((k) => k.name);
}

// ---------------------------------------------------------------------------
// Main flow.
// ---------------------------------------------------------------------------

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const filters = { since: opts.since, source: opts.source, reason: opts.reason };

  const keys = listKeys();
  console.error(`${keys.length} event key(s) found`);

  // Fetch each value. A key that expired between list and get, or whose value
  // is not JSON at all, is counted as skipped — one bad record must not abort
  // an export that may cover thousands of good ones.
  const entries = [];
  const skippedKeys = [];
  for (const key of keys) {
    let text;
    try {
      text = wrangler([
        "kv", "key", "get", "--binding=HP", "--remote", key,
      ]).trim();
    } catch {
      skippedKeys.push(key);
      continue;
    }
    let event;
    try {
      event = JSON.parse(text);
    } catch {
      skippedKeys.push(key);
      continue;
    }
    if (!event || typeof event !== "object" || Array.isArray(event)) {
      skippedKeys.push(key);
      continue;
    }
    entries.push({ key, event });
  }

  // Filter first, then keep the matching key/value pairs. Identity is safe
  // here: every entry holds its own object from its own JSON.parse call.
  const keptEvents = filterEvents(entries.map((e) => e.event), filters);
  const kept = new Set(keptEvents);
  const exported = entries.filter((e) => kept.has(e.event));
  const stats = summarize(keptEvents);

  // 1. NDJSON out (unless --stats-only). Written before the summary so a
  //    failed write cannot be reported as a successful export.
  if (!opts.statsOnly) {
    const body = keptEvents.map((event) => JSON.stringify(event)).join("\n");
    writeFileSync(opts.out, body + (keptEvents.length ? "\n" : ""));
  }

  // 2. Summary on stderr, never stdout: the NDJSON is what pipelines read.
  console.error(
    formatSummary(stats, {
      out: opts.out,
      statsOnly: opts.statsOnly,
      skipped: skippedKeys.length,
    }),
  );
  if (skippedKeys.length) {
    const shown = skippedKeys.slice(0, 3).join(", ");
    console.error(
      `  skipped key(s): ${shown}` +
        (skippedKeys.length > 3 ? ` … +${skippedKeys.length - 3}` : ""),
    );
  }

  // 3. Optionally delete what was exported so the namespace does not grow
  //    forever. Only the keys that actually reached the file are removed:
  //    deleting a filtered-out or unparseable key would destroy data that was
  //    never exported. The store also self-expires after 30 days.
  if (opts.del) {
    if (opts.statsOnly) {
      console.error("note: --delete ignored with --stats-only (nothing exported)");
    } else {
      let deleted = 0;
      for (const { key } of exported) {
        try {
          wrangler(["kv", "key", "delete", "--binding=HP", "--remote", key]);
          deleted += 1;
        } catch {
          // already gone
        }
      }
      console.error(`deleted ${deleted} exported key(s)`);
    }
  }
}

// Run only when invoked as a script. tools/export-selftest.mjs imports this
// module for its pure helpers and must not touch wrangler or the network.
const invokedDirectly =
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href;

if (invokedDirectly) {
  main().catch((err) => {
    if (err instanceof UsageError) {
      console.error(`error: ${err.message}\n\n${USAGE}`);
      process.exitCode = 2;
      return;
    }
    console.error(err && err.message ? err.message : String(err));
    process.exitCode = 1;
  });
}
