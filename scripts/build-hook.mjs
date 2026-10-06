// Builds the parle-hook helper and stages it as a Tauri sidecar.
//
// The helper is the process that owns the WH_KEYBOARD_LL hook (see
// crates/parle-hook). `tauri build` only compiles the `parle` package, so
// the helper has to be built separately and dropped where the bundler expects
// external binaries: src-tauri/binaries/parle-hook-<target-triple>[.exe].
// Tauri installs it next to the app binary with the triple stripped, which is
// exactly where platform::windows::helper_path() looks for it.
//
// Runs from tauri.conf.json's beforeBuildCommand. On non-Windows hosts the
// helper compiles to an immediate-exit stub; it is still staged so that
// externalBin resolves and macOS bundling is unaffected.
//
// The target comes from TAURI_ENV_TARGET_TRIPLE, which the Tauri CLI sets from
// `--target` (the release workflow builds `universal-apple-darwin`). A
// universal build needs THREE staged copies: tauri-build checks for the
// per-arch name while compiling each slice, and the bundler then looks for the
// universal name, which has to be a real fat binary made with lipo.

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const outDir = join(repoRoot, "src-tauri", "binaries");
const exeSuffix = process.platform === "win32" ? ".exe" : "";
const UNIVERSAL = "universal-apple-darwin";

function hostTriple() {
  const out = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const match = out.match(/^host:\s*(\S+)$/m);
  if (!match) throw new Error("could not read the host target triple from `rustc -vV`");
  return match[1];
}

// Builds the helper for one triple and returns the path of the built binary.
function buildFor(triple, host) {
  const args = ["build", "--release", "-p", "parle-hook", "--bin", "parle-hook"];
  // Only pass --target when cross-building: it moves the output directory, and
  // a host build should land exactly where it always has.
  if (triple !== host) args.push("--target", triple);
  execFileSync("cargo", args, { cwd: repoRoot, stdio: "inherit" });
  const dir = triple === host ? join("target", "release") : join("target", triple, "release");
  return join(repoRoot, dir, `parle-hook${exeSuffix}`);
}

function stage(from, triple) {
  const to = join(outDir, `parle-hook-${triple}${exeSuffix}`);
  copyFileSync(from, to);
  console.log(`staged sidecar ${to}`);
  return to;
}

const host = hostTriple();
const target = process.env.TAURI_ENV_TARGET_TRIPLE || host;
mkdirSync(outDir, { recursive: true });

if (target === UNIVERSAL) {
  const slices = ["aarch64-apple-darwin", "x86_64-apple-darwin"].map((t) => stage(buildFor(t, host), t));
  const fat = join(outDir, `parle-hook-${UNIVERSAL}`);
  execFileSync("lipo", ["-create", "-output", fat, ...slices], { stdio: "inherit" });
  console.log(`staged sidecar ${fat}`);
} else {
  stage(buildFor(target, host), target);
}
