param([string]$RepositoryRoot = (Join-Path $PSScriptRoot '../..'))

$ErrorActionPreference = 'Stop'
$manifestPath = Join-Path $RepositoryRoot 'Cargo.toml'
$lockPath = Join-Path $RepositoryRoot 'Cargo.lock'
$manifestText = [IO.File]::ReadAllText($manifestPath)
$lockText = [IO.File]::ReadAllText($lockPath)
$manifestPattern = [regex]'(?ms)(^\[package\]\r?\n.*?^version = ")(\d+\.\d+\.\d+)(")'
$lockPattern = [regex]'(?m)(^name = "flacura"\r?\nversion = ")(\d+\.\d+\.\d+)(")'
if ($manifestPattern.Matches($manifestText).Count -ne 1 -or $lockPattern.Matches($lockText).Count -ne 1) {
    throw 'Expected exactly one application version in Cargo.toml and Cargo.lock'
}
$baseVersion = [version]$manifestPattern.Match($manifestText).Groups[2].Value
if ($lockPattern.Match($lockText).Groups[2].Value -ne $baseVersion.ToString()) {
    throw 'Cargo.toml and Cargo.lock versions must match'
}

# Reuse the tag on retries; otherwise choose the next unused patch version.
$headTags = @(git -C $RepositoryRoot tag --points-at HEAD)
if ($LASTEXITCODE -ne 0) { throw 'Cannot read tags at HEAD' }
$allTags = @(git -C $RepositoryRoot tag --list 'v*')
if ($LASTEXITCODE -ne 0) { throw 'Cannot read version tags' }
$headVersion = $headTags | Where-Object { $_ -match '^v\d+\.\d+\.\d+$' } |
    ForEach-Object { [version]$_.Substring(1) } | Sort-Object -Descending | Select-Object -First 1
$latestVersion = $allTags | Where-Object { $_ -match '^v\d+\.\d+\.\d+$' } |
    ForEach-Object { [version]$_.Substring(1) } | Sort-Object -Descending | Select-Object -First 1
if ($headVersion) {
    $releaseVersion = $headVersion.ToString()
} elseif ($latestVersion -and $latestVersion -ge $baseVersion) {
    $releaseVersion = '{0}.{1}.{2}' -f $latestVersion.Major, $latestVersion.Minor, ($latestVersion.Build + 1)
} else {
    $releaseVersion = $baseVersion.ToString()
}

# Only the CI checkout is changed. Do not create version-bump commits on main.
$replaceVersion = { param($match) $match.Groups[1].Value + $releaseVersion + $match.Groups[3].Value }
[IO.File]::WriteAllText($manifestPath, $manifestPattern.Replace($manifestText, $replaceVersion, 1))
[IO.File]::WriteAllText($lockPath, $lockPattern.Replace($lockText, $replaceVersion, 1))
if ($env:GITHUB_OUTPUT) {
    "version=$releaseVersion" | Out-File -LiteralPath $env:GITHUB_OUTPUT -Encoding utf8 -Append
}
Write-Output "Release version: $releaseVersion"
