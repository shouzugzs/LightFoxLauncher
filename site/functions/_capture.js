// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Shared honeypot capture, used by the room registry to record hostile input.
//
// The decoy pages (private-site/admin/ etc.) capture everything submitted to
// them, because *any* submission to a fake login form is an attack. The room
// endpoint is different: it is a real service with real users, and a mistyped
// code is not hostile. So this module is only called for input that matches a
// known attack shape, and the caller decides that — see classifyHostile() in
// room.js.
//
// Events land in the same KV namespace the decoys write to (binding HP), under
// the same `hit:` prefix, so tools/honeypot-export.mjs reads both sources from
// one place. The `source` field keeps them distinguishable: decoy events have
// no `source`, room events say `"source":"room"`.
//
// Nothing here can affect a response. capture() never throws, never rejects,
// and the caller does not await it.
//
// This file is not a route: Pages Functions skips every path segment that
// starts with `_`, so `functions/_capture.js` is bundled but never dispatched.
// It also exports no `onRequest*` handler, which is the second lock on the
// same door — even if it were routed, it could not answer a request.
//
// OWNER: lead.

// Same shape as the decoy endpoint: one event per key, 30 days.
const TTL_SECONDS = 30 * 24 * 3600;

// Per-IP cap on captures. A scanner can generate thousands of hostile requests
// a minute; the free KV tier allows 1,000 writes a day, so an uncapped capture
// path would be a self-inflicted denial of service on the honeypot itself.
const MAX_PER_IP_PER_MINUTE = 5;

// Fixed one-minute bucket, the same wall-clock bucketing room.js uses for its
// own budgets: the limit resets on a minute boundary, which is fine for abuse
// control and keeps the counter to one KV read plus one write.
const WINDOW_SECONDS = 60;

// Counter lifetime is longer than the window it guards, so a counter cannot
// vanish mid-window and hand out a second full budget.
const RATE_TTL_SECONDS = 120;

// A single event must stay small. The interesting part is the first few
// hundred bytes of a hostile payload; the rest is noise in the export.
const MAX_FIELD = 300;

function clip(v) {
  if (typeof v !== "string") return v;
  return v.length <= MAX_FIELD ? v : v.slice(0, MAX_FIELD) + "…";
}

// `extra` is caller-supplied and can carry an attacker's payload, so it is
// clipped one level deep before it is stored. Without this a single 8 KB body
// would become an 8 KB KV value.
function clipExtra(extra) {
  if (!extra || typeof extra !== "object") return undefined;
  const out = {};
  for (const k of Object.keys(extra).slice(0, 8)) {
    const v = extra[k];
    out[k.slice(0, 32)] = typeof v === "string" ? clip(v)
      : typeof v === "number" || typeof v === "boolean" ? v
      : v === null || v === undefined ? v
      : clip(String(v));
  }
  return out;
}

// Source IP. CF-Connecting-IP is authoritative on Cloudflare; XFF is recorded
// only as a fallback so an event is never dropped for lack of an address.
function sourceIp(request) {
  const cf = request.headers.get("cf-connecting-ip");
  if (cf) return { ip: clip(cf), source: "cf-connecting-ip" };
  const xff = request.headers.get("x-forwarded-for");
  if (xff) {
    const first = xff.split(",")[0].trim();
    if (first) return { ip: clip(first), source: "x-forwarded-for" };
  }
  return { ip: "unknown", source: "none" };
}

// The IP goes into a KV key, so it is reduced to characters that can appear in
// an IP literal. `:` is kept for IPv6; nothing that could reshape the key
// (newline, `/`, `%`) survives. The event keeps the unmodified value.
//
// A missing address must NOT collapse to one shared bucket. Cloudflare sets
// CF-Connecting-IP on every request, so `unknown` should be unreachable in
// production — but if it ever is reached (wrangler dev, a proxy in front, a
// header-stripping middlebox), mapping every anonymous caller to the same key
// would cap capture at 5 per minute *globally* and let one scanner silence the
// honeypot for everyone. A random per-event suffix keeps the limiter per-event
// instead, which is the safe failure direction: over-recording, not silence.
function keyIp(ip) {
  const cleaned = String(ip).replace(/[^0-9A-Fa-f:.]/g, "").slice(0, 64);
  if (cleaned) return cleaned;
  return "anon-" + crypto.randomUUID().slice(0, 8);
}

// Header allowlist, identical to the decoy endpoint's. Cookies and bearer
// tokens are deliberately absent: this endpoint has no reason to hold a third
// party's credentials, and copying them here would be a liability.
function interestingHeaders(request) {
  const keep = [
    "user-agent",
    "accept",
    "accept-language",
    "content-type",
    "referer",
  ];
  const out = {};
  for (const name of keep) {
    const v = request.headers.get(name);
    if (v) out[name] = clip(v);
  }
  return out;
}

// Path plus query exactly as the client sent it. `new URL(...).pathname` would
// normalise `..` away, which is the one thing a traversal probe must preserve.
function rawPath(request) {
  const raw = typeof request.url === "string" ? request.url : "";
  const scheme = raw.indexOf("://");
  const start = scheme < 0 ? 0 : raw.indexOf("/", scheme + 3);
  if (start < 0) return "/";
  const rest = raw.slice(start);
  const cut = rest.search(/[?#]/);
  return cut < 0 ? rest : rest.slice(0, cut);
}

/**
 * Record one hostile event.
 *
 * @param {object} env        Pages environment (needs the HP KV binding).
 * @param {Request} request   The request that matched an attack shape.
 * @param {object} info       { source, reason, extra }
 * @returns {Promise<void>}   Always resolves; never rejects.
 */
export async function capture(env, request, { source = "room", reason = "unknown", extra } = {}) {
  try {
    const kv = env && env.HP;
    if (!kv) return; // binding absent: capture is silently off, service works

    const { ip, source: ipSource } = sourceIp(request);

    // Per-IP rate limit, charged before the write. A lost race costs one extra
    // event, which is the right failure mode.
    const minute = Math.floor(Date.now() / (WINDOW_SECONDS * 1000));
    const rateKey = `cap:${keyIp(ip)}:${minute}`;
    let count = 0;
    try {
      count = parseInt((await kv.get(rateKey)) || "0", 10) || 0;
    } catch {
      count = 0;
    }
    if (count >= MAX_PER_IP_PER_MINUTE) return;
    try {
      await kv.put(rateKey, String(count + 1), {
        expirationTtl: RATE_TTL_SECONDS,
      });
    } catch {
      // A failed counter must not block the capture itself.
    }

    const event = {
      time: new Date().toISOString(),
      source,
      reason,
      ip,
      ip_source: ipSource,
      method: request.method,
      path: clip(rawPath(request)),
      user_agent: clip(request.headers.get("user-agent") || ""),
      headers: interestingHeaders(request),
      // CF ray + colo let the same hit be correlated with Cloudflare's own
      // request logs if the zone has them enabled.
      ray: clip(request.headers.get("cf-ray") || ""),
      colo: (request.cf && request.cf.colo) || undefined,
      country: (request.cf && request.cf.country) || undefined,
      asn: request.cf && request.cf.asn !== undefined ? request.cf.asn : undefined,
      extra: clipExtra(extra),
    };

    const id = `hit:${Date.now().toString(36)}-${crypto.randomUUID().slice(0, 8)}`;
    await kv.put(id, JSON.stringify(event), { expirationTtl: TTL_SECONDS });
  } catch {
    // Capture is best-effort by design: it must never turn a 400 into a 500.
  }
}
