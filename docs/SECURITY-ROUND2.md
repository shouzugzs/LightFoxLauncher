# Security audit — round 2: debug interfaces and data-destruction paths

Scope: the debug/diagnostic surfaces and the destructive maintenance actions,
reviewed at 0.2.1, fixed in 0.3.0, after the first audit (`docs/SECURITY.md`). Second pass over
all 23 files under `src/`.

Every finding below was **reproduced by executing code**, not inferred from
reading. The proof for each is quoted from actual output.

Scoring: CVSS v3.1 base metrics. Scores assume the default configuration and a
local, unprivileged attacker unless stated otherwise.

---

## Summary

| ID | CVSS | Vector | Finding | Status |
|---|---|---|---|---|
| **C1** | **7.1 High** | `AV:L/AC:L/PR:N/UI:R/S:U/C:N/I:H/A:H` | 删除断点文件 deletes the user's own files (`.part` substring match) | **fixed** |
| **C2** | **5.5 Medium** | `AV:L/AC:L/PR:N/UI:R/S:U/C:H/I:N/A:N` | Debug log written unconditionally: unbounded, predictable, observable | **fixed** |
| **C3** | **4.6 Medium** | `AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:L/A:N` | Feed values inject lines into `lfl-seen.ini` | **fixed** |
| **C4** | **3.3 Low** | `AV:L/AC:L/PR:N/UI:R/S:U/C:N/I:L/A:N` | 工具页 downloader has no integrity check by design | accepted, documented |
| **C5** | **2.2 Low** | `AV:L/AC:H/PR:L/UI:R/S:U/C:N/I:L/A:N` | Predictable lock filenames in `%TEMP%` | accepted |
| **C6** | Info | — | `--dir` accepts any path; `LFL_DEBUG` is the only env switch | accepted |

No memory-safety issue was found in any debug path. `unsafe` remains confined to
Win32 FFI.

---

## C1 — 删除断点文件 destroys user data

**CVSS 3.1: 7.1 High** — `AV:L/AC:L/PR:N/UI:R/S:U/C:N/I:H/A:H`

### What it was

`net::is_partial_name` decided what 工具 → 删除断点文件 deletes:

```rust
pub fn is_partial_name(name: &str) -> bool {
    name.ends_with(".lflpart") || name.contains(".part")
}
```

`contains(".part")` matches files the launcher never wrote. The tool then walks
the **whole data directory** and deletes every match:

```rust
for p in launcher::walk(&dir) {
    let name = p.file_name()...;
    if lightfox::net::is_partial_name(&name) || name.ends_with(".tmp") {
        std::fs::remove_file(&p)
    }
}
```

### Proof

```
level.part1.mca                  -> deleted=true
mymod.particle.jar               -> deleted=true
world.partial                    -> deleted=true
notes.part.txt                   -> deleted=true
a.jar.lflpart                    -> deleted=true
```

### Impact

One click on a maintenance button silently destroys:

* **split saves** (`level.part1.mca`, …) — world data, unrecoverable;
* **mods whose filename contains `.part`** (`particle`, `partial`, `partner`) —
  the mod silently stops loading, and the cause is invisible;
* **any user backup** named `*.partial`.

The walk covers `instances/` and `versions/`, so this reaches saves, not just
downloads. `Integrity: High` because the deletion is silent and irreversible;
`Availability: High` because a save or a mod is destroyed. `UI:R` because the
user must click the button — but the button is labelled *删除断点文件*
("delete partial files"), so the consent obtained is for deleting the launcher's
own temp files, not the user's data.

### Fix

Only the two shapes the launcher actually writes are matched:

```rust
pub fn is_partial_name(name: &str) -> bool {
    if name.ends_with(".lflpart") { return true; }   // current layout
    let Some(idx) = name.rfind(".part") else { return false };
    let rest = &name[idx + ".part".len()..];
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())   // old `.part<N>`
}
```

`.particle`, `.partial` and `.part1.mca` no longer match. Regression test
`is_partial_name_does_not_match_user_files` asserts six user-file shapes survive.

---

## C2 — Debug log written unconditionally

**CVSS 3.1: 5.5 Medium** — `AV:L/AC:L/PR:N/UI:R/S:U/C:H/I:N/A:N`

### What it was

```rust
pub fn gui_debug(msg: &str) {
    let p = std::env::temp_dir().join("lfl-gui-debug.log");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "{msg}");
    }
}
```

Called on **every** `WM_DRAWITEM` and **every** `WM_COMMAND`, in release builds,
with no switch and no size cap.

### Findings

| Property | Consequence |
|---|---|
| No opt-in | Ships enabled to every user |
| `%TEMP%\lfl-gui-debug.log`, fixed name | Any local process can locate it without guessing |
| `append` mode, never truncated | Grows on every repaint — a long session with a download running can add hundreds of thousands of lines |
| Records control ids and button text | A local observer learns the exact sequence of user actions, including which version was launched and when |
| Never deleted | Persists after the launcher exits, indefinitely |

The contents are not secrets (no paths, no player name, no account data), so
confidentiality impact is on **behaviour**, not credentials. Hence `C:H` on the
activity pattern, not on data. `UI:R` because the user must run the launcher;
`AC:L` because reading `%TEMP%` needs no special access on a single-user machine.

### Fix

Opt-in via `LFL_DEBUG=1` or `LFL_DEBUG=true`; anything else means off. Capped at
512 KB, truncated rather than grown.

Verified by running the release binary both ways:

```
no log before run
OK: no debug log written without LFL_DEBUG=1

OK: log written when LFL_DEBUG=1 -> 24 lines
WM_COMMAND id=102 code=1024 userdata=0x2073397c880
```

A release build now writes nothing unless asked.

---

## C3 — Feed values inject lines into the state file

**CVSS 3.1: 4.6 Medium** — `AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:L/A:N`

### What it was

`notify::clean` stripped control characters **except `\n`**:

```rust
let s: String = s.chars().filter(|c| !c.is_control() || *c == '\n').collect();
```

That is correct for a note *body* (multi-line, shown in an edit control) but it
was applied to `latest`, `id`, `date`, `title` and `url` as well — all of which
are written to `lfl-seen.ini` as `key=value` lines.

### Proof

A feed containing `"latest": "0.3.0\nreported_update=9.9.9"` produced:

```
--- file ---
last_check=0
reported_update=0.3.0
reported_update=9.9.9
--- end ---
reloaded reported_update = Some("9.9.9")
```

The injected line won on reload. A note `id` of `"a\nseen=forged"` likewise
produced a forged `seen=` entry.

### Impact

The blast radius is confined to `lfl-seen.ini` — it is a separate file from
`lfl-settings.ini`, and its parser only understands three keys, so this cannot
escalate to code execution or arbitrary writes. What it can do is **suppress the
update prompt** (forge `reported_update`) or **mark arbitrary notes as read**
(forge `seen=`), which is a small but real integrity break chosen by a remote
party. `AV:N` because the feed is fetched over the network; `UI:N` because the
check runs automatically on start when enabled.

### Fix

Three layers:

1. **`clean_line`** for every single-line field (`latest`, `url`, `id`, `date`,
   `title`): strips all control characters, including `\n`. `clean` (which keeps
   `\n`) is now used **only** for the body.
2. **`is_version_shaped`** validates `latest` against a strict pattern. "Starts
   with a digit" was not enough — the newline-stripped `0.3.0reported_update=9.9.9`
   still passed it. A version is now digits, dots and an optional
   `-`/`+` suffix of identifier characters, and nothing else.
3. **`Seen::save` sanitises again** via `one_line`, because `Seen` is a public
   struct and a caller could set the fields directly.

`json.rs` is a fourth layer: a *literal* newline inside a JSON string is invalid
JSON and is rejected before `clean_line` runs. Only the `\n` escape reaches it.

Tests: `feed_cannot_inject_lines_into_the_state_file`,
`save_sanitises_even_a_directly_constructed_value`, `version_shape_is_strict`,
`raw_control_bytes_are_rejected_by_the_json_parser`.

---

## C4 — Tool-page downloader has no integrity check

**CVSS 3.1: 3.3 Low** — `AV:L/AC:L/PR:N/UI:R/S:U/C:N/I:L/A:N`

The 工具 page's multi-threaded downloader accepts arbitrary URLs and calls:

```rust
items.push(Download::new(u, dest, "", 0));   // empty sha1, unknown size
```

`net::download` skips both the digest and the size check when `sha1` is empty, so
the file is whatever the server sent.

This is **inherent to a generic downloader**: there is no expected hash for an
arbitrary URL. What was checked and is sound:

* the destination filename is derived with `rsplit('/')`, so only the **last**
  segment is used — `https://host/../../evil.jar` yields `evil.jar` inside the
  chosen directory, not an escape (verified);
* the destination is the directory the user picked in a folder dialog, not a
  launcher-managed path.

Residual risk, accepted and documented: a user who downloads from a URL they
chose gets a file they did not verify. This is the same trust model as a browser
download, and the tool is not used by any launcher-internal path.

---

## C5 — Predictable lock filenames in `%TEMP%`

**CVSS 3.1: 2.2 Low** — `AV:L/AC:H/PR:L/UI:R/S:U/C:N/I:L/A:N`

Locks live at `%TEMP%\lfl-locks\<sha1-24>.lck` and
`%TEMP%\lfl-runtime-<sha1-16>\download.lock`. Both names are deterministic from
the data-directory path, so a local process can pre-create them.

Verified on this host that `%TEMP%` is per-user:

```
TEMP=<user>\AppData\Local\Temp
Owner: BUILTIN\Administrators
  NT AUTHORITY\SYSTEM : FullControl
  BUILTIN\Administrators : FullControl
  <machine>\<user> : FullControl
```

Only the user and administrators can write there, so exploiting this needs code
already running as the user — at which point the lock is not the weakest link.
Worst case is a denial of *concurrency*, not corruption: the payload of every
download is sha1-verified after the fact, and the lock only serialises writers.

Accepted. Moving the locks into the data directory was rejected because it leaves
stray `.lck` files in a tree users inspect, which looks like corruption.

---

## C6 — Environment and argument surface

**Info.** Reviewed, no issue found.

| Surface | Assessment |
|---|---|
| `LFL_DEBUG` | The only debug switch. `1`/`true` enables, everything else is off. Read per call; cannot be used to change behaviour other than logging. |
| `--dir` / `-d` | Accepts any path as the data directory. **Intended** — it is how a user points the launcher at an existing install. It does not bypass any check: every path built from it still goes through `safe_join`/`version_dir`. |
| `--instance`, `--user`, `--memory` | Validated by `Options::validate` (single path component; 1–16 chars; numeric memory in range). |
| `JAVA_HOME`, `PATH`, `LOCALAPPDATA`, `USERPROFILE`, `ChocolateyInstall` | Read to *find* a Java runtime. Values are used as candidate directories and probed for `bin/java.exe`; they never become a download destination or a command line. |
| `reg.exe` / `where.exe` | Invoked with fixed arguments; output parsed as paths, then probed. No shell, no interpolation into a command string. |
| No hidden flags | Every accepted flag is documented in `--help`. No undocumented behaviour switch exists. |

---

## Areas re-verified as sound

| Area | Why it holds |
|---|---|
| Debug/`unsafe` paths | `unsafe` only in Win32 FFI plus one `unsafe impl Send` in `lock.rs` (handle used only to close itself). No `unwrap()` in production code. |
| Recursive deletes | Only `versionmgr::delete` (lexically confined to `versions/<id>`, rejects `default` and shared dirs) and `clean_parts` (now narrowly scoped, C1). |
| `walk()` | Follows `file_type()`; symlinks are not descended as directories. |
| Zip extraction | `safe_components` rejects `..`, absolute, drive letters, UNC; `META-INF` stripped; 256 MiB per-entry cap. |
| JSON / DEFLATE | Depth 200; output capped at expected+1 MiB; 64 MiB prealloc cap. |
| Process spawn | `Command::new(exe).args(...)`; `CREATE_NO_WINDOW`; no shell. |
| Feed path | One request, no retry; 256 KB document cap; 50 notes; 2000-char fields; `url` displayed, never opened. |

---

## Changes in this round

| File | Change |
|---|---|
| `src/net.rs` | `is_partial_name` narrowed to `.lflpart` / `.part<digits>` + regression test |
| `src/gui/main.rs` | `gui_debug` gated behind `LFL_DEBUG=1`, capped at 512 KB; `debug_enabled()` |
| `src/notify.rs` | `clean_line()` for single-line fields; `is_version_shaped()`; `one_line()` defence in `Seen::save`; 5 new tests |

`LFL.exe` 524,800 B (unchanged); `LFL-gui.exe` 653,824 → **656,384 B** (+2,560).
238 → **244 tests**, 0 failed.

## Priority for the next pass

1. **C1-class bugs.** The destructive-action audit found the worst issue in the
   project. Every other function that deletes, moves, or overwrites should get the
   same treatment: state the exact filename shapes it owns and test that a user
   file survives.
2. **Anything auto-running on start** (`check_feed`). It is the only code that
   contacts the network without a user gesture.
3. **Log/diagnostics output** generally. C2 was a policy bug, not a code bug — the
   question "does this need to exist in a release build?" should be asked of every
   diagnostic.
