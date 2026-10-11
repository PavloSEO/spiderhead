#!/bin/sh
# Linux packaging contract. It is intentionally not accepted from macOS.
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
export PYINSTALLER_CONFIG_DIR="$project_dir/.build/pyinstaller-cache"
core_source=${1:?"first argument must be a clean SEOHEAD Tools checkout"}
output=${2:?"second argument must be a new output directory"}
python_bin=${PYTHON_BIN:-"$project_dir/.venv/bin/python"}

[ "$(uname -s)" = "Linux" ] || { echo "build_linux.sh must run on Linux" >&2; exit 69; }
[ ! -e "$output" ] || { echo "Refusing to replace an existing bundle: $output" >&2; exit 73; }
mkdir -p "$project_dir/.build/scratch"
scratch=$(mktemp -d "$project_dir/.build/scratch/packaging.XXXXXX")
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
"$python_bin" "$project_dir/scripts/bundle_manifest.py" create --core-source "$core_source" \
    --desktop-source "$project_dir" --platform linux --output "$scratch/core-manifest.json" --verify-imports

"$python_bin" -m PyInstaller --clean --noconfirm --onedir --console --name seohead \
    --paths "$core_source" --collect-all seohead --distpath "$scratch/core-dist" \
    --workpath "$scratch/core-work" --specpath "$scratch" "$core_source/seohead/cli.py"
"$python_bin" -m PyInstaller --clean --noconfirm --onedir --console --name seohead-desktop-agent \
    --paths "$project_dir/src" \
    --collect-data seohead_desktop --collect-data mcp --distpath "$scratch/agent-dist" \
    --workpath "$scratch/agent-work" --specpath "$scratch" "$project_dir/scripts/control_entrypoint.py"
"$python_bin" -m PyInstaller --clean --noconfirm --onedir --windowed --name "SpiderHead" \
    --paths "$project_dir/src" \
    --collect-data seohead_desktop \
    --distpath "$scratch/app-dist" --workpath "$scratch/app-work" --specpath "$scratch" \
    "$project_dir/scripts/entrypoint.py"

bundle="$scratch/app-dist/SpiderHead"
resources="$bundle/resources"
mkdir -p "$resources/core" "$resources/agent"
cp -R "$scratch/core-dist/seohead" "$resources/core/seohead"
cp -R "$scratch/agent-dist/seohead-desktop-agent" "$resources/agent/seohead-desktop-agent"
cp "$project_dir/src/seohead_desktop/assets/app/seohead.svg" "$bundle/seohead.svg"
mkdir -p "$resources/licenses"
cp "$project_dir/LICENSE" "$resources/licenses/SpiderHead-Desktop-GPL-3.0-or-later.txt"
cp "$project_dir/THIRD_PARTY_NOTICES.md" "$resources/licenses/THIRD_PARTY_NOTICES.md"
cp "$core_source/LICENSE" "$resources/licenses/SEOHEAD-Tools-MIT.txt"
cp "$core_source/THIRD_PARTY_NOTICES.md" "$resources/licenses/SEOHEAD-Tools-THIRD_PARTY_NOTICES.md"
cp "$project_dir/src/seohead_desktop/assets/fonts/OFL.txt" "$resources/licenses/Roboto-OFL-1.1.txt"
cp "$project_dir/src/seohead_desktop/assets/fonts/OFL-RobotoMono.txt" "$resources/licenses/RobotoMono-OFL-1.1.txt"
cp "$project_dir/src/seohead_desktop/assets/fonts/LICENSE-MaterialSymbols.txt" "$resources/licenses/MaterialSymbols-Apache-2.0.txt"
cp "$project_dir/src/seohead_desktop/assets/icons/LICENSE.txt" "$resources/licenses/Material-Design-Icons-Apache-2.0.txt"
cp "$project_dir/src/seohead_desktop/assets/asset-manifest.json" "$resources/licenses/desktop-assets-manifest.json"
"$python_bin" "$project_dir/scripts/copy_runtime_notices.py" --output "$resources/licenses"
"$python_bin" "$project_dir/scripts/bundle_manifest.py" verify-sources \
    --manifest "$scratch/core-manifest.json" --core-source "$core_source" --desktop-source "$project_dir"
cp "$scratch/core-manifest.json" "$resources/core-manifest.json"
"$python_bin" "$project_dir/scripts/bundle_manifest.py" finalize --manifest "$resources/core-manifest.json" --cli "$resources/core/seohead/seohead" --agent "$resources/agent/seohead-desktop-agent/seohead-desktop-agent"
"$python_bin" "$project_dir/scripts/smoke_bundle.py" --bundle "$bundle"
mkdir -p "$(dirname -- "$output")"
mv "$bundle" "$output"
