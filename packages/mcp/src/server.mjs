// The MCP server over any transport (stdio in bin/railyard-mcp.mjs).
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { CallToolRequestSchema, ListToolsRequestSchema } from "@modelcontextprotocol/sdk/types.js";

import { loadCredentials } from "./credentials.mjs";
import { TOOLS, ToolError, validate } from "./tools.mjs";

export const VERSION = "0.1.0";

export function createServer({ env = process.env, fetchImpl = fetch } = {}) {
  const server = new Server({ name: "railyard", version: VERSION }, { capabilities: { tools: {} } });
  server.setRequestHandler(ListToolsRequestSchema, async () => ({
    tools: TOOLS.map((t) => ({ name: t.name, title: t.title, description: t.description, inputSchema: t.inputSchema, annotations: { title: t.title, ...t.annotations } })),
  }));
  server.setRequestHandler(CallToolRequestSchema, async (req) => {
    const tool = TOOLS.find((t) => t.name === req.params.name);
    if (!tool) return { isError: true, content: [{ type: "text", text: `Unknown tool ${req.params.name}.` }] };
    const args = req.params.arguments ?? {};
    const problem = validate(tool.inputSchema, args);
    if (problem) return { isError: true, content: [{ type: "text", text: problem }] };
    // Read the sign-in per call, so `railyard login` in another terminal takes effect at once.
    const ctx = { env, fetchImpl, creds: loadCredentials(env) };
    try {
      const result = await tool.handler(ctx, args);
      return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }], structuredContent: result };
    } catch (err) {
      const message = err instanceof ToolError ? err.message : `Railyard could not finish: ${err.message}`;
      return { isError: true, content: [{ type: "text", text: message }] };
    }
  });
  return server;
}
