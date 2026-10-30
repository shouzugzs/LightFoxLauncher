# Security audit — round 3: the announcement / update-check path

Scope: everything reachable from the 通知 page and from `check_feed` at startup —
`src/notify.rs`, the `net::get` / `net::get_opt` transport it sits on, the state
file `lfl-seen.ini`, and the settings field that configures it. Reviewed at
0.3.1, fixed in 0.3.2.

This is the first pass aimed at one feature end to end. Rounds 1 and 2
(`docs/SECURITY.md`, `docs/SECURITY-ROUND2.md`) reviewed everything shallowly;
this one follows a single attacker-controlled document through every layer it
touches.

Every finding was **reproduced by executing code**, not inferred from reading.
Where the proof is a unit test, the test is named.

Threat model: the feed URL is user-configured, so the document at the other end
is chosen by the user — but the *content* is still fully untrusted, and the
`http://` case lets a network attacker replace it wholesale. The startup check
is the only code in the launcher that touches the network without a user
gesture, which raises the bar for what a bad response may cost.

---

## Summary

| ID | CVSS | Vector | Finding | Status |
|---|---|---|---|---|
| **N1** | **6.1 Medium** | `AV:N/AC:L/PR:N/UI:R/S:U/C:N/I:H/A:N` | Oversized feed buffered before the size check — unbounded allocation | **fixed** |
| **N2** | **5.3 Medium** | `AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:L` | Slowloris wedges 检查更新 for the rest of the session | **fixed** |
| **N3** | **5.0 Medium** | `AV:N/AC:L/PR:N/UI:R/S:U/C:N/I:L/A:N` | Bidi / zero-width characters spoof announcement text | **fixed** |
| **N4** | **4.3 Medium** | `AV:N/AC:H/PR:N/UI:R/S:U/C:N/I:L/A:N` | CR/LF in a document-supplied URL reaches the HTTP request line | **fixed** |
| **N5** | **3.5 Low** | `AV:N/AC:H/PR:N/UI:R/S:U/C:N/I:L/A:N` | `http://` feed accepted silently — announcements forgeable in transit | **fixed (advisory)** |
| **N6** | **2.0 Low** | `AV:L/AC:L/PR:L/UI:R/S:U/C:N/I:N/A:L` | `reported_update` written but never read; dead state | **fixed** |
| **N7** | Info | — | No feed signature; a compromised feed host can say anything | accepted, documented |

No finding was found in: JSON parsing (depth-limited at 200), the `Seen` state
file parser (every value re-sanitised at write time), version comparison
(numeric, garbage-safe), or `Settings::validate`.

---

## N1 — Oversized feed buffered before the size check (Medium, fixed)

### What it was

`fetch` called `net::get`, which read the **entire** response body into a
`Vec<u8>` with no bound, and only then checked the length:

```rust
let body = crate::net::get(url)?;          // unbounded read
if body.len() > MAX_BODY {                 // too late
    return Err(format!("通知文档过大（{} 字节，上限 {MAX_BODY}）", body.len()));
}
```

`MAX_BODY` is 256 KiB, so the check was correct in intent and useless in effect:
by the time it ran, a hostile endpoint had already made the launcher allocate
whatever it wanted.

### Proof

The same shape applied to every `net::get` caller, not just the feed. With the
old code, a response of *N* bytes cost *N* bytes of resident memory before any
caller could object. Under `panic = "abort"` an allocation failure is not a
recoverable panic — it aborts the process.

### Fix

The limit is now a parameter of the read loop, enforced per chunk:

```rust
pub fn get_max(url: &str, limit: usize) -> Res<Vec<u8>>;
```

`read_all(limit, deadline)` checks `out.len() > limit` after each chunk and
returns an error immediately, so the body is abandoned while it is still
arriving. `notify::fetch` passes its own 256 KiB; `get`/`get_opt` pass
`MAX_DOC` (64 MiB), which is generous enough for the largest legitimate
document — a big modpack's asset index — while still turning an unbounded
allocation into a clean error.

Test: `notify::tests::a_huge_body_is_refused_with_a_clear_message` pins the
relationship `MAX_BODY < net::MAX_DOC`, so the tighter limit cannot silently
stop being the one that fires.

---

## N2 — Slowloris wedges 检查更新 (Medium, fixed)

### What it was

`notify_fetch` guards against stacked requests with a `feed_busy` flag:

```rust
if app.feed_busy.swap(true, SeqCst) { return; }   // one click, one request
```

The flag is cleared only by `notify_done`, which runs when the worker posts
`WM_APP_NOTIFY`. If that post never arrives, the button is dead for the rest of
the session — and nothing in the old code bounded how long the worker could take.

WinHTTP's receive timeout does not save this: it is a *per-operation* timeout
that restarts on every successful read. A server that sends one byte every few
seconds keeps the read alive indefinitely, so one such host holds `feed_busy`
set forever. On the startup path (`check_feed`) the same request leaks a thread
for the process lifetime.

### Proof

`net::Session::open` deliberately does **not** call `WinHttpSetTimeouts` — a
comment there records that setting them broke Modrinth on this host profile —
so there was no total deadline anywhere in the read path.

### Fix

Two independent bounds, because they cover different failures:

1. **`net::DOC_DEADLINE` (60 s)** — `read_all` takes a deadline and checks
   `Instant::now() >= d` on every iteration. This is the bound that actually
   stops a trickle, because it measures total elapsed time rather than time
   since the last byte.
2. **`gui::feed_unwedge` (90 s grace)** — the UI-side rescue. `App` gained
   `feed_started: Option<Instant>`; the 200 ms `TIMER_UI` handler calls
   `feed_unwedge`, which clears a flag whose worker has been silent past the
   grace period. It first re-reads `feed_q`, so a worker that stored its result
   but died before posting the message still gets its result displayed.

The grace period is deliberately longer than the network deadline so a slow but
working request is never cut off by the UI — this only rescues a worker that
vanished entirely.

---

## N3 — Bidi / zero-width spoofing of announcement text (Medium, fixed)

### What it was

Every feed field went through `clean` / `clean_line`, which filtered on
`char::is_control`. That covers Unicode category **Cc** only. Bidirectional
overrides and zero-width characters are category **Cf** and passed straight
through into the list box, the body edit control, and `lfl-seen.ini`.

The consequences are specific to this feature, because the announcement list is
the one place a hostile feed draws text the user is meant to trust:

* `U+202E` RIGHT-TO-LEFT OVERRIDE reverses rendering, so a title can be
  displayed as something other than what it says;
* `U+200B` ZERO WIDTH SPACE and `U+2060` WORD JOINER are invisible but count as
  characters, so two notes can render identically while carrying different
  `id`s — the second one never gets marked read, or gets marked read when it
  was not;
* `U+FEFF` and the tag block are standard spoofing filler.

### Proof

Reproduced before the fix: a note `id` of `"a\u202Eb"` survived parse intact and
was written to the state file as `seen=a\u202Eb`, rendering as `ab` in the list
but comparing unequal to `ab`.

### Fix

`is_invisible_format(c)` rejects the Cf ranges that matter for display
spoofing — soft hyphen, bidi embeddings/overrides/isolates, zero-width
space/joiners, Hangul fillers, the tag block, `U+FEFF` — and both `clean` and
`clean_line` apply it. Characters are dropped, not escaped: none of them is
legitimate in a launcher announcement, and dropping keeps the byte offsets out
of the state file.

Test: `notify::tests::invisible_format_characters_cannot_spoof_a_title` asserts
the four ranges are removed **and** that CJK and emoji survive, so this cannot
regress into a "strip everything non-ASCII" filter.

---

## N4 — CR/LF in a document-supplied URL reaches the request line (Medium, fixed)

### What it was

`Url::parse` split a URL into `host` / `port` / `path` and checked the host for
`space \t / \ # ?` — but never for `\r` or `\n`, and never checked the path at
all. Both values are handed to WinHTTP as wide strings:

```rust
let path = util::to_wide(&url.path);
WinHttpOpenRequest(connect.0, verb.as_ptr(), path.as_ptr(), ...)
```

The path becomes the **request target** of `GET <path> HTTP/1.1`. A CR/LF in it
terminates the request line early and makes whatever follows parse as a header —
classic request splitting.

### Reachability

This is not a hypothetical: URLs come from remote documents throughout the
launcher. `version.rs` reads `downloads.client.url` and `downloads.artifact.url`
from a version JSON, `modrinth.rs` reads `files[].url` from the Modrinth API, and
`json.rs` decodes `\r` and `\n` escapes into real control characters. A tampered
version JSON can therefore supply a download URL containing CRLF.

### Fix

Two layers, because `Url`'s fields are `pub` and `Url::parse` is not the only
way to build one:

1. **`Url::parse`** rejects any control character or space in the host and in
   the path, so the normal route cannot produce such a `Url`.
2. **`Session::open_with`** re-checks host and path immediately before the
   WinHTTP calls, and also validates the optional raw header string (`Range:`
   must be a single control-free line). This is the only place that matters,
   since it is the function that actually puts bytes on the wire.

Tests: `net::tests::url_rejects_control_characters_in_host_and_path` and
`net::tests::open_with_refuses_a_directly_built_url_with_crlf`. The second
builds a `Url` struct directly, bypassing `parse`, and asserts the refusal
happens before any socket is opened — while also pinning that the legitimate
`Range: bytes=0-\r\n` header still passes, so the guard cannot break resume.

---

## N5 — Plaintext `http://` feed accepted silently (Low, fixed as advisory)

### What it was

`fetch` accepted `http://` with no notice. Over plaintext, anyone on the network
path can rewrite the announcements — including the "有新版本 v…" hint, which is
the launcher telling the user to go download something.

### Fix

A one-shot advisory via `util::warn`, following the same **inform, do not
obstruct** policy the unverified-download warning uses (0.3.1): a LAN mirror or
a self-hosted host without TLS is a legitimate setup, so the URL is not refused.
The warning is once per process, and a no-op in the CLI, which installs no
warner.

---

## N6 — `reported_update` is dead state (Low, fixed)

### What it was

`Seen::reported_update` was written by `notify_done` and loaded by
`Seen::load`, with a comment claiming it stops the same update being announced
twice. Nothing ever **read** it back:

```rust
if let Some(v) = &update {
    // Remember it so the same update is not announced twice.
    app.seen.reported_update = Some(v.clone());
}
```

So the same "有新版本" line reappeared on every start, and the field — plus the
`one_line` defence written for it — was dead weight. Found by asking, for each
persisted field, "who reads this?"

### Fix

`notify_done` now compares the new `latest` against `seen.reported_update`:
a *different* version is announced as 有新版本, the already-reported one is
shown as 可更新到 without claiming it is news. The persisted value therefore
does what its comment says.

---

## N7 — No feed signature (Info, accepted)

The feed is plain JSON fetched over TLS with no signature and no pinning, so a
compromised feed **host** can publish any announcement, and can claim any
`latest` version. Accepted, because:

* the feature is off by default and has no built-in endpoint, so there is no
  server for the launcher to trust on the user's behalf;
* the document is display-only. `url` is never opened (see below), `latest`
  only produces a hint, and no field selects a file to download or execute.
  The blast radius of a forged feed is a misleading sentence, not code execution;
* signing would require key management for a project that deliberately has no
  server infrastructure.

Revisit if the feed ever gains the ability to trigger a download.

---

## Areas re-verified as sound

| Area | Why it holds |
|---|---|
| `url` is never opened | Displayed in the body control only; 打开下载页 jumps to the 下载 tab (`show_tab(app, 1)`), it does not `ShellExecute`. `file://` and `javascript:` are rejected at parse time. |
| State-file injection | `clean_line` drops control + format characters; `is_version_shaped` rejects anything that is not a version; `Seen::save` applies `one_line` as a third layer, tested with a directly-constructed `Seen`. |
| `Settings::save` | Values are `one_line`-flattened at write time, so a hand-edited `java=` or `feed_url=` cannot forge a second line. New test: `save_cannot_inject_a_second_key_line`. |
| Version comparison | Numeric per component, padded with zeros, garbage yields "not newer" — a malformed `latest` can never prompt an update. |
| Document caps | 256 KiB body (now enforced mid-stream), 50 notes, 2000-char fields, 32-char version, 128-char id, 200 remembered ids. |
| JSON parser | Depth 200; a top-level array or scalar is rejected; non-object note entries skipped rather than fatal. |
| TLS downgrade | `WINHTTP_OPTION_REDIRECT_POLICY = DISALLOW_HTTPS_TO_HTTP` set on every session (round 1, S3). |

---

## Changes in this round

| File | Change |
|---|---|
| `src/net.rs` | `MAX_DOC` + `TOO_LARGE` + `DOC_DEADLINE`; `read_all(limit, deadline)`; `get_max()`; control-char checks in `Url::parse` and `Session::open_with`; `AGENT` now built from `CARGO_PKG_VERSION`; 3 new tests |
| `src/notify.rs` | `is_invisible_format()` applied in `clean`/`clean_line`; `fetch` uses `get_max(MAX_BODY)`; `http://` advisory; 2 new tests |
| `src/settings.rs` | `one_line()` applied in `save`; 1 new test |
| `src/gui/main.rs` | `App::feed_started`; `feed_unwedge()` on `TIMER_UI`; `reported_update` now read |

`LFL.exe` 526,336 B; `LFL-gui.exe` 664,576 B (both still < 1 MB).
244 → **251 tests**, 0 failed.

---

## Priority for the next pass

1. **Every other `net::get` caller.** N1 was a property of the transport, not of
   the feed: `version.rs`, `java.rs`, `modrinth.rs` and `loaders.rs` all call it
   too. They now share `MAX_DOC`, but each should state its own expected size the
   way `notify` does.
2. **Startup-without-a-gesture code.** `check_feed` is the only network call the
   user does not ask for. Anything added to that path should be justified.
3. **Persisted fields.** N6 was found by asking "who reads this?" of every field
   in `lfl-seen.ini` and `lfl-settings.ini`. The same question is worth asking of
   `lfl-settings.ini` as a whole.
