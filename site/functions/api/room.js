// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Room registry for "join by room number" (Cloudflare Pages Function).
//
// The host of a LAN world asks the launcher for a 6-character room code, then
// tells that code to a friend. The friend's launcher looks the code up here,
// gets back the host's address, and connects to it directly. This endpoint is
// only a short-lived code -> address directory: game traffic never passes
// through it, and nothing here can observe or relay a session.
//
// Routes (all handled by this one module, dispatched by verb + pathname):
//
//   POST /api/room              {"code","addr","port"} -> {"ok":true,"expires":600}
//   GET  /api/room?code=ABC234  -> {"ok":true,"addr":...,"port":...}
//
// The room code is a **query parameter, not a path segment**. Pages is a file
// router: this module is deployed at `/api/room` and nothing else, so
// `/api/room/ABC234` never reaches a Function at all — it is answered by the
// static-asset 404 before any of this runs. Keeping the code in the query
// string means the route set stays exactly `{/api/room}` and no `[code].js`
// dynamic segment (nor a catch-all) is needed.
//
// A request that still uses the retired `/api/room/<code>` shape therefore
// lands on `onRequest`, which answers 400 `bad_request` — the same answer as
// a missing `code` parameter. Not 405, which would claim the verb is wrong,
// and not 404, which would be indistinguishable from an expired room.
//
// Anything else gets a bare {"ok":false} with 405. That includes unknown
// verbs and unknown paths, so probing the shape of the API tells a scanner
// nothing.
//
// Storage: the KV binding named ROOMS. One key per room, `room:<code>`, value
// is a single JSON object {"addr","port","ts"}, TTL 600s. The record expires
// on its own; there is no delete endpoint and no history, because the feature
// is a meeting point, not a directory of where players live. The source IP is
// deliberately NOT part of the record — only the rate-limit counters hold it,
// and those expire after 120s.
//
// Error responses are a closed enum (bad_code, bad_request, not_found,
// rate_limited, server_error). No field of the request is ever echoed back,
// no reason is attached, and no header beyond content-type is set. There is
// no CORS: the launcher is a native program, so a browser has no legitimate
// reason to call this and gets no Access-Control-* grant.

// The room alphabet is Crockford-style base32 minus I/O/0/1, so a code can be
// read over voice chat or written on paper without ambiguity. 32^6 ~= 1.07e9
// codes, which keeps guessing a live room impractical inside a 10 minute TTL.
const CODE_ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LENGTH = 6;
const CODE_RE = /^[ABCDEFGHJKLMNPQRSTUVWXYZ23456789]{6}$/;

// An address is an IPv6 or IPv4 literal. This is an allowlist, not a
// denylist: only hex digits, dots, colons and square brackets may appear, so
// no shell metacharacter, quote, space, percent (IPv6 zone id) or newline can
// survive into KV or into a peer's connection dialog. Length is capped well
// above the longest legitimate IPv6 literal (45 chars).
const ADDR_MAX_CHARS = 64;
const ADDR_RE = /^[0-9A-Fa-f:.\[\]]+$/;

// How long a room stays resolvable. Ten minutes is long enough to exchange a
// code in chat and short enough that stale mappings cannot accumulate.
const ROOM_TTL_SECONDS = 600;

// Per-IP budgets. Writes are cheap for us but each one is a KV write against
// the daily quota, so they are the tighter limit; reads are 60/min because a
// friend may retry while the host's world is still starting up.
const MAX_WRITES_PER_MINUTE = 10;
const MAX_READS_PER_MINUTE = 60;

// Counter lifetime is longer than the window it guards so a counter cannot
// vanish mid-window and hand out a second full budget.
const RATE_WINDOW_SECONDS = 60;
const RATE_TTL_SECONDS = 120;

// A registration body is a tiny JSON object. Anything larger is not a real
// launcher and is rejected before it is parsed.
const MAX_BODY_BYTES = 1024;

const REGISTER_PATH = "/api/room";
// Prefix of the *retired* lookup shape. Nothing is served here any more; the
// constant exists so the two places that must recognise a stale client agree
// on one string.
const LOOKUP_PREFIX = "/api/room/";
// Query parameter carrying the code. One parameter, one value: `?code=A&code=B`
// resolves to the first, so the lookup is always a single unambiguous code.
const CODE_PARAM = "code";

function json(obj, status) {
  return new Response(JSON.stringify(obj) + "\n", {
    status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

// Best-effort source IP for the rate limiter only. Cloudflare sets
// CF-Connecting-IP on every request; XFF is a fallback, never trusted for
// anything but counting. The value is reduced to characters that can appear
// in an IP literal so it cannot reshape a KV key.
function sourceIp(request) {
  const raw = request.headers.get("cf-connecting-ip") ||
    (request.headers.get("x-forwarded-for") || "").split(",")[0] ||
    "";
  const cleaned = raw.trim().slice(0, 64).replace(/[^0-9A-Fa-f:.]/g, "");
  return cleaned || "unknown";
}

// Fixed one-minute bucket. Using wall-clock minutes (instead of a sliding
// window) means the limit resets on a minute boundary, which is acceptable
// for abuse control and keeps the counter to a single KV read + write.
function minuteBucket() {
  return Math.floor(Date.now() / RATE_WINDOW_SECONDS / 1000);
}

// Increment-and-test. Returns true when this request is over budget.
//
// The counter lives in KV so it survives across isolates; a lost race just
// lets one extra request through. A failed counter write fails open on
// purpose: a KV outage should degrade to "no rate limit" rather than to "the
// whole feature is down", and the room write it guards is failing anyway.
async function overBudget(kv, key, limit) {
  let count = 0;
  try {
    count = parseInt((await kv.get(key)) || "0", 10) || 0;
  } catch {
    count = 0;
  }
  if (count >= limit) return true;
  try {
    await kv.put(key, String(count + 1), { expirationTtl: RATE_TTL_SECONDS });
  } catch {
    // See above: fail open.
  }
  return false;
}

// Read the body under a hard cap. Content-Length is checked first so an
// oversized upload is refused without being buffered, but the header is
// advisory (and absent on chunked requests), so the decoded bytes are
// measured as well.
async function readCappedBody(request, maxBytes) {
  const declared = request.headers.get("content-length");
  if (declared !== null) {
    const n = Number(declared);
    if (!Number.isFinite(n) || n < 0 || n > maxBytes) return { ok: false };
  }
  let text;
  try {
    text = await request.text();
  } catch {
    return { ok: false };
  }
  if (new TextEncoder().encode(text).length > maxBytes) return { ok: false };
  return { ok: true, text };
}

function isValidCode(value) {
  return typeof value === "string" && CODE_RE.test(value);
}

function isValidAddr(value) {
  return typeof value === "string" &&
    value.length > 0 &&
    value.length <= ADDR_MAX_CHARS &&
    ADDR_RE.test(value);
}

function isValidPort(value) {
  // Integers only: a numeric string would be accepted by parseInt and then
  // stored in a shape the launcher does not expect.
  return typeof value === "number" &&
    Number.isInteger(value) &&
    value >= 1 &&
    value <= 65535;
}

// Build the KV key. The regex above already guarantees no `:` or `/` can
// reach this point, but the key is the one place where a validation slip
// would turn into cross-room writes, so it is asserted again here. Returning
// null instead of throwing keeps the caller's failure path a plain 400.
function roomKey(code) {
  if (!isValidCode(code)) return null;
  if (code.includes(":") || code.includes("/")) return null;
  return "room:" + code;
}

// POST target must be the endpoint itself. A trailing slash is tolerated
// because some HTTP clients normalise that way; nothing deeper is.
function isRegisterPath(pathname) {
  return pathname === REGISTER_PATH || pathname === REGISTER_PATH + "/";
}

// Extract the code from the query string. `URL.searchParams` percent-decodes,
// which is what a query parameter is for, and the decoded value then goes
// through the same allowlist as the registration path — so `?code=ABC%32%33%34`
// means "ABC234" and `?code=AB%2F234` is a bad code, never a slash smuggled
// into a KV key.
//
// An absent parameter and an empty one both come back as "", and the caller
// answers bad_code for either: distinguishing "you forgot the parameter" from
// "you sent an empty one" would only help someone probe the shape.
function codeFromQuery(url) {
  const code = url.searchParams.get(CODE_PARAM);
  return code === null ? "" : code;
}

// The retired `/api/room/<code>` shape. It is not a route any more (see the
// header comment), but a client built before the move still asks for it, and
// if the request does reach this module the honest answer is "that is not a
// request this endpoint understands" — 400 bad_request — rather than 405,
// which would claim the verb is wrong, or 404, which would be mistaken for an
// expired room. A bare `/api/room/` is the register path with a trailing
// slash, not a retired lookup, so it is excluded here.
function isRetiredLookupPath(pathname) {
  return pathname.startsWith(LOOKUP_PREFIX) && pathname !== LOOKUP_PREFIX;
}

export async function onRequestPost(context) {
  const { request, env } = context;
  const pathname = new URL(request.url).pathname;
  // Same carve-out as `onRequest`: the retired `/api/room/<code>` shape is a
  // route that does not exist, so it is bad_request on every verb. Saying 405
  // here would contradict what GET on the same path answers.
  if (isRetiredLookupPath(pathname)) {
    return json({ ok: false, error: "bad_request" }, 400);
  }
  if (!isRegisterPath(pathname)) return json({ ok: false }, 405);

  const kv = env && env.ROOMS;
  if (!kv) {
    // Binding not configured. Fail closed with a generic enum value so the
    // deployment mistake is not visible to the caller.
    return json({ ok: false, error: "server_error" }, 500);
  }

  const ip = sourceIp(request);
  if (await overBudget(kv, `rl:w:${ip}:${minuteBucket()}`, MAX_WRITES_PER_MINUTE)) {
    return json({ ok: false, error: "rate_limited" }, 429);
  }

  const body = await readCappedBody(request, MAX_BODY_BYTES);
  if (!body.ok) return json({ ok: false, error: "bad_request" }, 400);

  let payload;
  try {
    payload = JSON.parse(body.text);
  } catch {
    // Malformed JSON is reported, never described.
    return json({ ok: false, error: "bad_request" }, 400);
  }
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    return json({ ok: false, error: "bad_request" }, 400);
  }

  const { code, addr, port } = payload;
  if (!isValidCode(code)) return json({ ok: false, error: "bad_code" }, 400);
  if (!isValidAddr(addr)) return json({ ok: false, error: "bad_request" }, 400);
  if (!isValidPort(port)) return json({ ok: false, error: "bad_request" }, 400);

  const key = roomKey(code);
  if (!key) return json({ ok: false, error: "bad_code" }, 400);

  // Re-registering the same code simply overwrites it and restarts the TTL;
  // that is the renewal path, so no ownership token is needed or kept.
  const record = { addr, port, ts: Math.floor(Date.now() / 1000) };
  try {
    await kv.put(key, JSON.stringify(record), { expirationTtl: ROOM_TTL_SECONDS });
  } catch {
    return json({ ok: false, error: "server_error" }, 500);
  }

  return json({ ok: true, expires: ROOM_TTL_SECONDS }, 200);
}

export async function onRequestGet(context) {
  const { request, env } = context;
  const url = new URL(request.url);
  if (isRetiredLookupPath(url.pathname)) {
    return json({ ok: false, error: "bad_request" }, 400);
  }
  const code = codeFromQuery(url);

  const kv = env && env.ROOMS;
  if (!kv) return json({ ok: false, error: "server_error" }, 500);

  // The read counter is charged before the code is validated: otherwise a
  // scanner could try every code in the alphabet for free and the limit would
  // protect nothing. Invalid attempts are exactly what this budget is for.
  const ip = sourceIp(request);
  if (await overBudget(kv, `rl:r:${ip}:${minuteBucket()}`, MAX_READS_PER_MINUTE)) {
    return json({ ok: false, error: "rate_limited" }, 429);
  }

  if (!isValidCode(code)) return json({ ok: false, error: "bad_code" }, 400);

  const key = roomKey(code);
  if (!key) return json({ ok: false, error: "bad_code" }, 400);

  let raw;
  try {
    raw = await kv.get(key);
  } catch {
    return json({ ok: false, error: "server_error" }, 500);
  }
  if (raw === null || raw === undefined) {
    return json({ ok: false, error: "not_found" }, 404);
  }

  let record;
  try {
    record = JSON.parse(raw);
  } catch {
    // A value we cannot parse is our corruption, not the caller's problem,
    // and saying "not_found" would send the friend to the wrong conclusion.
    return json({ ok: false, error: "server_error" }, 500);
  }
  if (!record || typeof record !== "object" ||
      !isValidAddr(record.addr) || !isValidPort(record.port)) {
    return json({ ok: false, error: "server_error" }, 500);
  }

  // Only the two fields the friend needs. The timestamp stays internal so the
  // response cannot be used to infer how long ago a host started.
  return json({ ok: true, addr: record.addr, port: record.port }, 200);
}

// Every other verb (PUT, DELETE, OPTIONS, HEAD, ...) and every other path.
// The body is identical to the other negative cases minus the error field, so
// a prober learns only that this is not the right request.
//
// Two carve-outs:
//
//  * A path under the retired `/api/room/<code>` shape gets 400 bad_request
//    instead. Pages is a file router, so that request normally never reaches
//    this module — but when it does (a client built before the query parameter
//    move), saying "405" would blame the verb and "404" would look like an
//    expired room. The code is simply not where that client put it, which is
//    exactly bad_request.
//  * GET and POST on the register path are forwarded to the verb handlers.
//    Pages picks `onRequestGet`/`onRequestPost` itself, so this normally never
//    runs; forwarding means a request it hands to the catch-all anyway (a
//    trailing slash, a future Pages routing change) still reaches the same
//    code instead of falling through to a 405 that would be a lie.
export async function onRequest(context) {
  const { request } = context;
  const pathname = new URL(request.url).pathname;
  if (isRetiredLookupPath(pathname)) {
    return json({ ok: false, error: "bad_request" }, 400);
  }
  if (isRegisterPath(pathname)) {
    if (request.method === "GET") return onRequestGet(context);
    if (request.method === "POST") return onRequestPost(context);
  }
  return json({ ok: false }, 405);
}
