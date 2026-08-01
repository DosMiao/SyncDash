#!/usr/bin/env node
// The single entry point for Rust → TypeScript type generation.
//
//   node Script/gen-types.mjs
//
// Two steps:
//   1. `cargo test export_bindings` — ts-rs exports from inside tests (see the
//      `#[ts(export, export_to = ...)]` on each type); the export directory is pinned at the
//      workspace root by TS_RS_EXPORT_DIR in `.cargo/config.toml`.
//   2. Sanitize the output — ts-rs copies Rust doc comments verbatim into JSDoc, and a lot of the
//      docs here describe FFS filter syntax (`*/big_temp/`, `*/*.log`). That literal `*/`
//      **terminates the JSDoc block early**, producing syntactically invalid .ts. This escapes
//      in-block `*/` to `*\/` (JSDoc still renders it as `*/`, but it no longer ends the block),
//      and removes trailing whitespace emitted around multiline struct fields.
//
// Never hand-edit anything under typescript/core/types/generated/.

import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const genDir = join(repoRoot, "typescript", "core", "types", "generated");

function run(args) {
  execFileSync("cargo", args, { cwd: repoRoot, stdio: "inherit" });
}

// Escape star-slash sequences inside a doc block; the real terminator line (those two characters alone) is left as is.
// This function's own comment is deliberately a line comment — as a block comment it would hit the very bug it fixes.
function sanitize(text) {
  const out = [];
  let inDoc = false;
  for (const line of text.split("\n")) {
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

console.log("[1/2] cargo test export_bindings …");
run(["test", "--workspace", "--quiet", "export_bindings"]);

console.log("[2/2] sanitizing generated output …");
let touched = 0;
for (const name of readdirSync(genDir)) {
  if (!name.endsWith(".ts")) continue;
  const p = join(genDir, name);
  const before = readFileSync(p, "utf8");
  const after = sanitize(before);
  if (after !== before) {
    writeFileSync(p, after);
    touched++;
    console.log(`      fixed ${name}`);
  }
}
console.log(`done: ${readdirSync(genDir).filter((f) => f.endsWith(".ts")).length} types, ${touched} needed sanitizing.`);
