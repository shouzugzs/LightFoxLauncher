# Changelog

## 0.3.0

The 通知 page, the two security audit rounds, and the open-source paperwork.
The launcher is now publishable: MPL-2.0, no default telemetry, no default
announcement endpoint, destructive actions scoped to files it owns.

### Added

* **通知 page** (seventh tab) — announcements and launcher-update check, from a
  feed URL the user configures. **Off by default, no built-in endpoint.**
  `src/notify.rs` plus `docs/CONTRACT.md` §notify rules the behaviour.
* `lfl-seen.ini` in the data directory: which notes are read, last check time,
  which launcher version was already reported. Capped at 200 ids.
* `Settings.feed_url` / `Settings.check_feed`, persisted in `lfl-settings.ini`.
* `docs/SECURITY.md` (round 1) and `docs/SECURITY-ROUND2.md` (round 2,
  CVSS-scored), both with executed proof-of-concept, not just code reading.
* Open-source set: `LICENSE` (MPL-2.0 verbatim), `CONTRIBUTING.md` (PRs closed,
  CAA required when they open), `docs/CAA.md`, `CONTRIBUTORS.md`.
* `SPDX-License-Identifier: MPL-2.0` header + Exhibit A notice on all 23 source
  files (MPL-2.0 §3.4).
* 9 hard rules in `docs/CONTRACT.md` → 11: deletions must match only filename
  shapes they own (rule 10), diagnostics off in release builds unless
  `LFL_DEBUG=1` (rule 11).

### Fixed — security round 1 (`docs/SECURITY.md`)

* **S1 High — path traversal from remote metadata.** Six sites joined a value
  from a version JSON / asset index / runtime manifest / Modrinth response onto a
  directory; `Path::join` replaces the base for absolute args and does not
  resolve `..`. `assets::index_path` was a write destination. Fixed with
  `util::safe_join`, `util::safe_component`, `version::version_dir`.
* **S2 Medium — panic on a non-ASCII asset hash.** `object_url` byte-sliced
  `&h[..2]`; fatal under `panic = "abort"`. Non-hex hashes are rejected.
* **S3 Medium — https→http redirect downgrade.** WinHTTP's default policy
  permitted it; every session now sets
  `WINHTTP_OPTION_REDIRECT_POLICY = DISALLOW_HTTPS_TO_HTTP`.

### Fixed — security round 2 (`docs/SECURITY-ROUND2.md`)

* **C1 7.1 High — 删除断点文件 deleted user files.** `is_partial_name` used
  `contains(".part")`, silently destroying split saves, backups and mods named
  `*.particle`. Now only `*.lflpart` and `*.part<digits>` match.
* **C2 5.5 Medium — debug log written unconditionally.** Every `WM_DRAWITEM` and
  `WM_COMMAND` appended to `%TEMP%\lfl-gui-debug.log` in release builds. Now
  opt-in via `LFL_DEBUG=1`, capped at 512 KB.
* **C3 4.6 Medium — feed values injected lines into `lfl-seen.ini`.** `clean`
  kept `
` for every field, so a feed could forge `reported_update` (suppress
  the update prompt) or `seen=` entries. Fixed with `clean_line`,
  `is_version_shaped`, and a second sanitise in `Seen::save`.

### Accepted and documented

* S4/C4 — metadata documents unsigned (no signing key exists); the tool-page
  downloader verifies nothing for arbitrary URLs (inherent to a generic
  downloader).
* S5→C2 re-scored Low → 5.5 Medium and **fixed**.
* S6/C5 — predictable lock names in per-user `%TEMP%`; payload sha1-checked.
* S7/C6 — TLS trust via the system store; `--dir` accepts any path by design.

### Privacy

* `docs/PRIVACY.md` — the complete list of domains the launcher contacts and
  when, a statement of what it writes locally, and the maintainer identity
  policy (collective name only; CAA submission address published in release
  notes, never committed).
* `Cargo.toml` gains no `authors` field; the email address that appeared in
  `CONTRIBUTING.md` and `docs/CAA.md` has been removed from the repository.
* `.gitignore` added for publishing (`target/`, `dist/`, `.minecraft/`).

### Size and tests

| | 0.2.1 | 0.3.0 |
|---|---|---|
| `LFL.exe` | 520,704 B | **524,800 B** |
| `LFL-gui.exe` | 634,880 B | **656,384 B** |
| tests | 216 | **244** |

Both binaries remain under the 1 MB budget.

## 0.2.1

### Fixed — layout alignment

The pages were visibly misaligned: rows of controls did not share a baseline, cards
did not contain the controls they framed, and section headings were painted across the
first row of their own card.

* **One control height.** `BTN_H` (28) and `ROW_H` (26) are gone; there is a single
  `ROW_H = 28`. A row of `[combo][edit][button]` used to come out with three different
  bottom edges, which is what read as "错位".
* **Combo boxes are pinned.** Windows sizes a combo's selection field from its font, not
  from the height passed to `MoveWindow`, so a 28 px request rendered 24–26 px tall.
  `normalize_combo()` sets it explicitly with `CB_SETITEMHEIGHT` after every layout.
* **`combo_h()` is pure.** It used to read the height back with `GetWindowRect`, so the
  first layout pass — before the list had grown — disagreed with every later one and
  combos changed height on the first resize.
* **Cards contain their rows.** Cards on the 下载 / 模组 / 版本 / 工具 pages started at
  `PAD` while their content started at `row_x()` (94), leaving a 76 px dead gutter inside
  every card. Content now starts at `card_x()` = `PAD + CARD_PAD`, and `card_end()` /
  `content_y()` derive card bounds from the same row geometry the controls use.
* **Headings have their own band.** `HEAD_H = 28` is reserved at the top of a card;
  before, a heading was drawn in the same band as the first control and struck through it.
* **Labels are no longer covered.** The 启动 page's `玩家名` / `内存` labels sat at exactly
  the same x as their edit boxes, so they were painted underneath them. They now occupy
  the gap before their control.
* **`paint()` shares `layout()`'s helpers** instead of carrying its own hard-coded offsets,
  so the two cannot drift apart again.
* **Bottom margins.** Lists and cards now stop at `ch - CARD_PAD - PAD` rather than
  `ch - PAD`, and the 版本 page's info line no longer overflowed the client area by 1 px.
* **Minimum window size** raised from 660×540 to 700×580, where the 工具 page's log had
  collapsed to 45 px.

## 0.2.0

The product gets its name, and the 下载 page finally works. The launcher is **LightFoxLauncher**;
the old `mcl` naming is gone from the binaries, the crate and every file the launcher writes.

### Renamed

| Thing | Now |
|---|---|
| Product / window title | `LightFoxLauncher` |
| CLI binary | `LFL.exe` |
| GUI binary | `LFL-gui.exe` |
| Cargo package | `lightfox` |
| Library crate | `lightfox` (`mcl::foo` → `lightfox::foo`) |
| Short tag (JVM args, dialogs) | `LFL` |
| Settings file | `<data>/.minecraft/lfl-settings.ini` |
| Launch profiles | `versions/<id>/lfl-profiles/<name>.ini` |
| Runtime marker | `.lfl-ok` (was `.mcl-ok`) |
| Natives marker | `.lfl-natives-ok` (was `.mcl-natives-ok`) |
| Partial downloads | `.lflpart` (was `.mclpart`) |
| Version | `0.2.0` |

The data directory is still `.minecraft` next to the executable, and the layout inside it is
unchanged — only the names of the files the launcher itself creates changed.

### Changed

* **启动** no longer has a `快速切换` combo. It listed the same installed versions as the `版本`
  dropdown and did the same thing, so there were two controls for one decision that could disagree
  with each other. Version switching now happens in exactly one place: the `版本` dropdown on
  启动.
* **版本** remains the place where versions are managed on disk — delete, clone, 管理模组 (mods for
  that version) and 补全文件 (verify/re-fetch the files) — so having a version *picker* on 启动 and
  a version *manager* on 版本 is the whole split.
* The 游戏核心 block splits Mojang's manifest into **正式版** (`type: release`) and **预览版**
  (`snapshot`, `old_beta`, `old_alpha`). A flat list interleaved `24w14a` between `1.21.8` and
  `1.21.7`, which made picking a stable version a guessing game. The manifest is fetched once, so
  switching channels is a local re-filter.

### Fixed

* The **游戏核心** version box was an empty box: it never offered a single version to pick, so the
  core could only be installed by typing an id exactly right.
* Core-download result messages landed on the **启动** page's status line, where they could not be
  seen while the 下载 page was on screen. Each of the three blocks — 游戏核心, 模组加载器, 模组 — now
  reports its own status under itself on the 下载 page.
* A version installed from 下载 did not appear in the 启动 version dropdown until the launcher was
  restarted; the list is refreshed when the install finishes.
* The **模组加载器** build list and the 安装加载器 result were written into a control that is only
  visible on a different page, so the 下载 page appeared to do nothing at all.
* The loader build query and the Modrinth search passed a loader profile id (for example
  `fabric-loader-0.16.9-1.20.1`) where a Minecraft version was expected, which matches nothing.
  The vanilla version behind a profile is resolved through its `inheritsFrom` chain.
* The Modrinth search never actually applied its loader filter, so a Forge setup could be offered
  Fabric builds. The filter follows the selected version's loader now.
* Network calls (search, loader-build lookup, loader install) ran on the UI thread and froze the
  window until they returned. They run on a worker thread now.
* **查找 Java** only found runtimes on `PATH`. It now also reads the `HKLM` *and* `HKCU` JavaSoft
  registry keys including the 32-bit `WOW6432Node` view (a JDK installed without elevation registers
  per-user), asks `where.exe` so a PATH that changed after launch still resolves, and scans the
  vendor install roots three levels deep — Adoptium, Corretto, Zulu, Microsoft, BellSoft, Semeru,
  JetBrains Toolbox/Runtime, Android Studio's JBR, `.jdks`, Gradle, scoop and chocolatey.
* The Java scan reported any folder that happened to contain a `bin/java.exe` as an installed JDK.
  Directories are name-filtered at *every* level now, so a folder like `apps\games` that bundles a
  private JRE is no longer offered as a system runtime. The scan stays bounded to the directories
  Java installers actually use — there is no full-disk walk anywhere in it.

### Added — the window is dressed up, for 2.5 KB

The visual layer is drawn entirely with GDI calls the binary already links — no crate, no image
resource, no extra control:

* A rounded accent **brand mark** (`LF`) beside the product name in the header.
* **Pill-shaped tabs** with a bold label and an accent bar under the active one, plus a full-width
  rule so the pages sit on a defined edge.
* A **card** behind the form on 启动 and behind each of the three blocks on 下载, so a status line is
  visibly attached to the action that produced it.
* **Owner-drawn buttons**: 启动游戏 gets a filled accent face, every other button a raised neutral
  one, both rounded, with the label nudged a pixel while pressed. Replaces the flat grey system
  buttons that ignored the dark palette.

### Fixed — the layout was misaligned and the cards were arbitrary

* **Misaligned排版**: `layout()` and `paint()` each carried their own hard-coded offsets, so a card
  could float away from the controls it was meant to hold and a row's label could sit a few pixels
  off its field. Both functions now derive every position from one set of metrics (`PAD`, `LABEL_W`,
  `ROW_H`, `BTN_H`, `GAP`, `ROW_GAP`, `CARD_PAD`, `BODY_TOP`), so a card wraps exactly its own
  controls and every row shares a baseline.
* **Arbitrary boxes**: cards were sized by hand per page, which is why some looked right and others
  did not. Each page now declares its cards from the same arithmetic the controls use, so the
  download page's three blocks (游戏核心 / 模组加载器 / 模组) each get a card that ends where its
  row ends, and the version page's two list panes split the width evenly with a shared gap.
* Row heights were inconsistent — fields 26, buttons 36, status 22, all chosen ad hoc. Fields are now
  `ROW_H`, buttons `BTN_H`, and the vertical pitch is `ROW_GAP` everywhere.
* Labels were left-aligned into the control column and collided with the fields beside them. Labels
  in the label column are now right-aligned to their row, so the eye runs down a clean edge.

### Sizes

| Binary | Size |
|---|---|
| `LFL.exe` | 520,704 B (0.50 MiB) |
| `LFL-gui.exe` | 634,880 B (0.60 MiB) |

Still zero third-party crates; 216 tests, all offline.

## 0.1.2

Restructures the window around one idea: **启动 is for launching, 下载 is for downloading**.
Adds per-version launch profiles, which is what "多个同版本的管理" needed.

### Changed — the window is reorganised

| Tab | Was | Now |
|---|---|---|
| 启动 | version list mixing installed and downloadable | **only installed versions**, plus profiles and quick switch |
| 下载 | — | **game core + mod loader + mods** in one page |
| 模组 | had loader install buttons | mod list + dependency check only |
| 版本 | — | unchanged (list, delete, clone, manage mods, repair) |
| 设置 | — | unchanged (threads, Java, 查找 Java) |
| 工具 | — | unchanged (verify, clean, downloader) |

The launch page's version box is now a **read-only list of installed versions**, so it can never
select something whose files do not exist. Downloading a new version moved to 下载, where the
correct follow-up actions (loader, mods) are already on the same page.

### Added — launch profiles per version

One installed version can now have several launch setups. A profile stores only the launch-time
choices — heap, player name, window size, isolation flag — in
`versions/<id>/mcl-profiles/<name>.ini`, so the jars, mods and saves stay shared and the cost is a
few bytes.

* The profile picker on 启动 lists a version's profiles (`默认 · 2G · Steve`).
* **保存配置** writes the current form into the selected profile; **新建配置** derives a new one
  (`配置`, `配置2`, …) from the current values.
* Every version implicitly has a `默认` profile, so the picker is never empty and a fresh install
  is immediately launchable.
* The version dropdown shows how many profiles each version has.
* Profile names are validated as file names (no separators, no `..`, no reserved device names).

### Added — download page

* **游戏核心**: type a version id and download it (vanilla core, libraries, assets, runtime).
* **模组加载器**: Fabric/Quilt build picker with 安装加载器 (writes the profile into `versions/`).
* **模组**: Modrinth search with dependency-aware installation.

### Fixed

* **Window opened at 160×28**, parked at the minimised coordinate. Creating it with
  `CW_USEDEFAULT` and immediately repositioning it left the shell in a default-placement state on
  this machine; it is now created directly at the centred geometry.
* Version-page buttons disappeared after the tab renumbering (they were gated on the old tab index).
* Settings-page help text bled onto the download page; paint arms now match the new page order.
* The isolation checkbox label was clipped by the window edge.

### Sizes

| Binary | Size |
|---|---|
| `mcl.exe` | ~521 KB (0.50 MiB) |
| `mcl-gui.exe` | ~600 KB (0.57 MiB) |

Still zero third-party crates; 209 tests pass with no network.

## 0.1.1

Mod dependency handling, usability work, and the fix for mods landing in a non-existent version
directory. See the git history for detail.

## 0.1.0

Initial version.
