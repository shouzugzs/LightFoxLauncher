// SPDX-License-Identifier: MPL-2.0
// Local harness: exercise site/functions/api/room.js without Cloudflare.
// Creates a fake KV in memory, imports the function module, fires requests at
// it, and prints an ok/FAIL line per assertion.
// Run: node tools/room-selftest.mjs
import { readFileSync, writeFileSync, mkdtempSync, mkdirSync, rmSync } from "node:fs";
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
// room.js imports the shared capture module as `../_capture.js`, so the
// temporary copy needs that file in the same relative position. Without it the
// import fails before a single assertion runs.
const captureSrc = readFileSync(
  new URL("../site/functions/_capture.js", import.meta.url),
  "utf8",
);
const dir = mkdtempSync(join(tmpdir(), "room-test-"));
const apiDir = join(dir, "api");
mkdirSync(apiDir, { recursive: true });
// Both files must be `.mjs` in the temp copy: Node treats a bare `.js` as
// CommonJS (the repo has no `type` field in package.json), and an ESM named
// import from a CommonJS module fails with "Named export not found". The
// extension is a harness detail — the deployed files stay `.js` for Pages.
writeFileSync(join(dir, "_capture.mjs"), captureSrc);
const modPath = join(apiDir, "room.mjs");
writeFileSync(modPath, src.replace('"../_capture.js"', '"../_capture.mjs"'));
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
let checks = 0;
function check(name, cond, extra) {
  checks += 1;
  if (cond) { console.log(`ok   - ${name}`); }
  else { failures += 1; console.log(`FAIL - ${name}${extra ? " :: " + extra : ""}`); }
}

// Every test gets its own KV so counters and rooms never bleed across cases.
//
// `waitUntil` is part of the Pages context contract and the endpoint now hands
// the capture promise to it, because a Workers isolate can be torn down the
// moment the response is returned — a bare floating promise is simply
// cancelled. The harness plays the platform here: it collects what was handed
// over so a test can await it instead of racing the microtask queue.
function ctx(env, method, url, ip, body, extraHeaders) {
  const headers = Object.assign(
    { "cf-connecting-ip": ip },
    extraHeaders || {},
  );
  if (body !== undefined && !headers["content-type"]) {
    headers["content-type"] = "application/json";
  }
  const waits = [];
  return {
    request: makeReq(method, url, headers, body),
    env,
    waitUntil(p) { waits.push(Promise.resolve(p)); },
    waits,
  };
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

// The lookup route is `GET /api/room?code=<CODE>` — a query parameter, not a
// path segment, because Pages is a file router and `site/functions/api/room.js`
// maps only to `/api/room`. The code is percent-encoded here the way a real
// client builds it; the server decodes it back before validating.
function lookupPath(code) {
  return `/api/room?code=${encodeURIComponent(String(code))}`;
}

// The retired `/api/room/<CODE>` shape. No longer a route, but a stale client
// can still produce it, so its answer is pinned below.
function retiredPath(code) {
  return `/api/room/${code}`;
}

// onRequest is what Pages calls for a path it has no specific handler for.
// Reached directly here so the fallback is exercised without Cloudflare.
async function fallback(env, method, path, ip, body) {
  const res = await mod.onRequest(ctx(env, method, `${BASE}${path}`, ip, body));
  const text = await res.text();
  let parsed = null;
  try { parsed = JSON.parse(text); } catch { parsed = text; }
  return { status: res.status, body: parsed, text, res };
}

// ---------------------------------------------------------------------------
// Honeypot harness.
//
// capture() is deliberately NOT awaited by the endpoint (see noteHostile in
// room.js): a 400 must never wait on a KV write. That means a response can be
// returned while the capture is still in flight, so every assertion about the
// HP namespace drains the queue first. One macrotask turn is enough — capture()
// awaits only promises that are already resolved — but the timer turn is kept
// as well so a future `await` on real I/O does not silently break the tests.
// ---------------------------------------------------------------------------
async function settle() {
  await new Promise((r) => setImmediate(r));
  await new Promise((r) => setTimeout(r, 0));
}

// An env carrying both bindings, plus the raw KV handles so a test can read
// what was captured without reaching back through the env object. The room
// tests above deliberately stay HP-less: they are the "no capture configured"
// baseline, and they prove the endpoint works with the binding absent.
function makeEnv() {
  const rooms = makeKV();
  const hp = makeKV();
  return { env: { ROOMS: rooms, HP: hp }, rooms, hp };
}

// Captured events are the `hit:` keys; `cap:` keys are the per-IP counters.
function hitKeys(hp) {
  return [...hp.store.keys()].filter((k) => k.startsWith("hit:"));
}

function events(hp) {
  return hitKeys(hp).map((k) => JSON.parse(hp.store.get(k)));
}

function capKeys(hp) {
  return [...hp.store.keys()].filter((k) => k.startsWith("cap:"));
}

// Dispatch the way Pages does — GET and POST have their own handlers, every
// other verb falls through to the catch-all — so one table can mix verbs.
// `payload` may be an object (JSON-encoded), a raw string, or undefined.
//
// `drain: false` skips awaiting the waitUntil promises. The default is to await
// them, which is the harness standing in for the platform: in production the
// isolate stays alive until they settle, so a test that skipped this would be
// asserting on a capture that had not necessarily been written yet. The escape
// hatch exists for the test that a *hanging* HP must not stall a response —
// there, waiting is precisely the wrong thing to do.
async function send(env, method, path, ip, payload, headers, opts) {
  const body = payload === undefined
    ? undefined
    : typeof payload === "string" ? payload : JSON.stringify(payload);
  const c = ctx(env, method, `${BASE}${path}`, ip, body, headers);
  const res = method === "GET" ? await mod.onRequestGet(c)
    : method === "POST" ? await mod.onRequestPost(c)
      : await mod.onRequest(c);
  const text = await res.text();
  if (!opts || opts.drain !== false) {
    await Promise.all(c.waits);
    await settle();
  }
  let parsed = null;
  try { parsed = JSON.parse(text); } catch { parsed = text; }
  return { status: res.status, body: parsed, text, res, waits: c.waits };
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
  const r = await get(env, "203.0.113.21", lookupPath("XYZ789"));
  check("lookup -> 200", r.status === 200, `got ${r.status}`);
  check("lookup -> ok:true", r.body && r.body.ok === true);
  check("lookup -> addr", r.body && r.body.addr === "2001:db8::5", JSON.stringify(r.body));
  check("lookup -> port", r.body && r.body.port === 25566);
  check("lookup does not expose ts", r.body && !("ts" in r.body));
  check("lookup sets no CORS header", !r.res.headers.get("access-control-allow-origin"));
  // The route is the endpoint itself plus a query string, so a trailing slash
  // on the path must still find the handler.
  const slash = await get(env, "203.0.113.22", `/api/room/?code=XYZ789`);
  check("lookup via /api/room/?code= -> 200", slash.status === 200, `got ${slash.status}`);
}

// ---------------------------------------------------------------------------
// 6. Unknown room is a clean 404 and does not distinguish "expired" from
//    "never existed".
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  const r = await get(env, "203.0.113.30", lookupPath("ZZZZZZ"));
  check("missing room -> 404", r.status === 404, `got ${r.status}`);
  check("missing room -> not_found", r.body && r.body.error === "not_found");
  check("missing room leaks nothing else", Object.keys(r.body || {}).length === 2);
}

// ---------------------------------------------------------------------------
// 7. Lookup with a malformed code is 400 bad_code, and the code never reaches
//    the KV layer (no key injection through the query string).
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  const bad = ["abc234", "ABC23", "ABC2345", "ABCI23", "ABC:23", "ABC%3A23", "ABC 23",
    "AB/234", "AB%2F234", "ABC234\\n"];
  for (let i = 0; i < bad.length; i += 1) {
    const r = await get(env, `203.0.113.${i + 40}`, lookupPath(bad[i]));
    check(`lookup bad code (${JSON.stringify(bad[i])}) -> 400 bad_code`,
      r.status === 400 && r.body && r.body.error === "bad_code",
      `${r.status} ${r.text.trim()}`);
  }
  // Only rate-limit counters may exist: a bad code must never produce a room
  // key, which is what would let a crafted query read across rooms.
  check("bad lookup codes read no room keys", env.ROOMS.roomKeys().length === 0,
    JSON.stringify(env.ROOMS.roomKeys()));

  // A missing code parameter and an empty one are the same answer.
  for (const [label, path] of [
    ["no parameter", "/api/room"],
    ["empty value", "/api/room?code="],
    ["whitespace value", "/api/room?code=%20"],
    ["repeated, first bad", "/api/room?code=&code=ABC234"],
    ["repeated, first good", "/api/room?code=ABC234&code="],
  ]) {
    const r = await get(env, "203.0.113.61", path);
    const expected = label === "repeated, first good" ? 404 : 400;
    check(`lookup ${label} -> ${expected}`,
      r.status === expected, `${r.status} ${r.text.trim()}`);
  }
  const empty = await get(env, "203.0.113.62", "/api/room?code=");
  check("lookup with empty code -> bad_code",
    empty.body && empty.body.error === "bad_code", empty.text.trim());

  // Extra parameters are ignored: the launcher appends nothing today, but a
  // future one may add a cache-buster and must not break the lookup.
  const extra = await get(env, "203.0.113.63", "/api/room?code=ABC234&extra=1");
  check("lookup with an extra parameter still answers 404 not_found",
    extra.status === 404 && extra.body && extra.body.error === "not_found",
    `${extra.status} ${extra.text.trim()}`);
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
    last = await get(env, "203.0.113.51", lookupPath("ABC234"));
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
  const r2 = await get({}, "203.0.113.60", lookupPath("ABC234"));
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
  const noBody = await get(env, "203.0.113.71", lookupPath("ABC234"));
  check("lookup without body -> 200", noBody.status === 200, `got ${noBody.status}`);
  check("lookup without body returns addr", noBody.body && noBody.body.addr === "1.2.3.4");
  // GET with a body is unusual but must not be parsed or trusted.
  const withBody = await get(env, "203.0.113.72", lookupPath("ABC234"),
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
// 11b. `oversize_body`: the one hostile reason that has to be decided before
//      the 1 KB cap answers.
//
// readCappedBody() rejects anything past 1 KB, so by the time the field checks
// in classifyHostile() could run there is no payload left. The oversize test
// therefore sits ahead of that call, on Content-Length alone. The three
// boundaries below are what keeps it from swallowing ordinary clients:
//
//   * 1025..8192 bytes — an over-chatty launcher. Plain 400, NOT captured.
//   * > 8192 bytes     — a scanner uploading a payload. Captured, same 400.
//   * no Content-Length — nothing to measure, so nothing is claimed; the
//     decoded length still rejects the body, it is just not called hostile.
// ---------------------------------------------------------------------------
{
  // Past 8 KB with a declared length: captured as oversize_body, answer
  // unchanged at 400 bad_request.
  {
    const { env, hp } = makeEnv();
    const big = "x".repeat(9000);
    const r = await send(env, "POST", "/api/room", "198.25.0.1", big,
      { "content-type": "application/json", "content-length": String(big.length) });
    await settle();
    const evs = events(hp);
    check("oversize body (>8192) -> 400 bad_request, unchanged",
      r.status === 400 && r.body && r.body.error === "bad_request",
      `${r.status} ${r.text.trim()}`);
    check("oversize body (>8192) captured exactly once", evs.length === 1,
      `${evs.length} captures`);
    check("oversize body reason is oversize_body",
      evs.length === 1 && evs[0].reason === "oversize_body",
      JSON.stringify(evs.map((e) => e.reason)));
    check("oversize body event is tagged source room",
      evs.length === 1 && evs[0].source === "room");
    check("oversize body event records the declared length",
      evs.length === 1 && evs[0].extra && evs[0].extra.content_length === 9000,
      JSON.stringify(evs[0] && evs[0].extra));
    check("oversize body wrote no room key", env.ROOMS.roomKeys().length === 0,
      JSON.stringify(env.ROOMS.roomKeys()));
  }

  // The 1 KB..8 KB band is a benign mistake: refused, but never recorded.
  {
    const { env, hp } = makeEnv();
    const mid = "x".repeat(2048);
    const r = await send(env, "POST", "/api/room", "198.25.0.2", mid,
      { "content-type": "application/json", "content-length": String(mid.length) });
    await settle();
    check("body between 1 KB and 8 KB -> 400 (still refused)",
      r.status === 400, `got ${r.status}`);
    check("body between 1 KB and 8 KB is NOT captured",
      hitKeys(hp).length === 0, JSON.stringify(events(hp).map((e) => e.reason)));
  }

  // Exactly 8192 is the boundary and must stay benign: the rule is `>`.
  {
    const { env, hp } = makeEnv();
    const edge = "x".repeat(8192);
    const r = await send(env, "POST", "/api/room", "198.25.0.3", edge,
      { "content-type": "application/json", "content-length": String(edge.length) });
    await settle();
    check("body of exactly 8192 -> 400", r.status === 400, `got ${r.status}`);
    check("body of exactly 8192 is NOT captured (boundary is exclusive)",
      hitKeys(hp).length === 0, JSON.stringify(events(hp).map((e) => e.reason)));
  }
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
  // GET on the endpoint with no `code` is no longer a 405: the route exists,
  // the parameter is what is missing.
  const rootGet = await get(env, "203.0.113.93", "/api/room");
  check("GET /api/room (no code) -> 400 bad_code",
    rootGet.status === 400 && rootGet.body && rootGet.body.error === "bad_code",
    `${rootGet.status} ${rootGet.text.trim()}`);
  const otherVerb = await fallback(env, "DELETE", "/api/room", "203.0.113.94");
  check("DELETE /api/room -> 405", otherVerb.status === 405, `got ${otherVerb.status}`);
  check("405 cases wrote nothing", env.ROOMS.roomKeys().length === 0,
    JSON.stringify([...env.ROOMS.store.keys()]));
}

// ---------------------------------------------------------------------------
// 12b. The retired `/api/room/<CODE>` path shape answers 400 bad_request.
//
// This is the bug the query parameter fixes: on Cloudflare Pages a path
// segment under the endpoint is answered by the static-asset 404 and never
// reaches this module. When it does reach it — a stale client, or `/api/room/`
// being routed to onRequest — the answer must be "bad_request", not 405
// (which blames the verb) and not 404 (which looks like an expired room).
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  env.ROOMS.store.set("room:ABC234", JSON.stringify({ addr: "1.2.3.4", port: 25565, ts: 0 }));

  // Directly on the GET handler: the handler must not treat the segment as a
  // code, even though a room under that code exists.
  const direct = await get(env, "203.0.113.110", retiredPath("ABC234"));
  check("GET /api/room/ABC234 (path form) -> 400",
    direct.status === 400, `got ${direct.status}`);
  check("GET /api/room/ABC234 (path form) -> bad_request",
    direct.body && direct.body.error === "bad_request", direct.text.trim());
  check("retired path does not leak the room",
    direct.body && direct.body.addr === undefined && direct.body.port === undefined,
    direct.text.trim());

  // Through onRequest, which is what Pages actually invokes for that path.
  const viaFallback = await fallback(env, "GET", retiredPath("ABC234"), "203.0.113.111");
  check("onRequest GET /api/room/ABC234 -> 400 bad_request",
    viaFallback.status === 400 && viaFallback.body &&
      viaFallback.body.error === "bad_request",
    `${viaFallback.status} ${viaFallback.text.trim()}`);
  check("retired path is not a 404 or 405",
    viaFallback.status !== 404 && viaFallback.status !== 405,
    `got ${viaFallback.status}`);

  // A nested segment is the same retired shape, not a different route.
  const nested = await fallback(env, "GET", "/api/room/ABC234/extra", "203.0.113.112");
  check("nested retired path -> 400 bad_request",
    nested.status === 400 && nested.body && nested.body.error === "bad_request",
    `${nested.status} ${nested.text.trim()}`);

  // A malformed code in the retired position is still bad_request: the
  // complaint is about the route shape, which is checked first.
  const retiredBad = await fallback(env, "GET", retiredPath("abc234"), "203.0.113.113");
  check("retired path with a bad code -> 400 bad_request",
    retiredBad.status === 400 && retiredBad.body &&
      retiredBad.body.error === "bad_request",
    `${retiredBad.status} ${retiredBad.text.trim()}`);

  // `/api/room/` (trailing slash, no segment) is the register path, not a
  // retired lookup, so it keeps the plain 405 on a verb it does not serve.
  const trailing = await fallback(env, "PUT", "/api/room/", "203.0.113.114", "{}");
  check("PUT /api/room/ -> 405 (register path with slash)",
    trailing.status === 405, `got ${trailing.status}`);

  // None of the above may have read or written a room key.
  check("retired paths leave the room untouched",
    env.ROOMS.roomKeys().length === 1, JSON.stringify(env.ROOMS.roomKeys()));
  check("retired paths do not charge the read budget",
    [...env.ROOMS.store.keys()].every((k) => !k.startsWith("rl:r:")),
    JSON.stringify([...env.ROOMS.store.keys()]));

  // Every verb agrees on the retired shape: a POST to it is not a 405 that
  // would blame the verb.
  const retiredPost = await mod.onRequestPost(
    ctx(env, "POST", `${BASE}${retiredPath("ABC234")}`, "203.0.113.115",
      JSON.stringify({ code: "ABC234", addr: "1.2.3.4", port: 25565 })),
  );
  const retiredPostText = await retiredPost.text();
  check("POST /api/room/ABC234 -> 400 bad_request",
    retiredPost.status === 400 && retiredPostText.includes("bad_request"),
    `${retiredPost.status} ${retiredPostText.trim()}`);
  // ... and it did not overwrite the room that is there.
  check("retired POST did not touch the room",
    JSON.parse(env.ROOMS.store.get("room:ABC234")).addr === "1.2.3.4" &&
      JSON.parse(env.ROOMS.store.get("room:ABC234")).port === 25565,
    env.ROOMS.store.get("room:ABC234"));
}

// ---------------------------------------------------------------------------
// 12c. The catch-all forwards GET/POST on the register path to the real
//      handlers. Pages normally dispatches `onRequestGet`/`onRequestPost`
//      itself, so this is a belt-and-braces path — but if a routing change
//      ever sends the request here, the answer must be the real one, not 405.
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  env.ROOMS.store.set("room:ABC234", JSON.stringify({ addr: "1.2.3.4", port: 25565, ts: 0 }));
  const fwdGet = await fallback(env, "GET", "/api/room?code=ABC234", "203.0.113.116");
  check("onRequest GET with code -> 200 (forwarded)",
    fwdGet.status === 200 && fwdGet.body && fwdGet.body.addr === "1.2.3.4",
    `${fwdGet.status} ${fwdGet.text.trim()}`);
  const fwdPost = await fallback(env, "POST", "/api/room", "203.0.113.117",
    JSON.stringify({ code: "NEW234", addr: "5.6.7.8", port: 25570 }));
  check("onRequest POST -> 200 (forwarded)",
    fwdPost.status === 200 && fwdPost.body && fwdPost.body.ok === true,
    `${fwdPost.status} ${fwdPost.text.trim()}`);
  check("forwarded POST stored the room",
    JSON.parse(env.ROOMS.store.get("room:NEW234")).addr === "5.6.7.8",
    env.ROOMS.store.get("room:NEW234"));
  // The forward is a delegation, not a bypass: a GET with no code is still
  // bad_code, not a 200.
  const fwdNoCode = await fallback(env, "GET", "/api/room", "203.0.113.118");
  check("onRequest GET without code -> 400 bad_code",
    fwdNoCode.status === 400 && fwdNoCode.body && fwdNoCode.body.error === "bad_code",
    `${fwdNoCode.status} ${fwdNoCode.text.trim()}`);
}

// ---------------------------------------------------------------------------
// 13. Re-registering a code renews it (overwrite, not error).
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  await post(env, "203.0.113.100", { code: "ABC234", addr: "1.1.1.1", port: 25565 });
  await post(env, "203.0.113.100", { code: "ABC234", addr: "2.2.2.2", port: 25566 });
  const r = await get(env, "203.0.113.101", lookupPath("ABC234"));
  check("re-register overwrites address",
    r.body && r.body.addr === "2.2.2.2" && r.body.port === 25566, JSON.stringify(r.body));
  check("re-register keeps a single key",
    [...env.ROOMS.store.keys()].filter((k) => k === "room:ABC234").length === 1);
}

// ---------------------------------------------------------------------------
// 14. Hostile-input capture: benign mistakes are NOT recorded.
//
// The endpoint is public and gets scanned, so a matching request is written to
// the honeypot KV (binding HP). The line is drawn at *shapes a launcher never
// produces*: a typo, a lowercase code, a missing port, an expired room and a
// malformed body are ordinary and must stay out of the honeypot, or real
// probing would be invisible in the noise. Each case below asserts both halves:
// the response is unchanged, and HP stays empty.
// ---------------------------------------------------------------------------
{
  const benign = [
    // [label, method, path, payload, expected status, expected error]
    ["lookup: code too short", "GET", "/api/room?code=ABC23", undefined, 400, "bad_code"],
    ["lookup: code too long", "GET", "/api/room?code=ABC2345", undefined, 400, "bad_code"],
    ["lookup: code contains I", "GET", "/api/room?code=ABI234", undefined, 400, "bad_code"],
    ["lookup: code contains O", "GET", "/api/room?code=ABO234", undefined, 400, "bad_code"],
    ["lookup: code contains 0", "GET", "/api/room?code=AB0234", undefined, 400, "bad_code"],
    ["lookup: code contains 1", "GET", "/api/room?code=AB1234", undefined, 400, "bad_code"],
    // Lowercase is rejected, not upcased: the code is a case-sensitive
    // identifier, and silently folding it would hide a client bug.
    ["lookup: lowercase code", "GET", "/api/room?code=abc234", undefined, 400, "bad_code"],
    ["lookup: empty code", "GET", "/api/room?code=", undefined, 400, "bad_code"],
    ["lookup: no code parameter", "GET", "/api/room", undefined, 400, "bad_code"],
    ["lookup: unknown room", "GET", "/api/room?code=ZZZZZZ", undefined, 404, "not_found"],
    ["register: code too short", "POST", "/api/room",
      { code: "ABC23", addr: "1.2.3.4", port: 25565 }, 400, "bad_code"],
    ["register: code too long", "POST", "/api/room",
      { code: "ABC2345", addr: "1.2.3.4", port: 25565 }, 400, "bad_code"],
    ["register: lowercase code", "POST", "/api/room",
      { code: "abc234", addr: "1.2.3.4", port: 25565 }, 400, "bad_code"],
    ["register: empty code", "POST", "/api/room",
      { code: "", addr: "1.2.3.4", port: 25565 }, 400, "bad_code"],
    // A missing port is an older client, not a probe: classifyHostile()
    // deliberately skips undefined/null and leaves it to validation.
    ["register: missing port", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4" }, 400, "bad_request"],
    // A *number* out of range is an ordinary mistake — an emptied input box
    // gives 0, a typo gives 65536 — and is deliberately not captured. Only a
    // non-number port is hostile; see section 15.
    ["register: port 0", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4", port: 0 }, 400, "bad_request"],
    ["register: port 65536", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4", port: 65536 }, 400, "bad_request"],
    ["register: port -1", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4", port: -1 }, 400, "bad_request"],
    ["register: port null", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4", port: null }, 400, "bad_request"],
    ["register: malformed JSON", "POST", "/api/room", "{not json", 400, "bad_request"],
    ["register: valid", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4", port: 25565 }, 200, undefined],
  ];

  for (let i = 0; i < benign.length; i += 1) {
    const [label, method, path, payload, status, error] = benign[i];
    const { env, hp } = makeEnv();
    const r = await send(env, method, path, `198.18.${i}.1`, payload);
    await settle();
    const okResponse = r.status === status &&
      (error === undefined ? r.body && r.body.ok === true : r.body && r.body.error === error);
    check(`benign (${label}) -> ${status}${error ? " " + error : ""} and not captured`,
      okResponse && hitKeys(hp).length === 0,
      `${r.status} ${r.text.trim()} hits=${JSON.stringify(events(hp).map((e) => e.reason))}`);
    check(`benign (${label}) wrote no capture counter`, capKeys(hp).length === 0,
      JSON.stringify(capKeys(hp)));
  }
}

// ---------------------------------------------------------------------------
// 15. Hostile input IS captured, with the right reason, and the response is
//     byte-for-byte what it was before capture existed.
//
// The `expected` field is the *pre-capture* answer, pinned literally rather
// than derived: capture must be a side effect only, so if a future refactor
// ever lets it change a status or a body, this table catches it. Only one
// capture is expected per request (the rate limit allows five per IP per
// window), and every event must carry source "room" so tools/honeypot-export
// can tell these apart from the decoy pages.
// ---------------------------------------------------------------------------
{
  const hostile = [
    // [label, method, path, payload, expected reason, expected status, expected error]
    ["code with NUL", "POST", "/api/room",
      { code: "AB\u0000CD", addr: "1.2.3.4", port: 25565 },
      "control_chars", 400, "bad_code"],
    ["code with newline", "POST", "/api/room",
      { code: "ABC23\n", addr: "1.2.3.4", port: 25565 },
      "control_chars", 400, "bad_code"],
    ["code with DEL", "POST", "/api/room",
      { code: "ABC23\u007f", addr: "1.2.3.4", port: 25565 },
      "control_chars", 400, "bad_code"],
    ["code with <script>", "POST", "/api/room",
      { code: "<script>", addr: "1.2.3.4", port: 25565 },
      "injection_chars", 400, "bad_code"],
    ["code with single quote", "POST", "/api/room",
      { code: "ABC'23", addr: "1.2.3.4", port: 25565 },
      "injection_chars", 400, "bad_code"],
    ["code with double quote", "POST", "/api/room",
      { code: 'ABC"23', addr: "1.2.3.4", port: 25565 },
      "injection_chars", 400, "bad_code"],
    ["addr with semicolon", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4;rm -rf /", port: 25565 },
      "injection_chars", 400, "bad_request"],
    ["addr with space", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4 5", port: 25565 },
      "injection_chars", 400, "bad_request"],
    ["addr with backtick", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4`id`", port: 25565 },
      "injection_chars", 400, "bad_request"],
    ["port as string", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4", port: "25565" },
      "non_numeric_port", 400, "bad_request"],
    ["port as object", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4", port: { value: 25565 } },
      "non_numeric_port", 400, "bad_request"],
    ["port as array", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4", port: [25565] },
      "non_numeric_port", 400, "bad_request"],
    ["port float", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4", port: 25565.5 },
      "non_numeric_port", 400, "bad_request"],
    ["port as bool", "POST", "/api/room",
      { code: "ABC234", addr: "1.2.3.4", port: true },
      "non_numeric_port", 400, "bad_request"],
    // NOTE: a *number* out of range (-1, 0, 65536) is deliberately NOT
    // hostile — classifyHostile() in room.js treats it as an ordinary mistake,
    // like a mistyped code, because an emptied port box produces 0 and a typo
    // produces 65536. Those cases are asserted as benign in section 14; the
    // hostile reason "non_numeric_port" is reserved for a non-number.
    //
    // Traversal on the *path*, not in the query. A traversal attempt in the
    // code parameter never reaches classifyHostile as path_traversal: the
    // code is validated as a bad_code, so it is covered in section 15b.
    ["path with %00", "GET", "/api/room%00", undefined,
      "path_traversal", 400, "bad_code"],
    ["path with %2e%2e%2f", "GET", "/api/room%2e%2e%2fetc", undefined,
      "path_traversal", 400, "bad_code"],
    ["PUT on the endpoint", "PUT", "/api/room", "{}",
      "bad_method", 405, undefined],
  ];

  for (let i = 0; i < hostile.length; i += 1) {
    const [label, method, path, payload, reason, status, error] = hostile[i];
    const { env, hp } = makeEnv();
    const r = await send(env, method, path, `198.19.${i}.1`, payload);
    await settle();
    const evs = events(hp);

    // The response must be exactly the pre-capture answer.
    const responseOk = r.status === status &&
      (error === undefined
        ? r.body && r.body.ok === false && r.body.error === undefined
        : r.body && r.body.error === error);
    check(`hostile (${label}) -> ${status}${error ? " " + error : ""}, unchanged`,
      responseOk, `${r.status} ${r.text.trim()}`);

    // Exactly one capture, tagged as coming from the room endpoint.
    check(`hostile (${label}) -> captured once as room/${reason}`,
      evs.length === 1 && evs[0].source === "room" && evs[0].reason === reason,
      JSON.stringify(evs.map((e) => `${e.source}/${e.reason}`)));
    check(`hostile (${label}) -> capture records method and path`,
      evs.length === 1 && evs[0].method === method && typeof evs[0].path === "string" &&
        evs[0].path.startsWith("/api/room"),
      evs.length ? `${evs[0].method} ${evs[0].path}` : "no event");
    check(`hostile (${label}) -> capture has a timestamp and an id`,
      evs.length === 1 && typeof evs[0].time === "string" &&
        !Number.isNaN(Date.parse(evs[0].time)) &&
        hitKeys(hp).length === 1 && hitKeys(hp)[0].startsWith("hit:"),
      hitKeys(hp).join(","));
  }
}

// ---------------------------------------------------------------------------
// 15b. Where the hostile *code* actually lands.
//
// This section pins the behaviour that exists, which is narrower than the
// endpoint's own comment suggests, and is recorded here so the difference is
// visible instead of assumed:
//
//   * A traversal probe in the `code` parameter (`?code=..%2F..%2Fetc`) is
//     NOT path_traversal. `new URL()` resolves dot segments only in the path;
//     the query keeps "../../etc", and `classifyHostile` tests TRAVERSAL_RE
//     against `pathname`, which is the plain "/api/room". The request is then
//     answered as a plain bad_code and never captured. The same shape survives
//     a second decode (`%252e%252e%252f`) and is captured as injection_chars
//     because the `%` is what the code field rejects.
//   * A NUL in the query (`?code=%00`) IS captured, as control_chars — the
//     decoded U+0000 matches CONTROL_RE before any traversal rule is reached.
//   * A NUL in the *path* (`/api/room%00`) is path_traversal, but only because
//     the path survives percent-decoding: classifyHostile sees the raw
//     "/api/room%00" and TRAVERSAL_RE matches the literal "%00".
// ---------------------------------------------------------------------------
{
  // Traversal in the query: rejected as a bad code, NOT captured.
  {
    const { env, hp } = makeEnv();
    const r = await send(env, "GET", "/api/room?code=..%2F..%2Fetc", "198.20.0.1");
    await settle();
    check("query traversal ?code=..%2F..%2Fetc -> 400 bad_code",
      r.status === 400 && r.body && r.body.error === "bad_code",
      `${r.status} ${r.text.trim()}`);
    check("query traversal is not captured (pathname stays /api/room)",
      hitKeys(hp).length === 0, JSON.stringify(events(hp).map((e) => e.reason)));
  }
  // A NUL in the query is a control character, whichever rule sees it first.
  {
    const { env, hp } = makeEnv();
    const r = await send(env, "GET", "/api/room?code=%00", "198.20.0.2");
    await settle();
    const evs = events(hp);
    check("query %00 -> 400 bad_code", r.status === 400 &&
      r.body && r.body.error === "bad_code", `${r.status} ${r.text.trim()}`);
    check("query %00 captured as control_chars",
      evs.length === 1 && evs[0].reason === "control_chars",
      JSON.stringify(evs.map((e) => e.reason)));
  }
  // A path traversal on the register route reaches the GET handler as a
  // lookup, so the path rule fires but the answer is still the lookup answer.
  {
    const { env, hp } = makeEnv();
    const r = await send(env, "GET", "/api/room%2e%2e%2fetc?code=ABC234", "198.20.0.3");
    await settle();
    const evs = events(hp);
    check("path traversal with a valid code -> 404 not_found (lookup still runs)",
      r.status === 404 && r.body && r.body.error === "not_found",
      `${r.status} ${r.text.trim()}`);
    check("path traversal captured as path_traversal",
      evs.length === 1 && evs[0].reason === "path_traversal",
      JSON.stringify(evs.map((e) => e.reason)));
    check("path traversal capture does not leak the room",
      evs.length === 1 && !JSON.stringify(evs[0]).includes("1.2.3.4"));
  }
}

// ---------------------------------------------------------------------------
// 16. Missing HP binding: capture is silently off, the endpoint still answers.
//
// capture() returns early when env.HP is absent, so a deployment that has the
// room KV but not the honeypot must behave exactly like one with both. This is
// also the shape every test above section 14 runs in, which is why they keep
// working unchanged.
// ---------------------------------------------------------------------------
{
  const env = { ROOMS: makeKV() };
  let threw = null;
  let r = null;
  try {
    r = await send(env, "POST", "/api/room", "198.21.0.1",
      { code: "AB\u0000CD", addr: "1.2.3.4", port: 25565 });
    await settle();
  } catch (e) {
    threw = e;
  }
  check("no HP binding: hostile request does not throw", threw === null,
    threw && String(threw));
  // A NUL in the code fails isValidCode() first, so the answer is bad_code —
  // the same answer the request got before capture existed.
  check("no HP binding: hostile request still -> 400 bad_code",
    r && r.status === 400 && r.body && r.body.error === "bad_code",
    r ? `${r.status} ${r.text.trim()}` : "no response");

  // Same for the verb path and the path-traversal path, which capture from
  // different places in the module.
  const put = await send(env, "PUT", "/api/room", "198.21.0.2", "{}");
  check("no HP binding: PUT still -> 405", put.status === 405, `got ${put.status}`);
  const trav = await send(env, "GET", "/api/room%00", "198.21.0.3");
  check("no HP binding: path traversal still -> 400 bad_code",
    trav.status === 400 && trav.body && trav.body.error === "bad_code",
    `${trav.status} ${trav.text.trim()}`);
  // An env with HP present but null must behave the same as an absent one.
  const nullEnv = { ROOMS: makeKV(), HP: null };
  const nullRes = await send(nullEnv, "PUT", "/api/room", "198.21.0.4", "{}");
  await settle();
  check("null HP binding: PUT still -> 405", nullRes.status === 405, `got ${nullRes.status}`);
}

// ---------------------------------------------------------------------------
// 17. Capture rate limit: at most MAX_PER_IP_PER_MINUTE (5) events per IP.
//
// A scanner can generate hostile requests far faster than the free KV tier can
// absorb, so capture() charges a counter under `cap:<ip>:<minute>` before each
// write. The counter must be the *capture* window (120s), not the endpoint's
// rate-limit window, and the sixth event must be dropped — not delayed, not
// buffered. Date.now is pinned so the test cannot straddle a window boundary.
// ---------------------------------------------------------------------------
{
  const realNow = Date.now;
  Date.now = () => 1_700_000_030_000;
  const { env, hp } = makeEnv();
  const statuses = [];
  for (let i = 0; i < 6; i += 1) {
    const r = await send(env, "PUT", "/api/room", "198.22.0.1", "{}");
    statuses.push(r.status);
    await settle();
  }
  const evs = events(hp);
  const keys = capKeys(hp);
  check("6 hostile requests all -> 405 (capture never changes the answer)",
    statuses.length === 6 && statuses.every((s) => s === 405), JSON.stringify(statuses));
  check("6 hostile requests from one IP -> at most 5 captures",
    evs.length <= 5, `${evs.length} captures`);
  check("capture count is exactly 5 for the first 5, 0 for the 6th",
    evs.length === 5, `${evs.length} captures`);
  check("capture counter key is cap:<ip>:<minute>",
    keys.length === 1 && keys[0].startsWith("cap:198.22.0.1:") &&
      /^cap:198\.22\.0\.1:\d+$/.test(keys[0]),
    JSON.stringify(keys));
  check("capture counter reaches 5", hp.store.get(keys[0]) === "5",
    String(hp.store.get(keys[0])));
  check("capture counter TTL is 120s", hp.ttl(keys[0]) === 120,
    String(hp.ttl(keys[0])));
  // The cap is per IP: a second address still gets its own five. The counter
  // key is derived from the IP, so a shared counter would show up here as the
  // second address being refused after the first exhausted its budget.
  for (let i = 0; i < 6; i += 1) {
    await send(env, "PUT", "/api/room", "198.22.0.2", "{}");
    await settle();
  }
  check("capture cap is per IP, not global", events(hp).length === 10,
    `${events(hp).length} captures`);
  check("a second IP gets its own counter key",
    capKeys(hp).length === 2 &&
      capKeys(hp).every((k) => /^cap:\d+\.\d+\.\d+\.\d+:\d+$/.test(k)),
    JSON.stringify(capKeys(hp)));
  Date.now = realNow;
}

// ---------------------------------------------------------------------------
// 18. Credentials never reach the honeypot.
//
// The room endpoint has no use for a cookie or a bearer token, and copying a
// third party's session into a log would turn the honeypot into a liability.
// _capture.js keeps an allowlist of headers and omits cookie/authorization; this
// asserts it end to end, over every captured field, including the free-form
// `extra` object and the path.
// ---------------------------------------------------------------------------
{
  const secret = "SECRET_TOKEN_12345";
  const cookie = `session=${secret}`;
  const { env, hp } = makeEnv();

  const put = await send(env, "PUT", "/api/room", "198.23.0.1", "{}",
    { cookie, authorization: `Bearer ${secret}` });
  await settle();
  check("hostile request with cookie -> 405 unchanged", put.status === 405,
    `got ${put.status}`);
  const evs = events(hp);
  check("hostile request with cookie was captured", evs.length === 1,
    `${evs.length} captures`);

  // Assert over the raw serialised values, not the parsed object: a secret in
  // a key name or a nested field would be missed by a shallow field check.
  const raw = [...hp.store.entries()].map(([k, v]) => `${k}=${v}`).join("\n");
  check("no captured key or value contains the cookie secret",
    !raw.includes(secret), raw.slice(0, 400));
  check("captured headers carry no cookie or authorization",
    evs.length === 1 && !("cookie" in (evs[0].headers || {})) &&
      !("authorization" in (evs[0].headers || {})),
    evs.length ? JSON.stringify(evs[0].headers) : "no event");
  check("no captured event has a cookie field anywhere",
    !/\bcookie\b/i.test(JSON.stringify(evs)), JSON.stringify(evs).slice(0, 400));

  // The same for a hostile GET, whose capture path is the lookup one.
  const { env: env2, hp: hp2 } = makeEnv();
  await send(env2, "GET", "/api/room?code=%00", "198.23.0.2", undefined, { cookie });
  await settle();
  const raw2 = [...hp2.store.entries()].map(([k, v]) => `${k}=${v}`).join("\n");
  check("GET capture with cookie is recorded", hitKeys(hp2).length === 1,
    `${hitKeys(hp2).length} captures`);
  check("GET capture does not contain the cookie secret",
    !raw2.includes(secret), raw2.slice(0, 400));
}

// ---------------------------------------------------------------------------
// 19. Capture is off the response path: a KV failure must not become a 500.
//
// noteHostile() is not awaited and capture() swallows its own errors, so a
// honeypot write that throws, rejects, or hangs must leave the endpoint's
// answer intact. A broken HP binding is the realistic version of this (quota
// exhausted, namespace deleted, permission revoked).
// ---------------------------------------------------------------------------
{
  const throwingKV = {
    store: new Map(),
    async get() { throw new Error("HP read failed"); },
    async put() { throw new Error("HP write failed"); },
    ttl() { return undefined; },
  };
  const env = { ROOMS: makeKV(), HP: throwingKV };
  let threw = null;
  let r = null;
  try {
    r = await send(env, "POST", "/api/room", "198.24.0.1",
      { code: "AB\u0000CD", addr: "1.2.3.4", port: 25565 });
    await settle();
  } catch (e) {
    threw = e;
  }
  check("HP that throws does not throw out of the request", threw === null,
    threw && String(threw));
  check("HP that throws leaves the response at 400 bad_code",
    r && r.status === 400 && r.body && r.body.error === "bad_code",
    r ? `${r.status} ${r.text.trim()}` : "no response");
  check("HP that throws leaves the response body byte-identical",
    r && r.text === '{"ok":false,"error":"bad_code"}\n',
    r ? JSON.stringify(r.text) : "no response");

  // A hanging HP must not delay the response either: the handler never awaits
  // capture, so the answer is already returned while the write is pending.
  // The waitUntil promise is deliberately left undrained — awaiting it would
  // hang the test, which is exactly what the endpoint must not do to a client.
  const hangingKV = {
    store: new Map(),
    get() { return new Promise(() => {}); },
    put() { return new Promise(() => {}); },
    ttl() { return undefined; },
  };
  const env2 = { ROOMS: makeKV(), HP: hangingKV };
  const r2 = await send(env2, "PUT", "/api/room", "198.24.0.2", "{}", undefined,
    { drain: false });
  check("HP that never settles does not block the 405",
    r2.status === 405 && r2.text.trim() === '{"ok":false}',
    `${r2.status} ${r2.text.trim()}`);
  check("HP that never settles still hands the capture to waitUntil",
    r2.waits.length === 1, `${r2.waits.length} waitUntil calls`);
}

// ---------------------------------------------------------------------------
// 20. The capture is a record, not a relay: nothing captured can reconstruct a
//     usable room.
//
// A hostile registration may carry a syntactically valid code and address
// alongside one hostile field, so the assertion is that the *capture* holds no
// addr/port and that no room key was created — the request is rejected, and the
// honeypot does not become a second copy of the registry.
// ---------------------------------------------------------------------------
{
  const { env, hp } = makeEnv();
  await send(env, "POST", "/api/room", "198.25.0.1",
    { code: "ABC234", addr: "1.2.3.4;touch /tmp/pwned", port: 25565 });
  await settle();
  const evs = events(hp);
  check("hostile register captured once", evs.length === 1, `${evs.length} captures`);
  check("hostile register wrote no room key", env.ROOMS.roomKeys().length === 0,
    JSON.stringify(env.ROOMS.roomKeys()));
  check("capture does not store the room address as a field",
    evs.length === 1 && evs[0].addr === undefined && evs[0].port === undefined,
    evs.length ? JSON.stringify(Object.keys(evs[0])) : "no event");
  // `extra` is the only place request-derived content may appear, and it is
  // clipped and scoped to a field name, never the value.
  check("capture extra names the offending field, not its value",
    evs.length === 1 && evs[0].extra && evs[0].extra.field === "addr" &&
      !JSON.stringify(evs[0].extra).includes("touch"),
    evs.length ? JSON.stringify(evs[0].extra) : "no event");
}

rmSync(dir, { recursive: true, force: true });
console.log(`\n${checks} assertions`);
console.log(failures === 0 ? "ALL PASS" : `${failures} FAILURE(S)`);
process.exit(failures === 0 ? 0 : 1);
