// SPDX-License-Identifier: MPL-2.0
// Local harness: exercise site/functions/api/login.js without Cloudflare.
// Creates a fake KV in memory, imports the function module, fires requests at
// it, and prints what was stored. Run: node tools/honeypot-selftest.mjs
import { readFileSync, writeFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const src = readFileSync(
  new URL("../private-site/functions/api/login.js", import.meta.url),
  "utf8",
);
const dir = mkdtempSync(join(tmpdir(), "hp-test-"));
const modPath = join(dir, "login.mjs");
// Strip the SPDX comment block? Not needed — it is comments only.
writeFileSync(modPath, src);
const mod = await import(pathToFileURL(modPath).href);

function makeKV() {
  const store = new Map();
  return {
    store,
    async get(k) { return store.has(k) ? store.get(k) : null; },
    async put(k, v) { store.set(k, String(v)); },
  };
}

function makeReq(method, url, headers, body) {
  const h = new Headers(headers || {});
  return new Request(url, { method, headers: h, body: body || undefined });
}

let failures = 0;
function check(name, cond, extra) {
  if (cond) { console.log(`ok   - ${name}`); }
  else { failures += 1; console.log(`FAIL - ${name}${extra ? " :: " + extra : ""}`); }
}

const env = { HP: makeKV() };
const ctx = (req) => ({ request: req, env });

// 1. Form post is captured with fields.
{
  const body = new URLSearchParams({ username: "admin", password: "hunter2", source: "admin" });
  const req = makeReq("POST", "https://site.example/api/login",
    { "content-type": "application/x-www-form-urlencoded", "user-agent": "curl/8.0",
      "cf-connecting-ip": "203.0.113.7" }, body);
  const res = await mod.onRequestPost(ctx(req));
  const text = await res.text();
  check("form post -> 500", res.status === 500);
  check("response is generic", text.includes('"ok":false') && !text.includes("hunter2"));
  const hits = [...env.HP.store.keys()].filter((k) => k.startsWith("hit:"));
  check("one event stored", hits.length === 1, JSON.stringify([...env.HP.store.keys()]));
  const ev = JSON.parse(env.HP.store.get(hits[0]));
  check("fields captured", ev.fields && ev.fields.username === "admin" && ev.fields.password === "hunter2");
  check("source page recorded", ev.fields.source === "admin");
  check("ua recorded", ev.user_agent === "curl/8.0");
  check("ip from cf header", ev.ip === "203.0.113.7");
  check("time present", typeof ev.time === "string" && ev.time.includes("T"));
}

// 2. Rate limit: 21st post from same IP is dropped, counter keys exist.
{
  const ip = "198.51.100.9";
  for (let i = 0; i < 21; i += 1) {
    const body = new URLSearchParams({ u: `u${i}`, p: "x" });
    const req = makeReq("POST", "https://site.example/api/login",
      { "content-type": "application/x-www-form-urlencoded", "cf-connecting-ip": ip }, body);
    await mod.onRequestPost(ctx(req));
  }
  const hits = [...env.HP.store.keys()].filter((k) => k.startsWith("hit:") && env.HP.store.get(k).includes(ip));
  check("rate limit holds 20", hits.length === 20, `got ${hits.length}`);
}

// 3. Non-form JSON body is kept raw, not parsed into fields.
{
  const req = makeReq("POST", "https://site.example/api/login",
    { "content-type": "application/json", "cf-connecting-ip": "192.0.2.5" }, '{"a":1}');
  const res = await mod.onRequestPost(ctx(req));
  check("json body -> 500", res.status === 500);
  const hits = [...env.HP.store.keys()].filter((k) => k.startsWith("hit:") && env.HP.store.get(k).includes("192.0.2.5"));
  const ev = JSON.parse(env.HP.store.get(hits[0]));
  check("raw body kept", ev.raw_body === '{"a":1}' && ev.fields === null);
}

// 4. GET leaks nothing and stores nothing.
{
  const before = [...env.HP.store.keys()].filter((k) => k.startsWith("hit:")).length;
  const req = makeReq("GET", "https://site.example/api/login",
    { "cf-connecting-ip": "192.0.2.5" });
  const res = await mod.onRequest(ctx(req));
  const text = await res.text();
  const after = [...env.HP.store.keys()].filter((k) => k.startsWith("hit:")).length;
  check("GET -> 500 generic", res.status === 500 && text.includes('"ok":false'));
  check("GET stores nothing", before === after);
}

// 5. No KV binding -> 500, no crash.
{
  const req = makeReq("POST", "https://site.example/api/login",
    { "content-type": "application/x-www-form-urlencoded" }, "u=1&p=2");
  const res = await mod.onRequestPost({ request: req, env: {} });
  check("missing binding -> 500", res.status === 500);
}

// 6. Cookie header is NOT stored (allowlist).
{
  const body = new URLSearchParams({ u: "x", p: "y" });
  const req = makeReq("POST", "https://site.example/api/login",
    { "content-type": "application/x-www-form-urlencoded", "cf-connecting-ip": "192.0.2.77",
      cookie: "session=secret-token-value" }, body);
  await mod.onRequestPost(ctx(req));
  const hits = [...env.HP.store.keys()].filter((k) => k.startsWith("hit:") && env.HP.store.get(k).includes("192.0.2.77"));
  check("cookie never stored", !hits.some((k) => env.HP.store.get(k).includes("secret-token-value")));
}

rmSync(dir, { recursive: true, force: true });
console.log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILURE(S)`);
process.exit(failures === 0 ? 0 : 1);
