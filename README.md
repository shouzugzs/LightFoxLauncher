# LightFoxLauncher — a minimal Minecraft launcher in Rust

Version 0.3.0 — see [CHANGELOG.md](CHANGELOG.md) for what changed.

[![Licence: MPL-2.0](https://img.shields.io/badge/licence-MPL--2.0-brightgreen)](LICENSE)
[![Dependencies: none](https://img.shields.io/badge/dependencies-none-blue)](Cargo.lock)
[![Binary: <1 MB](https://img.shields.io/badge/binary-%3C1%20MB-informational)](docs/CONTRACT.md)
[![PRs: not accepted](https://img.shields.io/badge/PRs-not%20accepted-lightgrey)](CONTRIBUTING.md)

The product is **LightFoxLauncher**. The command line is `LFL`, the window title is
`LightFoxLauncher`, and the crate is `lightfox`.

A small Windows launcher for Minecraft, in two front-ends over one shared engine. No GUI toolkit,
no login server, no runtime dependencies — just the game.

| Binary | Size | What it is |
|---|---|---|
| `LFL-gui.exe` | **657,920 B (0.63 MiB)** | native Win32 window: pick version, click play |
| `LFL.exe` | **524,800 B (0.50 MiB)** | command line, scriptable |

```
LFL-gui.exe             # the window
LFL                     # install 1.20.1 (first run) and launch it
LFL 1.8.9               # a different version
LFL list                # what versions Mojang offers
LFL status              # data dir, disk usage, Java in use
LFL --dry-run           # print the exact java command line
```

Both binaries drive the same `lightfox` library: the GUI never re-implements a download, it calls
`launcher::prepare` exactly like the CLI does, so the two cannot drift apart.

## The window

Seven pages, a tab bar, and nothing else:

| Page | What it does |
|---|---|
| **启动** | installed versions only, launch profiles, launch/repair, live log |
| **下载** | game core, mod loader (Fabric/Quilt), and mods — everything installable in one place, each block reporting its own status on this page |
| **模组** | the selected version's mod jars, dependency check, open folder |
| **版本** | list installed versions, delete, clone, manage mods, repair files |
| **设置** | download threads, Java path, 查找 Java |
| **工具** | verify files, clean logs, delete partials, open folder, copy command, multi-threaded downloader |
| **通知** | announcements and launcher-update check, from a feed URL you configure |

The chrome is owner-drawn with GDI: a rounded accent brand mark beside the name, pill-shaped tabs
with an accent bar under the active one, a **card behind every group of controls** with a bold
heading and an accent tick, and a three-tone dark palette. Buttons are owner-drawn too — the primary
action (启动游戏) gets a filled accent face, the rest a raised neutral one, both rounded, with the
label nudged a pixel while pressed. Switching tabs shows/hides control groups rather than
re-creating them, so the window never flickers and no toolkit is involved.

`layout()` and `paint()` share one set of metrics (`PAD`, `LABEL_W`, `ROW_H`, `BTN_H`, `GAP`,
`ROW_GAP`, `CARD_PAD`, `BODY_TOP`) instead of each carrying its own offsets — that is what keeps a
card wrapped around exactly the controls it belongs to, and every row on the same baseline.

The whole visual layer costs **3 KB**: it reuses `RoundRect`/`DrawTextW`/`FillRect` that the binary
already links, and adds no crate, no image resource and no extra control.

Which version you are about to launch is chosen in exactly one place: the **版本** dropdown on
**启动**. There is no second quick-switch control duplicating it. The 下载 page's three blocks
(游戏核心 / 模组加载器 / 模组) each report their own progress and result on that page, right under
the block they belong to, instead of writing into the 启动 page's status line — and a version that
finishes downloading is added to the 启动 dropdown immediately, without a restart.

### 正式版 and 预览版 are separate

Mojang's version manifest mixes stable releases and snapshots in one flat list, so a plain dropdown
would interleave `24w14a` between `1.21.8` and `1.21.7`. The 游戏核心 block therefore has a second
picker next to the version list:

* **正式版** (default) — manifest entries whose `type` is `release`
* **预览版** — `type` `snapshot`, `old_beta` and `old_alpha`

The manifest is fetched once into memory, so switching between the two is a local re-filter and
never touches the network again. The version box still accepts a hand-typed id for anything the
manifest does not list.

Everything still runs on a worker thread: the progress bar and log keep updating while ~800 MB
downloads, and the window stays responsive. Network work (Modrinth search, loader-build lookup,
loader install) also runs off the UI thread, so the window never freezes waiting for a reply.

### Download concurrency

Downloads are parallel **and** configurable. `download_many` fetches up to 12 files at once by
default (`-` in 设置 → 下载线程, or `workers=` in the ini), bounded by the machine's core count and
the batch size. Each file streams on its own thread over the shared WinHTTP path, hash-verified
while it lands. The 设置 page shows the effective value and the detected core count.

The window accepts the CLI options too, so a shortcut can open a specific profile or pre-fetch:

```powershell
LFL-gui.exe --dir D:\mc 1.20.1                  # open pointed at a profile
LFL-gui.exe --dir D:\mc 1.20.1 --install        # open and start downloading
LFL-gui.exe -u Notch -m 4G 1.8.9                # presets
```

## Finding Java

`LFL` normally installs Mojang's runtime on demand, but an existing JDK/JRE on the machine is often
what you actually want. **设置 → 查找 Java** scans, in order:

1. runtimes this launcher installed (`<data>/runtime/*`)
2. `%JAVA_HOME%`
3. every `PATH` entry (and the JDK root above each `bin`), plus `where.exe java` so a PATH that
   changed *after* the launcher started still resolves
4. the `HKLM` **and** `HKCU` `JavaSoft\…` registry keys, including the 32-bit `WOW6432Node` view —
   a JDK installed without elevation registers per-user, and a 32-bit JRE registers under
   `WOW6432Node` even on 64-bit Windows
5. vendor install roots, three levels deep: `Java`, Eclipse Adoptium, AdoptOpenJDK, Amazon
   Corretto, Zulu, Microsoft, BellSoft, Semeru, Red Hat, JetBrains Toolbox/Runtime, Android
   Studio's JBR, `.jdks`, Gradle's `jdks`, `scoop`, chocolatey, and a bare `D:\jdk`-style checkout

Every directory is *name-filtered* before it is read and every candidate is *probed* (`java
-version`) rather than trusted by name. That filter is what keeps the list honest: a folder like
`apps\games` that happens to bundle a `bin/java.exe` is not reported as an installed JDK, which is
exactly the noise a scan is meant to avoid. Depth is bounded and the walk never touches the whole
disk — it reads only the specific directories Java installers are known to use, so it is fast on a
machine with many JDKs and never does a full-volume read.

Results are de-duplicated, sorted newest-first, and shown with their source (`JAVA_HOME`, `PATH`,
`注册表`, `安装目录`). Selecting one fills the Java path field; saving makes it stick. The scan runs
on a worker thread so the window never freezes.

## Mod loaders and mods

A loader is installed the way its own installer does it: fetch the loader's **version profile** — a
normal Minecraft version JSON with `inheritsFrom` pointing at the vanilla version — write it to
`versions/<profile-id>/`, and let the existing `inheritsFrom` chain merge handle the rest. No
loader-specific launch code exists; `mainClass` and the extra libraries come from the profile.

```
LFL loader list 1.20.1                 # what is available for a version
LFL loader install fabric 1.20.1       # newest stable Fabric build
LFL loader install fabric 1.20.1 --loader-version 0.20.1
```

```
1.20.1 可用的加载器：
  Fabric     41 个构建，最新 0.20.1, 0.20.0, 0.19.5, …
  Quilt      41 个构建，最新 0.20.0-beta.9, …
  Forge      需官方安装器（无 profile 源）
  NeoForge   需官方安装器（无 profile 源）
```

Fabric and Quilt publish ready-made profiles and are supported. Forge and NeoForge only ship a Java
installer, which would mean executing downloaded code — deliberately out of scope; the UI says so
instead of failing obscurely.

The **下载 → 模组加载器** block does the same job with controls: pick the family, pick a build for
the version, then 安装加载器. The build list and the result message are both reported on that block
itself, so the page never looks like it did nothing.

Loader profiles put their Maven repository root in a sibling `url` field
(`{"name":"net.fabricmc:fabric-loader:0.20.1","url":"https://maven.fabricmc.net/"}`) with no
`downloads` block, so library resolution honours that field and falls back to Mojang's repo.

Mods are plain jars in `instances/<instance>/mods/`:

```
LFL mods                               # list, with name and version from fabric.mod.json
LFL mods add ~/Downloads/sodium.jar     # copy in, refusing to overwrite
LFL mods remove sodium                  # by name or file name
```

The **模组** page does the same with buttons, plus 打开模组目录.

## Searching and downloading mods

The 下载 page's **模组** block searches Modrinth, which needs no API key (CurseForge does) and
publishes a `sha1` for every file — so a download goes through the same verified path as the game
files. Its result messages stay on that block's own status line.

```
LFL search sodium                      # newest matches, with loader tags and download counts
LFL search sodium --limit 5 --install 2   # download the second hit
```

The selected version narrows the search when the version happens to exist as plain vanilla
(`1.20.1`); a loader profile id such as `1.20.1-fabric` is *not* a valid Modrinth game version, so
the version filter falls back to "any version" instead of silently returning nothing. The loader
filter is derived from the selected version's family (Fabric, Quilt, Forge, NeoForge), so a Forge
setup is not offered Fabric builds. Both searches run off the UI thread.

Downloads land in the current game directory's `mods/` — `versions/<id>/mods/` with isolation on
(the default), `instances/<instance>/mods/` with it off — and are refused when a file of the same
name is already there, so re-clicking cannot clobber a working setup.

## Managing installed versions

**版本** (or `LFL ver`) is local-only management of the `versions/` tree:

```
LFL ver                       # id, size, and a flag for incomplete entries
LFL ver delete 1.20.1         # removes that version directory only
LFL ver clone 1.20.1 1.20.1-modded
```

* **delete** frees the version's own bytes (jar, natives, logs) and never touches `libraries/`,
  `assets/` or `runtime/`, which every other version shares. It refuses any id that is not a single
  path component, so `../x` cannot escape `versions/`.
* **clone** copies the directory, renames `<id>.jar`/`<id>.json` to the new id and rewrites the
  JSON's top-level `"id"` **without re-serialising the document** (a targeted replacement keeps
  `inheritsFrom`, `name` and every other field byte-identical). The source is left untouched.

## Version isolation

By default each version runs **isolated**: `versions/<id>/` *is* the game directory, so a version
keeps its own `saves/`, `options.txt`, `logs/`, `config/` and `mods/`. This is the layout PCL and
the official launcher use (PCL records it as `VersionArgumentIndieV2`), and it means switching
versions switches worlds and mod sets instead of mixing them.

```
versions/1.20.1/          <- isolated: this whole folder is --gameDir
├── 1.20.1.jar
├── 1.20.1.json
├── saves/  options.txt  logs/  config/
└── mods/                 <- mods for this version only
```

Turn it off with `--no-isolate` (or unchecking 版本隔离) to share one `instances/<name>/` across
versions — useful for keeping a single set of worlds while hopping between loader builds.

Because the layout is per-version, **mod downloads follow the selected version**: searching and
downloading from the 下载 page's 模组 block writes into `versions/<id>/mods`, and the version page's
管理模组 button first makes that version (and its mods folder) the active target.

## Version page extras

**版本** manages installed versions, and now also:

* **管理模组** — select that version, switch isolation on, and open the mods view with mod
  downloads pointed at that version's `mods/` (the resolved path is shown).
* **补全文件** — re-run the prepare pipeline for that version, which verifies every SHA-1 and
  re-fetches only what is missing or corrupt (client jar, libraries, natives, assets).
* Selecting a version lists its mods beside the version list, with the resolved mods path shown.

## Multi-threaded downloader (tools page)

**工具 → 多线程下载** is the same engine the game files go through, exposed directly: paste one or
more URLs (whitespace-separated for a concurrent batch), choose the thread count and an arbitrary
target directory. Files stream to a temp name, are hashed as they land, and are renamed only on
success, so an interrupted run resumes rather than leaving a truncated file.

## Mod dependencies

Downloading a mod also installs what it requires. Modrinth reports dependencies per version, either
as an exact `version_id` (used directly) or just a `project_id` (the newest build matching the
selected Minecraft version and loader is chosen). All three kinds are honoured:

| Kind | Behaviour |
|---|---|
| `required` | downloaded automatically |
| `optional` | reported in the log, **not** installed |
| `incompatible` | flagged if already in the mods folder |

**模组 → 检查依赖** applies the same logic to every installed mod at once, which catches a modpack
assembled by hand. Reports read like `Sodium (必需) — 待下载`.

## Launch profiles (several setups per version)

One installed version is often launched several ways — a 4G single-player setup, a 1G server one,
a different player name. **启动 → 配置** keeps those side by side:

```
versions/1.20.1/lfl-profiles/
├── 默认.ini          # 2G · Steve · isolated
└── 服务器.ini        # 4G · Notch  · shared game dir
```

A profile stores only the launch-time choices (heap, player, window, isolation), a few bytes each,
so the jars, mods and saves stay shared. Every version implicitly has `默认`, and the version
dropdown shows the profile count so a version with several setups is obvious.

## Why it is small

Both binaries are **well under a megabyte** with **zero third-party crates** (`Cargo.lock` contains
only `lightfox` itself):

| Normally | Here |
|---|---|
| `serde_json` | hand-written RFC 8259 parser (`src/json.rs`) |
| `reqwest`/`hyper` | direct WinHTTP FFI, 12 worker threads (`src/net.rs`) |
| `sha1`/`md-5` crates | own SHA-1 and MD5 (`src/hash.rs`) |
| `zip` + `flate2` | own ZIP central-directory reader and DEFLATE decoder (`src/zip.rs`, `src/inflate.rs`) |
| `egui`/`iced`/`slint` | raw Win32 + GDI via a ~470-line binding module (`src/gui/win32.rs`) |

The GUI is a single window class with plain child controls (combo boxes, edit fields, a progress
bar) and GDI text drawing — no retained-mode toolkit, no layout engine, no embedded fonts. That is
what keeps it well under a megabyte instead of the several megabytes a Rust GUI crate would add.

`unsafe` is confined to the Win32 FFI declarations and their call sites (`src/net.rs` for WinHTTP,
`src/gui/win32.rs`); all launcher logic is safe Rust. The release profile is tuned for size:
`opt-level="z"`, `lto=true`, `codegen-units=1`, `panic="abort"`, `strip=true`.

Measured: 238 unit tests, all offline (no network needed); the 620 KB asset index parses in ~3 ms.

## What it does

1. **Resolves the version** from Mojang's `version_manifest_v2.json` (cached for 30 minutes).
2. **Ensures a Java runtime.** Uses `%JAVA_HOME%`/`PATH` if a suitable JDK is installed; otherwise
   installs Mojang's own JRE (component `java-runtime-gamma`, 403 files / ~87 MB) into the data
   directory. Nothing is installed system-wide.
3. **Downloads libraries**, the client jar and the natives jars, verifying every file's SHA-1.
4. **Extracts native DLLs** into `versions/<id>/natives`.
5. **Downloads the asset index and all asset objects** (~650 MB for 1.20.1), skipping anything
   already present and correct.
6. **Launches** `net.minecraft.client.main.Main` with a classpath and argument list built exactly
   like the vanilla launcher, including `arguments.game`/`arguments.jvm` rule evaluation
   (`os.name`, `os.arch`, `features`) and the pre-1.13 `minecraftArguments` format.

Downloads are streamed to a temporary file, hashed while they stream, and renamed into place only
after the SHA-1 matches, so an interrupted run never leaves a corrupt file that later looks valid.
Interrupted downloads resume on the next run (already-correct files are skipped in ~30 ms).

## Offline mode

`LFL` does **not** authenticate with Microsoft. It launches in offline mode with a local player
name and the standard deterministic offline UUID
(`UUIDv3(MD5("OfflinePlayer:" + name))`), which is what single-player and LAN need.

```powershell
LFL --user Steve
```

## Verified end to end

Both eras of the format were installed and launched from an empty directory on Windows 10,
and every file was re-checked with `verify.py` against Mojang's published SHA-1s:

| | 1.20.1 (modern) | 1.8.9 (legacy) |
|---|---|---|
| version JSON | `arguments.{game,jvm}` | `minecraftArguments` string |
| natives | 7 separate `:natives-windows` artifacts | 4 `classifiers` entries + `natives` map |
| natives extracted | 7 DLLs, flat | 14 DLLs, flat |
| classpath | 43 jars + client jar (44) | 32 jars + client jar (33) |
| assets | 3 598 objects, 621 MB | 734 objects, 110 MB |
| Java | managed `java-runtime-gamma` 17.0.15 | managed `jre-legacy` 8u51 |
| result | main menu, "Minecraft 1.20.1" | main menu, "Minecraft 1.8.9" |

The only error in the game log is the expected
`InvalidCredentialsException: Status: 401` from offline mode.

Both front-ends were exercised separately against empty directories:

* **CLI**: `LFL install 1.20.1` and `LFL install 1.8.9` in a clean room with no Java, then
  `LFL run` — both reached the main menu.
* **GUI**: `LFL-gui.exe --dir <empty> 1.20.1 --install` downloaded 393 MB of libraries + 621 MB of
  assets + an 87 MB JRE with the progress bar and log live, reported
  "已就绪：1.20.1（库与资源已校验）", and the "启动游戏" button brought up
  "Minecraft 1.20.1". `verify.py` then passed on that directory with **0 warnings**.

A full install of one modern version costs **~800 MB** (client 22 MB, libraries 56 MB,
assets 621 MB, managed JRE 87 MB); legacy 1.8.9 adds its own JRE because it needs Java 8.

## Interrupted downloads, concurrency, and version detection

**Downloads resume.** Partially transferred files are kept as `<name>.lflpart` in place, and the
next run asks the server for `Range: bytes=<have>-` and appends the rest. Killing the launcher
half-way through a 620 MB asset set costs only the missing bytes on the next run. If the server
ignores the range and answers `200`, the partial file is discarded and the transfer restarts, so
correctness never depends on the server honouring the header. Every resumed file is re-hashed and
still must match Mojang's SHA-1 before it is renamed into place.

**Several launchers can download at the same time.** There is no directory-wide lock; instead each
destination file has its own lock (in the OS temp dir, so the game folder stays clean). Two
launchers preparing the same or different versions run concurrently; when they collide on a file,
the second waits for the artifact rather than re-downloading it, and adopts the lock if the first
one crashed. Instantiating a Java runtime is exclusive, because ~400 files go into one directory.

**Versions are detected, not guessed.** The version list is built from this machine plus Mojang's
full manifest (~920 entries), with installed versions marked `[已安装]`. A bare `LFL` with no
version launches the newest installed one instead of always fetching `latest-release`:

```
$ LFL run -d E:\yy\cc --dry-run
auto-selected installed version 1.20.1
[1/4] version 1.20.1 (release)
```

A jar copied in by hand (no manifest entry) still shows up and launches, since discovery reads the
`versions/` directory too.

## Layout

```
.minecraft\                     # next to the exe; --dir to change
├── libraries\                   # shared by every instance
├── assets\                      # shared
├── runtime\                     # shared managed JREs
├── versions\                    # shared client jars + natives
│   └── <id>\lfl-profiles\       # launch profiles for that version
├── lfl-settings.ini             # 设置 page, saved atomically
├── launcher_profiles.json
└── instances\
    ├── default\                 # per-instance game dir (= --gameDir)
    │   ├── saves\
    │   ├── options.txt
    │   ├── logs\
    │   └── mods\
    └── pvp\
        └── …
```

The data directory is a `.minecraft` folder **next to the executable**, so everything the
launcher needs lives in one place and can be copied or moved as a unit. Use `--dir` to point
elsewhere (a shared `%APPDATA%\.minecraft`, a second profile, an external drive).

### Several instances at once

Libraries, assets and Java runtimes are downloaded once and shared; each **instance** gets its own
game directory, so two games can run side by side with separate worlds, settings and logs:

```powershell
LFL run 1.20.1 -i alpha -u Alice &
LFL run 1.20.1 -i beta  -u Bob
```

```
LFL-gui.exe -i pvp            # the window's 实例名 field does the same
```

Only one process downloads at a time: a launcher preparing files takes an exclusive lock on
`download.lock`, so a second one prints "another launcher is preparing files, waiting…" and
proceeds once the first is done. The lock is a kernel file handle, so it is released even if a
launcher crashes — it can never go stale. Instance names are validated to stay inside
`instances/` (no separators, no `..`, no reserved device names).

## Options

| Option | Meaning |
|---|---|
| `-d, --dir <PATH>` | data directory (default `.minecraft` next to the exe) |
| `-i, --instance <NAME>` | per-instance game dir; several instances can run at once |
| `-u, --user <NAME>` | offline player name, 1–16 chars |
| `-m, --memory <SIZE>` | max heap, e.g. `2G`, `4096M` |
| `--width/--height <N>` | window size |
| `--java <PATH>` | use a specific java/javaw (file or JRE directory) |
| `--demo` | launch the demo mode |
| `--wait` | keep the console attached and forward the game log |
| `-n, --dry-run` | print the command instead of running it |
| `-q, --quiet` | errors only |
| `--limit <N>` | rows for `LFL list` (default 30) |
| `--loader <NAME>` | loader family for `loader install` |
| `--loader-version <V>` | pin a loader build |
| `--mods-dir` | also print the mods directory |
| `--isolate` / `--no-isolate` | per-version game dir (default on) or one shared one |
| `--install <N>` | with `search`, download the Nth result |

## Build

```
cargo build --release      # builds both binaries
cargo test                 # 238 offline unit tests, no network needed
```

Requires stable Rust with the MSVC toolchain. Windows 8.1+ (WinHTTP). No other build inputs.

Layout:

```
src/lib.rs        shared engine entry point (module list, version constants)
src/launcher.rs   prepare/build_launch/spawn — the code both front-ends share
src/main.rs       LFL      CLI: argument parsing + the subcommands
src/gui/main.rs   LFL-gui  window, controls, worker thread, log/progress plumbing
src/gui/win32.rs  Win32 + GDI bindings (the only unsafe on the UI side)
src/settings.rs   persisted settings (lfl-settings.ini), read/written atomically
src/profiles.rs   per-version launch profiles (lfl-profiles/<name>.ini)
src/javafind.rs   deep Java discovery (registry, vendor roots, PATH, managed)
src/loaders.rs    loader profiles (Fabric/Quilt) and mod jar management
src/modrinth.rs   Modrinth search + verified mod downloads
src/deps.rs       mod dependency resolution (required/optional/incompatible)
src/versionmgr.rs local version management: list, delete, clone
src/lock.rs       cross-process file locks (Windows share-mode handles)
src/{json,hash,net,version,libraries,assets,java,zip,inflate,util}.rs
```

## Verifying an install

`verify.py` re-checks a completed data directory against Mojang's own metadata, independently of
the launcher's code:

```
python verify.py .minecraft
```

It verifies every library/client-jar/asset SHA-1, classpath completeness, extracted natives,
the Java runtime, leftover partial files, and that the data directory stays inside the 3 GB budget.

## Notes and limits

- Windows only (WinHTTP, `;` classpath separator, `javaw.exe`).
- No Microsoft authentication, no Realms, no skins/capes.
- Fabric and Quilt are installed from their published version profiles; Forge and NeoForge only ship
  a Java installer, which would mean executing downloaded code — out of scope.
- Old versions that need `assets/virtual` (pre-1.7) are not supported.

## Announcements and update checks

The **通知** page reads one JSON document from a URL *you* provide and shows two
things: announcements, and whether a newer launcher exists.

**There is no default endpoint and the feature is off until you set one.** A
hard-coded URL would send a request to a third party on every start, which is
exactly the kind of thing a launcher with no telemetry should not do. The address
is set on the 通知 page itself and stored as `feed_url` in `lfl-settings.ini`.

The document:

```json
{
  "latest": "0.3.0",
  "url": "https://example.invalid/lfl/releases",
  "notes": [
    { "id": "2025-11-a", "date": "2025-11-02", "title": "1.21.9 support",
      "body": "Adds the 1.21.9 version JSON shape.", "level": "info" }
  ]
}
```

Every field is optional; `notes` may be omitted; unknown fields are ignored. A
publisher can therefore clear every announcement by serving `{}` — that is a
valid, empty feed, not an error.

| Field | Meaning |
|---|---|
| `latest` | newest launcher version. Compared **numerically**, so `0.10.0` is newer than `0.9.0` |
| `url` | where a human should go. **Displayed only** — the launcher never opens it |
| `notes[].id` | stable id used to remember what you have read; falls back to `date|title` |
| `notes[].level` | `info` (default), `warn`, or `update` |
| `notes[].date` | free-form date string, shown as-is |
| `notes[].title`, `.body` | shown in the list and the detail pane |

Behaviour:

- **Checked on start** if enabled, and on demand via 检查更新. One request, no
  retry — a broken endpoint must not make the launcher feel slow.
- **Read state persists** in `<data dir>/lfl-seen.ini`: unread notes get a `•`,
  and 全部已读 clears them. The file is capped at 200 ids.
- **A new launcher version is reported once.** The version already announced is
  remembered, so you are not told about `0.3.0` on every start.
- The launcher **never downloads or replaces its own executable.** You get a
  version number and a link; updating stays a manual action.
- Everything from the feed is treated as hostile: lengths are capped, control
  characters stripped, only `http(s)` URLs accepted, and the whole document is
  capped at 256 KB.

`LFL-gui.exe --help` has no flag for this; the URL and the auto-check toggle live
on the 通知 page and are stored in `lfl-settings.ini` as `feed_url` and
`check_feed`.

## Security

The launcher downloads and runs code on your behalf, so it treats every network
document as hostile input. Two audits have been done, both with executed
proof-of-concept rather than reading alone:

* [`docs/SECURITY.md`](docs/SECURITY.md) — round 1: path traversal, panics,
  TLS downgrade.
* [`docs/SECURITY-ROUND2.md`](docs/SECURITY-ROUND2.md) — round 2: destructive
  maintenance actions, debug interfaces, feed injection. CVSS-scored.

The short version:

- **Nothing remote decides a file path.** A version JSON, asset index, runtime
  manifest or Modrinth response supplies *names*; `util::safe_join` and
  `util::safe_component` decide whether a name is usable as a path. Absolute
  paths, `..`, drive letters and reserved device names are rejected.
- **Every download is sha1-verified** against metadata before it is renamed into
  place, and a mismatch deletes the partial file.
- **Archives cannot escape.** ZIP entries with `..`, absolute paths or drive
  letters are skipped; `META-INF` is stripped; each entry is size-capped.
- **Parsers are bounded.** JSON nesting is capped at 200 levels, DEFLATE output at
  the declared size plus 1 MiB, `inheritsFrom` chains at 8 links.
- **TLS downgrade is refused.** Redirects may not go from `https` to `http`.
- **No `unsafe` outside Win32 FFI**, and no `unwrap()` in production code.
- **Destructive actions state exactly what they own.** 删除断点文件 only matches
  `*.lflpart` and `*.part<digits>`; a split save or a mod named `*.particle` is
  never touched.
- **No debug output in a release build** unless `LFL_DEBUG=1` is set.

There is no authentication, so there are no credentials to leak:
`auth_access_token` is the literal `"0"` and the launcher never contacts a login
server. Full details, including the complete list of domains contacted and when:
[`docs/PRIVACY.md`](docs/PRIVACY.md).

## Contributing

**Pull requests are not being merged at this time.** The project is early and the
maintainer is the only committer; keeping the architecture coherent while it is still moving
matters more than accepting patches. Bug reports, feature requests and questions are welcome
today and carry no paperwork — please send those.

When contributions do open, they will require a **copyright assignment**
([`docs/CAA.md`](docs/CAA.md)), not a DCO. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the
reasoning, the project rules a patch must respect, and how to report a bug well.

## Licence

**Mozilla Public License 2.0.** The full text is in [`LICENSE`](LICENSE);
`SPDX-License-Identifier: MPL-2.0` is on every source file.

MPL-2.0 is *file-level* copyleft:

- Use it, modify it, redistribute it, sell it — all permitted.
- If you distribute a modified version, the **files you modified must stay under MPL-2.0** and you
  must make their source available (MPL-2.0 §3.1–3.2). Files you add that are not derived from this
  project may be under any licence, provided the combination does not restrict the MPL-covered parts.
- Linking against the library from a separate program does not force that program under MPL-2.0.
- No warranty.

If you ship a modified `LFL.exe` or `LFL-gui.exe`, §3.2 requires you to make the corresponding
source available and to tell your recipients how to get it.

The Minecraft client, libraries, assets and Java runtime that the launcher *downloads* are not part
of this project and remain under their own licences (Mojang's EULA for the game). The launcher
downloads them on the user's behalf; it neither bundles nor redistributes them.

Copyright (c) 2025 LightFoxLauncher contributors.
