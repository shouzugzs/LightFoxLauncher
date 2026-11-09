# Changelog

## 0.5.1

控件外观收尾 + 两处对齐修复。

### Changed

* **控件外观统一**：此前每个输入框、列表都带着 `WS_EX_CLIENTEDGE`，Windows 会给它们
  画一圈不受主题控制的 2px 灰色凹陷边——放在暖色卡片上就是一块没做完的 Win95 控件。
  现在去掉系统边框，改由绘制层按控件自己的窗口矩形画 1px 主题描边，所以边框永远不会
  和布局脱节。
* **下拉框**：`CBS_DROPDOWNLIST` 自带的灰色箭头按钮、分隔线以及客户区内侧那圈灰边
  全部被接管，换成主题色的细描边和 V 形箭头。通过子类化 `WM_PAINT` 实现——子窗口
  在父窗口之后绘制，所以在 `paint()` 里盖是盖不住的。
* **进度条**：`msctls_progress32` 换成自绘的圆角轨道 + 填充，配色跟随主题。空进度
  不再是一个没有意义的米色空槽，而是一条安静的细线。
* **导航栏悬停**：鼠标移上去会有一层浅色底和加重的文字；离开即恢复。这需要
  `TrackMouseEvent` + `WM_MOUSELEAVE`，否则第一次移动之后状态会一直挂着。
* **输入框自动选中**：下拉框用 `SetWindowTextW` 填值后整段文字处于选中态，内存那里
  的「2G」会顶着色块。布局末尾用 `CB_SETEDITSEL` 把选区收成光标。

### Fixed

* **单行输入框文字偏高**：同一行、等高的控件里，组合框文字中心在 151，编辑框文字
  中心在 148，差了 3.5px——这就是「输入框普遍偏移」。`settle_edits()` 在 `layout()`
  末尾把单行编辑框下移 `EDIT_DY`（3px）让文字落到行中心，`outline_inputs()` 减去
  同样的值，使绘制的 1px 边框留在原行、与旁边的组合框齐平。两处共用同一个运行时
  判定 `is_single_line_edit`（类名 `Edit`、无 `ES_MULTILINE`、可见），不再依赖手写
  句柄列表，因此组合框不会再被误判下移；可见性判定也避免了每次切换标签时偏移累积。
* **「Java 路径」标签被裁**：标签框宽 `LABEL_W - GAP` = 64px，而文字在该字体下需要
  约 58px，几乎没有余量，开头的 `J` 竖笔被切掉。`LABEL_W` 改为 86px（框 74px）。

## 0.5.0

界面重做 + 主题系统 + 跨平台。

### Added

* **五套主题**：暖狐（默认）/ 雪白 / 夜色 / 森绿 / 星紫，在「设置 → 主题」里切换，
  立即生效并写回 `lfl-settings.ini` 的 `theme=` 键，下次启动自动恢复。调色板是纯
  sRGB 数据表（`THEMES`），只有 `apply_theme` 一个函数把它翻译成 COLORREF /
  `HBRUSH` / `BTN_THEME`，所以将来换渲染层只改那一处，主题数据不用动。标题栏
  图标、进度条配色、所有控件的背景与文字颜色都跟着主题走。
* **新的窗口结构**：左侧导航栏 + 顶部品牌栏 + 页头（标题 + 一句话说明）+ 底部
  状态栏，取代原来一行挤八个标签的横排标签。窗口放大到 960×640，内容区宽度
  基本不变，但每个标签终于有地方写清楚自己是干什么的。导航标签、页头标题和
  副标题来自同一张 `PAGES` 表，不会出现两处写法不一致。
* **「联机」页补齐了绘制**：这一页此前完全没有绘制代码，按钮直接浮在背景上，
  没有卡片也没有标题，看起来像没做完。
* **跨平台构建**：
  - `net` 在非 Windows 上用 `std::net::TcpStream` 现写了一个 HTTP/1.1 客户端
    （支持重定向、`chunked`、`Range` 断点续传），零依赖；`https://` 会明确报错
    而不是悄悄降级成明文；
  - `firewall` / `hostnet` 在非 Windows 上变成有文档的降级实现，不再引用 `netsh`
    与 `iphlpapi`；
  - `LFL-gui` 是原生 Win32，在没有窗口工具链的平台上编译成一个指向 CLI 的说明
    程序，而不是链接 `user32`/`gdi32` 失败；
  - `java` 的运行时清单键按目标平台选择（`windows-x64` / `linux` / `mac-os-arm64` …）；
  - `scripts/cross-check.ps1` 把上面这些固定成可重复的检查，并断言每个平台接缝
    两边都有实现。

### Fixed

* **图标与 GDI 句柄泄漏**：`make_fox_icon` 造出的位图和 `HICON` 之前从不释放，
  每次切换主题都再建两个；现在旧图标在换新之后立刻 `DestroyIcon`，窗口销毁时
  连同两个 `HBRUSH` 一起交还。

## 0.4.3

### Fixed

* 通知页「点击旧公告没反应」的真正原因：所有列表框都少了 `LBS_NOTIFY` 样式。
  列表照样记录选中项（`LB_GETCURSEL` 答得对），但从不把 `LBN_SELCHANGE` 发给
  父窗口，于是绑定在 `WM_COMMAND` 上的 `notify_selection_changed` 永远不会跑 ——
  正文只在刷新列表时由 `notify_rebuild_list` 填一次，之后点哪一条都不变。
  同一个缺陷也影响 版本 / 模组 / 搜索 页的列表。
* 通知页「回看不了」的另外三处原因：
  - 缓存写进 `util::default_dir()`，读取却用「启动」页的数据目录，改过目录的
    用户永远读不到自己的缓存 —— 现在写入与读取用同一目录（`notify::fetch_into`）；
  - 正文换行是 JSON 的 `\n`，而 Win32 多行 `EDIT` 只在 `\r\n` 处换行，整段正文
    被画成一行 —— 显示前用 `notify::edit_text` 转换；
  - `site/feed.json` 的换行曾写成 `\\n`（双重转义），解析出来是字面的反斜杠加
    n，现在修正，并由 `the_shipped_feed_json_parses` 守住这个格式。

## 0.4.2

### Fixed

* 联机页「检测网络」在本机上永远为空的 Bug。三个叠加的 FFI 错误：
  - `GetAdaptersAddresses` 的 `Family` 参数声明成了 `u16`（真实原型是 `ULONG`/u32），
    x64 调用时寄存器高半部分是垃圾值，API 恒返回 87（ERROR_INVALID_PARAMETER）；
  - `DAD_STATE_PREFERRED` 常量写成 1（那是 Tentative），真实地址全部报 4，过滤器
    拒绝了所有地址；同时接受 Deprecated=3 —— RA 租期短的地址几分钟后会进入该状态，
    入站仍然可达；
  - `SOCKET_ADDRESS` 没有 family 成员（多写的字段把后面字段全部偏移搞错）；
    `FirstGatewayAddress` 在当前 Windows 位于 +232 且指向内联 sockaddr，实测确认。

## 0.4.1

联机（play together）— IPv6/IPv4 direct hosting plus a room-code shortcut so
nobody has to read out a 39-character IPv6 address.

### Added

* **联机 page** (eighth tab). 检测网络 reports what this machine can host:
  globally-routable IPv6 addresses, the IPv4 gateway, and whether the firewall
  rule exists. Read-only — it changes nothing.
* **开服（映射端口）** — adds the firewall rule, then tries a port mapping:
  UPnP IGD first (SSDP discovery → device description → SOAP AddPortMapping),
  falling back to NAT-PMP (RFC 6886) only when UPnP found no gateway at all. A
  4-hour lease, with a retry as a permanent lease when the router answers
  error 725. 关闭映射 tears the mapping down again.
* **房间号** — a 6-character code (32-symbol alphabet, no I/0/1 lookalikes)
  maps to the host's address for 10 minutes, so friends type `ABC234` instead
  of an IPv6 address. 建房号 registers, 用房间号连接 looks up, 复制房号 copies.
  The registry is a Cloudflare Pages Function over KV; it stores only
  `code → addr:port` and never proxies game traffic.
* `servermod` — server.jar download (sha1-verified), `eula.txt`,
  `server.properties` (`online-mode=false` and `enforce-secure-profile=false`
  with reasons), and `whitelist.json` built from offline-mode UUIDs.
* `hash::offline_uuid` + MD5: offline-mode UUIDs are
  `MD5("OfflinePlayer:" + name)` with UUID-v3 version/variant bits. A premium
  UUID in the whitelist does **not** match on an offline server, so the
  launcher generates the right ones.
* `Settings.room_server` — optional override for the room registry; the
  built-in default is used when unset.

### Changed

* **IPv6 直连优先**. When the machine has a global IPv6 address, the 开服
  workflow now recommends publishing that address directly instead of relying
  on IPv4 port mapping. This matters when the network sits behind CGNAT or has
  symmetric NAT (so UPnP/NAT-PMP cannot work): IPv6 bypasses the entire problem.
* The report notes now show which address family works and why. If both IPv6 and
  IPv4 succeed, IPv6 is still recommended because it has no NAT and no randomised
  external port. If only IPv6 succeeds (common on CGNAT networks), the notes make
  that explicit rather than just saying "IPv4 failed".

### Security notes

* The room registry validates the code against a strict 32-symbol alphabet and
  the address against an IPv6/IPv4 character set before storing either, so a
  hostile client cannot inject into KV keys or the stored JSON.
* Rate limits: 10 registrations/minute and 60 lookups/minute per IP. Room
  entries expire after 10 minutes.
* UPnP/NAT-PMP responses are only accepted from the default gateway — a
  spoofed reply would otherwise be a mapping-injection primitive. For NAT-PMP
  this was already enforced (reply source must equal the gateway). For UPnP it
  is now enforced too: each SSDP answer's source IP is recorded and a `LOCATION`
  whose host is neither the default gateway nor that responder is dropped before
  the launcher fetches or POSTs to it — closing the "rogue host answers SSDP
  first" redirection hole that the prior build was vulnerable to.
* The launcher is offline-mode only, so hosting exposes the server to anyone
  with the address. The 联机 page says so and the generated
  `server.properties` enables the whitelist.

### Fixed

* **Firewall rule pile-up.** `firewall::add_rule` was non-idempotent: every
  开服 click added fresh TCP + UDP rules under the same name, so repeated
  hosting could accumulate dozens of identical entries. It now removes any
  prior rule it owns before adding the requested port, so there is exactly one
  rule set per port and a port change does not leave stale holes behind.
* **UPnP SSDP source not checked** (the security note above): SSDP answers are
  now attributed to their source IP and a `LOCATION` is only followed when its
  host is the gateway or that responder.

## 0.3.2

Security round 3, aimed at one feature end to end: the 通知 page and the
startup update check. Seven findings, all fixed or documented, each with a
regression test. Full write-up in [docs/SECURITY-ROUND3.md](docs/SECURITY-ROUND3.md).

Plus a first-run consent question for announcements, and a data-directory bug
that question exposed.

### Added

* **First start asks once whether to enable announcement reception.** The
  default button is 否, and the answer is remembered in a new
  `lfl-settings.ini` key, `feed_prompted`, so the question never reappears.
  `feed_prompted` is deliberately separate from `check_feed`: the latter is the
  live switch the user can flip freely, the former records only that the
  question is spent — without it the launcher cannot tell "the user said no"
  from "the user has never been asked".
* There is still **no built-in endpoint**. Answering yes contacts nothing;
  the URL remains something the user must fill in on the 通知 page. That is the
  whole point of asking rather than defaulting the feature on.
* The 通知 page's idle status line now reflects the switch: with the feature off
  it says so, instead of always prompting for a URL.

### Fixed

* **The GUI read settings from one directory and wrote them to another.** It
  loaded `lfl-settings.ini` from the default data directory and then pointed the
  game-directory box at `--dir`, while every save went to `--dir`. So with
  `--dir` set, preferences were never read back — the new first-run question
  was asked again on every single start, and any other setting saved from the
  设置 page was silently ignored on the next launch. The CLI is now parsed
  *before* the settings are loaded, and both sides use the same directory.
* An existing `feed_url` in a pre-0.3.2 settings file counts as "already opted
  in", so upgrading users are not asked a question about a feature they are
  demonstrably using.
* Toggling 启动时自动检查 marks the consent question answered, so a settings file
  written by an older build cannot cause a re-ask after the switch was used.

### Fixed — security round 3

* **N1 Medium — oversized feed buffered before the size check.** `fetch` read the
  whole response with no bound and only then compared it against the 256 KiB cap,
  so a hostile endpoint could make the launcher allocate anything. The limit is
  now a parameter of the read loop (`net::get_max`) and is enforced per chunk.
* **N2 Medium — a trickling server wedges 检查更新 for the whole session.** The
  `feed_busy` flag is cleared only when the worker reports back; WinHTTP's receive
  timeout restarts on every read, so a slowloris kept the button dead forever. A
  60 s total deadline now bounds the read, and a 90 s UI-side grace period
  (`feed_unwedge`) rescues a worker that vanished without reporting.
* **N3 Medium — bidi and zero-width characters spoof announcement text.**
  `char::is_control` covers category Cc only, so `U+202E` (which reverses
  rendering) and `U+200B` (invisible, so two notes can look identical while
  carrying different ids) reached the list box and the state file. Both are now
  dropped, while CJK and emoji still pass.
* **N4 Medium — CR/LF in a document-supplied URL reached the request line.** The
  path becomes the request target of `GET <path> HTTP/1.1`, so a newline in it
  split the request and let the remainder parse as headers. Download URLs come
  from remote version JSON and the Modrinth API, and JSON escapes carry `\r\n`
  verbatim. Refused in `Url::parse` *and* again in `Session::open_with`, since
  `Url`'s fields are public.
* **N5 Low — plaintext `http://` feed accepted silently.** A network attacker
  could rewrite announcements, including the "new version" hint. Now warns once
  (CLI unaffected); still not blocked, following the same advisory policy as the
  unverified-download warning.
* **N6 Low — `reported_update` was written but never read**, so the same update
  was announced on every start despite the comment saying otherwise. It is now
  compared against, and a repeat is shown as 可更新到 rather than 有新版本.

### Also changed

* `Settings::save` flattens every value onto one line, so a hand-edited
  `lfl-settings.ini` cannot forge a second `key=value` line — the same defence
  `lfl-seen.ini` already had.
* The `User-Agent` is now built from the crate version instead of a hard-coded
  `LFL/0.1`, which had been stale since 0.1.
* `net::MAX_DOC` (64 MiB) caps every metadata document, and `get`/`get_opt`
  share a 60 s read deadline.

## 0.3.1

Bugfix/UX follow-up to the security-audit hardening.

### Changed

* **Downloads without a checksum no longer abort.** When a file's `sha1` is empty
  the launcher only verifies byte size. Previously this was a hard failure; it now
  shows an advisory message box (once per run) and continues, so offline/optional
  content still downloads. CLI is unaffected — the warning is a no-op without a GUI.

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

### Fixed — Java 扫描漏掉数据盘上的 JDK

The 设置 page's scan only looked for `D:\jdk` / `D:\java` / `D:\Java` — three
literal paths. A JDK unpacked as `D:\jdk-21.0.8`, or under a hand-made parent
like `D:\develop\Java\…`, was invisible. Now:

* every fixed drive's root is scanned through the existing name filter
  (`could_hold_jdk`) and depth bound, so `D:\jdk-25.0.1` is found while
  `D:\Photos` costs one `stat`;
* the name filter knows the parent folders people actually create —
  `develop`, `dev`, `environment`, `soft`, `software`, `program`, `tool`;
* the system drive is skipped (already covered by the `ProgramFiles` roots)
  and removable drives present at scan time are included.

Verified against a real machine with three JDKs unpacked at the D: root —
all three now appear.

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
