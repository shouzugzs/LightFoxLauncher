# Security audit — LightFoxLauncher 0.2.1 (round 1)

Scope: all 23 files under `src/` (18,400 lines), reviewed at revision 0.2.1. Fixed in 0.3.0.
Method: source review plus executed proof-of-concept against the actual
library, not inference. Every finding marked **fixed** has a regression test
that fails on the old code.

Threat model: the launcher downloads and executes code from the network on the
user's behalf. The interesting adversaries are therefore

1. **a tampered or hostile HTTP response** (version JSON, asset index, runtime
   manifest, Modrinth API) — the launcher trusts these documents to say *which
   file goes where*;
2. **a hostile archive** — a `.jar` the launcher unpacks into the data dir;
3. **another local process** sharing the data directory.

Out of scope: Microsoft authentication (there is none), the security of the
Minecraft client itself, and anything requiring an attacker to already have
write access to the user's profile.

---

## Summary

| ID | Severity | Finding | Status |
|---|---|---|---|
| S1 | **High** | Remote-controlled path traversal → arbitrary file write (6 sites) | fixed |
| S2 | **Medium** | Panic on non-ASCII asset hash (fatal under `panic = "abort"`) | fixed |
| S3 | **Medium** | https→http redirect downgrade permitted | fixed |
| S4 | **Low** | Manifest and runtime documents accepted without a signature | documented |
| S5 | **Low** | GUI debug log written unconditionally to `%TEMP%` | **fixed** — see round 2 C2 |
| S6 | **Low** | World-writable `%TEMP%` lock directory | documented |
| S7 | Info | No pinned TLS, no certificate pinning (WinHTTP defaults) | accepted |

No finding was found in: ZIP extraction (already hardened), JSON parsing (depth
limited), DEFLATE (output capped), the version-chain walk (depth limited), or
process spawning (no shell involved).

---

## S1 — Remote-controlled path traversal (High, fixed)

### The bug

`std::path::Path::join` **replaces** the base when the argument is absolute:

```rust
Path::new("C:/mc/mods").join("C:/Windows/evil.jar")  // => "C:/Windows/evil.jar"
```

and it does not resolve `..`, so `join("../../evil.jar")` produces a path that
resolves two levels above the data directory. Six places joined a value that
comes from a network document straight onto a directory:

| Site | Value | Source document |
|---|---|---|
| `libraries.rs:48` `library_path()` | `d.path` | version JSON (`downloads.artifact.path`) |
| `assets.rs:52` `object_path()` | hash | asset index (`objects.<hash>.hash`, and the map key) |
| `assets.rs:57` `index_path()` | `index.id` | version JSON (`assetIndex.id`) |
| `java.rs:436` runtime install | `key` | Mojang runtime manifest (`files.<path>`) |
| `modrinth.rs:286` `download_file()` | `f.filename` | Modrinth API (`files[].filename`) |
| `loaders.rs:213` `install()` | `b.profile_id` | loader meta API (`loader.version`, `intermediary.version`) |

### Proof

Executed against the shipped code before the fix:

```
index_path("../../../evil")  = "C:/mc\assets\indexes\../../../evil.json"
index_path("C:/Windows/evil") = "C:/Windows/evil.json"
object_path("..")            = Some("C:/mc\assets\objects\..\..")
```

`index_path` is worse than the others: `load_index()` **writes** to that path
(`net::download` → temp file → rename), so a tampered version JSON chooses the
destination of a file write.

### Exploitability

`https://piston-meta.mojang.com` is the normal source, so this is not remotely
reachable in a clean environment. It becomes reachable through:

- a **captive portal or transparent proxy** on an untrusted network, which can
  serve any response for any host if the TLS connection is intercepted (a proxy
  CA installed by corporate policy is the common case);
- **S3** (redirect downgrade) on a network where that downgrade is possible;
- a **modified `versions/<id>/<id>.json`** on disk, which is the normal state of
  a hand-installed or modded version — the launcher deliberately supports those.

Because every download is sha1-checked *after* it lands, a traversed write
usually fails its hash and is deleted. But the write itself has already
happened, and `index_path`/`object_path` are reached before any hash exists for
the destination. Treat it as arbitrary file write.

### Fix

Three new validators in `src/util.rs`:

- **`safe_join(base, rel)`** — joins only a relative path, rejecting drive
  letters, leading separators, `.`/`..`, empty components, and NUL. `\` is
  normalised to `/` first so a Windows-authored path is validated the same way
  it resolves.
- **`safe_component(value)`** — rejects separators, `.`/`..`, drive letters,
  control characters, `< > " | ? *`, and the 22 Windows reserved device names
  (`con`, `nul`, `com1`…), which are legal path components but unusable as files.
- **`version::version_dir()` / `version_dir_or_fallback()`** — validates a
  version id before it becomes a directory name, so the eleven
  `versions_dir(dir).join(id)` call sites all route through one check.

Applied at every site in the table. Infallible signatures were kept: an invalid
value maps to a fixed name inside the intended directory (`invalid-artifact`,
`invalid-index.json`, `invalid-version-id`), so the subsequent sha1 check fails
loudly instead of a write landing somewhere unexpected.

### Regression tests

`safe_join_accepts_relative_paths`, `safe_join_rejects_escapes`,
`safe_component_accepts_real_names`, `safe_component_rejects_paths_and_devices`
(`util.rs`); `object_path_rejects_non_hex`, `index_path_rejects_escapes`
(`assets.rs`); `version_dir_rejects_escapes`,
`version_dir_fallback_stays_inside_versions` (`version.rs`).

---

## S2 — Panic on a non-ASCII asset hash (Medium, fixed)

`object_url()` took a byte slice `&h[..2.min(h.len())]` of a hash that comes from
the asset index. A multi-byte character straddling byte 2 panics:

```
thread 'main' panicked: end byte index 2 is not a char boundary;
it is inside 'é' (bytes 1..3) of `aé`
```

With `panic = "abort"` in the release profile this is an immediate process
termination — a one-line denial of service from a tampered index, and for the
GUI a window that vanishes with no message.

Fixed by rejecting non-hex hashes before slicing (the same check `object_path`
now applies) and returning an empty URL, which fails the download rather than
the process. Regression test: `object_url_never_panics_on_non_ascii`.

---

## S3 — https→http redirect downgrade (Medium, fixed)

WinHTTP follows redirects by default, and its default redirect policy permits
`https` → `http`. The launcher never called `WinHttpSetOption`, so a redirect
chain could downgrade the connection to plaintext.

Impact is bounded by the sha1 checks on file downloads, but **not** on the
documents that supply those hashes: the asset index is fetched with `net::get`
and only its *contents* are used to build download URLs. A downgraded index
fetch is a direct route into S1.

Fixed by setting `WINHTTP_OPTION_REDIRECT_POLICY` to
`REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP` on every session, best-effort (an older
WinHTTP that does not know the option keeps its default rather than failing the
request).

---

## S8 — Announcement feed is fully remote (reviewed, by design)

The 通知 page reads a JSON document from a URL the user configures. That document
is as untrusted as any other network input, and it is the one place a *third
party* chooses what the launcher displays.

Controls, all enforced in `src/notify.rs`:

| Risk | Control |
|---|---|
| Phishing via a "download here" link | `url` is **displayed only**; the launcher never opens it. Only `http(s)` is kept in the model at all. |
| Fake update prompt | `latest` is parsed as numeric parts and compared numerically; a non-version string is dropped, so `"latest": "urgent!"` cannot prompt anything. |
| Unbounded memory / display flooding | Document capped at 256 KB, notes at 50, each field at 2000 chars, ids at 128. |
| Terminal / control-character injection into a list box | Control characters stripped from every field before use. |
| Byte-slice panic on non-ASCII | No byte indexing of feed strings; length caps use `chars().count()`. |
| Retry storm against a dead endpoint | Exactly one request per check, no retry loop. |
| State file growth | `lfl-seen.ini` capped at 200 ids, written atomically via temp+rename. |

Verified by test: HTTP 500, a non-JSON body, and a refused connection each
produce an error string on the status line and leave the previous feed intact.
Nothing in this path can panic.

Residual risk, accepted: the feed is trusted to the same degree as TLS. A
compromised feed host can show arbitrary *text* to the user. It cannot write
files, run code, open a URL, or affect a launch.

**The launcher never downloads or replaces its own executable.** An update is a
version number plus a link; acting on it is manual.

## S4 — Unsigned metadata documents (Low, accepted)

The version manifest, asset index, runtime manifest and Modrinth responses are
accepted on TLS alone. There is no signature check and no pinned public key.

This is standard for third-party Minecraft launchers — Mojang does not publish a
signing key for these documents — and cannot be fixed without an external trust
root the project does not have. It is why S1 and S3 mattered: the documents are
trusted, so the code that consumes them must not amplify a bad one into a write
outside the data directory.

The `FALLBACK` list in `version.rs` (nine hard-coded release URLs and sha1s) is a
pinned, offline-safe subset and is *not* affected.

---

## S5 — GUI debug log (Low) — superseded by round-2 C2

> **Resolved.** The second audit (, finding C2) scored
> this 5.5 Medium rather than Low, because the log was unbounded, predictable, and
> recorded the user's action sequence in a release build. It is now gated behind
> `LFL_DEBUG=1` and capped at 512 KB. The text below records the original
> finding.

`gui_debug()` in `src/gui/main.rs` appends every `WM_DRAWITEM` and `WM_COMMAND`
to `%TEMP%\lfl-gui-debug.log` on every run, unconditionally, and never truncates.
It logs control ids and button labels only — no paths, no player name, no
account data — so it is not a confidentiality leak, but it is unbounded growth
in a shared directory and it is diagnostic scaffolding left switched on.

Recommendation: gate it behind an environment variable
(`LFL_DEBUG=1`) before a public release. Not changed here because it is the only
observability the GUI has and removing it would make the next layout regression
undebuggable in this environment.

---

## S6 — Lock files in `%TEMP%` (Low, accepted)

Per-file download locks live in `%TEMP%\lfl-locks\<sha1-prefix>.lck`
(`net.rs:760`). `%TEMP%` is per-user on Windows and not world-writable by
default, so this is only a concern on a misconfigured multi-user machine. The
lock is an optimisation (two processes downloading the same file); a hostile
process creating lock files can slow a download but cannot corrupt one, because
the payload is sha1-verified.

---

## S7 — TLS trust (Info, accepted)

Certificates are validated by WinHTTP against the system store. There is no
pinning. This is correct behaviour for a launcher that must keep working across
Mojang's CDN changes, and pinning would break on any certificate rotation.

---

## Verified sound (no action)

These were checked and are genuinely safe; they are listed so the next audit does
not redo the work.

| Area | Why it holds |
|---|---|
| **ZIP extraction** (`zip.rs`) | `safe_components()` rejects `..`, `/abs`, `C:`, `\\srv\share`; `META-INF` stripped; per-entry size capped at 256 MiB; traversal entries are *skipped*, not fatal. Existing tests cover all of it. |
| **DEFLATE** (`inflate.rs`) | Output capped at `expected + 1 MiB`, initial allocation capped at 64 MiB — a decompression bomb is an error, not an OOM. |
| **JSON** (`json.rs`) | `MAX_DEPTH = 200`; deeper input is a parse error. No recursion bomb. |
| **`inheritsFrom` chain** (`version.rs`) | `MAX_DEPTH = 8`, self-reference and cycle both detected. |
| **Process spawning** | `Command::new(exe).args(...)` throughout; no shell, no `cmd /c`, no string interpolation into a command line. `reg.exe` and `where.exe` are called with fixed arguments. |
| **JVM arguments** | Player name is validated to 1–16 chars with no control characters; memory validated to a numeric form in 256 MB–64 GB. `auth_access_token` is the literal `"0"` — the launcher has no authentication to leak. |
| **Instance / profile names** | `validate_instance_name` and `validate_profile_name` already rejected separators, `..`, reserved names and control characters. |
| **Delete paths** (`versionmgr.rs`) | Lexical containment check (`normalize_lexically`) that the target is a direct child of `versions/`; rejects `default` and shared directories. |
| **Panics** | Zero `unwrap()`/`expect()` in production code (all 94 occurrences are inside `#[cfg(test)]`); errors are `Result<T, String>`. |
| **Secrets** | No credentials, tokens, or API keys anywhere in the tree. |
| **`unsafe`** | Confined to documented Win32 FFI plus one `unsafe impl Send` in `lock.rs`, which is sound: the handle is only used to close itself. |
| **Concurrency** | Shared state behind `Arc<Mutex<_>>`; the per-file lock prevents two processes writing the same destination. |

---

## Changes made

| File | Change |
|---|---|
| `src/util.rs` | **new** `safe_join()`, `safe_component()` + 4 tests |
| `src/version.rs` | **new** `version_dir()`, `version_dir_or_fallback()` + 2 tests; 3 call sites routed through them |
| `src/assets.rs` | `object_path()` requires hex; `index_path()` validates its id; `object_url()` no longer panics + 2 tests |
| `src/libraries.rs` | `library_path()` uses `safe_join` |
| `src/java.rs` | runtime install uses `safe_join`, skips unsafe entries |
| `src/modrinth.rs` | `download_file()` validates the filename |
| `src/loaders.rs` | `install()` validates `profile_id` |
| `src/launcher.rs` | version dir via `version_dir_or_fallback` |
| `src/net.rs` | `WinHttpSetOption` + `WINHTTP_OPTION_REDIRECT_POLICY` |

Binary cost: `LFL.exe` 520,704 → **524,800 B** (+4,096); `LFL-gui.exe`
634,880 → **637,952 B** (+3,072). Both remain under the 1 MB budget.

Tests: 216 → **225 passing**, 0 failed, 3 ignored (network).
