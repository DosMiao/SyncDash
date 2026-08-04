#!/usr/bin/env node
// Type-check the syncdash library and CLI for the foreign supported hosts, so platform-seam
// drift surfaces on the author's machine instead of when the other machine next boots.
//
// Outcomes per target, in order of authority:
//   PASS        cargo check completed — our code type-checks for that host.
//   FAIL        our workspace code failed to compile — exit 1, fix before handoff.
//   UNVERIFIED  a dependency's build script needs a cross C toolchain (ring and blake3 compile
//               native code) that this machine lacks — reported loudly, not silently skipped.
//               Installing LLVM clang usually unblocks the Linux target; the authoritative gate
//               for unverified targets remains a native `cargo check --workspace --all-targets`
//               on the other machine, as AGENTS.md requires for platform-seam changes.
//
// The desktop crate is deliberately out of scope: its Linux dependency stack (gtk/webkit sys
// crates) probes pkg-config at build time and cannot cross-check from a foreign host.

import { spawnSync } from "node:child_process";

const SUPPORTED = [
  { triple: "x86_64-pc-windows-msvc", os: "windows" },
  { triple: "aarch64-apple-darwin", os: "macos" },
  { triple: "x86_64-unknown-linux-gnu", os: "linux" },
];

function run(command, args) {
  const result = spawnSync(command, args, { encoding: "utf8", shell: false });
  if (result.error) {
    throw new Error(`${command} did not run: ${result.error.message}`);
  }
  return result;
}

const hostLine = run("rustc", ["-vV"])
  .stdout.split("\n")
  .find((line) => line.startsWith("host: "));
if (!hostLine) {
  console.error("check:cross: rustc -vV reported no host triple");
  process.exit(1);
}
const hostTriple = hostLine.slice("host: ".length).trim();
const installed = run("rustup", ["target", "list", "--installed"])
  .stdout.split("\n")
  .map((line) => line.trim())
  .filter(Boolean);

const foreign = SUPPORTED.filter((target) => target.triple !== hostTriple);
let failed = false;
const unverified = [];

for (const target of foreign) {
  if (!installed.includes(target.triple)) {
    unverified.push(target.triple);
    console.error(
      `check:cross: ${target.triple} UNVERIFIED — standard library not installed; ` +
        `run: rustup target add ${target.triple}`,
    );
    continue;
  }
  console.error(`check:cross: cargo check -p syncdash --lib --bins --target ${target.triple}`);
  const check = run("cargo", [
    "check",
    "-p",
    "syncdash",
    "--lib",
    "--bins",
    "--target",
    target.triple,
  ]);
  if (check.status === 0) {
    console.error(`check:cross: ${target.triple} PASS`);
    continue;
  }
  const stderr = check.stderr ?? "";
  const toolchainGap =
    stderr.includes("error occurred in cc-rs") || stderr.includes("failed to find tool");
  if (toolchainGap) {
    unverified.push(target.triple);
    const tool = stderr.match(/failed to find tool "([^"]+)"/)?.[1];
    console.error(
      `check:cross: ${target.triple} UNVERIFIED — a dependency needs a cross C compiler` +
        `${tool ? ` (${tool})` : ""} this machine lacks; ` +
        "check natively on that host before handing off seam changes",
    );
  } else {
    failed = true;
    console.error(stderr);
    console.error(`check:cross: ${target.triple} FAIL — workspace code does not compile`);
  }
}

if (failed) {
  process.exit(1);
}
if (unverified.length > 0) {
  console.error(
    `check:cross: UNVERIFIED targets remain (${unverified.join(", ")}); ` +
      "the native check on the other machine stays authoritative",
  );
}
