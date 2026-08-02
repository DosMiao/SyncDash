#!/usr/bin/env node
// Generate Rust-owned TypeScript contracts, then sanitize ts-rs JSDoc and whitespace.
// Never edit Dev/typescript/core/types/generated/ by hand.

import { execFileSync } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const genDir = join(repoRoot, "Dev", "typescript", "core", "types", "generated");
const checkOnly = process.argv.includes("--check");
const scratch = checkOnly ? mkdtempSync(join(tmpdir(), "syncdash-types-")) : null;
const outputDir = scratch
  ? join(scratch, "Dev", "typescript", "core", "types", "generated")
  : genDir;

function run(args) {
  execFileSync("cargo", args, {
    cwd: repoRoot,
    stdio: "inherit",
    env: { ...process.env, TS_RS_EXPORT_DIR: join(scratch ?? repoRoot, "bindings") },
  });
}

// Escape star-slash sequences inside a doc block; the real terminator line (those two characters alone) is left as is.
// This function's own comment is deliberately a line comment — as a block comment it would hit the very bug it fixes.
function sanitize(text) {
  const out = [];
  let inDoc = false;
  for (const line of text.replace(/\r\n?/g, "\n").split("\n")) {
    const t = line.trim();
    if (!inDoc && t.startsWith("/**")) {
      inDoc = !t.endsWith("*/") || t === "/**";
      out.push(inDoc ? line : line.replace(/(?!^)\*\//g, "*\\/"));
      continue;
    }
    if (inDoc) {
      if (t === "*/") {
        inDoc = false;
        out.push(line);
        continue;
      }
      out.push(line.replaceAll("*/", "*\\/"));
      continue;
    }
    out.push(line);
  }
  return out.join("\n").replace(/[ \t]+$/gm, "");
}

function typeFiles(directory) {
  return readdirSync(directory).filter((name) => name.endsWith(".ts")).sort();
}

try {
  console.log("[1/2] exporting Rust wire types …");
  run(["test", "--workspace", "--quiet", "--lib", "--bins", "--features", "export-types", "export_bindings"]);

  console.log("[2/2] sanitizing generated output …");
  let touched = 0;
  for (const name of typeFiles(outputDir)) {
    const path = join(outputDir, name);
    const before = readFileSync(path, "utf8");
    const after = sanitize(before);
    if (after !== before) {
      writeFileSync(path, after);
      touched++;
      console.log(`      fixed ${name}`);
    }
  }

  const files = typeFiles(outputDir);
  if (checkOnly) {
    const committed = typeFiles(genDir);
    const stale = files.filter((name) => (
      !committed.includes(name)
      || readFileSync(join(outputDir, name), "utf8") !== readFileSync(join(genDir, name), "utf8")
    ));
    const removed = committed.filter((name) => !files.includes(name));
    if (stale.length > 0 || removed.length > 0) {
      throw new Error(`generated TypeScript bindings are stale: ${[...stale, ...removed].join(", ")}`);
    }
    console.log(`done: ${files.length} generated types are current.`);
  } else {
    console.log(`done: ${files.length} types, ${touched} needed sanitizing.`);
  }
} finally {
  if (scratch) rmSync(scratch, { recursive: true, force: true });
}
