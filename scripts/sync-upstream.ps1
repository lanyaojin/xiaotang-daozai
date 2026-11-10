<#
  Weekly upstream merge for the custom presence branch.
  Keeps the custom camera feature while bringing in upstream changes.
  All output is ASCII to be safe with Windows PowerShell 5.1 encoding.
#>
param(
    [string]$Repo = "F:\Software Engineering\xiaotang-daozai",
    [string]$Branch = "presence-v2",
    [switch]$Push,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$report = Join-Path $Repo "docs\UPSTREAM_SYNC.md"

function Note([string]$line) {
    Add-Content -Path $report -Value $line -Encoding UTF8
}

# A local cargo build needs the real MSVC linker. Git Bash ships a GNU `link`
# that rustc may pick up by mistake, which makes the check hang and fail.
# When MSVC is absent we skip local verification and rely on the
# "Build presence" GitHub Actions workflow after the push instead.
function Test-MsvcLinker {
    $link = Get-Command link.exe -ErrorAction SilentlyContinue
    if (-not $link) { return $false }
    foreach ($l in @($link)) {
        try {
            if ($l.FileVersionInfo.FileDescription -like '*Linker*') { return $true }
        } catch { }
    }
    return $false
}

if (-not (Test-Path (Join-Path $Repo ".git"))) { throw "Not a git repository: $Repo" }

git -C $Repo fetch upstream --prune
if ($LASTEXITCODE -ne 0) { throw "git fetch upstream failed" }
git -C $Repo fetch origin --prune
if ($LASTEXITCODE -ne 0) { throw "git fetch origin failed" }

$dirty = git -C $Repo status --porcelain
if ($dirty) { throw "Working tree is not clean. Commit or stash first: $dirty" }

$current = (git -C $Repo branch --show-current).Trim()
if ($current -ne $Branch) {
    git -C $Repo checkout $Branch
    if ($LASTEXITCODE -ne 0) { throw "Cannot checkout $Branch" }
}

$before = (git -C $Repo rev-parse HEAD).Trim()
$upstreamSha = (git -C $Repo rev-parse "upstream/main").Trim()
$newCommits = @(git -C $Repo log --oneline "$before..upstream/main")

# Detect whether upstream has started to implement equivalent features.
# `behind` is too generic: upstream fluid/native/model code uses it as an
# ordinary English word and produced false overlap candidates in earlier runs.
$overlapRaw = @(git -C $Repo grep -il -E "nokhwa|FaceDetector|LockWorkStation|presence|webcam|posture" "upstream/main" -- src Cargo.toml 2>$null)
$overlap = @($overlapRaw | Where-Object { $_ -notmatch '(^|/)(fluid|native|side_dock|model|exchange)\.rs$' })

$mergeResult = "already-up-to-date"
if ($before -ne $upstreamSha) {
    git -C $Repo merge "upstream/main" --no-edit
    if ($LASTEXITCODE -ne 0) {
        git -C $Repo merge --abort 2>$null
        Note ""
        Note "## $(Get-Date -Format 'yyyy-MM-dd HH:mm') CONFLICT"
        Note "- Upstream: $upstreamSha"
        Note "- New upstream commits: $($newCommits.Count)"
        Note "- Merge aborted. Resolve conflicts manually, then re-run this script."
        throw "Merge conflicts detected; merge aborted. Manual resolution required."
    }
    $mergeResult = "merged"
}

$buildResult = "skipped"
if (-not $SkipBuild) {
    if (-not (Test-MsvcLinker)) {
        $buildResult = "skipped-no-msvc-linker (GitHub Actions verifies after push)"
    } else {
        $cargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
        if (Test-Path $cargo) {
            & $cargo check --locked --lib --manifest-path (Join-Path $Repo "Cargo.toml")
            if ($LASTEXITCODE -ne 0) { $buildResult = "cargo-check-FAILED" }
            else {
                & $cargo test --locked presence --manifest-path (Join-Path $Repo "Cargo.toml") -- --test-threads=1
                if ($LASTEXITCODE -ne 0) { $buildResult = "cargo-test-FAILED" } else { $buildResult = "OK" }
            }
        } else {
            $buildResult = "cargo-not-installed"
        }
    }
}

$pushResult = "no"
if ($Push) {
    git -C $Repo push origin $Branch
    if ($LASTEXITCODE -ne 0) { throw "git push failed" }
    $pushResult = "yes"
}

Note ""
Note "## $(Get-Date -Format 'yyyy-MM-dd HH:mm')"
Note "- Upstream: $upstreamSha"
Note "- New upstream commits: $($newCommits.Count)"
foreach ($c in ($newCommits | Select-Object -First 20)) { Note "  - $c" }
Note "- Merge: $mergeResult"
Note "- Build: $buildResult"
Note "- Push: $pushResult"
if ($overlap.Count -gt 0) {
    Note "- FEATURE OVERLAP CANDIDATES (evaluate whether upstream now implements the custom features better):"
    foreach ($f in $overlap) { Note "  - $f" }
    Note "- If upstream is better, run scripts/drop-presence.ps1 -Confirm -Push after manual review."
} else {
    Note "- Feature overlap: none detected"
}
