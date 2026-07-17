#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../.."
ROOT_DIR="$(pwd)"
WORK_DIR="$(mktemp -d)"
TMP_WORK_DIR="$WORK_DIR/tmp"
mkdir -p "$TMP_WORK_DIR"
export TMPDIR="$TMP_WORK_DIR"
trap 'rm -rf "$WORK_DIR"' EXIT

section() {
   echo ""
   echo "=== $1 ==="
}

smoke_cli() {
   local omp_bin="$1"
   local runtime_dir
   runtime_dir="$(mktemp -d "$WORK_DIR/compiled-runtime.XXXXXX")"
   XDG_DATA_HOME="$runtime_dir/xdg" HOME="$runtime_dir/home" "$omp_bin" --version
   XDG_DATA_HOME="$runtime_dir/xdg" HOME="$runtime_dir/home" "$omp_bin" --help >/dev/null
   XDG_DATA_HOME="$runtime_dir/xdg" HOME="$runtime_dir/home" "$omp_bin" stats --summary >/dev/null
   # Spawns bundled workers and serves the stats dashboard once. Regression
   # probe for #1011/#1027 worker loading and for npm/compiled distributions
   # missing the dashboard assets that `stats --summary` never touches.
   XDG_DATA_HOME="$runtime_dir/xdg" HOME="$runtime_dir/home" "$omp_bin" --smoke-test
}

find_tarball() {
   local pattern="$1"
   local matches=()
   shopt -s nullglob
   matches=("$pattern")
   shopt -u nullglob

   if [ "${#matches[@]}" -ne 1 ]; then
      echo "Expected exactly one tarball matching: $pattern"
      exit 1
   fi

   echo "${matches[0]}"
}

section "Binary install smoke"
bun --cwd=packages/natives run build
bun --cwd=packages/coding-agent run build

BINARY_DIR="$WORK_DIR/binary-bin"
mkdir -p "$BINARY_DIR"
cp packages/coding-agent/dist/omp "$BINARY_DIR/omp"
smoke_cli "$BINARY_DIR/omp"

section "Source install smoke"
SOURCE_BUN_HOME="$WORK_DIR/bun-source"
(
   export BUN_INSTALL="$SOURCE_BUN_HOME"
   export PATH="$BUN_INSTALL/bin:$PATH"
   bun --cwd="$ROOT_DIR/packages/coding-agent" link
   smoke_cli "$BUN_INSTALL/bin/omp"
)

section "Tarball install smoke"
TARBALL_DIR="$WORK_DIR/tarballs"
mkdir -p "$TARBALL_DIR"
host_tag="$(bun -e "process.stdout.write(\`\${process.platform}-\${process.arch}\`)")"

# Native addon split: the published core ships only the loader (no `.node`); the
# prebuilt binary lives in a per-platform leaf package pulled in as an optional
# dependency. Reproduce that exact published topology so this smoke proves the
# installed core resolves its addon through the leaf, not a bundled binary.

# 1. Generate + pack the host-platform leaf (carries the built `.node`).
bun --cwd=packages/natives run gen:npm --tag "$host_tag" >/dev/null
(
   cd "$ROOT_DIR/packages/natives/npm/$host_tag"
   bun pm pack --destination "$TARBALL_DIR" --quiet >/dev/null
)

# 2. Pack the core with its *published* manifest: the same rewrite release uses
#    drops `.node` from `files` and adds the leaf `optionalDependencies`. Always
#    restore the working-tree manifest so local runs aren't left mutated.
natives_pkg_backup="$WORK_DIR/natives-package.json.orig"
cp "$ROOT_DIR/packages/natives/package.json" "$natives_pkg_backup"
core_rc=0
{
   bun -e 'import { prepareNativeCorePackage } from "./scripts/ci-release-publish.ts"; await prepareNativeCorePackage("packages/natives", true);' &&
      (cd "$ROOT_DIR/packages/natives" && bun pm pack --destination "$TARBALL_DIR" --quiet >/dev/null)
} || core_rc=$?
cp "$natives_pkg_backup" "$ROOT_DIR/packages/natives/package.json"
[ "$core_rc" -eq 0 ] || exit "$core_rc"

# 3. Pack the remaining workspace packages (natives core and coding-agent
#    handled separately). `collab-web` is private but still packed here so its
#    prepack build and tarball file list stay release-safe.
for pkg in utils wire hashline catalog ai mnemopi snapcompact agent tui stats collab-web; do
   (
      cd "$ROOT_DIR/packages/$pkg"
      bun pm pack --destination "$TARBALL_DIR" --quiet >/dev/null
   )
done

# 4. Pack the coding agent with its published manifest and declarations. Release
# emits dist/types, rewrites every source types export to that tree, and swaps
# bin.omp to dist/cli.js. Back up both mutable paths so this smoke leaves a
# developer worktree unchanged.
agent_pkg_backup="$WORK_DIR/coding-agent-package.json.orig"
cp "$ROOT_DIR/packages/coding-agent/package.json" "$agent_pkg_backup"
agent_types_backup="$WORK_DIR/coding-agent-dist-types.orig"
agent_had_types=0
if [ -d "$ROOT_DIR/packages/coding-agent/dist/types" ]; then
   cp -a "$ROOT_DIR/packages/coding-agent/dist/types" "$agent_types_backup"
   agent_had_types=1
fi
publish_state_active=1
restore_publish_state() {
   if [ "$publish_state_active" -eq 0 ]; then return; fi
   rm -rf "$ROOT_DIR/packages/coding-agent/dist/types"
   if [ "$agent_had_types" -eq 1 ] && [ -d "$agent_types_backup" ]; then
      mv "$agent_types_backup" "$ROOT_DIR/packages/coding-agent/dist/types"
   fi
   if [ -f "$agent_pkg_backup" ]; then
      cp "$agent_pkg_backup" "$ROOT_DIR/packages/coding-agent/package.json"
   fi
   publish_state_active=0
}
cleanup() {
   local rc=$?
   trap - EXIT
   restore_publish_state
   rm -rf "$WORK_DIR"
   exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
agent_rc=0
{
   bun -e 'import { packages, preparePackageForPublish } from "./scripts/ci-release-publish.ts"; const pkg = packages.find(candidate => candidate.dir === "packages/coding-agent"); if (!pkg) throw new Error("Coding-agent publish package not found"); await preparePackageForPublish(pkg);' &&
      (cd "$ROOT_DIR/packages/coding-agent" && bun pm pack --destination "$TARBALL_DIR" --quiet >/dev/null)
} || agent_rc=$?
restore_publish_state
[ "$agent_rc" -eq 0 ] || exit "$agent_rc"

utils_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-pi-utils-*.tgz)"
wire_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-pi-wire-*.tgz)"
natives_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-pi-natives-[0-9]*.tgz)"
natives_leaf_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-pi-natives-"$host_tag"-*.tgz)"
hashline_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-hashline-*.tgz)"
catalog_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-pi-catalog-*.tgz)"
ai_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-pi-ai-*.tgz)"
mnemopi_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-pi-mnemopi-*.tgz)"
snapcompact_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-snapcompact-*.tgz)"
agent_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-pi-agent-core-*.tgz)"
tui_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-pi-tui-*.tgz)"
stats_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-omp-stats-*.tgz)"
coding_agent_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-pi-coding-agent-*.tgz)"
collab_web_tgz="$(find_tarball "$TARBALL_DIR"/oh-my-pi-collab-web-*.tgz)"

TARBALL_APP_DIR="$WORK_DIR/tarball-install"
mkdir -p "$TARBALL_APP_DIR"
(
   cd "$TARBALL_APP_DIR"
   bun init -y >/dev/null

   # Write overrides so bun resolves inter-package deps from tarballs, not the registry
   # (the version under test has not necessarily been published yet).
   node -e "
		const pkg = JSON.parse(require('fs').readFileSync('package.json', 'utf8'));
		pkg.overrides = {
			'@oh-my-pi/pi-utils': '$utils_tgz',
			'@oh-my-pi/pi-wire': '$wire_tgz',
			'@oh-my-pi/pi-natives': '$natives_tgz',
			'@oh-my-pi/pi-natives-$host_tag': '$natives_leaf_tgz',
			'@oh-my-pi/hashline': '$hashline_tgz',
			'@oh-my-pi/pi-ai': '$ai_tgz',
			'@oh-my-pi/pi-catalog': '$catalog_tgz',
			'@oh-my-pi/pi-mnemopi': '$mnemopi_tgz',
			'@oh-my-pi/snapcompact': '$snapcompact_tgz',
			'@oh-my-pi/pi-agent-core': '$agent_tgz',
			'@oh-my-pi/pi-tui': '$tui_tgz',
			'@oh-my-pi/omp-stats': '$stats_tgz',
			'@oh-my-pi/pi-coding-agent': '$coding_agent_tgz',
			'@oh-my-pi/collab-web': '$collab_web_tgz'
		};
		require('fs').writeFileSync('package.json', JSON.stringify(pkg, null, 2));
	"

   bun add "$utils_tgz" "$wire_tgz" "$natives_tgz" "$hashline_tgz" "$catalog_tgz" "$ai_tgz" "$mnemopi_tgz" "$snapcompact_tgz" "$agent_tgz" "$tui_tgz" "$stats_tgz" "$coding_agent_tgz" "$collab_web_tgz"
   # The platform leaf must arrive through the core's optionalDependencies +
   # override, not as a direct dependency — assert it landed before smoking so a
   # resolution regression is distinguishable from a runtime loader bug.
   leaf_dir="node_modules/@oh-my-pi/pi-natives-$host_tag"
   [ -d "$leaf_dir" ] || {
      echo "Platform leaf package not installed: $leaf_dir"
      exit 1
   }
   wire_proto="$(bun -e 'import { COLLAB_PROTO } from "@oh-my-pi/pi-wire"; process.stdout.write(String(COLLAB_PROTO));')"
   [ "$wire_proto" = "3" ] || {
      echo "Unexpected @oh-my-pi/pi-wire COLLAB_PROTO: $wire_proto"
      exit 1
   }
   [ -f "node_modules/@oh-my-pi/collab-web/dist/index.html" ] || {
      echo "Collab web tarball did not install built dist/index.html"
      exit 1
   }
   sdk_types="node_modules/@oh-my-pi/pi-coding-agent/dist/types/sdk.d.ts"
   [ -f "$sdk_types" ] || {
      echo "Published SDK declaration missing: $sdk_types"
      exit 1
   }
   sdk_types_export="$(bun -e 'const manifest = await Bun.file("node_modules/@oh-my-pi/pi-coding-agent/package.json").json(); process.stdout.write(manifest.exports?.["./sdk"]?.types ?? "missing");')"
   [ "$sdk_types_export" = "./dist/types/sdk.d.ts" ] || {
      echo "Published SDK types export is incorrect: $sdk_types_export"
      exit 1
   }
   sdk_exports="$(bun -e 'import { AgentSession, AuthStorage, ModelRegistry, SessionManager, createAgentSession } from "@oh-my-pi/pi-coding-agent/sdk"; process.stdout.write([AgentSession, AuthStorage, ModelRegistry, SessionManager, createAgentSession].every(value => typeof value === "function") ? "ok" : "invalid");')"
   [ "$sdk_exports" = "ok" ] || {
      echo "Published SDK entrypoint exports are incomplete: $sdk_exports"
      exit 1
   }
   sdk_identity="$(bun -e 'import { createAgentSession as rootCreateAgentSession } from "@oh-my-pi/pi-coding-agent"; import { createAgentSession as sdkCreateAgentSession } from "@oh-my-pi/pi-coding-agent/sdk"; process.stdout.write(rootCreateAgentSession === sdkCreateAgentSession ? "same" : "different");')"
   [ "$sdk_identity" = "same" ] || {
      echo "Root and SDK entrypoints resolve different createAgentSession implementations"
      exit 1
   }
   smoke_cli ./node_modules/.bin/omp
)

echo ""
echo "All install method smoke tests passed"
