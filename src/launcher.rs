// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Shared launch engine: turn user options into a running Minecraft process.
//!
//! Both front-ends (`LFL` CLI and `LFL-gui`) drive the launcher through this
//! module, so the download/verify/launch behaviour cannot drift between them.
//! OWNER: lead.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::util::{self, info, mkdirs, Res};
use crate::{assets, hash, java, libraries, net, version, BRAND, VERSION};

/// Everything that influences a launch, independent of how it was collected.
#[derive(Clone, Debug)]
pub struct Options {
    /// Version id or `latest`/`latest-release`/`latest-snapshot`; `None` ⇒ latest release.
    pub version: Option<String>,
    /// Data directory; `None` ⇒ `%APPDATA%\.LFL`.
    pub dir: Option<PathBuf>,
    pub user: String,
    /// `-Xmx` value such as `2G` or `4096M`.
    pub memory: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Explicit java/javaw file or JRE home; otherwise the managed runtime is used.
    pub java: Option<PathBuf>,
    pub demo: bool,
    /// Instance name. Its game directory (`instances/<name>/`) holds saves,
    /// options and logs, so several can run at once against one shared
    /// `libraries/`, `assets/` and `runtime/`. `None` ⇒ `default`.
    pub instance: Option<String>,
    /// Version isolation: when set, the game runs with `versions/<version>/` as
    /// its game directory (the layout PCL and the official launcher use), so
    /// each version keeps its own `saves/`, `options.txt`, `mods/` and logs
    /// instead of sharing `instances/<name>/`.
    pub isolate: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            version: None,
            dir: None,
            user: "Player".to_string(),
            memory: "2G".to_string(),
            width: None,
            height: None,
            java: None,
            demo: false,
            instance: None,
            isolate: true,
        }
    }
}

impl Options {
    /// Resolve the data directory, expanding a leading `~/` and making the
    /// result absolute so a child process with a different cwd agrees with us.
    pub fn data_dir(&self) -> PathBuf {
        let raw = match &self.dir {
            Some(p) => {
                let s = p.display().to_string();
                if let Some(rest) = s.strip_prefix("~/") {
                    match util::home_dir() {
                        Some(h) => h.join(rest),
                        None => p.clone(),
                    }
                } else {
                    p.clone()
                }
            }
            None => util::default_dir(),
        };
        if util::is_absolute_win(&raw.display().to_string()) {
            raw
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(raw)
        }
    }

    /// The version id to ask the manifest for, verbatim.
    pub fn want(&self) -> String {
        self.version.clone().unwrap_or_else(|| "latest-release".to_string())
    }

    /// Version to launch when the user did not name one.
    ///
    /// Rather than always going to `latest-release` (which would download a new
    /// version every time Mojang ships one), prefer the newest *already installed*
    /// version. Falls back to the manifest's latest release when nothing is
    /// installed yet.
    pub fn resolve_want(&self) -> String {
        if let Some(v) = &self.version {
            if !v.trim().is_empty() {
                return v.trim().to_string();
            }
        }
        let dir = self.data_dir();
        let choices = match crate::version::available_versions(&dir) {
            Ok(c) => c,
            Err(_) => return "latest-release".to_string(),
        };
        let auto = crate::version::default_version(&choices);
        if choices.iter().any(|c| c.installed && c.id == auto) {
            info(&format!("auto-selected installed version {auto}"));
        }
        auto
    }

    /// Validate combination rules that no front-end may bypass.
    pub fn validate(&self) -> Res<()> {
        if self.user.is_empty() || self.user.len() > 16 {
            return Err("player name must be 1..=16 characters".to_string());
        }
        if self.user.chars().any(|c| c.is_control()) {
            return Err("player name must not contain control characters".to_string());
        }
        if self.width.is_some() != self.height.is_some() {
            return Err("width and height must be given together".to_string());
        }
        if let Some(name) = &self.instance {
            validate_instance_name(name)?;
        }
        validate_memory(&self.memory)?;
        Ok(())
    }

    /// Instance name, or `default`.
    pub fn instance_name(&self) -> String {
        match &self.instance {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => "default".to_string(),
        }
    }

    /// Game directory for this instance: `<dir>/instances/<name>`.
    ///
    /// This is what the game gets as `--gameDir`, so each instance has its own
    /// `saves/`, `options.txt`, `logs/` and `mods/` while libraries, assets and
    /// Java runtimes stay shared and are downloaded only once.
    pub fn game_dir(&self) -> PathBuf {
        if self.isolate {
            // Isolated: the version directory *is* the game directory.
            let v = self
                .version
                .clone()
                .unwrap_or_else(|| "default".to_string());
            return version::versions_dir(&self.data_dir()).join(sanitize_dir(&v));
        }
        self.data_dir().join("instances").join(self.instance_name())
    }

    /// Where this configuration expects mods to live.
    ///
    /// Isolated versions use `<versions>/<id>/mods`; non-isolated ones use
    /// `instances/<name>/mods`. Mod downloads and the version page both read
    /// this, so a mod always lands next to the game that will load it.
    pub fn mods_dir(&self) -> PathBuf {
        self.game_dir().join("mods")
    }

    /// Mods directory of an arbitrary installed version, for the version page.
    ///
    /// Mirrors [`Options::mods_dir`] for the isolated layout: a version's mods
    /// belong in its own directory so switching versions switches mod sets.
    pub fn mods_dir_for(data: &Path, version_id: &str) -> PathBuf {
        version::versions_dir(data).join(sanitize_dir(version_id)).join("mods")
    }
}

/// Make a string safe to use as a single directory component (version ids such
/// as `1.21.8-Fabric 0.17.2` are valid folder names and must stay readable).
fn sanitize_dir(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let t = cleaned.trim().trim_matches('.').to_string();
    if t.is_empty() {
        "default".to_string()
    } else {
        t
    }
}

/// Instance names become directory names, so they must stay inside
/// `instances/`: no separators, no `..`, no drive letters, no reserved names.
pub fn validate_instance_name(name: &str) -> Res<()> {
    let n = name.trim();
    if n.is_empty() {
        return Err("instance name must not be empty".to_string());
    }
    if n.len() > 40 {
        return Err("instance name must be at most 40 characters".to_string());
    }
    if n == "." || n == ".." {
        return Err("instance name must not be '.' or '..'".to_string());
    }
    // Anything that could turn the name into a path (or escape `instances/`)
    // is rejected by letting `std::path` classify components for us.
    let mut comps = std::path::Path::new(n).components();
    if !matches!(
        (comps.next(), comps.next()),
        (Some(std::path::Component::Normal(_)), None)
    ) {
        return Err(
            "instance name must be a single folder name (no path separators)".to_string(),
        );
    }
    if n.chars().any(|c| c.is_control()) {
        return Err("instance name must not contain control characters".to_string());
    }
    // The remaining characters Windows rejects in file names.
    const BAD: [char; 7] = ['<', '>', ':', '"', '|', '?', '*'];
    if n.chars().any(|c| BAD.contains(&c)) {
        return Err(
            "instance name must not contain any of < > : \" | ? *".to_string(),
        );
    }
    // Windows reserves these device names regardless of extension.
    let stem = n.split('.').next().unwrap_or(n).to_ascii_uppercase();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        return Err(format!("instance name '{n}' is a reserved Windows name"));
    }
    Ok(())
}

/// Accept `2G`, `512M`, `4096`, `1536m` …; reject nonsense before it reaches the JVM.
pub fn validate_memory(mem: &str) -> Res<()> {
    let m = mem.trim();
    if m.is_empty() {
        return Err("memory must not be empty (e.g. 2G)".to_string());
    }
    let digits = m.trim_end_matches(['g', 'G', 'm', 'M']);
    if digits.is_empty() || digits.len() > 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("memory '{mem}' must look like 2G or 4096M"));
    }
    let n: u64 = digits.parse().unwrap_or(0);
    let mb = if m.ends_with(['g', 'G']) { n * 1024 } else { n };
    if mb < 256 {
        return Err(format!("memory '{mem}' is too small; use at least 512M"));
    }
    if mb > 64 * 1024 {
        return Err(format!("memory '{mem}' is too large; use at most 64G"));
    }
    Ok(())
}

/// Result of a successful [`prepare`].
pub struct Prepared {
    /// Shared data root (libraries, assets, runtime, versions).
    pub dir: PathBuf,
    /// Per-instance game directory (saves, options, logs).
    pub game_dir: PathBuf,
    pub v: version::Version,
    pub java: java::Java,
    pub cp: libraries::Classpath,
}

/// A launch command that has been fully resolved but not yet executed.
pub struct Launch {
    pub exe: PathBuf,
    pub args: Vec<String>,
    /// Working directory for the child process: the instance game dir.
    pub dir: PathBuf,
}

/// Download/verify the version, a Java runtime, libraries, natives and assets.
pub fn prepare(o: &Options) -> Res<Prepared> {
    o.validate()?;
    let dir = o.data_dir();
    let game_dir = o.game_dir();
    mkdirs(&dir)?;
    info(&format!("data dir  {}", dir.display()));
    if o.isolate {
        info(&format!("隔离模式  游戏目录 = {}", game_dir.display()));
    } else {
        info(&format!("instance  {}", o.instance_name()));
    }

    // No directory-wide lock: downloads are serialised per destination file
    // (see `net::download_inner`), so several launchers can prepare different
    // versions — or different files of the same version — concurrently. A shared
    // file is simply waited for, which is safe because both sides write the same
    // verified bytes.

    let entry = version::resolve_entry(&dir, &o.resolve_want())?;
    info(&format!("[1/4] version {} ({})", entry.id, entry.kind));
    let v = version::load(&dir, &entry)?;
    info(&format!(
        "      {} libraries, assets index '{}', java {}",
        v.libraries.len(),
        v.asset_index.id,
        v.java_major
    ));

    info(&format!("[2/4] java {} (needs {}+)", v.java_component, v.java_major));
    let java = match &o.java {
        Some(p) => {
            let exe = if p.is_dir() {
                let javaw = p.join("bin").join("javaw.exe");
                if javaw.exists() {
                    javaw
                } else {
                    p.join("bin").join("java.exe")
                }
            } else {
                p.clone()
            };
            let major = java::probe(&exe)?;
            let home = exe
                .parent()
                .and_then(|b| b.parent())
                .map(Path::to_path_buf)
                .unwrap_or_default();
            java::Java { exe, home, major }
        }
        None => java::ensure(&dir, &v.java_component, v.java_major)?,
    };
    info(&format!("      using {}", java.exe.display()));

    info("[3/4] libraries, natives, client jar");
    let cp = libraries::prepare(&dir, &v)?;

    info(&format!(
        "[4/4] assets ({})",
        util::human_size(v.asset_index.total_size)
    ));
    let n = assets::ensure(&dir, &v.asset_index)?;
    info(&format!("      {n} asset objects present"));

    ensure_launcher_profiles(&dir)?;
    Ok(Prepared {
        dir,
        game_dir,
        v,
        java,
        cp,
    })
}

/// Some mod loaders want this file to exist; vanilla ignores it.
pub fn ensure_launcher_profiles(dir: &Path) -> Res<()> {
    let p = dir.join("launcher_profiles.json");
    if p.exists() {
        return Ok(());
    }
    let body = "{\n  \"profiles\": {},\n  \"settings\": {},\n  \"version\": 3\n}\n";
    std::fs::write(&p, body).map_err(|e| format!("cannot write {}: {e}", p.display()))
}

/// Build the executable + argv the vanilla launcher would use.
pub fn build_launch(p: &Prepared, o: &Options) -> Res<Launch> {
    // `--gameDir` is the instance directory: that is what isolates saves,
    // options and logs between concurrently running instances.
    let dir_s = p.game_dir.display().to_string();
    let assets_root = p.dir.join("assets").display().to_string();
    let natives = p.cp.natives_dir.display().to_string();
    let classpath = p
        .cp
        .entries
        .iter()
        .map(|x| x.display().to_string())
        .collect::<Vec<_>>()
        .join(";");

    let features = version::Features {
        demo_user: o.demo,
        custom_resolution: o.width.is_some() && o.height.is_some(),
        ..version::Features::default()
    };

    let uuid = offline_uuid(&o.user);
    let (w, h) = match (o.width, o.height) {
        (Some(w), Some(h)) => (w.to_string(), h.to_string()),
        _ => ("854".to_string(), "480".to_string()),
    };

    let kv: Vec<(&str, String)> = vec![
        ("auth_player_name", o.user.clone()),
        ("version_name", p.v.id.clone()),
        ("version_type", p.v.version_type.clone()),
        ("game_directory", dir_s.clone()),
        ("assets_root", assets_root.clone()),
        ("game_assets", assets_root.clone()),
        ("assets_index_name", p.v.asset_index.id.clone()),
        ("auth_uuid", uuid),
        ("auth_access_token", "0".to_string()),
        ("auth_session", "0".to_string()),
        ("clientid", String::new()),
        ("auth_xuid", String::new()),
        ("user_type", "legacy".to_string()),
        ("user_properties", "{}".to_string()),
        ("natives_directory", natives),
        ("launcher_name", BRAND.to_string()),
        ("launcher_version", VERSION.to_string()),
        ("classpath", classpath),
        ("classpath_separator", ";".to_string()),
        ("library_directory", p.dir.join("libraries").display().to_string()),
        ("resolution_width", w),
        ("resolution_height", h),
        ("quickPlayPath", String::new()),
        ("quickPlaySingleplayer", String::new()),
        ("quickPlayMultiplayer", String::new()),
        ("quickPlayRealms", String::new()),
    ];

    // ---- JVM arguments -----------------------------------------------------
    let mut jvm: Vec<String> = if p.v.jvm_args.is_empty() {
        // Legacy versions (<= 1.12) ship no `arguments.jvm`; supply the
        // standard set ourselves, mirroring the vanilla launcher.
        vec![
            "-Djava.library.path=${natives_directory}".to_string(),
            "-Dminecraft.launcher.brand=${launcher_name}".to_string(),
            "-Dminecraft.launcher.version=${launcher_version}".to_string(),
            "-cp".to_string(),
            "${classpath}".to_string(),
        ]
    } else {
        version::resolve_args(&p.v.jvm_args, &features)
    };
    for a in &mut jvm {
        *a = expand(a, &kv);
    }
    apply_jvm_defaults(&mut jvm, &o.memory);

    // ---- game arguments ----------------------------------------------------
    let game: Vec<String> = match &p.v.legacy_args {
        Some(legacy) => legacy.split_whitespace().map(|t| expand(t, &kv)).collect(),
        None => version::resolve_args(&p.v.game_args, &features)
            .iter()
            .map(|a| expand(a, &kv))
            .collect(),
    };

    // ---- log4j config ------------------------------------------------------
    if let Some(lg) = &p.v.logging {
        let dest = p.dir.join("assets").join("log_configs").join(&lg.id);
        if let Some(parent) = dest.parent() {
            mkdirs(parent)?;
        }
        net::download(&lg.url, &dest, &lg.sha1, lg.size)?;
        jvm.push(lg.arg.replace("${path}", &dest.display().to_string()));
    }

    let mut args = jvm;
    args.push(p.v.main_class.clone());
    if o.demo && !game.iter().any(|g| g == "--demo") {
        args.push("--demo".to_string());
    }
    args.extend(game);
    Ok(Launch {
        exe: java_exe(&p.java),
        args,
        dir: p.game_dir.clone(),
    })
}

/// Prefer `javaw.exe` (no console window) when it exists.
pub fn java_exe(j: &java::Java) -> PathBuf {
    let javaw = j.home.join("bin").join("javaw.exe");
    if javaw.exists() {
        return javaw;
    }
    j.exe.clone()
}

/// With `wait` prefer `java.exe` so the game log lands in the calling console.
pub fn pick_console_variant(exe: &Path, wait: bool) -> PathBuf {
    if !wait {
        return exe.to_path_buf();
    }
    let name = exe
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if name == "javaw.exe" {
        let java = exe.with_file_name("java.exe");
        if java.exists() {
            return java;
        }
    }
    exe.to_path_buf()
}

/// Spawn the game detached (no console window).
pub fn spawn_detached(l: &Launch) -> Res<u32> {
    let mut cmd = Command::new(&l.exe);
    cmd.args(&l.args).current_dir(&l.dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", l.exe.display()))?;
    Ok(child.id())
}

/// Spawn the game and block until it exits; returns its exit code.
pub fn spawn_and_wait(l: &Launch) -> Res<i32> {
    let exe = pick_console_variant(&l.exe, true);
    let mut cmd = Command::new(&exe);
    cmd.args(&l.args).current_dir(&l.dir);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    let st = child.wait().map_err(|e| format!("waiting for java failed: {e}"))?;
    Ok(st.code().unwrap_or(1))
}

/// Add heap/GC/log4j defaults only when the version JSON did not specify them.
pub fn apply_jvm_defaults(jvm: &mut Vec<String>, memory: &str) {
    let has = |jvm: &Vec<String>, prefix: &str| jvm.iter().any(|a| a.starts_with(prefix));
    if !has(jvm, "-Xmx") {
        jvm.push(format!("-Xmx{memory}"));
    }
    if !has(jvm, "-Xms") {
        jvm.push("-Xms512M".to_string());
    }
    if !has(jvm, "-Dlog4j2.formatMsgNoLookups") {
        jvm.push("-Dlog4j2.formatMsgNoLookups=true".to_string());
    }
    if !has(jvm, "-Dfile.encoding") {
        jvm.push("-Dfile.encoding=UTF-8".to_string());
    }
    if !has(jvm, "-XX:+UseG1GC") && !has(jvm, "-XX:-UseG1GC") {
        jvm.push("-XX:+UseG1GC".to_string());
        jvm.push("-XX:MaxGCPauseMillis=50".to_string());
    }
}

/// Replace every `${key}` occurrence from `kv`.
pub fn expand(template: &str, kv: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    for (k, v) in kv {
        let needle = format!("${{{k}}}");
        if out.contains(&needle) {
            out = out.replace(&needle, v);
        }
    }
    out
}

/// Deterministic offline UUID: MD5("OfflinePlayer:<name>") as a v3 UUID.
pub fn offline_uuid(name: &str) -> String {
    let mut data = b"OfflinePlayer:".to_vec();
    data.extend_from_slice(name.as_bytes());
    let mut b = hash::md5(&data);
    b[6] = (b[6] & 0x0f) | 0x30;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hash::hex(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// Total size of the data directory subtree, in bytes.
pub fn dir_size(root: &Path) -> u64 {
    walk(root).iter().map(|p| util::file_size(p)).sum()
}

/// All files under `root` (best effort; unreadable directories are skipped).
pub fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let rd = match std::fs::read_dir(&d) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(_) => out.push(p),
                Err(_) => {}
            }
        }
    }
    out
}

/// Is this version already installed well enough to launch without downloading?
pub fn is_installed(dir: &Path, id: &str) -> bool {
    let base = version::version_dir_or_fallback(dir, id);
    base.join(format!("{id}.jar")).exists() && base.join("natives").exists()
}

/// Versions that have a directory on disk, newest-looking first is not implied.
pub fn installed_versions(dir: &Path) -> Vec<String> {
    let root = version::versions_dir(dir);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(name) = e.file_name().to_str() {
                    if e.path().join(format!("{name}.jar")).exists() {
                        out.push(name.to_string());
                    }
                }
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::{compare_version_ids, default_version, filter_versions, VersionChoice};

    /// A backslash, built without writing an escape sequence.
    const BS: &str = "\\";
    /// A non-ASCII instance name (valid on Windows).
    const CJK_NAME: &str = "我的世界";

    #[test]
    fn uuid_is_stable_and_v3() {
        let a = offline_uuid("Player");
        assert_eq!(a, offline_uuid("Player"));
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'3', "version nibble must be 3: {}", a);
        let variant = a.as_bytes()[19];
        assert!(variant == b'8' || variant == b'9' || variant == b'a' || variant == b'b');
        assert_ne!(offline_uuid("Notch"), a);
    }

    #[test]
    fn expand_replaces_placeholders() {
        let kv = vec![("a", "1".to_string()), ("bb", "2".to_string())];
        assert_eq!(expand("x${a}y${bb}z", &kv), "x1y2z");
        assert_eq!(expand("${unknown}", &kv), "${unknown}");
        assert_eq!(expand("plain", &kv), "plain");
    }

    #[test]
    fn jvm_defaults_do_not_duplicate() {
        let mut jvm = vec!["-Xmx4G".to_string()];
        apply_jvm_defaults(&mut jvm, "2G");
        assert_eq!(jvm.iter().filter(|a| a.starts_with("-Xmx")).count(), 1);
        assert!(jvm.iter().any(|a| a == "-Xms512M"));
        assert!(jvm.iter().any(|a| a == "-XX:+UseG1GC"));
        assert!(!jvm.iter().any(|a| a == "-Xmx2G"), "explicit -Xmx must win");
    }

    #[test]
    fn options_validation() {
        let mut o = Options::default();
        assert!(o.validate().is_ok());
        o.user = String::new();
        assert!(o.validate().is_err());
        o.user = "x".repeat(17);
        assert!(o.validate().is_err());
        o.user = "Player".into();
        o.width = Some(800);
        assert!(o.validate().is_err(), "width without height must fail");
        o.height = Some(600);
        assert!(o.validate().is_ok());
    }

    #[test]
    fn memory_validation_reaches_validate() {
        let mut o = Options::default();
        o.memory = "nonsense".into();
        assert!(o.validate().is_err(), "validate() must reject bad memory");
        o.memory = "4G".into();
        assert!(o.validate().is_ok());
    }

    #[test]
    fn absolute_data_dir_stays_absolute() {
        let o = Options {
            dir: Some(PathBuf::from("C:")),
            ..Options::default()
        };
        assert_eq!(o.data_dir(), PathBuf::from("C:"));
    }

    #[test]
    fn want_defaults_to_latest_release() {
        assert_eq!(Options::default().want(), "latest-release");
        let o = Options {
            version: Some("1.20.1".into()),
            ..Options::default()
        };
        assert_eq!(o.want(), "1.20.1");
    }

    #[test]
    fn instance_paths_are_isolated_but_share_data_root() {
        let root = PathBuf::from("ROOT");
        let a = Options {
            dir: Some(root.clone()),
            instance: Some("alpha".into()),
            isolate: false,
            ..Options::default()
        };
        let b = Options {
            dir: Some(root),
            instance: Some("beta".into()),
            isolate: false,
            ..Options::default()
        };
        assert_ne!(
            a.game_dir(),
            b.game_dir(),
            "instances must not share a game dir"
        );
        assert_eq!(a.data_dir(), b.data_dir(), "libraries and assets stay shared");
        let ga = a.game_dir();
        let gs = ga.to_string_lossy().into_owned();
        assert!(gs.contains("instances"), "game dir must sit under instances/: {gs}");
        assert!(gs.contains("alpha"));
        assert!(b.game_dir().to_string_lossy().contains("beta"));
    }

    #[test]
    fn instance_defaults_to_default() {
        // These exercise the shared-instance layout, so isolation is off.
        let o = Options {
            isolate: false,
            ..Options::default()
        };
        assert_eq!(o.instance_name(), "default");
        assert!(o.game_dir().to_string_lossy().contains("instances"));
        let blank = Options {
            instance: Some("   ".into()),
            isolate: false,
            ..Options::default()
        };
        assert_eq!(blank.instance_name(), "default");
    }

    #[test]
    fn instance_names_are_sanitized() {
        for good in ["alpha", "1.20.1 test", "a-b_c.d", "Server 2"] {
            assert!(validate_instance_name(good).is_ok(), "should accept {good:?}");
        }
        assert!(validate_instance_name(CJK_NAME).is_ok(), "non-ASCII names are legal");
        for bad in [
            "", "   ", ".", "..", "a/b", "C:evil", "<x>", "a|b", "a?b", "a*b", "con", "NUL",
            "com1", "lpt9",
        ] {
            assert!(validate_instance_name(bad).is_err(), "should reject {bad:?}");
        }
        assert!(validate_instance_name(&format!("a{BS}b")).is_err());
        assert!(validate_instance_name(&format!("{BS}abs")).is_err());
        assert!(validate_instance_name(&"x".repeat(41)).is_err());
    }

    #[test]
    fn memory_strings_are_validated() {
        for good in ["2G", "512M", "4096", "1g", "1536m", "8G", "64G"] {
            assert!(validate_memory(good).is_ok(), "should accept {good:?}");
        }
        for bad in ["", "  ", "abc", "2GB", "G", "-1G", "1.5G", "128M", "65G", "9999999"] {
            assert!(validate_memory(bad).is_err(), "should reject {bad:?}");
        }
    }
    #[test]
    fn version_ids_sort_numerically() {
        use std::cmp::Ordering;
        assert_eq!(compare_version_ids("1.20.1", "1.9.4"), Ordering::Greater);
        assert_eq!(compare_version_ids("1.20.1", "1.20.1"), Ordering::Equal);
        assert_eq!(compare_version_ids("1.21", "1.20.9"), Ordering::Greater);
        assert_eq!(compare_version_ids("1.8.9", "1.12.2"), Ordering::Less);
    }

    #[test]
    fn default_version_prefers_installed_release() {
        let choices = vec![
            VersionChoice::new("1.21.8", "release", false),
            VersionChoice::new("1.20.1", "release", true),
            VersionChoice::new("1.19.4", "release", true),
        ];
        assert_eq!(default_version(&choices), "1.20.1");
    }

    #[test]
    fn default_version_falls_back_to_latest_release() {
        let choices = vec![
            VersionChoice::new("1.21.8", "release", false),
            VersionChoice::new("26.4-snapshot-1", "snapshot", false),
        ];
        assert_eq!(default_version(&choices), "1.21.8");
        assert_eq!(default_version(&[]), "latest-release");
    }

    #[test]
    fn default_version_accepts_installed_only() {
        // A hand-copied jar with no manifest entry must still be launchable.
        let choices = vec![VersionChoice::new("my-modpack", "installed", true)];
        assert_eq!(default_version(&choices), "my-modpack");
    }

    #[test]
    fn version_filtering_is_case_insensitive() {
        let choices = vec![
            VersionChoice::new("1.20.1", "release", true),
            VersionChoice::new("1.8.9", "release", false),
            VersionChoice::new("26.4-snapshot-1", "snapshot", false),
        ];
        assert_eq!(filter_versions(&choices, "1.20").len(), 1);
        assert_eq!(filter_versions(&choices, "SNAPSHOT").len(), 1);
        assert_eq!(filter_versions(&choices, "").len(), 3);
        assert_eq!(filter_versions(&choices, "zzz").len(), 0);
    }
    #[test]
    fn isolation_puts_game_dir_inside_the_version() {
        let o = Options {
            dir: Some(PathBuf::from("ROOT")),
            version: Some("1.21.8-Fabric 0.17.2".into()),
            isolate: true,
            ..Options::default()
        };
        let g = o.game_dir();
        let s = g.to_string_lossy();
        assert!(s.contains("versions"), "{s}");
        assert!(s.contains("1.21.8-Fabric 0.17.2"), "{s}");
        assert!(!s.contains("instances"), "isolated layout has no instances/: {s}");
        // Mods follow the game dir, so they are per version.
        assert_eq!(o.mods_dir(), g.join("mods"));
    }

    #[test]
    fn non_isolated_uses_instances() {
        let o = Options {
            dir: Some(PathBuf::from("ROOT")),
            version: Some("1.20.1".into()),
            instance: Some("pvp".into()),
            isolate: false,
            ..Options::default()
        };
        let s = o.game_dir().to_string_lossy().into_owned();
        assert!(s.contains("instances"), "{s}");
        assert!(s.contains("pvp"), "{s}");
        assert_eq!(o.mods_dir(), o.game_dir().join("mods"));
    }

    #[test]
    fn isolation_defaults_to_on() {
        assert!(Options::default().isolate, "PCL/official layout should be the default");
    }

    #[test]
    fn isolated_and_shared_layouts_differ() {
        let base = Options {
            dir: Some(PathBuf::from("ROOT")),
            version: Some("1.20.1".into()),
            ..Options::default()
        };
        let iso = Options { isolate: true, ..base.clone() };
        let shared = Options { isolate: false, ..base };
        assert_ne!(iso.game_dir(), shared.game_dir());
        assert_ne!(iso.mods_dir(), shared.mods_dir());
    }

    #[test]
    fn mods_dir_for_any_version_is_isolated() {
        let d = PathBuf::from("ROOT");
        let m = Options::mods_dir_for(&d, "1.20.1");
        assert_eq!(m, d.join("versions").join("1.20.1").join("mods"));
        assert_ne!(m, Options::mods_dir_for(&d, "1.8.9"));
    }
}
