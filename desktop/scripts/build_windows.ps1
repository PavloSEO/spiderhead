param(
    [Parameter(Mandatory = $true)][string]$CoreSource,
    [Parameter(Mandatory = $true)][string]$Output,
    [string]$Python = "$PSScriptRoot\..\.venv\Scripts\python.exe"
)

$ErrorActionPreference = "Stop"
function Invoke-Python {
    & $Python @args
    if ($LASTEXITCODE -ne 0) { throw "Python command failed with exit code $LASTEXITCODE" }
}
$ProjectRoot = (Resolve-Path "$PSScriptRoot\..").Path
if (Test-Path $Output) { throw "Refusing to replace an existing bundle: $Output" }
if (-not (Test-Path "$CoreSource\pyproject.toml")) { throw "Core source is not a toolkit checkout" }
$ScratchRoot = Join-Path $ProjectRoot ".build\scratch"
New-Item -ItemType Directory -Force -Path $ScratchRoot | Out-Null
$Scratch = Join-Path $ScratchRoot ("packaging-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $Scratch | Out-Null
$PreviousPyInstallerConfig = $env:PYINSTALLER_CONFIG_DIR
$env:PYINSTALLER_CONFIG_DIR = "$ProjectRoot\.build\pyinstaller-cache"
try {
    Invoke-Python "$ProjectRoot\scripts\bundle_manifest.py" create --core-source $CoreSource --desktop-source $ProjectRoot --platform windows --output "$Scratch\core-manifest.json" --verify-imports
    Invoke-Python -m PyInstaller --clean --noconfirm --onedir --console --name seohead --paths $CoreSource --collect-all seohead --distpath "$Scratch\core-dist" --workpath "$Scratch\core-work" --specpath $Scratch "$CoreSource\seohead\__main__.py"
    Invoke-Python -m PyInstaller --clean --noconfirm --onedir --console --name seohead-desktop-agent --paths "$ProjectRoot\src" --collect-submodules seohead_desktop --collect-data seohead_desktop --collect-data mcp --distpath "$Scratch\agent-dist" --workpath "$Scratch\agent-work" --specpath $Scratch "$ProjectRoot\scripts\control_entrypoint.py"
    Invoke-Python -m PyInstaller --clean --noconfirm --onedir --windowed --name "SpiderHead" --paths "$ProjectRoot\src" --icon "$ProjectRoot\src\seohead_desktop\assets\app\seohead.ico" --collect-submodules seohead_desktop --collect-data seohead_desktop --distpath "$Scratch\app-dist" --workpath "$Scratch\app-work" --specpath $Scratch "$ProjectRoot\scripts\entrypoint.py"
    $Bundle = "$Scratch\app-dist\SpiderHead"
    $Resources = "$Bundle\resources"
    New-Item -ItemType Directory -Force -Path "$Resources\agent" | Out-Null
    New-Item -ItemType Directory -Force -Path "$Resources\core" | Out-Null
    Copy-Item -Recurse "$Scratch\core-dist\seohead" "$Resources\core\seohead"
    Copy-Item -Recurse "$Scratch\agent-dist\seohead-desktop-agent" "$Resources\agent\seohead-desktop-agent"
    New-Item -ItemType Directory -Force -Path "$Resources\licenses" | Out-Null
    Copy-Item "$ProjectRoot\LICENSE" "$Resources\licenses\SpiderHead-Desktop-GPL-3.0-or-later.txt"
    Copy-Item "$ProjectRoot\THIRD_PARTY_NOTICES.md" "$Resources\licenses\THIRD_PARTY_NOTICES.md"
    Copy-Item "$CoreSource\LICENSE" "$Resources\licenses\SEOHEAD-Tools-MIT.txt"
    Copy-Item "$CoreSource\THIRD_PARTY_NOTICES.md" "$Resources\licenses\SEOHEAD-Tools-THIRD_PARTY_NOTICES.md"
    Copy-Item "$ProjectRoot\src\seohead_desktop\assets\fonts\OFL.txt" "$Resources\licenses\Roboto-OFL-1.1.txt"
    Copy-Item "$ProjectRoot\src\seohead_desktop\assets\fonts\OFL-RobotoMono.txt" "$Resources\licenses\RobotoMono-OFL-1.1.txt"
    Copy-Item "$ProjectRoot\src\seohead_desktop\assets\fonts\LICENSE-MaterialSymbols.txt" "$Resources\licenses\MaterialSymbols-Apache-2.0.txt"
    Copy-Item "$ProjectRoot\src\seohead_desktop\assets\icons\LICENSE.txt" "$Resources\licenses\Material-Design-Icons-Apache-2.0.txt"
    Copy-Item "$ProjectRoot\src\seohead_desktop\assets\asset-manifest.json" "$Resources\licenses\desktop-assets-manifest.json"
    Invoke-Python "$ProjectRoot\scripts\copy_runtime_notices.py" --output "$Resources\licenses"
    Invoke-Python "$ProjectRoot\scripts\bundle_manifest.py" verify-sources --manifest "$Scratch\core-manifest.json" --core-source $CoreSource --desktop-source $ProjectRoot
    Copy-Item "$Scratch\core-manifest.json" "$Resources\core-manifest.json"
    Invoke-Python "$ProjectRoot\scripts\bundle_manifest.py" finalize --manifest "$Resources\core-manifest.json" --cli "$Resources\core\seohead\seohead.exe" --agent "$Resources\agent\seohead-desktop-agent\seohead-desktop-agent.exe"
    Invoke-Python "$ProjectRoot\scripts\smoke_bundle.py" --bundle $Bundle
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Output) | Out-Null
    Move-Item $Bundle $Output
} finally {
    $env:PYINSTALLER_CONFIG_DIR = $PreviousPyInstallerConfig
    if (Test-Path $Scratch) { Remove-Item -Recurse -Force $Scratch }
}
