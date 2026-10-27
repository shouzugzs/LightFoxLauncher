// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! LFL — command-line front-end.
//!
//! All real work lives in the `LFL` library (`launcher`, `version`, `net`, …),
//! which the graphical front-end drives too, so the two cannot diverge.

use std::path::PathBuf;

use lightfox::launcher::{self, Options};
use lightfox::util::{self, info, mkdirs, Res};
use lightfox::{java, version, NAME, VERSION};

#[derive(Clone, Copy, PartialEq)]
enum Cmd {
    Run,
    Install,
    List,
    Status,
    Clean,
    Help,
    Version,
    /// List or install mod loaders.
    Loader,
    /// List, add or remove mods of an instance.
    Mods,
    /// Search and download mods from Modrinth.
    Search,
    /// Manage locally installed versions (list/delete/clone).
    Versions,
}

struct Cli {
    cmd: Cmd,
    opt: Options,
    quiet: bool,
    wait: bool,
    dry_run: bool,
    limit: usize,
    /// `loader list|install [<name>]` — loader family and version.
    loader_name: Option<String>,
    loader_version: Option<String>,
    /// `mods add <jar>` / `mods remove <jar>`.
    mod_action: Option<String>,
    mod_path: Option<String>,
    /// `--mods` for `loader install`: also print the mods directory.
    print_mods_dir: bool,
    /// Positionals that belong to `loader`/`mods`/`search`/`versions`.
    extra: Vec<String>,
    /// `search --limit` is also used as the result count; `--install` picks a hit.
    install_index: Option<usize>,
}

impl Cli {
    fn parse() -> Res<Cli> {
        let mut c = Cli {
            cmd: Cmd::Run,
            opt: Options::default(),
            quiet: false,
            wait: false,
            dry_run: false,
            limit: 30,
            loader_name: None,
            loader_version: None,
            mod_action: None,
            mod_path: None,
            print_mods_dir: false,
            extra: Vec::new(),
            install_index: None,
        };
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0usize;

        if let Some(first) = argv.first() {
            if !first.starts_with('-') {
                match first.as_str() {
                    "run" | "launch" | "start" => {
                        c.cmd = Cmd::Run;
                        i = 1;
                    }
                    "install" | "download" => {
                        c.cmd = Cmd::Install;
                        i = 1;
                    }
                    "list" | "versions" => {
                        c.cmd = Cmd::List;
                        i = 1;
                    }
                    "status" | "info" => {
                        c.cmd = Cmd::Status;
                        i = 1;
                    }
                    "clean" => {
                        c.cmd = Cmd::Clean;
                        i = 1;
                    }
                    "loader" | "loaders" => {
                        c.cmd = Cmd::Loader;
                        i = 1;
                    }
                    "mods" | "mod" => {
                        c.cmd = Cmd::Mods;
                        i = 1;
                    }
                    "search" | "find" => {
                        c.cmd = Cmd::Search;
                        i = 1;
                    }
                    "ver" | "vlist" => {
                        c.cmd = Cmd::Versions;
                        i = 1;
                    }
                    "help" => {
                        c.cmd = Cmd::Help;
                        i = 1;
                    }
                    "version" => {
                        c.cmd = Cmd::Version;
                        i = 1;
                    }
                    _ => {
                        // A bare first token is a version id for an implicit `run`.
                        c.opt.version = Some(first.clone());
                        i = 1;
                    }
                }
            }
        }

        while i < argv.len() {
            let a = argv[i].as_str();
            match a {
                "-h" | "--help" => c.cmd = Cmd::Help,
                "-V" | "--version" => c.cmd = Cmd::Version,
                "-q" | "--quiet" => c.quiet = true,
                "--wait" => c.wait = true,
                "--demo" => c.opt.demo = true,
                "-n" | "--dry-run" => c.dry_run = true,
                "-d" | "--dir" | "--game-dir" => {
                    i += 1;
                    c.opt.dir = Some(PathBuf::from(need(&argv, i, a)?));
                }
                "-u" | "--user" | "--username" => {
                    i += 1;
                    c.opt.user = need(&argv, i, a)?.to_string();
                }
                "-m" | "--memory" | "--xmx" => {
                    i += 1;
                    c.opt.memory = need(&argv, i, a)?.to_string();
                }
                "--width" => {
                    i += 1;
                    c.opt.width = Some(numeric(&need(&argv, i, a)?, "width")?);
                }
                "--height" => {
                    i += 1;
                    c.opt.height = Some(numeric(&need(&argv, i, a)?, "height")?);
                }
                "--java" => {
                    i += 1;
                    c.opt.java = Some(PathBuf::from(need(&argv, i, a)?));
                }
                "--instance" | "-i" => {
                    i += 1;
                    c.opt.instance = Some(need(&argv, i, a)?.to_string());
                }
                "--isolate" => c.opt.isolate = true,
                "--no-isolate" => c.opt.isolate = false,
                "--limit" => {
                    i += 1;
                    c.limit = numeric(&need(&argv, i, a)?, "limit")? as usize;
                }
                "--loader" => {
                    i += 1;
                    c.loader_name = Some(need(&argv, i, a)?.to_string());
                }
                "--loader-version" => {
                    i += 1;
                    c.loader_version = Some(need(&argv, i, a)?.to_string());
                }
                "--mods-dir" => c.print_mods_dir = true,
                "--install" => {
                    i += 1;
                    c.install_index = Some(numeric(&need(&argv, i, a)?, "install")? as usize);
                }
                _ if a.starts_with("--") => {
                    return Err(format!("unknown option '{a}' (try `LFL help`)"))
                }
                _ => {
                    // `search`/`ver` take only their own positionals (a query or
                    // an action); treating the first one as a version id would
                    // turn `LFL search sodium` into a search for nothing.
                    match c.cmd {
                        Cmd::Search | Cmd::Versions => c.extra.push(a.to_string()),
                        Cmd::Loader | Cmd::Mods => {
                            if c.opt.version.is_none() {
                                c.opt.version = Some(a.to_string());
                            } else {
                                c.extra.push(a.to_string());
                            }
                        }
                        _ => {
                            if c.opt.version.is_none() {
                                c.opt.version = Some(a.to_string());
                            } else {
                                return Err(format!("unexpected argument '{a}'"));
                            }
                        }
                    }
                }
            }
            i += 1;
        }

        // `loader <action> <args...>` / `mods <action> <args...>`.
        if matches!(c.cmd, Cmd::Loader | Cmd::Mods) {
            let mut words: Vec<String> = Vec::new();
            if let Some(v) = &c.opt.version {
                words.push(v.clone());
            }
            words.extend(c.extra.iter().cloned());
            // `words[0]` is the action (`list`/`install`/`add`/`remove`).
            if let Some(action) = words.first().cloned() {
                match c.cmd {
                    Cmd::Loader => {
                        if action == "install" {
                            if c.loader_name.is_none() {
                                c.loader_name = words.get(1).cloned();
                            }
                            if c.opt.version.is_some() {
                                // The version was consumed as words[0]; recover
                                // the real Minecraft version from words[2].
                                c.opt.version = words.get(2).cloned();
                            }
                        } else if action == "list" {
                            // `loader list <mc>`: words[1] is the version.
                            c.opt.version = words.get(1).cloned();
                        }
                    }
                    Cmd::Mods => {
                        c.mod_action = Some(action);
                        c.mod_path = words.get(1).cloned();
                        c.opt.version = None;
                    }
                    _ => {}
                }
            }
        }
        Ok(c)
    }
}

fn need<'a>(argv: &'a [String], i: usize, flag: &str) -> Res<&'a str> {
    argv.get(i)
        .map(|s| s.as_str())
        .ok_or_else(|| format!("option '{flag}' needs a value"))
}

fn numeric(s: &str, what: &str) -> Res<u32> {
    s.parse::<u32>()
        .map_err(|_| format!("{what} must be a positive integer, got '{s}'"))
}

fn main() {
    let cli = match Cli::parse() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("LFL: {e}");
            std::process::exit(2);
        }
    };
    util::set_quiet(cli.quiet);
    let code = match dispatch(cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!();
            eprintln!("LFL: error: {e}");
            1
        }
    };
    std::process::exit(code);
}

fn dispatch(c: Cli) -> Res<i32> {
    match c.cmd {
        Cmd::Help => {
            print_help();
            Ok(0)
        }
        Cmd::Version => {
            println!("{NAME} {VERSION}");
            Ok(0)
        }
        Cmd::List => cmd_list(&c),
        Cmd::Status => cmd_status(&c),
        Cmd::Clean => cmd_clean(&c),
        Cmd::Loader => cmd_loader(&c),
        Cmd::Mods => cmd_mods(&c),
        Cmd::Search => cmd_search(&c),
        Cmd::Versions => cmd_versions(&c),
        Cmd::Install => {
            launcher::prepare(&c.opt)?;
            info("ready");
            Ok(0)
        }
        Cmd::Run => cmd_run(&c),
    }
}

fn print_help() {
    println!(
        "{NAME} {VERSION} — a minimal zero-dependency Minecraft launcher

USAGE:
    LFL [run] [VERSION] [OPTIONS]      install if needed, then launch
    LFL install [VERSION] [OPTIONS]    download version, libraries and assets
    LFL list [OPTIONS]                 list versions from Mojang manifest
    LFL status [OPTIONS]               show data dir, sizes and Java
    LFL clean [OPTIONS]                remove leftover partial downloads
    LFL loader list [MC]               show loaders available for a version
    LFL loader install <L> <MC>        install a loader profile (fabric/quilt)
    LFL mods [list]                    list mods of the current instance
    LFL mods add <file.jar>            copy a mod into instances/<name>/mods
    LFL mods remove <name>             delete a mod
    LFL search <关键词>                search Modrinth, --install N to download
    LFL ver [list]                     list installed versions on disk
    LFL ver delete <id>                remove a version directory
    LFL ver clone <id> <new>           copy a version under a new name
    LFL-gui                            the same launcher, with a window

VERSION:
    a version id (e.g. 1.20.1, 1.8.9) or `latest-release` (default),
    `latest-snapshot`, `latest`.

OPTIONS:
    -d, --dir <PATH>      data directory (default: %APPDATA%\\.LFL)
    -u, --user <NAME>     offline player name (default: Player)
    -m, --memory <SIZE>   max heap, e.g. 2G or 4096M (default: 2G)
        --width <N>       window width   (requires --height)
        --height <N>      window height  (requires --width)
        --java <PATH>     use this java/javaw instead of the managed runtime
    -i, --instance <NAME> per-instance game dir (default: default); several
                          instances can run at once sharing libraries/assets
        --isolate         run with versions/<id>/ as the game dir, so saves and
                          mods are per version (the layout PCL uses)
        --no-isolate      share one game dir across versions
        --demo            launch the demo version
        --wait            keep the console attached until the game exits
    -n, --dry-run         print the command line instead of launching
    -q, --quiet           only print errors
    -h, --help            show this help
    -V, --version         show launcher version

NOTES:
    Offline mode only: no Microsoft account is used, the game starts with a
    local player name and a deterministic offline UUID. Java is provided by
    Mojang's own runtime feed when no suitable JDK/JRE is installed."
    );
}

fn cmd_run(c: &Cli) -> Res<i32> {
    let p = launcher::prepare(&c.opt)?;
    let launch = launcher::build_launch(&p, &c.opt)?;
    if c.dry_run {
        println!();
        println!("{}", launch.exe.display());
        for a in &launch.args {
            println!("{a}");
        }
        return Ok(0);
    }
    if c.wait {
        let code = launcher::spawn_and_wait(&launch)?;
        info(&format!("minecraft exited with code {code}"));
        return Ok(code);
    }
    let pid = launcher::spawn_detached(&launch)?;
    info(&format!("minecraft started (pid {pid}) — have fun!"));
    Ok(0)
}

fn cmd_list(c: &Cli) -> Res<i32> {
    let dir = c.opt.data_dir();
    mkdirs(&dir)?;
    let list = version::fetch_manifest(&dir)?;
    for e in list.iter().take(c.limit) {
        println!("{:<24} {}", e.id, e.kind);
    }
    if list.len() > c.limit {
        info(&format!("… {} more (use --limit N)", list.len() - c.limit));
    }
    Ok(0)
}

fn cmd_status(c: &Cli) -> Res<i32> {
    let dir = c.opt.data_dir();
    println!("data dir   {}", dir.display());
    println!("launcher   {NAME} {VERSION}");
    for (label, sub) in [
        ("versions", "versions"),
        ("libraries", "libraries"),
        ("assets", "assets"),
        ("runtime", "runtime"),
    ] {
        println!(
            "{:<10} {}",
            label,
            util::human_size(launcher::dir_size(&dir.join(sub)))
        );
    }
    let installed = launcher::installed_versions(&dir);
    if !installed.is_empty() {
        println!("installed  {}", installed.join(", "));
    }
    if let Some(j) = java::find_system(8) {
        println!("java       {} (major {})", j.exe.display(), j.major);
    } else {
        println!("java       not found in JAVA_HOME/PATH (managed runtime is used instead)");
    }
    Ok(0)
}

fn cmd_loader(c: &Cli) -> Res<i32> {
    use lightfox::loaders::{self, Loader};
    let dir = c.opt.data_dir();

    // `loader list <mc> [<loader>]` or `loader install <loader> <mc>`
    let action = c
        .loader_name
        .as_deref()
        .filter(|n| Loader::parse(n).is_some())
        .map(|_| "install")
        .unwrap_or("list");

    if action == "install" {
        let name = c
            .loader_name
            .clone()
            .ok_or("用法: LFL loader install <fabric|quilt> [<版本>]")?;
        let loader = Loader::parse(&name)
            .ok_or_else(|| format!("未知加载器 '{name}'（可用：fabric, quilt）"))?;
        if !loader.has_profile_feed() {
            return Err(format!(
                "{} 没有 profile 源，需要在官方安装器中安装",
                loader.name()
            ));
        }
        let mc = c
            .opt
            .version
            .clone()
            .ok_or("用法: LFL loader install <loader> <Minecraft 版本>")?;
        util::set_quiet(c.quiet);
        info(&format!("为 {} 查找 {} 版本…", mc, loader.name()));
        let builds = loaders::available_builds(loader, &mc)?;
        if builds.is_empty() {
            return Err(format!("{} 没有可用的 {mc} 构建", loader.name()));
        }
        let build = match &c.loader_version {
            Some(v) => builds
                .iter()
                .find(|b| &b.loader_version == v)
                .ok_or_else(|| format!("{} 没有 {} 版本", loader.name(), v))?
                .clone(),
            None => builds[0].clone(),
        };
        println!("安装 {}", build.label());
        let path = loaders::install(&dir, &build)?;
        info(&format!("已写入 {}", path.display()));
        if c.print_mods_dir {
            let mods = loaders::mods_dir(&dir, &c.opt.instance_name());
            info(&format!("模组目录 {}", mods.display()));
        }
        println!("启动: LFL run {} -d {}", build.profile_id, dir.display());
        return Ok(0);
    }

    // list: which loaders + available builds for a Minecraft version
    let mc = c
        .opt
        .version
        .clone()
        .unwrap_or_else(|| "latest-release".to_string());
    println!("{mc} 可用的加载器：");
    for l in Loader::all() {
        if !l.has_profile_feed() {
            println!("  {:<10} 需官方安装器（无 profile 源）", l.name());
            continue;
        }
        match loaders::available_builds(l, &mc) {
            Ok(b) if b.is_empty() => println!("  {:<10} 无构建", l.name()),
            Ok(b) => {
                let head: Vec<&str> = b.iter().take(5).map(|x| x.loader_version.as_str()).collect();
                println!("  {:<10} {} 个构建，最新 {}", l.name(), b.len(), head.join(", "));
            }
            Err(e) => println!("  {:<10} 查询失败: {e}", l.name()),
        }
    }
    Ok(0)
}

fn cmd_search(c: &Cli) -> Res<i32> {
    use lightfox::modrinth;
    let dir = c.opt.data_dir();
    let query = c.extra.first().cloned().unwrap_or_default();
    if query.trim().is_empty() {
        return Err("用法: LFL search <关键词> [--limit N] [--loader fabric] [--install N]".to_string());
    }
    // Filter by a real Minecraft version: an explicit one, else the newest
    // installed, else none at all (searching for `latest-release` would never
    // match a project's `versions` list).
    let mc = c
        .opt
        .version
        .clone()
        .filter(|v| !v.starts_with("latest"))
        .or_else(|| {
            let installed = lightfox::version::installed_versions(&dir);
            if installed.is_empty() {
                None
            } else {
                Some(lightfox::version::default_version(&installed))
            }
        })
        .unwrap_or_default();
    let loader = c.loader_name.clone().unwrap_or_default();
    let limit = c.limit as u32;
    let scope = match (mc.is_empty(), loader.is_empty()) {
        (true, true) => "全部版本".to_string(),
        (false, true) => mc.clone(),
        (true, false) => loader.clone(),
        (false, false) => format!("{mc}, {loader}"),
    };
    println!("搜索 '{query}'（{scope}）…");
    let hits = modrinth::search(&query, &mc, &loader, limit)?;
    if hits.is_empty() {
        println!("没有结果");
        return Ok(0);
    }
    for (i, h) in hits.iter().enumerate() {
        println!(
            "{:>3}. {:<34} {}  [{}]",
            i + 1,
            h.title,
            util::human_size(h.downloads),
            h.loaders.join(",")
        );
        if !h.description.is_empty() {
            println!("     {}", truncate(&h.description, 96));
        }
    }
    match c.install_index {
        Some(n) => {
            let hit = hits
                .get(n.wrapping_sub(1))
                .ok_or_else(|| format!("没有第 {n} 个结果（共 {} 个）", hits.len()))?;
            println!("\n准备安装 {}", hit.title);
            let files = modrinth::versions(&hit.project_id, &mc, &loader)?;
            let file = files.first().ok_or_else(|| {
                format!("{} 没有匹配 {mc} 的文件", hit.title)
            })?;
            let mods = lightfox::loaders::mods_dir(&dir, &c.opt.instance_name());
            let dest = modrinth::download_file(&mods, file)?;
            println!("已下载 {}", dest.display());
            Ok(0)
        }
        None => Ok(0),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn cmd_versions(c: &Cli) -> Res<i32> {
    use lightfox::versionmgr;
    let dir = c.opt.data_dir();
    let action = c.extra.first().cloned().unwrap_or_else(|| "list".to_string());

    match action.as_str() {
        "list" | "ls" => {
            let list = versionmgr::list(&dir);
            if list.is_empty() {
                println!("{} 里没有已安装版本", dir.display());
                return Ok(0);
            }
            println!("{}（{} 个版本，共 {}）", dir.display(), list.len(), util::human_size(versionmgr::total_size(&list)));
            for v in &list {
                let flags = match (v.has_json, v.has_jar) {
                    (true, true) => "",
                    (true, false) => "  [缺 jar]",
                    (false, true) => "  [缺 json]",
                    (false, false) => "  [不完整]",
                };
                println!(
                    "  {:<34} {:>10}{}",
                    v.id,
                    util::human_size(v.size),
                    flags
                );
            }
            Ok(0)
        }
        "delete" | "rm" => {
            let id = c
                .extra
                .get(1)
                .ok_or("用法: LFL ver delete <版本名>")?;
            let freed = versionmgr::delete(&dir, id)?;
            println!("已删除 {id}，释放 {}", util::human_size(freed));
            Ok(0)
        }
        "clone" | "copy" => {
            let from = c.extra.get(1).ok_or("用法: LFL ver clone <源版本> <新名字>")?;
            let to = c
                .extra
                .get(2)
                .ok_or("用法: LFL ver clone <源版本> <新名字>")?;
            let dst = versionmgr::clone_version(&dir, from, to)?;
            println!("已克隆为 {}（{}）", to, dst.display());
            Ok(0)
        }
        other => Err(format!(
            "未知操作 '{other}'（可用：list, delete, clone）"
        )),
    }
}

fn cmd_mods(c: &Cli) -> Res<i32> {
    use lightfox::loaders;
    let dir = c.opt.data_dir();
    let mods = loaders::mods_dir(&dir, &c.opt.instance_name());

    match c.mod_action.as_deref() {
        None | Some("list") => {
            let found = loaders::list_mods(&mods);
            if found.is_empty() {
                println!("{} 里没有模组", mods.display());
                return Ok(0);
            }
            println!("{}（{} 个）", mods.display(), found.len());
            for m in found {
                let ver = if m.version.is_empty() {
                    String::new()
                } else {
                    format!(" v{}", m.version)
                };
                println!(
                    "  {:<28} {}{}  {}",
                    m.name,
                    ver.trim(),
                    "",
                    util::human_size(m.size)
                );
            }
            Ok(0)
        }
        Some("add") => {
            let src = c
                .mod_path
                .as_deref()
                .ok_or("用法: LFL mods add <文件.jar>")?;
            let src_path = PathBuf::from(src);
            let dest = loaders::install_mod(&mods, &src_path)?;
            println!("已安装 {}", dest.display());
            Ok(0)
        }
        Some("remove") | Some("rm") => {
            let name = c.mod_path.as_deref().ok_or("用法: LFL mods remove <名字或文件>")?;
            let found = loaders::list_mods(&mods);
            let target = found
                .iter()
                .find(|m| {
                    m.name.eq_ignore_ascii_case(name)
                        || m.file
                            .file_name()
                            .map(|f| f.to_string_lossy().eq_ignore_ascii_case(name))
                            .unwrap_or(false)
                })
                .ok_or_else(|| format!("{} 里没有找到 {name}", mods.display()))?;
            loaders::remove_mod(&target.file)?;
            println!("已删除 {}", target.file.display());
            Ok(0)
        }
        Some(other) => Err(format!(
            "未知操作 '{other}'（可用：list, add, remove）"
        )),
    }
}

fn cmd_clean(c: &Cli) -> Res<i32> {
    let dir = c.opt.data_dir();
    let mut removed = 0u64;
    let mut count = 0usize;
    for p in launcher::walk(&dir) {
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if name.contains(".part") {
            count += 1;
            removed += util::file_size(&p);
            if !c.dry_run {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    info(&format!(
        "{} {} partial download(s), {} reclaimed",
        if c.dry_run { "would remove" } else { "removed" },
        count,
        util::human_size(removed)
    ));
    Ok(0)
}
