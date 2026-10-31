// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Honeypot capture endpoint (Cloudflare Pages Function).
//
// Deployed at /api/login on the Pages project. It receives what an attacker
// submits to the fake admin pages and records enough context for a network-
// security workflow: source IP, user agent, submitted credentials, headers,
// and a timestamp. Nothing is ever echoed back — the response is the same
// generic failure regardless of input, so the endpoint gives an attacker no
// feedback about what was stored, whether their input was parsed, or whether
// the endpoint exists at all beyond "POST failed".
//
// Storage: the KV binding named HP. Each event is one key under the
// `hit:` prefix, value is a single JSON line. Reading the store requires
// Cloudflare dashboard or wrangler access to the binding — there is no HTTP
// read path, so the captured data is not reachable from the web.
//
// OWNER: lead.

// Rate limit: at most this many events per source IP per TTL window, so a
// scanner hammering the endpoint cannot exhaust the KV write budget (free
// tier: 1,000 writes/day) or grow the namespace without bound.
const MAX_HITS_PER_IP = 20;
const WINDOW_SECONDS = 3600;

// A single event must stay small; oversized bodies are truncated, not stored.
const MAX_BODY_BYTES = 4096;

// How long an event is retained. Short on purpose: honeypot data is for
// triage, not an evidence archive.
const TTL_SECONDS = 30 * 24 * 3600;

function json(obj, status) {
  return new Response(JSON.stringify(obj) + "\n", {
    status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

async function readTruncatedBody(request) {
  try {
    const raw = await request.text();
    const bytes = new TextEncoder().encode(raw);
    if (bytes.length <= MAX_BODY_BYTES) return raw;
    return new TextDecoder().decode(bytes.slice(0, MAX_BODY_BYTES)) +
      "\n[truncated]";
  } catch {
    return "";
  }
}

// Read the body once and derive both representations from the same text.
// `request.formData()` consumes the body, so calling it after a plain
// `request.text()` fails with "Body has already been read"; parsing the
// urlencoded (or bare) form ourselves avoids that and also keeps the raw
// string for anything that is not a form.
async function readEventBody(request) {
  const raw = await readTruncatedBody(request);
  const contentType = request.headers.get("content-type") || "";
  if (/application\/x-www-form-urlencoded/i.test(contentType)) {
    try {
      const fields = {};
      for (const pair of raw.split("&")) {
        if (!pair) continue;
        const eq = pair.indexOf("=");
        const k = decodeURIComponent((eq < 0 ? pair : pair.slice(0, eq)).replace(/\+/g, " "));
        const v = decodeURIComponent((eq < 0 ? "" : pair.slice(eq + 1)).replace(/\+/g, " "));
        if (k) fields[k.slice(0, 64)] = v.slice(0, 512);
      }
      return { fields, raw: undefined, contentType };
    } catch {
      // Malformed encoding: keep the raw text instead of dropping the event.
    }
  }
  return { fields: null, raw, contentType };
}

// Pick the request headers worth keeping. Deliberately an allowlist: storing
// every header would capture cookies and any bearer tokens the scanner sends,
// which is third-party data this endpoint has no reason to hold.
function interestingHeaders(request) {
  const keep = [
    "user-agent",
    "accept",
    "accept-language",
    "content-type",
    "referer",
    "x-forwarded-proto",
  ];
  const out = {};
  for (const name of keep) {
    const v = request.headers.get(name);
    if (v) out[name] = v.slice(0, 300);
  }
  return out;
}

// Best-effort source IP. Cloudflare sets CF-Connecting-IP on every request;
// the XFF chain is recorded only as supplementary context, never trusted.
function sourceIp(request, env) {
  const cf = request.headers.get("cf-connecting-ip");
  if (cf) return { ip: cf.slice(0, 64), source: "cf-connecting-ip" };
  const xff = request.headers.get("x-forwarded-for");
  if (xff) {
    const first = xff.split(",")[0].trim();
    if (first) return { ip: first.slice(0, 64), source: "x-forwarded-for" };
  }
  return { ip: "unknown", source: "none" };
}

// RFC 3339 UTC timestamp without depending on a date library.
function isoNow() {
  return new Date().toISOString();
}

export async function onRequestPost(context) {
  const { request, env } = context;
  const kv = env && env.HP;
  if (!kv) {
    // Binding not configured: fail closed and silently. This must look the
    // same as every other failure mode from the outside.
    return json({ ok: false }, 500);
  }

  const { ip, source } = sourceIp(request);

  // Per-IP rate limit. The counter lives in KV too, so it survives across
  // the function's isolates; a lost race just means one extra event.
  const windowKey = `rl:${ip}:${Math.floor(Date.now() / (WINDOW_SECONDS * 1000))}`;
  let count = 0;
  try {
    count = parseInt((await kv.get(windowKey)) || "0", 10) || 0;
  } catch {
    count = 0;
  }
  if (count >= MAX_HITS_PER_IP) {
    return json({ ok: false }, 500);
  }
  try {
    await kv.put(windowKey, String(count + 1), { expirationTtl: WINDOW_SECONDS });
  } catch {
    // A failed counter write must not block capture.
  }

  const contentType = request.headers.get("content-type") || "";
  const { fields, raw: rawBody, contentType: parsedType } = await readEventBody(request);

  const event = {
    time: isoNow(),
    ip,
    ip_source: source,
    method: request.method,
    path: new URL(request.url).pathname,
    user_agent: (request.headers.get("user-agent") || "").slice(0, 300),
    headers: interestingHeaders(request),
    content_type: parsedType.slice(0, 200),
    fields,
    raw_body: fields ? undefined : rawBody,
    // CF ray + colo let the same hit be correlated with Cloudflare's own
    // request logs if the zone has them enabled.
    ray: (request.headers.get("cf-ray") || "").slice(0, 64),
    colo: (request.cf && request.cf.colo) || undefined,
    country: (request.cf && request.cf.country) || undefined,
    asn: (request.cf && request.cf.asn) !== undefined ? request.cf.asn : undefined,
  };

  const id = `hit:${Date.now().toString(36)}-${crypto.randomUUID().slice(0, 8)}`;
  try {
    await kv.put(id, JSON.stringify(event), { expirationTtl: TTL_SECONDS });
  } catch {
    return json({ ok: false }, 500);
  }

  // Same generic response in every case. No count, no id, no echo.
  return json({ ok: false }, 500);
}

// Any other verb gets the identical body. A GET to /api/login reveals nothing
// beyond what a 405 would, and OPTIONS keeps CORS off by not advertising any.
export async function onRequest(context) {
  return json({ ok: false }, 500);
}
