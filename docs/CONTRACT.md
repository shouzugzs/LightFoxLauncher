# LightFoxLauncher — module contract (frozen interfaces)

Zero-dependency Rust Minecraft launcher for Windows. **No external crates. No new `[dependencies]`.**
Toolchain: stable 1.95.0, target `x86_64-pc-windows-msvc`. Edition 2021.

Product: **LightFoxLauncher** (window title, CLI `-V`, help header); CLI binary `LFL.exe`, GUI binary
`LFL-gui.exe`, crate/package `lightfox`. The short tag `LFL` is used only for JVM arguments and
message-box captions. Version 0.3.0.

Licence: **MPL-2.0**. Every source file starts with an `SPDX-License-Identifier: MPL-2.0` header and
the Exhibit A notice; that header must not be removed (MPL-2.0 §3.4). Contribution policy lives in
`CONTRIBUTING.md` — pull requests are currently closed, and accepted contributions require the
copyright assignment in `docs/CAA.md`.

## Hard rules for every module

1. `use crate::util::{Res, ...};` — all fallible fns return `Res<T> = Result<T, String>`; errors are
   human-readable and name the file/URL involved.
2. Do **not** change signatures in this document. Internal helpers are yours.
3. Do not edit files you do not own. Do not touch `Cargo.toml`, `src/main.rs`, `src/util.rs`.
4. Every module ends with `#[cfg(test)] mod tests { ... }`; `cargo test` must stay green.
5. Only `unsafe` allowed is documented Win32 FFI (`#[link(name=...)] extern "system"`).
   Never `unsafe` for memory shortcuts in pure-Rust code.
6. Progress/log output goes through `util::info()` / the module's own progress renderer; nothing
   may panic on I/O (no `unwrap()` on filesystem/network results). `unwrap()` is fine where
   provably infallible (e.g. formatting).
7. Keep release impact small: no big lookup tables beyond hashing constants, no formatting bloat.
8. Do not read whole large files into memory when streaming suffices (client.jar is 23 MB,
   JRE `lib/modules` is 57 MB — those are downloaded by `net`, never parsed).
9. Every source file keeps its `SPDX-License-Identifier: MPL-2.0` header. Removing or altering it
   is a licence violation, not a style change.
10. A function that **deletes, moves or overwrites** files must match only the exact filename shapes
    it owns, and must have a test proving a user file survives. `net::is_partial_name` used
    `contains(".part")` and 删除断点文件 silently destroyed split saves and mods named `*.particle`
    (round-2 finding C1, CVSS 7.1). Never use a substring match to decide what to delete.
11. Diagnostic output is **off in release builds** unless `LFL_DEBUG=1`. Anything that writes to
    `%TEMP%` must be opt-in and size-capped.

Binary budget: `LFL.exe` and `LFL-gui.exe` must each stay **< 1 MB** after `--release` with
`opt-level="z", lto=true, codegen-units=1, panic="abort", strip=true`, already configured in
`Cargo.toml`.

## Product-name constants (src/lib.rs)

```rust
pub const NAME:  &str = "LightFoxLauncher"; // window title, CLI -V, help header
pub const BRAND: &str = "LFL";              // JVM args, message-box captions
pub const TITLE: &str = "LightFoxLauncher"; // window title
```

`version::is_plain_version(id)` and `version::base_version(dir, id)` exist because loader profile
ids (`fabric-loader-0.16.9-1.20.1`) are *not* Minecraft versions: Modrinth and the loader feeds
reject them. `base_version` follows `inheritsFrom` and falls back to the version-shaped suffix of the
id when the profile JSON is gone.

## On-disk names (fixed by the 0.2.0 rename)

The data directory itself is still `.minecraft` next to the executable. Only the names the launcher
creates inside it carry the product prefix:

| Thing | Path / name |
|---|---|
| Data directory | `<exe dir>/.minecraft` (`--dir` overrides) |
| Settings | `<data>/lfl-settings.ini` (temporary `<data>/lfl-settings.ini.tmp` during the atomic save) |
| Launch profiles | `<data>/versions/<id>/lfl-profiles/<name>.ini` |
| Managed-JRE marker | `<runtime home>/.lfl-ok` (contents = component) |
| Natives marker | `<data>/versions/<id>/natives/.lfl-natives-ok` |
| Partial download | `<dest file name>.lflpart` next to the destination |
| Data-directory lock | `<data>/download.lock` |

The GUI page order is fixed as 启动 / 下载 / 模组 / 版本 / 设置 / 工具 (tabs 0..5); page indices in
`src/gui/main.rs` must stay in that order.

## GUI layout metrics (`src/gui/main.rs`)

`layout()` and `paint()` must derive **every** position from these constants, through the shared
helpers below, never from a literal offset. When the two functions carried their own numbers, cards
drifted away from the controls they wrapped, headings were painted across the first row of their own
card, and rows lost their common baseline.

```rust
const PAD: i32 = 18;        // outer margin, left edge of the label column
const LABEL_W: i32 = 76;    // width of the "版本" / "游戏目录" label column
const ROW_H: i32 = 28;      // the height of *every* control in a row
const GAP: i32 = 12;        // horizontal gap between two controls in a row
const ROW_GAP: i32 = 38;    // vertical distance between row baselines
const CARD_PAD: i32 = 12;   // inner padding between a card's edge and its content
const HEAD_H: i32 = 28;     // band reserved at the top of a card for its heading
const DROP_ROW: i32 = 22;   // height of one drop-down list row
const BODY_TOP: i32 = CHROME_TOP + TAB_H + 16;
```

Helpers — both `layout()` and `paint()` must go through these:

| Helper | Meaning |
|---|---|
| `row_x()` | `PAD + LABEL_W` — first control of a row, on a page with a label column |
| `card_x()` | `PAD + CARD_PAD` — first control inside a card, on a page without one |
| `row_y(top, n)` | `top + n * ROW_GAP` |
| `content_y(top, heading)` | first control row inside a card (`HEAD_H` if it has a heading, else `CARD_PAD`) |
| `card_end(top, rows, heading)` | bottom edge of a card holding `rows` rows |
| `combo_h(rows)` | `ROW_H + rows * DROP_ROW` — a **pure** function of the item count |
| `normalize_combo(h)` | pins a combo's *closed* height to `ROW_H` via `CB_SETITEMHEIGHT` |

Three rules that exist because breaking them is what produced the visible misalignment:

1. **One height per row.** `ROW_H` is the height of edits, combos *and* buttons. There is no
   separate `BTN_H`.
2. **Combos must be normalised after every layout.** Windows sizes a combo's selection field from
   its font, not from the height passed to `MoveWindow`, so without `normalize_combo()` a 28 px
   request renders 24–26 px and sits off from its neighbours.
3. **Card content is inset by `CARD_PAD`, not `LABEL_W`.** Using the label column's width as the
   card's inner inset left a 76 px dead gutter inside every card on the 下载 / 模组 / 版本 / 工具
   pages.

Lists and cards stop at `ch - CARD_PAD - PAD`. The minimum window size is 700×580.

## src/notify.rs (lead)

Announcements and launcher-update checks. Reads one JSON document from a
user-supplied URL; **there is no default endpoint**, and `fetch` is never called
unless `Settings.feed_url` is set.

```rust
pub const MAX_BODY: usize = 256 * 1024;   // document cap
pub const MAX_NOTES: usize = 50;          // notes accepted per document

pub enum Level { Info, Warn, Update }     // Level::parse, Level::label

pub struct Note { pub id: String, pub date: String, pub title: String,
                  pub body: String, pub level: Level }
impl Note { pub fn list_line(&self) -> String; }

pub struct Feed { pub latest: Option<String>, pub url: Option<String>,
                  pub notes: Vec<Note> }

pub fn is_newer(candidate: &str, current: &str) -> bool;  // numeric, not lexical
pub fn parse(text: &str) -> Res<Feed>;
pub fn fetch(url: &str) -> Res<Feed>;
pub fn now_secs() -> u64;

pub struct Seen { pub ids: Vec<String>, pub last_check: u64,
                  pub reported_update: Option<String> }
impl Seen {
    pub fn file(dir: &Path) -> PathBuf;   // <dir>/lfl-seen.ini
    pub fn load(dir: &Path) -> Seen;
    pub fn save(&self, dir: &Path) -> Res<()>;
    pub fn is_seen(&self, id: &str) -> bool;
    pub fn mark(&mut self, id: &str);
    pub fn mark_all(&mut self, feed: &Feed);
    pub fn unseen<'a>(&self, feed: &'a Feed) -> Vec<&'a Note>;
}
```

Rules that must not regress:

1. **`is_newer` compares numerically.** `0.10.0 > 0.9.0`. A value that does not
   parse as at least two numeric parts is never "newer", so a hostile `latest`
   cannot fake an update.
2. **`Feed.url` is display-only.** The launcher must never open it; opening a
   remote-supplied URL is a phishing vector. Only `http(s)` is even kept.
3. **Everything from the feed is untrusted**: cap lengths, strip control
   characters, cap note count, cap document size. Never index a feed string by
   byte without checking it is ASCII.
4. **An empty feed is valid.** `{}` means "no announcements", not an error.
5. **`Seen.ids` is bounded** at 200, oldest dropped. `save` writes oldest-first so
   a reload restores the same order.

## Ownership map (disjoint write scopes)

| Owner | Files |
|---|---|
| lead | `Cargo.toml`, `src/main.rs`, `src/util.rs`, `docs/`, `README.md` |
| net-core | `src/json.rs`, `src/hash.rs`, `src/net.rs` |
| meta | `src/version.rs`, `src/libraries.rs`, `src/assets.rs` |
| runtime | `src/java.rs`, `src/zip.rs`, `src/inflate.rs` |

## src/util.rs (lead, stable)

```rust
pub type Res<T> = Result<T, String>;
pub fn set_quiet(v: bool); pub fn quiet() -> bool;
pub fn info(msg: &str);                       // println! unless quiet
pub fn to_wide(s: &str) -> Vec<u16>;          // UTF-16 + NUL
pub unsafe fn from_wide(p: *const u16) -> String;
pub fn env(name: &str) -> Option<String>;     // empty string treated as absent
pub fn home_dir() -> Option<PathBuf>;
pub fn default_dir() -> PathBuf;              // `.minecraft` next to the exe
pub fn mkdirs(p: &Path) -> Res<()>;
pub fn file_size(p: &Path) -> u64;            // 0 when missing
pub fn human_size(n: u64) -> String;          // "512 B" "1 KB" "3.0 MB" "2.00 GB"
pub fn exe_dir() -> PathBuf;
pub fn os_name() -> &'static str;             // "windows"
pub fn os_arch() -> &'static str;             // "x64"
pub fn os_version() -> &'static str;          // "10.0"
pub fn now_ms() -> u128;
pub fn is_absolute_win(s: &str) -> bool;
```

## src/json.rs (net-core)

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),      // preserves document order
}

impl Json {
    pub fn parse(text: &str) -> Res<Json>;             // strict RFC 8259; error carries byte offset
    pub fn is_null(&self) -> bool;
    pub fn as_str(&self) -> Option<&str>;
    pub fn as_bool(&self) -> Option<bool>;
    pub fn as_f64(&self) -> Option<f64>;
    pub fn as_u64(&self) -> Option<u64>;               // integral only, >= 0
    pub fn as_i64(&self) -> Option<i64>;
    pub fn as_arr(&self) -> Option<&[Json]>;
    pub fn as_obj(&self) -> Option<&[(String, Json)]>;
    pub fn get(&self, key: &str) -> Option<&Json>;
    pub fn at(&self, index: usize) -> Option<&Json>;   // array index
    pub fn path(&self, keys: &[&str]) -> Option<&Json>; // nested get chain, e.g. &["downloads","client","url"]
    pub fn str_at(&self, key: &str) -> Option<&str>;
    pub fn u64_at(&self, key: &str) -> Option<u64>;
    pub fn arr_at(&self, key: &str) -> Option<&[Json]>;
}

pub fn escape(s: &str) -> String;  // escaped JSON string *content* (no surrounding quotes)
```
Requirements: must parse the 620 KB asset index (`assets/indexes/36.json`) and 1.2 MB JRE manifest
in well under a second; handle `\/`, `\uXXXX` incl. surrogate pairs, exponents, `-0`.
Duplicate keys: keep the last one for `get()` lookups (document order preserved).

## src/hash.rs (net-core)

```rust
pub fn hex(bytes: &[u8]) -> String;              // lowercase
pub struct Sha1 { pub fn new() -> Self; pub fn update(&mut self, d: &[u8]); pub fn finalize(self) -> [u8; 20]; }
pub fn sha1(data: &[u8]) -> [u8; 20];
pub fn sha1_hex(data: &[u8]) -> String;
pub fn md5(data: &[u8]) -> [u8; 16];
pub fn file_sha1(path: &Path) -> Res<String>;    // streaming, 64 KiB buffer
pub fn file_matches(path: &Path, sha1_hex: &str) -> bool;  // false when file missing; true when sha1_hex is empty
```

## src/net.rs (net-core)

WinHTTP via FFI: `#[link(name = "winhttp")]`. Auto proxy (`WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY`),
auto redirects, TLS via schannel (built in).

```rust
pub fn get(url: &str) -> Res<Vec<u8>>;                 // GET, 2xx -> body; else Err("HTTP <code> for <url>")
pub fn get_opt(url: &str) -> Res<Option<Vec<u8>>>;     // 404/403/410 -> Ok(None)
pub fn download(url: &str, dest: &Path, sha1: &str, size: u64) -> Res<()>;
pub fn download_many(items: &[Download]) -> Res<()>;   // ≤ 12 worker threads, aggregated progress
pub struct Download { pub url: String, pub dest: PathBuf, pub sha1: String, pub size: u64, pub label: String }
impl Download { pub fn new(url: &str, dest: PathBuf, sha1: &str, size: u64) -> Self; } // default label = file name
pub fn set_progress(enabled: bool);
```

`download` semantics:
- If `dest` exists and matches `sha1` (or `sha1` is empty) → return `Ok(())` without touching it.
- Otherwise stream to `<dest file name>.lflpart` (stable name, no pid/suffix, so a killed run is
  resumed by the next one), verify sha1 while streaming, then rename. On any error delete the temp
  file. 3 attempts with 400/800/1600 ms backoff.
- Never buffer a whole download in RAM; 256 KiB chunks.
- Progress: when enabled and not `util::quiet()`, print per-file percentage with `\r` at most every
  200 ms (`label  42%  3.2 MB / 7.6 MB`), final line on completion. Thread-safe global rendering.
- Return `Err` mentioning url + destination on failure.

## src/version.rs (meta)

```rust
pub struct Download { pub url: String, pub path: String, pub sha1: String, pub size: u64 }

pub struct Rule { pub action: String,                     // "allow" | "disallow"
                  pub os_name: Option<String>, pub os_arch: Option<String>, pub os_version: Option<String>,
                  pub features: Vec<(String, bool)> }     // features map from rules[].features

pub fn rules_allow(rules: &[Rule], features: &Features) -> bool;   // no rules => true

#[derive(Default, Clone)]
pub struct Features { pub demo_user: bool, pub custom_resolution: bool, pub quick_play_support: bool,
                      pub quick_play_singleplayer: bool, pub quick_play_multiplayer: bool,
                      pub quick_play_realms: bool }

pub struct Arg { pub rules: Vec<Rule>, pub value: Vec<String> }    // value: JSON string OR array of strings
pub fn resolve_args(args: &[Arg], features: &Features) -> Vec<String>;  // filters by rules, keeps expanded values

pub struct AssetIndex { pub id: String, pub sha1: String, pub size: u64, pub total_size: u64, pub url: String }
pub struct Logging { pub arg: String, pub id: String, pub url: String, pub sha1: String, pub size: u64 }
pub struct Library { pub name: String,
                     pub artifact: Option<Download>,          // downloads.artifact (may be absent)
                     pub classifiers: Vec<(String, Download)>,// downloads.classifiers (key = "natives-windows" etc.)
                     pub natives: Vec<(String, String)>,      // legacy natives { "windows": "natives-windows-${arch}" }
                     pub extract_exclude: Vec<String>,
                     pub rules: Vec<Rule> }
pub struct Version {
    pub id: String, pub version_type: String, pub main_class: String,
    pub asset_index: AssetIndex, pub assets: Option<String>,   // legacy pre-1.7 "assets" id
    pub libraries: Vec<Library>, pub game_args: Vec<Arg>, pub jvm_args: Vec<Arg>,
    pub legacy_args: Option<String>,                            // pre-1.13 minecraftArguments
    pub logging: Option<Logging>, pub client: Download, pub java_major: u32,
    pub java_component: String, pub inherits_from: Option<String>, pub jar: Option<String>,
}

pub fn versions_dir(dir: &Path) -> PathBuf;                       // <dir>/versions
pub fn fetch_manifest(dir: &Path) -> Res<Vec<ManifestEntry>>;     // cached <dir>/versions/version_manifest_v2.json
pub struct ManifestEntry { pub id: String, pub kind: String, pub url: String }
pub fn resolve_entry(dir: &Path, want: &str) -> Res<ManifestEntry>;
// want: "latest"|"latest-release"|"release"|"latest-snapshot"|"snapshot"|exact id (case-insensitive)
pub fn ensure(client: &net::Client?, ...) — NOT used; download with `net::download` directly.
pub fn load(dir: &Path, entry: &ManifestEntry) -> Res<Version>;   // downloads <dir>/versions/<id>/<id>.json
                                                                  // if missing, parses it, follows inherits_from,
                                                                  // and downloads client.jar (via libraries::prepare)

// Added for the 下载 page: a loader profile id is not a game version, so anything that needs
// "the Minecraft version this thing is built on" must go through these.
pub fn is_plain_version(id: &str) -> bool;         // "1.20.1" yes; "fabric-loader-…-1.20.1"/"latest-release" no
pub fn base_version(dir: &Path, id: &str) -> String;  // inheritsFrom chain, else the version-shaped
                                                      // suffix of the id, else "" (callers: no filter)
```
`load` must handle `inheritsFrom` (merge parent libs/args/mainClass, child wins) even though the
common 1.20.1/1.8.9 case has none. `legacy_args` (`minecraftArguments` string) and modern
`arguments.{game,jvm}` are both supported.

`ManifestEntry.kind` is what splits the 下载 page into **正式版** and **预览版**: `release` is the
stable channel, and `snapshot` / `old_beta` / `old_alpha` are the preview channel.

## src/libraries.rs (meta)

```rust
pub struct Classpath { pub entries: Vec<PathBuf>, pub natives_dir: PathBuf, pub client_jar: PathBuf }
pub fn prepare(dir: &Path, v: &version::Version) -> Res<Classpath>;
```
- Downloads every rule-allowed library artifact to `<dir>/libraries/<downloads.artifact.path>`.
- Extracts allowed natives classifiers for `os_name=windows`, `os_arch=x64` (legacy `natives`
  mapping substituted `${arch}`→`64`) into `<dir>/versions/<id>/natives` via `zip::extract`.
- Ensures `<dir>/versions/<id>/<client.jar or id.jar>` exists (from `Version::client`).
- Returns ordered classpath (library order) ending with the client jar.
- Honours `extract.exclude`.
- Parallel downloads through `net::download_many`.

## src/assets.rs (meta)

```rust
pub struct AssetObject { pub hash: String, pub size: u64 }
pub fn load_index(dir: &Path, index: &version::AssetIndex) -> Res<Vec<AssetObject>>;
pub fn ensure(dir: &Path, index: &version::AssetIndex) -> Res<usize>;   // == load_index + download all missing
```
Index file: `<dir>/assets/indexes/<id>.json`, objects: `<dir>/assets/objects/<h[0..2]>/<h>`,
URL `https://resources.download.minecraft.net/<h[0..2]>/<h>`.
Return count of objects present after the call. Legacy (`<dir>/assets/virtual/<id>` / pre-1.7) not required.

## src/java.rs (runtime)

```rust
pub struct Java { pub exe: PathBuf, pub home: PathBuf, pub major: u32 }
pub fn probe(exe: &Path) -> Res<u32>;                 // run `exe -version`, parse major from stderr+stdout
pub fn find_system(need_major: u32) -> Option<Java>;  // %JAVA_HOME%\bin\javaw.exe, then PATH javaw/java
pub fn ensure(dir: &Path, component: &str, need_major: u32) -> Res<Java>;
```
`ensure`: prefer a Mojang JRE already installed at `<dir>/runtime/<component>`; else accept system
Java ≥ need_major; else install the Mojang component:
`https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json`
→ `all["windows-x64"][component][0]["manifest"]["url"]` → `files` map; each `type=="file"` entry has
`downloads.raw.{url,sha1,size}` and is written to `<dir>/runtime/<component>/<key>` (with
`type=="directory"` creating the dir; `executable` flag ignored). Skip `*_*.lzma`-only entries — raw
is always present. ~87 MB. Progress through `net::download_many` in batches; verify sha1.
Return `Java { exe: <home>/bin/javaw.exe (fallback java.exe), home, major }`; create a marker
`<home>/.lfl-ok` (contents = component) so later runs skip verification work.

## src/javafind.rs

```rust
pub struct Found { pub exe: PathBuf, pub home: PathBuf, pub major: u32, pub source: String, pub managed: bool }
pub fn scan_all(data_dir: &Path) -> Vec<Found>;          // newest major first, de-duplicated by canonical home
pub fn best_for(found: &[Found], need: u32) -> Option<&Found>;
```

`scan_all` looks for runtimes this launcher installed, then `%JAVA_HOME%`, then `where.exe java`
plus every `PATH` entry, then the JavaSoft registry keys (HKLM, HKCU and the 32-bit WOW6432Node
view), then the vendor install roots. Every candidate is probed with `java -probe`-style version
parsing; a directory is only *read* when its name says it could hold a runtime
(`could_hold_jdk`), and it is only *reported* when it contains `bin/java.exe` (`has_java`).

The walk is bounded to 3 levels under a fixed list of roots and **never** walks the whole disk: a
full-volume scan would spend minutes on I/O and find nothing a targeted list misses. This is a
deliberate property — do not "improve" it into a recursive search.

## src/zip.rs + src/inflate.rs (runtime)

```rust
// inflate.rs
pub fn inflate_raw(input: &[u8], expected: usize) -> Res<Vec<u8>>;   // raw DEFLATE (no zlib header)
pub fn inflate_zlib(input: &[u8], expected: usize) -> Res<Vec<u8>>;  // with 2-byte header + adler32 (unchecked)

// zip.rs
pub fn list(zip: &Path) -> Res<Vec<String>>;
pub fn extract(zip: &Path, out_dir: &Path, excludes: &[&str]) -> Res<usize>;
```
`extract` reads the central directory (EOCD), skips directory entries, `META-INF/*` and any entry whose
path contains an `excludes` fragment, writes `<out_dir>/<entry path>` creating parents (stored and
DEFLATE methods only; method 8 → `inflate_raw`), and additionally copies any `.dll`/`.exe` to
`<out_dir>/<basename>` when the entry has a directory prefix (so `java.library.path` finds it).
Reject entries escaping `out_dir` (`..`, absolute) with `Err`. Return number of files written.
Entry sizes may be 0 in the local header — use the central directory record.
