<#
  Remove the custom presence feature when upstream provides a better implementation.
  Requires -Confirm. Restores the four modified upstream files from upstream/main
  and deletes the three new source files, then commits.
  All output is ASCII for Windows PowerShell 5.1 compatibility.
#>
param(
    [string]$Repo = "F:\Software Engineering\xiaotang-daozai",
    [string]$Branch = "presence-v2",
    [switch]$Confirm,
    [switch]$Push
)

$ErrorActionPreference = "Stop"
if (-not $Confirm) { throw "This removes the custom presence feature. Re-run with -Confirm after reviewing the upstream implementation." }

$dirty = git -C $Repo status --porcelain
if ($dirty) { throw "Working tree is not clean: $dirty" }

git -C $Repo checkout $Branch
if ($LASTEXITCODE -ne 0) { throw "Cannot checkout $Branch" }

$files = @("src\presence.rs", "src\presence_backend.rs", "src\presence_ui.rs")
foreach ($f in $files) {
    if (Test-Path (Join-Path $Repo $f)) {
        git -C $Repo rm $f
        if ($LASTEXITCODE -ne 0) { throw "Cannot remove $f" }
    }
}

# Restore files that only contain custom hooks to their upstream state.
git -C $Repo checkout "upstream/main" -- Cargo.toml Cargo.lock src/lib.rs src/app.rs src/extras.rs
if ($LASTEXITCODE -ne 0) { throw "Cannot restore upstream files" }

git -C $Repo commit -m "Remove custom presence feature in favour of upstream implementation"
if ($LASTEXITCODE -ne 0) { throw "Commit failed (nothing to commit?)" }

if ($Push) {
    git -C $Repo push origin $Branch
    if ($LASTEXITCODE -ne 0) { throw "git push failed" }
}

Write-Output "Custom presence feature removed. docs/ and scripts/ were kept for reference."
