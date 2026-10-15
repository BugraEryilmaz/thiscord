# Inspect one Cargo build directory. Destructive modes require an idle window;
# never run them concurrently with runners, editors, builds or artifact consumers.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$BuildRoot,
    [ValidateSet('Report', 'Incremental', 'ResetDebug')][string]$Mode = 'Report',
    [ValidateRange(0, 3650)][int]$MinimumAgeDays = 14,
    [ValidateRange(0, 1048576)][double]$MaximumGiB = 40,
    [switch]$Apply,
    [switch]$BuildsStopped
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
if ($Apply -and ($Mode -eq 'Report' -or !$BuildsStopped)) {
    throw 'Apply requires a cleanup mode and -BuildsStopped after stopping all consumers.'
}
if (![IO.Path]::IsPathFullyQualified($BuildRoot)) { throw 'BuildRoot must be absolute.' }
$root = Get-Item -LiteralPath $BuildRoot -Force
if (!$root.PSIsContainer) { throw 'BuildRoot must be a directory.' }
for ($ancestor = $root; $null -ne $ancestor; $ancestor = $ancestor.Parent) {
    if ($ancestor.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "Linked build roots are not supported: $($ancestor.FullName)"
    }
}
if (!(Test-Path -LiteralPath (Join-Path $root.FullName 'CACHEDIR.TAG') -PathType Leaf) -and
    !(Test-Path -LiteralPath (Join-Path $root.FullName 'thiscord-cache.json') -PathType Leaf)) {
    throw 'BuildRoot must contain a Cargo CACHEDIR.TAG or a thiscord-cache.json slot marker.'
}
$prefix = $root.FullName.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
$comparison = if ($IsWindows) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }

function Get-SafeTree($Directory, [switch]$AllowLinks) {
    $path = [IO.Path]::GetFullPath($Directory.FullName)
    if (!$path.StartsWith($prefix, $comparison)) { throw "Candidate escaped build root: $path" }
    # Inspect ancestors too, before traversing the candidate.
    for ($parent = $Directory; $parent.FullName -ne $root.FullName; $parent = $parent.Parent) {
        if ($null -eq $parent -or ($parent.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Linked candidate paths are not supported: $path"
        }
    }
    $entries = @(Get-ChildItem -LiteralPath $path -Force -Recurse)
    $links = @($entries | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint })
    if ($links.Count -and !$AllowLinks) {
        throw "Candidate contains links: $path"
    }
    [long]$bytes = 0
    $latest = $Directory.LastWriteTimeUtc
    foreach ($entry in $entries) {
        # Get-ChildItem does not recurse through directory links without
        # -FollowSymlink. Reports omit links, including links in installer bundles.
        if ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) { continue }
        if (!$entry.PSIsContainer) { $bytes += $entry.Length }
        if ($entry.LastWriteTimeUtc -gt $latest) { $latest = $entry.LastWriteTimeUtc }
    }
    [pscustomobject]@{ Path = $path; Bytes = $bytes; LastWriteUtc = $latest; Links = $links.Count }
}

# Only known profile trees, directly or under an explicit target triple. A Linux
# installer slot wraps its build output in target/ and keeps cargo-home/ intact.
$bases = @($root)
if (Test-Path -LiteralPath (Join-Path $root.FullName 'thiscord-cache.json')) {
    $containerTarget = Join-Path $root.FullName 'target'
    if (Test-Path -LiteralPath $containerTarget -PathType Container) {
        $bases += Get-Item -LiteralPath $containerTarget -Force
    }
}
$profiles = @()
foreach ($base in $bases) {
    if ($base.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Linked target directory.' }
    $parents = @($base)
    $parents += @(Get-ChildItem -LiteralPath $base.FullName -Directory -Force |
        Where-Object { $_.Name -cmatch '^[a-zA-Z0-9_]+(?:-[a-zA-Z0-9_]+){2,}$' })
    foreach ($parent in $parents) {
        if ($parent.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Linked target triple.' }
        foreach ($name in @('debug', 'ci', 'release')) {
            $profilePath = Join-Path $parent.FullName $name
            if (Test-Path -LiteralPath $profilePath -PathType Container) {
                $profiles += Get-Item -LiteralPath $profilePath -Force
            }
        }
    }
}
# Reporting can inspect retained installer bundles containing symlinks. Cleanup
# validates every selected deletion tree without allowing links before mutation.
$sizes = @($profiles | ForEach-Object { Get-SafeTree $_ -AllowLinks })
[long]$total = ($sizes | Measure-Object -Property Bytes -Sum).Sum
if ($Mode -eq 'Report') {
    foreach ($size in $sizes) {
        $incremental = Join-Path $size.Path 'incremental'
        $incrementalBytes = if (Test-Path -LiteralPath $incremental -PathType Container) {
            (Get-SafeTree (Get-Item -LiteralPath $incremental -Force) -AllowLinks).Bytes
        } else { 0 }
        [pscustomobject]@{
            GiB = [math]::Round($size.Bytes / 1GB, 2)
            IncrementalGiB = [math]::Round($incrementalBytes / 1GB, 2)
            LinksOmitted = $size.Links
            Path = $size.Path
        }
    }
    if ($total -gt $MaximumGiB * 1GB) {
        Write-Warning "Build profiles exceed the $MaximumGiB GiB budget. Schedule idle maintenance; no files were deleted."
    }
    return
}
$cutoff = [DateTime]::UtcNow.AddDays(-$MinimumAgeDays)
$candidates = @()
if ($Mode -eq 'ResetDebug') {
    # Reset complete debug trees to preserve Cargo's fingerprint consistency.
    # Release/ci output, slot markers, registry downloads and tools are retained.
    if ($total -le $MaximumGiB * 1GB) { return }
    $candidates = @($profiles | Where-Object Name -eq 'debug' | ForEach-Object { Get-SafeTree $_ })
} else {
    foreach ($profile in $profiles) {
        $incremental = Join-Path $profile.FullName 'incremental'
        if (Test-Path -LiteralPath $incremental -PathType Container) {
            if ((Get-Item -LiteralPath $incremental -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw 'Linked incremental directory.'
            }
            foreach ($crate in Get-ChildItem -LiteralPath $incremental -Directory -Force) {
                $size = Get-SafeTree $crate
                if ($MinimumAgeDays -eq 0 -or $size.LastWriteUtc -lt $cutoff) { $candidates += $size }
            }
        }
    }
}
foreach ($candidate in $candidates) {
    if ($Apply) {
        Remove-Item -LiteralPath $candidate.Path -Recurse -Force
    }
    [pscustomobject]@{
        Path = $candidate.Path; Bytes = $candidate.Bytes; LastWriteUtc = $candidate.LastWriteUtc
        Status = if ($Apply) { 'Removed' } else { 'Would-remove' }
    }
}
