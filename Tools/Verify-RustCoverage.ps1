[CmdletBinding()]
param(
    [ValidateRange(0, 100)] [double] $MinimumLinePercent = 90,
    [ValidateRange(0, 100)] [double] $MinimumBranchPercent = 85
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$manifestPath = Join-Path $repositoryRoot 'Sources\Rust\Cargo.toml'
$outputPath = Join-Path $repositoryRoot 'Builds\rust-core-coverage.json'

& cargo +nightly-2026-08-30 llvm-cov clean --workspace --manifest-path $manifestPath
if ($LASTEXITCODE -ne 0) {
    throw "cargo llvm-cov clean failed with exit code $LASTEXITCODE"
}

& cargo +nightly-2026-08-30 llvm-cov `
    --manifest-path $manifestPath `
    -p aurora-types `
    -p aurora-control-contracts `
    -p aurora-test-support `
    --branch `
    --json `
    --output-path $outputPath
if ($LASTEXITCODE -ne 0) {
    throw "cargo llvm-cov failed with exit code $LASTEXITCODE"
}

$coverage = Get-Content -Raw $outputPath | ConvertFrom-Json -Depth 100
$totals = $coverage.data[0].totals
$linePercent = [double] $totals.lines.percent
$branchPercent = [double] $totals.branches.percent

if ($linePercent -lt $MinimumLinePercent) {
    throw "Rust core line coverage $linePercent% is below $MinimumLinePercent%"
}
if ($branchPercent -lt $MinimumBranchPercent) {
    throw "Rust core branch coverage $branchPercent% is below $MinimumBranchPercent%"
}

Write-Output "Rust core coverage passed: lines $linePercent%, branches $branchPercent%."
