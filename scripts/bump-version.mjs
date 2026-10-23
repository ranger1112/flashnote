#!/usr/bin/env node
/**
 * 统一更新发版需要的五处版本号：
 *   package.json / package-lock.json / src-tauri/Cargo.toml / src-tauri/Cargo.lock
 *   / src-tauri/tauri.conf.json
 *
 * 用法：
 *   node scripts/bump-version.mjs 0.4.0               # 写入新版本
 *   node scripts/bump-version.mjs --check             # 只校验是否一致（CI 用）
 *   node scripts/bump-version.mjs 0.4.0 --dry-run     # 预览，不写文件
 *   npm run bump -- 0.4.0 --dry-run                  # 通过 npm 时必须加 `--`
 *
 * 不会改 CHANGELOG、README 里的安装包文件名或 git tag，写完会提示剩余步骤。
 */
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const FILES = {
  packageJson: path.join(ROOT, "package.json"),
  lock: path.join(ROOT, "package-lock.json"),
  cargoToml: path.join(ROOT, "src-tauri", "Cargo.toml"),
  cargoLock: path.join(ROOT, "src-tauri", "Cargo.lock"),
  tauriConf: path.join(ROOT, "src-tauri", "tauri.conf.json"),
};

const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;
const KNOWN_FLAGS = new Set(["--check", "--dry-run", "--help", "-h"]);

const args = process.argv.slice(2);
const flags = new Set(args.filter((arg) => arg.startsWith("-")));
const versionArg = args.find((arg) => !arg.startsWith("-"));
const checkOnly = flags.has("--check");
const dryRun = flags.has("--dry-run");

function fail(message) {
  console.error(`错误：${message}`);
  process.exit(1);
}

function readJson(file) {
  try {
    return JSON.parse(readFileSync(file, "utf8"));
  } catch (error) {
    fail(`无法解析 ${path.relative(ROOT, file)}：${error.message}`);
  }
}

function writeJson(file, value) {
  if (dryRun) return;
  writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function packageVersion() {
  return readJson(FILES.packageJson).version;
}

function lockVersion() {
  const lock = readJson(FILES.lock);
  const hasPackages = Boolean(lock.packages) && typeof lock.packages === "object";
  return {
    root: lock.version,
    rootPackage: hasPackages ? lock.packages[""]?.version : undefined,
    hasPackages,
  };
}

function cargoVersion() {
  const text = readFileSync(FILES.cargoToml, "utf8");
  let inPackage = false;
  for (const line of text.split(/\r?\n/)) {
    if (/^\s*\[/.test(line)) {
      inPackage = line.trim() === "[package]";
      continue;
    }
    if (!inPackage) continue;
    const match = /^\s*version\s*=\s*"([^"]+)"/.exec(line);
    if (match) return match[1];
  }
  fail("src-tauri/Cargo.toml 的 [package] 段里没有 version");
}

function tauriVersion() {
  return readJson(FILES.tauriConf).version;
}

function eolOf(text) {
  return text.includes("\r\n") ? "\r\n" : "\n";
}

function updateCargoToml(version) {
  const text = readFileSync(FILES.cargoToml, "utf8");
  const lines = text.split(/\r?\n/);
  let inPackage = false;
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (/^\s*\[/.test(line)) {
      inPackage = line.trim() === "[package]";
      continue;
    }
    if (inPackage && /^\s*version\s*=\s*"/.test(line)) {
      lines[index] = line.replace(/"[^"]*"/, `"${version}"`);
      if (!dryRun) writeFileSync(FILES.cargoToml, lines.join(eolOf(text)));
      return;
    }
  }
  fail("src-tauri/Cargo.toml 的 [package] 段里没有 version");
}

/** 定位 Cargo.lock 里根包 flashnote 的 version 行；跳过同名依赖。 */
function cargoLockVersionLine(lines) {
  let inPackage = false;
  let isRoot = false;
  for (let index = 0; index < lines.length; index += 1) {
    const trimmed = lines[index].trim();
    if (trimmed === "[[package]]") {
      inPackage = true;
      isRoot = false;
      continue;
    }
    if (!inPackage) continue;
    if (trimmed === "" || trimmed.startsWith("[")) {
      inPackage = false;
      isRoot = false;
      continue;
    }
    if (trimmed === 'name = "flashnote"') {
      isRoot = true;
      continue;
    }
    if (isRoot && /^version\s*=\s*"/.test(trimmed)) {
      return index;
    }
  }
  return -1;
}

function cargoLockVersion() {
  const lines = readFileSync(FILES.cargoLock, "utf8").split(/\r?\n/);
  const index = cargoLockVersionLine(lines);
  if (index < 0) fail("src-tauri/Cargo.lock 中找不到 flashnote 包版本");
  const match = /^\s*version\s*=\s*"([^"]+)"/.exec(lines[index]);
  if (!match) fail("src-tauri/Cargo.lock 的 flashnote 包没有 version");
  return match[1];
}

function updateCargoLock(version) {
  const text = readFileSync(FILES.cargoLock, "utf8");
  const lines = text.split(/\r?\n/);
  const index = cargoLockVersionLine(lines);
  if (index < 0) fail("src-tauri/Cargo.lock 中找不到 flashnote 包版本");
  lines[index] = lines[index].replace(/"[^"]*"/, `"${version}"`);
  if (!dryRun) writeFileSync(FILES.cargoLock, lines.join(eolOf(text)));
}

function currentVersions() {
  const lock = lockVersion();
  const versions = [
    ["package.json", packageVersion()],
    ["package-lock.json", lock.root],
  ];
  // lockfile v1 没有 packages 字段，v2+ 才有根包版本
  if (lock.hasPackages) {
    versions.push(['package-lock.json packages[""]', lock.rootPackage]);
  }
  versions.push(
    ["src-tauri/Cargo.toml", cargoVersion()],
    ["src-tauri/Cargo.lock", cargoLockVersion()],
    ["src-tauri/tauri.conf.json", tauriVersion()],
  );
  return versions;
}

function check(expectation) {
  const versions = currentVersions();
  const target = expectation ?? versions[0][1];
  const mismatched = versions.filter(([, value]) => value !== target);

  for (const [name, value] of versions) {
    const mark = value === target ? "✓" : "✗";
    console.log(`${mark} ${name}: ${value}`);
  }

  if (mismatched.length > 0) {
    console.error(`\n版本号不一致，请统一为 ${target}。可运行：npm run bump ${target}`);
    process.exit(1);
  }
  console.log(`\n版本号一致：${target}`);
}

const unknownFlags = [...flags].filter((flag) => !KNOWN_FLAGS.has(flag));
if (unknownFlags.length > 0) {
  fail(`未知参数：${unknownFlags.join(" ")}（--help 查看用法）`);
}

if (flags.has("--help") || flags.has("-h")) {
  console.log(`用法：
  node scripts/bump-version.mjs <version>            写入新版本
  node scripts/bump-version.mjs <version> --dry-run  只预览
  node scripts/bump-version.mjs --check              校验一致性
  npm run bump -- <version> [--dry-run]              通过 npm 调用
版本格式：1.2.3 或 1.2.3-beta.1`);
  process.exit(0);
}

if (checkOnly) {
  if (versionArg && !SEMVER.test(versionArg)) fail(`版本格式无效：${versionArg}`);
  check(versionArg);
  process.exit(0);
}

if (!versionArg) fail("缺少版本号，例如：npm run bump 0.4.0");
if (!SEMVER.test(versionArg)) fail(`版本格式无效：${versionArg}`);

const packageJson = readJson(FILES.packageJson);
packageJson.version = versionArg;
writeJson(FILES.packageJson, packageJson);

const lock = readJson(FILES.lock);
lock.version = versionArg;
if (lock.packages && typeof lock.packages === "object") {
  lock.packages[""] ??= {};
  lock.packages[""].version = versionArg;
}
writeJson(FILES.lock, lock);

updateCargoToml(versionArg);
updateCargoLock(versionArg);

const tauriConf = readJson(FILES.tauriConf);
tauriConf.version = versionArg;
writeJson(FILES.tauriConf, tauriConf);

console.log(`${dryRun ? "（预览）" : ""}已把五处版本号更新为 ${versionArg}：`);
console.log("  package.json");
console.log("  package-lock.json");
console.log("  src-tauri/Cargo.toml");
console.log("  src-tauri/Cargo.lock");
console.log("  src-tauri/tauri.conf.json");
console.log("\n还需要手动完成：");
console.log("  1. CHANGELOG.md 把 [Unreleased] 落到新版本标题，并更新 compare 链接");
console.log("  2. README.md / README.en.md 里的安装包文件名版本号");
console.log(`  3. 提交后打 tag：git tag v${versionArg} && git push origin v${versionArg}`);
