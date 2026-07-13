# Windows tool-availability spike.
#
# Run in PowerShell on a real Windows machine, then paste the whole output back.
# It reports whether each external tool the app drives can be installed via winget
# (and scoop, if you have it) — the make-or-break question for the Windows port,
# since native Windows has no packages for some Unix tools (cpmtools/mtools).
#
#   powershell -ExecutionPolicy Bypass -File windows-tool-spike.ps1
#
# (or open it in PowerShell and run it)

Write-Host "==== environment ===="
Write-Host "Windows: $([System.Environment]::OSVersion.Version)"
if (Get-Command winget -ErrorAction SilentlyContinue) { Write-Host "winget: $(winget --version)" } else { Write-Host "winget: NOT FOUND" }
if (Get-Command scoop  -ErrorAction SilentlyContinue) { Write-Host "scoop: present" } else { Write-Host "scoop: not installed" }
if (Get-Command python -ErrorAction SilentlyContinue) { Write-Host "python: $(python --version 2>&1)" } else { Write-Host "python: not on PATH" }

$queries = @(
  @{ Tool = "greaseweazle (gw)";  Q = @("greaseweazle") },
  @{ Tool = "cpmtools";           Q = @("cpmtools") },
  @{ Tool = "mtools";             Q = @("mtools") },
  @{ Tool = "VICE (c1541)";       Q = @("VICE", "vice-emu") },
  @{ Tool = "HxC (hxcfe)";        Q = @("HxC") },
  @{ Tool = "Python 3";           Q = @("Python.Python.3") },
  @{ Tool = "Java (Temurin JRE)"; Q = @("EclipseAdoptium.Temurin.21.JRE") },
  @{ Tool = "Git";                Q = @("Git.Git") }
)

foreach ($item in $queries) {
  Write-Host "`n==== $($item.Tool) ===="
  foreach ($q in $item.Q) {
    Write-Host "-- winget search $q --"
    try { winget search $q --source winget --accept-source-agreements 2>&1 | Select-Object -First 8 }
    catch { Write-Host "  (winget error)" }
    if (Get-Command scoop -ErrorAction SilentlyContinue) {
      Write-Host "-- scoop search $q --"
      try { scoop search $q 2>&1 | Select-Object -First 8 } catch { Write-Host "  (scoop error)" }
    }
  }
}
Write-Host "`n==== DONE ===="
