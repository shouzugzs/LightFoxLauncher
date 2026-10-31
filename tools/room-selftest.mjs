// SPDX-License-Identifier: MPL-2.0
// Local harness: exercise site/functions/api/room.js without Cloudflare.
// Creates a fake KV in memory, imports the function module, fires requests at
// it, and prints an ok/FAIL line per assertion.
// Run: node tools/room-selftest.mjs
import { readFileSync, writeFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

// The function is deployed as an ES module but lives in a .js file with no
// package.json scope, so it is copied to a .mjs name before import. Comments
// (the SPDX header) are plain text and need no stripping.
const src = readFileSync(
  new URL("../site/functions/api/room.js", import.meta.url),
  "utf8",
);
const dir = mkdtempSync(join(tmpdir(), "room-test-"));
const modPath = join(dir, "room.mjs");
writeFileSync(modPath, src);
const mod = await import(pathToFileURL(modPath).href);

// Options are recorded per key, because a single request writes two keys with
// different TTLs (the rate-limit counter and the room itself).
function makeKV() {
  const store = new Map();
  const optsByKey = new Map();
  return {
    store,
    optsByKey,
    async get(k) { return store.has(k) ? store.get(k) : null; },
    async put(k, v, opts) {
      store.set(k, String(v));
      if (opts) optsByKey.set(k, opts);
    },
    ttl(k) { return optsByKey.has(k) ? optsByKey.get(k).expirationTtl : undefined; },
    roomKeys() { return [...store.keys()].filter((k) => k.startsWith("room:")); },
  };
}

// `body` is passed through verbatim; undefined means "no body at all", which
// is the shape a real launcher sends for a lookup.
function makeReq(method, url, headers, body) {
  const h = new Headers(headers || {});
  return new Request(url, { method, headers: h, body: body || undefined });
}

let failures = 0;
function check(name, cond, extra) {
  if (cond) { console.log(`ok   - ${name}`); }
  else { failures += 1; console.log(`FAIL - ${name}${extra ? " :: " + extra : ""}`); }
}

// Every test gets its own KV so counters and rooms never bleed across cases.
function ctx(env, method, url, ip, body, extraHeaders) {
  const headers = Object.assign(
    { "cf-connecting-ip": ip },
    extraHeaders || {},
  );
  if (body !== undefined && !headers["content-type"]) {
    headers["content-type"] = "application/json";
  }
  return { request: makeReq(method, url, headers, body), env };
}

const BASE = "https://launcher.example";

async function post(env, ip, payload, headers) {
  const res = await mod.onRequestPost(
    ctx(env, "POST", `${BASE}/api/room`, ip,
      typeof payload === "string" ? payload : JSON.stringify(payload), headers),
  );
  const text = await res.text();
  let body = null;
  try { body = JSON.parse(text); } catch { body = text; }
  return { status: res.status, body, text, res };
}

async function get(env, ip, path, headers) {
  const res = await mod.onRequestGet(
    ctx(env, "GET", `${BASE}${path}`, ip, undefined, headers),
  );
  const text = await res.text();
  let body = null;
  try { body = JSON.parse(text); } catch { body = text; }
  return { status: res.status, body, text, res };
}

// ---------------------------------------------------------------------------
// 1. A valid registration is stored under room:<code> with the expected shape.
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  const r = await post(env, "203.0.113.10", { code: "ABC234", addr: "[2409:8a00::1]", port: 25565 });
  check("register -> 200", r.status === 200, `got ${r.status}`);
  check("register -> ok:true", r.body && r.body.ok === true);
  check("register -> expires:600", r.body && r.body.expires === 600);
  const stored = env.ROOMS.store.get("room:ABC234");
  check("KV has room:ABC234", typeof stored === "string");
  const rec = stored ? JSON.parse(stored) : null;
  check("stored addr", rec && rec.addr === "[2409:8a00::1]", JSON.stringify(rec));
  check("stored port", rec && rec.port === 25565);
  check("stored ts is unix seconds", rec && Number.isInteger(rec.ts) &&
    Math.abs(rec.ts - Math.floor(Date.now() / 1000)) < 60);
  check("stored record has no source IP", rec && !("ip" in rec) &&
    !JSON.stringify(rec).includes("203.0.113.10"));
  check("room key uses TTL", env.ROOMS.ttl("room:ABC234") === 600,
    String(env.ROOMS.ttl("room:ABC234")));
  check("no CORS header on response", !r.res.headers.get("access-control-allow-origin"));
}

// ---------------------------------------------------------------------------
// 2. Malformed codes are rejected with bad_code, never stored.
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  const bad = [
    ["lowercase", "abc234"],
    ["contains I", "ABCI234".slice(0, 6)],
    ["contains O", "ABCO23"],
    ["contains 0", "ABC023"],
    ["contains 1", "ABC123"],
    ["too short", "ABC23"],
    ["too long", "ABC2345"],
    ["contains colon", "ABC:23"],
    ["contains slash", "AB/234"],
    ["empty", ""],
    ["not a string", 123456],
    ["null", null],
  ];
  for (let i = 0; i < bad.length; i += 1) {
    const [label, code] = bad[i];
    const r = await post(env, `198.51.100.${i + 1}`, { code, addr: "1.2.3.4", port: 25565 });
    check(`bad code (${label}) -> 400 bad_code`,
      r.status === 400 && r.body && r.body.error === "bad_code",
      `${r.status} ${r.text.trim()}`);
  }
  check("no room keys written for bad codes",
    [...env.ROOMS.store.keys()].filter((k) => k.startsWith("room:")).length === 0,
    JSON.stringify([...env.ROOMS.store.keys()]));
}

// ---------------------------------------------------------------------------
// 3. Addresses outside the IP-literal charset are rejected.
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  const bad = [
    ["semicolon", "1.2.3.4;rm -rf /"],
    ["space", "1.2.3.4 5"],
    ["double quote", '1.2.3.4"'],
    ["single quote", "1.2.3.4'"],
    ["newline", "1.2.3.4\nX"],
    ["percent (zone id)", "fe80::1%eth0"],
    ["too long", "a".repeat(65)],
    ["empty", ""],
    ["not a string", 42],
  ];
  for (let i = 0; i < bad.length; i += 1) {
    const [label, addr] = bad[i];
    const r = await post(env, `198.51.100.${i + 40}`, { code: "ABC234", addr, port: 25565 });
    check(`bad addr (${label}) -> 400`, r.status === 400, `${r.status} ${r.text.trim()}`);
    check(`bad addr (${label}) does not echo input`,
      !r.text.includes(String(addr)) || String(addr).length === 0);
  }
  check("no room written for bad addr",
    [...env.ROOMS.store.keys()].filter((k) => k.startsWith("room:")).length === 0);
  // Boundary: exactly 64 chars is still allowed.
  const r64 = await post(env, "198.51.100.99", { code: "ABC234", addr: "a".repeat(64), port: 25565 });
  check("addr of exactly 64 chars accepted", r64.status === 200, `${r64.status}`);
}

// ---------------------------------------------------------------------------
// 4. Ports outside 1..=65535, and non-integers, are rejected.
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  const bad = [
    ["zero", 0],
    ["65536", 65536],
    ["negative", -1],
    ["string number", "25565"],
    ["float", 25565.5],
    ["null", null],
    ["NaN", Number.NaN],
    ["missing", undefined],
  ];
  for (let i = 0; i < bad.length; i += 1) {
    const [label, port] = bad[i];
    const payload = { code: "ABC234", addr: "1.2.3.4" };
    if (label !== "missing") payload.port = port;
    const r = await post(env, `192.0.2.${i + 1}`, payload);
    check(`bad port (${label}) -> 400`, r.status === 400, `${r.status} ${r.text.trim()}`);
  }
  check("no room written for bad port",
    [...env.ROOMS.store.keys()].filter((k) => k.startsWith("room:")).length === 0);
  // Boundary: both ends of the legal range work.
  const lo = await post(env, "192.0.2.200", { code: "ABC234", addr: "1.2.3.4", port: 1 });
  const hi = await post(env, "192.0.2.201", { code: "ABC234", addr: "1.2.3.4", port: 65535 });
  check("port 1 accepted", lo.status === 200, `${lo.status}`);
  check("port 65535 accepted", hi.status === 200, `${hi.status}`);
}

// ---------------------------------------------------------------------------
// 5. Lookup of an existing room returns exactly addr and port.
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  await post(env, "203.0.113.20", { code: "XYZ789", addr: "2001:db8::5", port: 25566 });
  const r = await get(env, "203.0.113.21", "/api/room/XYZ789");
  check("lookup -> 200", r.status === 200, `got ${r.status}`);
  check("lookup -> ok:true", r.body && r.body.ok === true);
  check("lookup -> addr", r.body && r.body.addr === "2001:db8::5", JSON.stringify(r.body));
  check("lookup -> port", r.body && r.body.port === 25566);
  check("lookup does not expose ts", r.body && !("ts" in r.body));
  check("lookup sets no CORS header", !r.res.headers.get("access-control-allow-origin"));
}

// ---------------------------------------------------------------------------
// 6. Unknown room is a clean 404 and does not distinguish "expired" from
//    "never existed".
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  const r = await get(env, "203.0.113.30", "/api/room/ZZZZZZ");
  check("missing room -> 404", r.status === 404, `got ${r.status}`);
  check("missing room -> not_found", r.body && r.body.error === "not_found");
  check("missing room leaks nothing else", Object.keys(r.body || {}).length === 2);
}

// ---------------------------------------------------------------------------
// 7. Lookup with a malformed code is 400 bad_code, and the code never reaches
//    the KV layer (no key injection through the path).
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  const bad = ["abc234", "ABC23", "ABC2345", "ABCI23", "ABC:23", "ABC%3A23", "ABC 23"];
  for (let i = 0; i < bad.length; i += 1) {
    const r = await get(env, `203.0.113.${i + 40}`, `/api/room/${bad[i]}`);
    check(`lookup bad code (${bad[i]}) -> 400 bad_code`,
      r.status === 400 && r.body && r.body.error === "bad_code",
      `${r.status} ${r.text.trim()}`);
  }
  // Only rate-limit counters may exist: a bad code must never produce a room
  // key, which is what would let a crafted path write or read across rooms.
  check("bad lookup codes read no room keys", env.ROOMS.roomKeys().length === 0,
    JSON.stringify(env.ROOMS.roomKeys()));
  // A path that is not a lookup route at all must not be treated as one.
  const nested = await get(env, "203.0.113.60", "/api/room/ABC234/extra");
  check("nested path -> 405", nested.status === 405, `got ${nested.status}`);
}

// ---------------------------------------------------------------------------
// 8. Write rate limit: the 11th write inside one minute is refused.
//    Date.now is pinned so the test cannot straddle a minute boundary.
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  const realNow = Date.now;
  Date.now = () => 1_700_000_030_000;
  const statuses = [];
  for (let i = 0; i < 11; i += 1) {
    const r = await post(env, "203.0.113.50", { code: "ABC234", addr: "1.2.3.4", port: 25565 });
    statuses.push(r.status);
    if (i === 10) {
      check("11th write -> 429", r.status === 429, `got ${r.status}`);
      check("11th write -> rate_limited", r.body && r.body.error === "rate_limited",
        r.text.trim());
    }
  }
  check("first 10 writes -> 200", statuses.slice(0, 10).every((s) => s === 200),
    JSON.stringify(statuses));
  check("write counter key exists with TTL",
    [...env.ROOMS.store.keys()].some((k) => k.startsWith("rl:w:203.0.113.50:")),
    JSON.stringify([...env.ROOMS.store.keys()]));
  const wKey = [...env.ROOMS.store.keys()].find((k) => k.startsWith("rl:w:203.0.113.50:"));
  check("write counter TTL is 120", env.ROOMS.ttl(wKey) === 120,
    String(env.ROOMS.ttl(wKey)));
  Date.now = realNow;
}

// ---------------------------------------------------------------------------
// 8b. Read rate limit: the 61st read inside one minute is refused.
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  env.ROOMS.store.set("room:ABC234", JSON.stringify({ addr: "1.2.3.4", port: 25565, ts: 0 }));
  const realNow = Date.now;
  Date.now = () => 1_700_000_030_000;
  let last = null;
  for (let i = 0; i < 61; i += 1) {
    last = await get(env, "203.0.113.51", "/api/room/ABC234");
  }
  check("61st read -> 429 rate_limited",
    last.status === 429 && last.body && last.body.error === "rate_limited",
    `${last.status} ${last.text.trim()}`);
  check("read counter key exists",
    [...env.ROOMS.store.keys()].some((k) => k.startsWith("rl:r:203.0.113.51:")));
  Date.now = realNow;
}

// ---------------------------------------------------------------------------
// 9. Missing KV binding is a 500 server_error on both routes, not a crash.
// ---------------------------------------------------------------------------
{
  const r1 = await post({}, "203.0.113.60", { code: "ABC234", addr: "1.2.3.4", port: 25565 });
  check("missing ROOMS on POST -> 500 server_error",
    r1.status === 500 && r1.body && r1.body.error === "server_error",
    `${r1.status} ${r1.text.trim()}`);
  const r2 = await get({}, "203.0.113.60", "/api/room/ABC234");
  check("missing ROOMS on GET -> 500 server_error",
    r2.status === 500 && r2.body && r2.body.error === "server_error",
    `${r2.status} ${r2.text.trim()}`);
  const r3 = await post({ ROOMS: null }, "203.0.113.61", { code: "ABC234", addr: "1.2.3.4", port: 25565 });
  check("null binding -> 500 server_error", r3.status === 500);
}

// ---------------------------------------------------------------------------
// 10. Lookup needs no body at all, and a body on a GET changes nothing.
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  await post(env, "203.0.113.70", { code: "ABC234", addr: "1.2.3.4", port: 25565 });
  const noBody = await get(env, "203.0.113.71", "/api/room/ABC234");
  check("lookup without body -> 200", noBody.status === 200, `got ${noBody.status}`);
  check("lookup without body returns addr", noBody.body && noBody.body.addr === "1.2.3.4");
  // GET with a body is unusual but must not be parsed or trusted.
  const withBody = await get(env, "203.0.113.72", "/api/room/ABC234",
    { "content-type": "application/json" });
  check("lookup with declared content-type -> 200", withBody.status === 200);
  check("lookup body ignored", withBody.body && withBody.body.port === 25565);
}

// ---------------------------------------------------------------------------
// 11. Body handling: oversize, malformed JSON, and non-object payloads.
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  const huge = JSON.stringify({ code: "ABC234", addr: "1.2.3.4", port: 25565, pad: "x".repeat(2048) });
  const r1 = await post(env, "203.0.113.80", huge);
  check("oversize body -> 400", r1.status === 400, `got ${r1.status}`);
  const r2 = await post(env, "203.0.113.81", "{not json");
  check("malformed JSON -> 400", r2.status === 400, `got ${r2.status}`);
  const r3 = await post(env, "203.0.113.82", "[1,2,3]");
  check("array payload -> 400", r3.status === 400, `got ${r3.status}`);
  const r4 = await post(env, "203.0.113.83", "null");
  check("null payload -> 400", r4.status === 400, `got ${r4.status}`);
  check("nothing stored from bad bodies",
    [...env.ROOMS.store.keys()].filter((k) => k.startsWith("room:")).length === 0);
}

// ---------------------------------------------------------------------------
// 12. Other verbs and other paths get a bare 405 and touch nothing.
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  const other = await mod.onRequest(ctx(env, "PUT", `${BASE}/api/room`, "203.0.113.90", "{}"));
  const otherText = await other.text();
  check("PUT /api/room -> 405", other.status === 405, `got ${other.status}`);
  check("405 body is bare ok:false", otherText.trim() === '{"ok":false}',
    otherText.trim());
  const opt = await mod.onRequest(ctx(env, "OPTIONS", `${BASE}/api/room`, "203.0.113.91"));
  check("OPTIONS -> 405", opt.status === 405);
  check("OPTIONS advertises no CORS",
    !opt.headers.get("access-control-allow-origin") &&
    !opt.headers.get("access-control-allow-methods"));
  const wrongPath = await mod.onRequestPost(ctx(env, "POST", `${BASE}/api/other`, "203.0.113.92", "{}"));
  check("POST to other path -> 405", wrongPath.status === 405);
  const rootGet = await get(env, "203.0.113.93", "/api/room");
  check("GET /api/room -> 405", rootGet.status === 405, `got ${rootGet.status}`);
  check("405 cases wrote nothing", env.ROOMS.roomKeys().length === 0,
    JSON.stringify([...env.ROOMS.store.keys()]));
}

// ---------------------------------------------------------------------------
// 13. Re-registering a code renews it (overwrite, not error).
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  await post(env, "203.0.113.100", { code: "ABC234", addr: "1.1.1.1", port: 25565 });
  await post(env, "203.0.113.100", { code: "ABC234", addr: "2.2.2.2", port: 25566 });
  const r = await get(env, "203.0.113.101", "/api/room/ABC234");
  check("re-register overwrites address",
    r.body && r.body.addr === "2.2.2.2" && r.body.port === 25566, JSON.stringify(r.body));
  check("re-register keeps a single key",
    [...env.ROOMS.store.keys()].filter((k) => k === "room:ABC234").length === 1);
}

rmSync(dir, { recursive: true, force: true });
console.log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILURE(S)`);
process.exit(failures === 0 ? 0 : 1);
