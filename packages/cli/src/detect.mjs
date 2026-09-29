// Framework detection for `railyard deploy` (RYP-D4). Returns a supported
// framework with its build plan, or a one-line refusal that says what to do.

import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

export const SUPPORTED = ["nextjs", "vite", "astro", "static"];

// Checked before Vite, because these build on Vite but need a server runtime.
const UNSUPPORTED = [
  ["@remix-run/react", "Remix"],
  ["@react-router/dev", "React Router framework mode"],
  ["@sveltejs/kit", "SvelteKit"],
  ["nuxt", "Nuxt"],
  ["gatsby", "Gatsby"],
  ["@angular/core", "Angular"],
  ["@solidjs/start", "SolidStart"],
  ["@tanstack/react-start", "TanStack Start"],
  ["@nestjs/core", "NestJS"],
];

function readJson(p) {
  try {
    return JSON.parse(readFileSync(p, "utf8"));
  } catch {
    return null;
  }
}

export function packageManager(dir) {
  if (existsSync(join(dir, "pnpm-lock.yaml"))) return "pnpm";
  if (existsSync(join(dir, "yarn.lock"))) return "yarn";
  if (existsSync(join(dir, "bun.lockb")) || existsSync(join(dir, "bun.lock"))) return "bun";
  return "npm";
}

const WHAT = "Railyard deploys Next.js, Vite, Astro or static sites";

/**
 * @returns {{ ok: true, framework: string, outDir: string | null, build: string[] | null } | { ok: false, message: string }}
 */
export function detect(dir) {
  const pkg = readJson(join(dir, "package.json"));
  if (!pkg) {
    if (existsSync(join(dir, "index.html"))) return { ok: true, framework: "static", outDir: ".", build: null };
    return { ok: false, message: `${WHAT}; no package.json or index.html in ${dir}, so run it from your project folder.` };
  }
  const deps = { ...(pkg.dependencies ?? {}), ...(pkg.devDependencies ?? {}) };
  for (const [name, label] of UNSUPPORTED) {
    if (deps[name]) return { ok: false, message: `${WHAT}; this is ${label} (${name}), which needs a server runtime Railyard does not deploy yet.` };
  }
  const pm = packageManager(dir);
  const run = pm === "npm" ? ["npm", "run", "build"] : [pm, "run", "build"];
  const hasBuild = typeof pkg.scripts?.build === "string";
  if (deps.next) return { ok: true, framework: "nextjs", outDir: ".open-next", build: null };
  if (deps.astro) {
    const cfg = ["astro.config.mjs", "astro.config.ts", "astro.config.js"].map((f) => join(dir, f)).find(existsSync);
    const src = cfg ? readFileSync(cfg, "utf8") : "";
    if (/output\s*:\s*["'](server|hybrid)["']/.test(src)) {
      return { ok: false, message: `${WHAT}; this Astro project uses server output, so set output: "static" in ${cfg?.split("/").pop()} or deploy it another way.` };
    }
    if (!hasBuild) return { ok: false, message: `${WHAT}; add a "build" script (astro build) to package.json.` };
    return { ok: true, framework: "astro", outDir: "dist", build: run };
  }
  if (deps.vite) {
    if (!hasBuild) return { ok: false, message: `${WHAT}; add a "build" script (vite build) to package.json.` };
    return { ok: true, framework: "vite", outDir: "dist", build: run };
  }
  if (existsSync(join(dir, "index.html"))) return { ok: true, framework: "static", outDir: ".", build: null };
  return { ok: false, message: `${WHAT}; found a package.json without next, astro or vite and no index.html in ${dir}.` };
}

/** `<app>` on main or master, `<app>-<branch>` elsewhere; a valid Worker name (63 chars max). */
export function workerName(app, branch) {
  const clean = (s) => String(s).toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "");
  const base = clean(app).slice(0, 40) || "app";
  if (!branch || branch === "main" || branch === "master" || branch === "HEAD") return base;
  return `${base}-${clean(branch)}`.slice(0, 63).replace(/-+$/, "");
}
