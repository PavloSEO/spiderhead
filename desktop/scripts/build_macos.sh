#!/bin/sh
# Build a self-contained SpiderHead app plus the exact local seohead core revision.
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
export PYINSTALLER_CONFIG_DIR="$project_dir/.build/pyinstaller-cache"
core_source=
output=
python_bin=${PYTHON_BIN:-"$project_dir/.venv/bin/python"}
incremental=false

usage() {
    echo "Usage: $0 --core-source PATH --output PATH [--python PATH] [--incremental]" >&2
    exit 64
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --core-source) core_source=${2:-}; shift 2 ;;
        --output) output=${2:-}; shift 2 ;;
        --python) python_bin=${2:-}; shift 2 ;;
        --incremental) incremental=true; shift ;;
        *) usage ;;
    esac
done

[ -n "$core_source" ] && [ -n "$output" ] || usage
[ -x "$python_bin" ] || { echo "Python build environment is not executable: $python_bin" >&2; exit 66; }
[ -f "$core_source/pyproject.toml" ] || { echo "Core source is not a toolkit checkout: $core_source" >&2; exit 66; }
[ ! -e "$output" ] || { echo "Refusing to replace an existing bundle: $output" >&2; exit 73; }
"$python_bin" -c 'import PyInstaller, PyQt5' >/dev/null

case "$(uname -s)" in
    Darwin) ;;
    *) echo "build_macos.sh must run on macOS" >&2; exit 69 ;;
esac

mkdir -p "$project_dir/.build/scratch"
scratch=$(mktemp -d "$project_dir/.build/scratch/packaging.XXXXXX")
cache_lock=
trap 'rm -rf "$scratch"; if [ -n "$cache_lock" ]; then rmdir "$cache_lock"; fi' EXIT HUP INT TERM

preview_flag=
[ "$incremental" = false ] || preview_flag=--preview
"$python_bin" "$project_dir/scripts/bundle_manifest.py" create \
    --core-source "$core_source" --desktop-source "$project_dir" --platform macos \
    --output "$scratch/core-manifest.json" --verify-imports $preview_flag

core_build="$scratch/core"
agent_build="$scratch/agent"
app_build="$scratch/app"
clean_flag=--clean
if [ "$incremental" = true ]; then
    cache="$project_dir/.build/macos-preview-cache"
    mkdir -p "$cache"
    mkdir "$cache/active.lock" || { echo "A cached build is already active: $cache" >&2; exit 75; }
    cache_lock="$cache/active.lock"
    core_key=$("$python_bin" "$project_dir/scripts/bundle_manifest.py" cache-key --manifest "$scratch/core-manifest.json" --component core)
    app_key=$("$python_bin" "$project_dir/scripts/bundle_manifest.py" cache-key --manifest "$scratch/core-manifest.json" --component app)
    core_build="$cache/core-$core_key"
    agent_build="$cache/agent-$app_key"
    app_build="$cache/app-$app_key"
    clean_flag=
fi
mkdir -p "$core_build" "$agent_build" "$app_build"

"$python_bin" -m PyInstaller \
    $clean_flag --noconfirm --onedir --console --name seohead \
    --paths "$core_source" --collect-all seohead \
    --distpath "$core_build/dist" --workpath "$core_build/work" --specpath "$core_build" \
    "$core_source/seohead/__main__.py"

"$python_bin" -m PyInstaller \
    $clean_flag --noconfirm --onedir --console --name seohead-desktop-agent \
    --paths "$project_dir/src" \
    --collect-submodules seohead_desktop --collect-data seohead_desktop --collect-data mcp \
    --distpath "$agent_build/dist" --workpath "$agent_build/work" --specpath "$agent_build" \
    "$project_dir/scripts/control_entrypoint.py"

"$python_bin" -m PyInstaller \
    $clean_flag --noconfirm --onedir --windowed --name "SpiderHead" \
    --paths "$project_dir/src" \
    --osx-bundle-identifier tech.seohead.desktop \
    --icon "$project_dir/src/seohead_desktop/assets/app/seohead.icns" \
    --collect-submodules seohead_desktop --collect-data seohead_desktop --collect-submodules seohead_desktop \
    --distpath "$app_build/dist" --workpath "$app_build/work" --specpath "$app_build" \
    "$project_dir/scripts/entrypoint.py"

bundle="$scratch/SpiderHead.app"
ditto "$app_build/dist/SpiderHead.app" "$bundle"
resources="$bundle/Contents/Resources"
mkdir -p "$resources/core"
# PyInstaller maps --add-data into Contents/Frameworks for a macOS .app.
# The desktop resolver deliberately owns Contents/Resources, so copy the
# complete frozen core there after BUNDLE without changing its internal layout.
ditto "$core_build/dist/seohead" "$resources/core/seohead"
mkdir -p "$resources/agent"
ditto "$agent_build/dist/seohead-desktop-agent" "$resources/agent/seohead-desktop-agent"
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
"$python_bin" "$project_dir/scripts/bundle_manifest.py" finalize \
    --manifest "$resources/core-manifest.json" --cli "$resources/core/seohead/seohead" \
    --agent "$resources/agent/seohead-desktop-agent/seohead-desktop-agent"
"$python_bin" "$project_dir/scripts/smoke_bundle.py" --bundle "$bundle"

# Seal the final resource layout after Core, agent and manifest are added.
# Re-sign only the outer bundle; nested runtime inventories must remain intact.
codesign --force --sign - "$bundle"
codesign --verify --deep --strict "$bundle"

mkdir -p "$(dirname -- "$output")"
mv "$bundle" "$output"
echo "Created $output"
