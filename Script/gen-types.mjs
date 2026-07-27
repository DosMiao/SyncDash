#!/usr/bin/env node
// Rust → TypeScript 类型生成的唯一入口。
//
//   node Script/gen-types.mjs
//
// 两步：
//   1. `cargo test export_bindings` —— ts-rs 的导出发生在测试里（见各类型上的
//      `#[ts(export, export_to = ...)]`），导出目录由 `.cargo/config.toml` 的
//      TS_RS_EXPORT_DIR 钉在工作区根。
//   2. 净化产物 —— ts-rs 把 Rust 的文档注释原样搬进 JSDoc，而本仓大量文档在讲
//      FFS 过滤器语法（`*/big_temp/`、`*/*.log`）。那个字面的 `*/` 会**提前终止**
//      JSDoc 块，生成语法不合法的 .ts。这里把块内的 `*/` 转义成 `*\/`
//      （JSDoc 里照样显示成 `*/`，但不再终止块）。
//
// 不要手改 typescript/core/types/generated/ 下的任何文件。

import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const genDir = join(repoRoot, "typescript", "core", "types", "generated");

function run(args) {
  execFileSync("cargo", args, { cwd: repoRoot, stdio: "inherit" });
}

// 转义文档块内部的星-斜杠序列；真正的终止行（整行只有那两个字符）保持原样。
// 这个函数的注释本身刻意用行注释——写成块注释就会踩到它要修的那个坑。
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
  return out.join("\n");
}

console.log("[1/2] cargo test export_bindings …");
run(["test", "--workspace", "--quiet", "export_bindings"]);

console.log("[2/2] 净化生成产物 …");
let touched = 0;
for (const name of readdirSync(genDir)) {
  if (!name.endsWith(".ts")) continue;
  const p = join(genDir, name);
  const before = readFileSync(p, "utf8");
  const after = sanitize(before);
  if (after !== before) {
    writeFileSync(p, after);
    touched++;
    console.log(`      修正 ${name}`);
  }
}
console.log(`完成：${readdirSync(genDir).filter((f) => f.endsWith(".ts")).length} 个类型，${touched} 个需要净化。`);
