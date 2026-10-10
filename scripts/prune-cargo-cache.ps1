# Retire legacy per-PR CI caches. Preview by default; never remove current slots.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$CacheRoot,
    [ValidateRange(1, 3650)][int]$MinimumAgeDays = 30,
    [switch]$Apply,
    [switch]$RunnersStopped
)
$ErrorActionPreference = 'Stop'
if ($Apply -and !$RunnersStopped) {
    throw 'Stop every runner/build using this root, then pass -RunnersStopped with -Apply.'
}
if (![IO.Path]::IsPathFullyQualified($CacheRoot)) { throw 'CacheRoot must be absolute.' }
$root = Get-Item -LiteralPath $CacheRoot -Force
if (!$root.PSIsContainer -or $root.Name -notin @('tc', 'thiscord-cargo')) {
    throw 'Select a runner cache directory named tc or thiscord-cargo.'
}
# Do not traverse junctions/symlinks, including an ancestor of the selected root.
for ($ancestor = $root; $null -ne $ancestor; $ancestor = $ancestor.Parent) {
    if ($ancestor.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "Linked cache paths are not supported: $($ancestor.FullName)"
    }
}
$prefix = $root.FullName.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
$comparison = if ($IsWindows) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
$cutoff = [DateTime]::UtcNow.AddDays(-$MinimumAgeDays)
foreach ($directory in Get-ChildItem -LiteralPath $root.FullName -Directory -Force) {
    # Only the old action's hash-named directories belong to this cleanup policy.
    if ($directory.Name -cnotmatch '^(?:[a-f0-9]{32}|[a-f0-9]{64})$') { continue }
    $path = [IO.Path]::GetFullPath($directory.FullName)
    if (!$path.StartsWith($prefix, $comparison) -or (Split-Path $path -Parent) -ne $root.FullName) {
        throw "Cache candidate escaped its root: $path"
    }
    if ($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        Write-Warning "Skipping linked directory: $path"
        continue
    }
    if (Test-Path -LiteralPath (Join-Path $path 'thiscord-cache.json')) {
        [pscustomobject]@{ Path = $path; Status = 'Keep-current'; Bytes = $null; LastWriteUtc = $null }
        continue
    }
    $entries = @(Get-ChildItem -LiteralPath $path -Recurse -Force)
    if ($entries | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }) {
        Write-Warning "Skipping cache containing links: $path"
        continue
    }
    $latest = $directory.LastWriteTimeUtc
    [long]$bytes = 0
    foreach ($entry in $entries) {
        if ($entry.LastWriteTimeUtc -gt $latest) { $latest = $entry.LastWriteTimeUtc }
        if (!$entry.PSIsContainer) { $bytes += $entry.Length }
    }
    $status = if ($latest -ge $cutoff) { 'Keep-recent' } else { 'Would-remove' }
    if ($Apply -and $status -eq 'Would-remove') {
        # All runners must remain stopped for the whole inspection/deletion.
        # Cargo locks alone do not protect bundle consumers after Cargo exits.
        Remove-Item -LiteralPath $path -Recurse -Force
        $status = 'Removed'
    }
    [pscustomobject]@{ Path = $path; Status = $status; Bytes = $bytes; LastWriteUtc = $latest }
}
