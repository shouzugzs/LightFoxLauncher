// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Local harness for tools/honeypot-export.mjs. No wrangler, no Cloudflare,
// no network: it imports the exported pure helpers from the export tool and
// runs them over fabricated events.
//
// The fixture models the real store: two capture sources share one KV
// namespace and are told apart by the `source` field alone.
//
//   * decoy  — 5 events from the fake admin pages. login.js writes no
//              `source` field at all, which is exactly what the filter must
//              treat as decoy.
//   * room   — 5 events from site/functions/api/room.js, each carrying
//              `"source":"room"` and a `reason`.
//   * legacy — 2 older events written before any `source` field existed.
//              Still decoy events; must not be lost.
//
// Run: node tools/export-selftest.mjs
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  eventSource,
  filterEvents,
  matchesFilters,
  summarize,
  formatSummary,
  parseArgs,
} from "./honeypot-export.mjs";

let failures = 0;
let checks = 0;
function check(name, cond, extra) {
  checks += 1;
  if (cond) {
    console.log(`ok   - ${name}`);
  } else {
    failures += 1;
    console.log(`FAIL - ${name}${extra ? " :: " + extra : ""}`);
  }
}
const eq = (name, actual, expected) =>
  check(name, actual === expected, `expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);

// ---------------------------------------------------------------------------
// Fixture.
// ---------------------------------------------------------------------------

// Decoy pages: no `source` key whatsoever. Shaped like a real login.js event.
const decoyEvents = Array.from({ length: 5 }, (_, i) => ({
  time: `2025-11-01T00:00:0${i}.000Z`,
  ip: `203.0.113.${10 + i}`,
  ip_source: "cf-connecting-ip",
  method: "POST",
  path: "/api/login",
  user_agent: `curl/8.${i}`,
  fields: { username: "admin", password: `pw${i}` },
}));

// Room endpoint: `source: "room"` plus a reason. Two reasons repeat so the
// by-reason grouping has something to group.
const roomReasons = [
  "injection_chars",
  "injection_chars",
  "path_traversal",
  "path_traversal",
  "control_chars",
];
const roomEvents = roomReasons.map((reason, i) => ({
  time: `2025-11-01T01:00:0${i}.000Z`,
  source: "room",
  reason,
  ip: `198.51.100.${20 + i}`,
  method: "GET",
  path: "/api/room",
  user_agent: "python-requests/2.31",
  query: `code=${i}`,
}));

// Legacy decoy events: no `source`, older than the cutoff used below, and
// missing most modern fields.
const legacyEvents = [
  { time: "2025-09-01T00:00:00.000Z", ip: "192.0.2.1", path: "/api/login" },
  { time: "2025-09-02T00:00:00.000Z", ip: "192.0.2.2", path: "/api/login" },
];

const all = [...decoyEvents, ...roomEvents, ...legacyEvents];

// Events at/after this survive the --since test; the two legacy events are
// older and get dropped by it.
const SINCE = "2025-10-01T00:00:00.000Z";

// ---------------------------------------------------------------------------
// 1. eventSource: the whole classification rule, in one place.
// ---------------------------------------------------------------------------
{
  eq("room event -> room", eventSource(roomEvents[0]), "room");
  eq("decoy event (no source) -> decoy", eventSource(decoyEvents[0]), "decoy");
  eq("legacy event -> decoy", eventSource(legacyEvents[0]), "decoy");
  eq("explicit source:decoy -> decoy", eventSource({ source: "decoy" }), "decoy");
  eq("source:ROOM (wrong case) -> decoy", eventSource({ source: "ROOM" }), "decoy");
  eq("source:null -> decoy", eventSource({ source: null }), "decoy");
  eq("null -> decoy", eventSource(null), "decoy");
  eq("array -> decoy", eventSource([]), "decoy");
}

// ---------------------------------------------------------------------------
// 2. filterEvents: no filter is the backward-compatible path.
// ---------------------------------------------------------------------------
{
  eq("no options keeps all 12", filterEvents(all, {}).length, 12);
  eq("undefined options keeps all 12", filterEvents(all).length, 12);
  eq("empty options object keeps all 12", filterEvents(all, { source: null, reason: null, since: null }).length, 12);
  check("filterEvents returns a new array", filterEvents(all, {}) !== all);
  check("filterEvents does not mutate its input", all.length === 12 && all[0] === decoyEvents[0]);
  eq("non-array input -> []", filterEvents(null, {}).length, 0);
  eq("undefined input -> []", filterEvents(undefined, {}).length, 0);
}

// ---------------------------------------------------------------------------
// 3. --source decoy / room.
// ---------------------------------------------------------------------------
{
  const decoy = filterEvents(all, { source: "decoy" });
  eq("--source decoy -> 7 (5 decoy + 2 legacy)", decoy.length, 7);
  check("--source decoy keeps the modern decoy events",
    decoyEvents.every((e) => decoy.includes(e)));
  check("--source decoy keeps the legacy events",
    legacyEvents.every((e) => decoy.includes(e)));
  check("--source decoy excludes every room event",
    roomEvents.every((e) => !decoy.includes(e)));

  const room = filterEvents(all, { source: "room" });
  eq("--source room -> 5", room.length, 5);
  check("--source room keeps exactly the room events",
    roomEvents.every((e) => room.includes(e)));
  check("--source room excludes decoy and legacy",
    [...decoyEvents, ...legacyEvents].every((e) => !room.includes(e)));

  eq("decoy + room partition the whole set", decoy.length + room.length, all.length);
}

// ---------------------------------------------------------------------------
// 4. --reason. Only room events carry it; a reason never matches decoy data.
// ---------------------------------------------------------------------------
{
  eq("--reason injection_chars -> 2", filterEvents(all, { reason: "injection_chars" }).length, 2);
  eq("--reason path_traversal -> 2", filterEvents(all, { reason: "path_traversal" }).length, 2);
  eq("--reason control_chars -> 1", filterEvents(all, { reason: "control_chars" }).length, 1);
  eq("--reason unknown -> 0", filterEvents(all, { reason: "no_such_reason" }).length, 0);
  eq("--source room --reason injection_chars -> 2",
    filterEvents(all, { source: "room", reason: "injection_chars" }).length, 2);
  eq("--source decoy --reason injection_chars -> 0 (no decoy has a reason)",
    filterEvents(all, { source: "decoy", reason: "injection_chars" }).length, 0);
  eq("--source room --reason control_chars -> 1",
    filterEvents(all, { source: "room", reason: "control_chars" }).length, 1);
  const onlyRoom = filterEvents(all, { reason: "path_traversal" });
  check("--reason only ever returns room events",
    onlyRoom.every((e) => eventSource(e) === "room"));
}

// ---------------------------------------------------------------------------
// 5. --since, alone and combined.
// ---------------------------------------------------------------------------
{
  const recent = filterEvents(all, { since: SINCE });
  eq("--since drops the 2 legacy events -> 10", recent.length, 10);
  eq("--since + decoy -> 5 (only the modern decoy events)",
    filterEvents(all, { since: SINCE, source: "decoy" }).length, 5);
  eq("--since + room -> 5",
    filterEvents(all, { since: SINCE, source: "room" }).length, 5);
  eq("--since + reason injection_chars -> 2",
    filterEvents(all, { since: SINCE, reason: "injection_chars" }).length, 2);
  eq("--since + room + path_traversal -> 2",
    filterEvents(all, { since: SINCE, source: "room", reason: "path_traversal" }).length, 2);
  eq("--since after every event -> 0",
    filterEvents(all, { since: "2030-01-01T00:00:00.000Z" }).length, 0);
  eq("--since before every event -> 12",
    filterEvents(all, { since: "2020-01-01T00:00:00.000Z" }).length, 12);

  // Original semantics: a missing `time` is never a reason to drop an event.
  const noTime = [{ source: "room", reason: "control_chars" }, { ip: "192.0.2.9" }];
  eq("event without time survives --since", filterEvents(noTime, { since: SINCE }).length, 2);

  // Exact boundary is inclusive (>=), matching the pre-existing behavior.
  eq("--since at the exact event time keeps it",
    filterEvents([{ time: SINCE }], { since: SINCE }).length, 1);
  eq("--since one millisecond after the event drops it",
    filterEvents([{ time: SINCE }], { since: "2025-10-01T00:00:00.001Z" }).length, 0);
}

// ---------------------------------------------------------------------------
// 6. Junk entries never crash a filter run.
// ---------------------------------------------------------------------------
{
  const mixed = [null, undefined, 42, "str", [], {}, { source: "room" }, ...decoyEvents];
  // Survivors: the two bare objects plus the 5 decoy events. null, undefined,
  // 42, "str" and [] are not event objects and are dropped.
  eq("non-object entries are dropped", filterEvents(mixed, {}).length, 7);
  eq("empty object is a decoy event", filterEvents([{}], { source: "decoy" }).length, 1);
  eq("bare room object is a room event", filterEvents([{ source: "room" }], { source: "room" }).length, 1);
  check("matchesFilters rejects a non-object", matchesFilters(null, {}) === false);
  check("matchesFilters accepts a decoy object", matchesFilters({}, { source: "decoy" }) === true);
}

// ---------------------------------------------------------------------------
// 7. summarize.
// ---------------------------------------------------------------------------
{
  const stats = summarize(all);
  eq("summarize total = 12", stats.total, 12);
  eq("summarize decoy = 7 (5 decoy + 2 legacy)", stats.bySource.decoy, 7);
  eq("summarize room = 5", stats.bySource.room, 5);
  eq("summarize byReason.injection_chars = 2", stats.byReason.injection_chars, 2);
  eq("summarize byReason.path_traversal = 2", stats.byReason.path_traversal, 2);
  eq("summarize byReason.control_chars = 1", stats.byReason.control_chars, 1);
  eq("byReason has exactly 3 keys", Object.keys(stats.byReason).length, 3);
  eq("byReason sums to the room count",
    Object.values(stats.byReason).reduce((a, b) => a + b, 0), 5);
  eq("bySource sums to total", stats.bySource.decoy + stats.bySource.room, stats.total);

  // Empty input must not throw and must produce zeroed counters.
  const empty = summarize([]);
  eq("summarize([]) total = 0", empty.total, 0);
  eq("summarize([]) decoy = 0", empty.bySource.decoy, 0);
  eq("summarize([]) room = 0", empty.bySource.room, 0);
  eq("summarize([]) has no reasons", Object.keys(empty.byReason).length, 0);
  const nullStats = summarize(null);
  eq("summarize(null) total = 0", nullStats.total, 0);
  check("summarize(null) has both source counters",
    nullStats.bySource.decoy === 0 && nullStats.bySource.room === 0);

  // Missing fields: no source -> decoy, no reason -> absent from byReason.
  const sparse = summarize([{}, { ip: "x" }, { reason: "" }, { reason: 7 }]);
  eq("events without source count as decoy", sparse.bySource.decoy, 4);
  eq("empty/non-string reasons are not counted", Object.keys(sparse.byReason).length, 0);

  // Junk is ignored rather than counted as a decoy event.
  eq("summarize skips non-objects", summarize([null, 1, "s", [], {}]).total, 1);

  // `reason` is attacker-influenced: a literal "__proto__" must stay a key.
  const proto = summarize([{ source: "room", reason: "__proto__" }]);
  eq("__proto__ reason is an ordinary key", proto.byReason.__proto__, 1);
  eq("__proto__ reason did not pollute Object.prototype",
    ({}).polluted, undefined);
}

// ---------------------------------------------------------------------------
// 8. formatSummary — the exact stderr shape the operator reads.
// ---------------------------------------------------------------------------
{
  const stats = summarize(all);
  const text = formatSummary(stats, { out: "hits.ndjson" });
  const lines = text.split("\n");
  eq("summary line 1", lines[0], "exported 12 event(s) to hits.ndjson");
  eq("summary line 2", lines[1], "  by source: decoy=7 room=5");
  check("summary line 3 groups by reason",
    lines[2].startsWith("  by reason: ") &&
      lines[2].includes("injection_chars=2") &&
      lines[2].includes("path_traversal=2") &&
      lines[2].includes("control_chars=1"),
    lines[2]);
  check("reasons are ordered by count then name",
    lines[2] === "  by reason: injection_chars=2 path_traversal=2 control_chars=1",
    lines[2]);

  const noReason = formatSummary(summarize(decoyEvents), { out: "d.ndjson" });
  eq("no reason line when nothing has a reason", noReason.split("\n").length, 2);
  check("decoy-only summary counts", noReason.includes("by source: decoy=5 room=0"));

  const skipped = formatSummary(stats, { out: "hits.ndjson", skipped: 3 });
  check("skipped count is reported", skipped.includes("(3 skipped)"), skipped);

  const statsOnly = formatSummary(stats, { statsOnly: true, out: "hits.ndjson" });
  check("--stats-only wording says no file was written",
    statsOnly.startsWith("12 event(s) matched, stats only (no file written)"),
    statsOnly.split("\n")[0]);
  check("--stats-only still prints both breakdowns",
    statsOnly.includes("by source: decoy=7 room=5") &&
      statsOnly.includes("by reason: injection_chars=2"));

  const zero = formatSummary(summarize([]), { out: "hits.ndjson" });
  eq("empty export still prints one summary line", zero.split("\n").length, 2);
  check("empty export counts zero", zero.includes("exported 0 event(s)") &&
    zero.includes("decoy=0 room=0"));
}

// ---------------------------------------------------------------------------
// 9. parseArgs: defaults, both spellings, and rejected input.
// ---------------------------------------------------------------------------
{
  const defaults = parseArgs([]);
  eq("default --out", defaults.out, "hits.ndjson");
  eq("default --since", defaults.since, null);
  eq("default --source (both)", defaults.source, null);
  eq("default --reason (none)", defaults.reason, null);
  eq("default --stats-only off", defaults.statsOnly, false);
  eq("default --delete off", defaults.del, false);

  const full = parseArgs([
    "--since", SINCE, "--source", "room", "--reason", "injection_chars",
    "--out", "room.ndjson", "--stats-only", "--delete",
  ]);
  eq("parse --since", full.since, SINCE);
  eq("parse --source room", full.source, "room");
  eq("parse --reason", full.reason, "injection_chars");
  eq("parse --out", full.out, "room.ndjson");
  eq("parse --stats-only", full.statsOnly, true);
  eq("parse --delete", full.del, true);

  eq("parse --source=room (equals form)", parseArgs(["--source=room"]).source, "room");
  eq("parse --out=x.ndjson (equals form)", parseArgs(["--out=x.ndjson"]).out, "x.ndjson");
  eq("parse --source decoy", parseArgs(["--source", "decoy"]).source, "decoy");

  const throws = (name, argv) => {
    let threw = false;
    try { parseArgs(argv); } catch { threw = true; }
    check(name, threw, `expected a throw for ${JSON.stringify(argv)}`);
  };
  throws("--source bogus is rejected", ["--source", "bogus"]);
  throws("--source ROOM (wrong case) is rejected", ["--source", "ROOM"]);
  throws("--source with no value is rejected", ["--source"]);
  throws("--source followed by another flag is rejected", ["--source", "--delete"]);
  throws("--reason with no value is rejected", ["--reason"]);
  throws("--since with no value is rejected", ["--since"]);
  throws("unknown option is rejected", ["--nope"]);
}

// ---------------------------------------------------------------------------
// 10. The script itself still parses, and importing it did not run main().
// ---------------------------------------------------------------------------
{
  const toolPath = fileURLToPath(new URL("./honeypot-export.mjs", import.meta.url));
  let ok = true;
  let detail = "";
  try {
    execFileSync(process.execPath, ["--check", toolPath], { encoding: "utf8" });
  } catch (err) {
    ok = false;
    detail = String((err && err.stderr) || err).slice(0, 400);
  }
  check("node --check tools/honeypot-export.mjs passes", ok, detail);
  check("importing the tool exported the pure helpers",
    typeof filterEvents === "function" && typeof summarize === "function");
}

console.log(`\n${checks - failures}/${checks} checks passed`);
console.log(failures === 0 ? "ALL PASS" : `${failures} FAILURE(S)`);
process.exit(failures === 0 ? 0 : 1);
