// The published entry point speaks MCP over stdio.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const BIN = join(dirname(fileURLToPath(import.meta.url)), "../bin/railyard-mcp.mjs");

test("initialize and tools/list over stdio", async () => {
  const child = spawn(process.execPath, [BIN], { stdio: ["pipe", "pipe", "inherit"], env: { ...process.env, RAILYARD_CONFIG_DIR: "/nonexistent" } });
  const lines = [];
  let buf = "";
  child.stdout.on("data", (d) => {
    buf += d;
    let i;
    while ((i = buf.indexOf("\n")) >= 0) {
      lines.push(JSON.parse(buf.slice(0, i)));
      buf = buf.slice(i + 1);
    }
  });
  const send = (m) => child.stdin.write(`${JSON.stringify(m)}\n`);
  send({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "t", version: "1" } } });
  send({ jsonrpc: "2.0", method: "notifications/initialized" });
  send({ jsonrpc: "2.0", id: 2, method: "tools/list" });
  for (let i = 0; i < 100 && lines.length < 2; i += 1) await new Promise((r) => setTimeout(r, 50));
  child.kill();
  assert.equal(lines[0].result.serverInfo.name, "railyard");
  assert.equal(lines[1].result.tools.length, 7);
});
