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

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");

function hostTriple() {
  const out = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const match = out.match(/^host:\s*(\S+)$/m);
  if (!match) throw new Error("could not read the host target triple from `rustc -vV`");
  return match[1];
}

const triple = hostTriple();
const exeSuffix = process.platform === "win32" ? ".exe" : "";

execFileSync("cargo", ["build", "--release", "-p", "parle-hook", "--bin", "parle-hook"], {
  cwd: repoRoot,
  stdio: "inherit",
});

const from = join(repoRoot, "target", "release", `parle-hook${exeSuffix}`);
const outDir = join(repoRoot, "src-tauri", "binaries");
const to = join(outDir, `parle-hook-${triple}${exeSuffix}`);

mkdirSync(outDir, { recursive: true });
copyFileSync(from, to);
console.log(`staged sidecar ${to}`);
