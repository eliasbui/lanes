# Installs Lanes for the current user from the latest GitHub release, or updates it: no administrator rights, a
# shortcut in the Start menu, and an entry in Settings, Apps that removes it.
#
#   irm https://raw.githubusercontent.com/eliasbui/lanes/main/scripts/install.ps1 | iex
#
# The download list and settings in %APPDATA%\Lanes stay when the app is updated or removed. The script runs in a
# block of its own, so iex leaves no variables or preferences behind in the session.
& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repo = 'eliasbui/lanes'
    $dir = Join-Path $env:LOCALAPPDATA 'Programs\Lanes'
    $exe = Join-Path $dir 'Lanes.exe'
    $shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'Lanes.lnk'
    $key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Lanes'
    function Quote($text) { "'" + $text.Replace("'", "''") + "'" }

    $headers = @{}
    if ($env:GITHUB_TOKEN) { $headers.Authorization = "Bearer $env:GITHUB_TOKEN" }
    $releases = Invoke-RestMethod "https://api.github.com/repos/$repo/releases?per_page=10" -Headers $headers
    $release = $releases | Where-Object { -not $_.draft -and -not $_.prerelease -and ($_.assets | Where-Object name -Like 'Lanes-*-windows-x64.zip') } | Select-Object -First 1
    if (-not $release) { throw 'Chua co ban nao cho Windows.' }
    $asset = $release.assets | Where-Object name -Like 'Lanes-*-windows-x64.zip' | Select-Object -First 1
    $version = $release.tag_name.TrimStart('v')
    Write-Host "Dang tai Lanes $version..."

    $zip = Join-Path ([IO.Path]::GetTempPath()) $asset.name
    Invoke-WebRequest $asset.browser_download_url -OutFile $zip -UseBasicParsing
    if ($asset.digest -and $asset.digest -ne "sha256:$((Get-FileHash $zip -Algorithm SHA256).Hash)") {
        Remove-Item $zip
        throw 'File tai ve bi hong, hay chay lai lenh.'
    }

    $running = @(Get-Process Lanes -ErrorAction SilentlyContinue | Where-Object Path -EQ $exe)
    $running | Stop-Process -Force -PassThru | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $dir | Out-Null
    Expand-Archive $zip -DestinationPath $dir -Force
    Remove-Item $zip

    $link = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut)
    $link.TargetPath = $exe
    $link.WorkingDirectory = $dir
    $link.Save()

    $remove = "Get-Process Lanes -ErrorAction SilentlyContinue | Where-Object Path -EQ $(Quote $exe) | " +
        'Stop-Process -Force -PassThru | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue; ' +
        "Remove-Item -Recurse -Force $(Quote $dir), $(Quote $shortcut) -ErrorAction SilentlyContinue; " +
        "Remove-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' 'Lanes' -ErrorAction SilentlyContinue; " +
        "Remove-Item $(Quote $key)"
    $powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $entry = @{
        DisplayName = 'Lanes'
        DisplayVersion = $version
        Publisher = 'eliasbui'
        DisplayIcon = $exe
        InstallLocation = $dir
        UninstallString = "`"$powershell`" -NoProfile -WindowStyle Hidden -EncodedCommand " +
            [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($remove))
    }
    New-Item $key -Force | Out-Null
    foreach ($name in $entry.Keys) { Set-ItemProperty $key $name $entry[$name] }

    if ($running) { Start-Process $exe }
    Write-Host "Da cai Lanes $version vao $dir."
    Write-Host 'Mo app tu menu Start. Chay lai lenh nay de cap nhat; go app trong Settings > Apps.'
}
