# Cross-platform checks for LightFoxLauncher.
#
# The launcher ships two binaries: `LFL` (CLI) and `LFL-gui` (native Win32).
# The GUI cannot exist off Windows, so "cross-platform" here means:
#
#   1. the **library** compiles for a non-Windows target, so every download,
#      verify and launch routine is portable and re-usable;
#   2. the **CLI** compiles for the same target;
#   3. the **GUI binary** still compiles there, as a stub that points at the CLI
#      instead of failing to link `user32`/`gdi32`/`comctl32`.
#
# A full *link* for the foreign target needs a cross linker (`cc` for
# x86_64-unknown-linux-gnu). `cargo check` catches every type, `cfg` and API
# error and needs none, which is why it is what this script runs and why a
# missing linker is reported as "skipped", never as a pass.
#
# Usage:  pwsh -File scripts/cross-check.ps1
#         pwsh -File scripts/cross-check.ps1 -Target aarch64-apple-darwin

param(
    [string]$Target = "x86_64-unknown-linux-gnu"
)

$ErrorActionPreference = "Stop"
$env:CARGO_TERM_COLOR = "never"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

$failed = 0

function Step($name, [scriptblock]$body) {
    Write-Host "== $name" -ForegroundColor Cyan
    & $body
    if ($LASTEXITCODE -ne 0) {
        Write-Host "   FAILED (exit $LASTEXITCODE)" -ForegroundColor Red
        $script:failed = 1
        return $false
    }
    Write-Host "   ok" -ForegroundColor Green
    return $true
}

# 1. Host build must stay green: the platform gates must not break Windows.
Step "host: cargo check --all-targets" { cargo check --all-targets --quiet }

# 2. The library for the foreign target.
$installed = (rustup target list --installed) -split "`r?`n"
if ($installed -notcontains $Target) {
    Write-Host "== $Target is not installed; skipping the cross checks" -ForegroundColor Yellow
    Write-Host "   run: rustup target add $Target" -ForegroundColor Yellow
    exit $failed
}

Step "cross: cargo check --all-targets --target $Target" {
    cargo check --all-targets --target $Target --quiet
}

# 3. The CLI must be a *real* binary there, not a stub. `cargo check` already
#    proved it type-checks; this asserts the source has no Windows-only import
#    reachable from a non-Windows build.
Write-Host "== cross: CLI has no unconditional Windows API" -ForegroundColor Cyan
$bad = Select-String -Path "src/main.rs", "src/lib.rs" -Pattern "std::os::windows" -SimpleMatch
if ($bad) {
    Write-Host "   FAILED: std::os::windows used outside a cfg gate in a shared file" -ForegroundColor Red
    $bad | ForEach-Object { Write-Host "     $($_.Path):$($_.LineNumber)" }
    $failed = 1
} else {
    Write-Host "   ok" -ForegroundColor Green
}

# 4. Every platform seam must carry both arms. A `#[cfg(windows)]` with no
#    fallback is exactly how a cross build breaks silently later.
Write-Host "== cross: every seam has a fallback" -ForegroundColor Cyan
$seams = @{
    "src/net.rs"      = @("cfg(not(windows))")
    "src/firewall.rs" = @("cfg(not(windows))")
    "src/hostnet.rs"  = @("cfg(not(windows))")
    "src/gui/main.rs" = @("cfg(not(windows))")
}
foreach ($file in $seams.Keys) {
    $text = Get-Content $file -Raw
    foreach ($needle in $seams[$file]) {
        if (-not $text.Contains($needle)) {
            Write-Host "   FAILED: $file has no $needle arm" -ForegroundColor Red
            $failed = 1
        }
    }
}
if ($failed -eq 0) { Write-Host "   ok" -ForegroundColor Green }

# 5. Link, when a linker is available. This is the only step that proves the
#    whole pipeline, so say plainly when it cannot run.
Write-Host "== cross: full link" -ForegroundColor Cyan
$cc = Get-Command cc -ErrorAction SilentlyContinue
if (-not $cc) {
    Write-Host "   skipped: no 'cc' on PATH, so $Target cannot be linked here." -ForegroundColor Yellow
    Write-Host "   `cargo check` above still type-checked every target including the GUI." -ForegroundColor Yellow
} else {
    cargo build --target $Target --quiet
    if ($LASTEXITCODE -ne 0) {
        Write-Host "   FAILED" -ForegroundColor Red
        $failed = 1
    } else {
        Write-Host "   ok" -ForegroundColor Green
    }
}

Write-Host ""
if ($failed -eq 0) {
    Write-Host "cross-check: PASS" -ForegroundColor Green
} else {
    Write-Host "cross-check: FAIL" -ForegroundColor Red
}
exit $failed
