// Component test for the "Add app" flow (RYP-D1 acceptance 1, the fallback the
// dispatch allows when a browser install is blocked by the disk rule): the
// real AddAppView is rendered for every state the reducer passes through, and
// the rendered copy is linted with the vendored denylist (surface app-ui).
import { test, after } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import ts from "typescript";

import { addAppFlow, CLICKS, type FlowAction, type FlowState } from "../lib/add-app-flow.ts";
import { errors, formatFindings, lintUnits } from "../../../policy/copy-lint.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const OUT = join(ROOT, ".test-build");
after(() => rmSync(OUT, { recursive: true, force: true }));

// Transpile the .tsx components with the installed TypeScript; relative
// imports become absolute file URLs so they resolve to the real sources.
function build(rel: string): string {
  const src = join(ROOT, rel);
  let js = ts.transpileModule(readFileSync(src, "utf8"), {
    compilerOptions: { jsx: ts.JsxEmit.ReactJSX, module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  js = js.replace(/from\s+["'](\.{1,2}\/[^"']+)["']/g, (_m, spec: string) => {
    const target = resolve(dirname(src), spec);
    if (target.endsWith(".tsx")) return `from ${JSON.stringify(pathToFileURL(join(OUT, build(target.slice(ROOT.length + 1)))).href)}`;
    return `from ${JSON.stringify(pathToFileURL(target).href)}`;
  });
  const outRel = rel.replace(/\.tsx$/, ".mjs");
  mkdirSync(dirname(join(OUT, outRel)), { recursive: true });
  writeFileSync(join(OUT, outRel), js);
  return outRel;
}

function text(html: string): string[] {
  return html
    .replace(/<(script|style)[\s\S]*?<\/\1>/g, " ")
    .split(/<[^>]+>/)
    .map((s) => s.replace(/&quot;/g, '"').replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&").replace(/&#x27;/g, "'").trim())
    .filter(Boolean);
}

test("Add app: list -> form -> snippet in 2 clicks, every state renders and passes the copy lint", async () => {
  const { AddAppView } = await import(pathToFileURL(join(OUT, build("components/AddApp.tsx"))).href);
  const React = (await import("react")).default;
  const { renderToString } = await import("react-dom/server");
  const noop = () => {};
  const render = (state: FlowState) =>
    renderToString(React.createElement(AddAppView, { state, remaining: 3, onAdd: noop, onName: noop, onCreate: noop, onClose: noop, onCopy: noop, copied: null }));

  const snippet = { script: '<script defer src="https://insights.example/s.js" data-site="ry_site_abc"></script>', deploy: "npx railyard deploy --app my-app" };
  const actions: FlowAction[] = [{ type: "click_add" }, { type: "type_name", name: "my-app" }, { type: "click_create" }, { type: "created", snippet }];
  let state: FlowState = { step: "list" };
  let clicks = 0;
  const pages = [render(state)];
  assert.match(pages[0], />Add app<\/button>/);
  for (const a of actions) {
    if (CLICKS.has(a.type)) clicks += 1;
    state = addAppFlow(state, a);
    pages.push(render(state));
  }
  assert.equal(state.step, "done");
  assert.ok(clicks <= 3, `${clicks} clicks`);
  const done = pages.at(-1)!;
  assert.match(done, /data-testid="snippet"/);
  assert.ok(done.includes("npx railyard deploy --app my-app"));
  assert.ok(done.includes("data-site=&quot;ry_site_abc&quot;"));
  assert.match(done, /aria-label="Copy deploy"/, "icon-only copy control has a label");

  const units = pages.flatMap((html, i) => text(html).map((t) => ({ text: t, surface: "app-ui", where: `state ${i}` })));
  const found = errors(lintUnits(units));
  assert.deepEqual(found, [], formatFindings(found));
});

test("console source copy passes the vendored denylist (app-ui)", () => {
  const files = ["app/page.tsx", "app/(app)/apps/page.tsx", "app/(app)/usage/page.tsx", "app/(app)/billing/page.tsx", "app/device/page.tsx", "components/BillingActions.tsx", "components/AddApp.tsx", "components/Nav.tsx", "lib/apps.ts"];
  const units = files.flatMap((f) => {
    const src = readFileSync(join(ROOT, f), "utf8");
    const strings = [...src.matchAll(/>([^<>{}]+)</g), ...src.matchAll(/"([^"\n]{3,})"/g), ...src.matchAll(/`([^`\n]{3,})`/g)].map((m) => m[1]);
    return strings.map((t) => ({ text: t, surface: "app-ui", where: f, path: f }));
  });
  const found = errors(lintUnits(units));
  assert.deepEqual(found, [], formatFindings(found));
});
