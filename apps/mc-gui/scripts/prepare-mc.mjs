// Builds `eli5` with Whisper and places it where Tauri expects its sidecar (binaries/eli5-<target triple>).
// Usage: node scripts/prepare-mc.mjs [--target <triple>]   (the host when left out)
import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..", "..", "..");
const host = execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(/^host: (.+)$/m)[1];
const asked = process.argv.indexOf("--target");
const target = asked === -1 ? "" : process.argv[asked + 1];
if (asked !== -1 && !target) throw new Error("--target needs a target triple");
const triple = target || host;
const exe = triple.includes("windows") ? ".exe" : "";

const env = { ...process.env };
const llvm = "C:\\Program Files\\LLVM\\bin";
// whisper-rs needs libclang to generate its bindings.
if (process.platform === "win32" && !env.LIBCLANG_PATH && existsSync(llvm)) env.LIBCLANG_PATH = llvm;

const args = ["build", "--release", "--locked", "-p", "mc-cli", "--features", "whisper"];
if (target) args.push("--target", target);
execFileSync("cargo", args, { cwd: repo, env, stdio: "inherit" });

const targetDir = env.CARGO_TARGET_DIR ? resolve(env.CARGO_TARGET_DIR) : join(repo, "target");
const built = join(targetDir, ...(target ? [target] : []), "release", `eli5${exe}`);
const sidecar = join(here, "..", "src-tauri", "binaries", `eli5-${triple}${exe}`);
mkdirSync(dirname(sidecar), { recursive: true });
copyFileSync(built, sidecar);
console.log(`sidecar: ${sidecar}`);
