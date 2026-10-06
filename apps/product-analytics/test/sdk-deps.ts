// The SDK packages (packages/analytics-js, packages/analytics-node) are not in
// any pnpm workspace that installs their dependencies, so on a fresh checkout
// their `posthog-*` imports have nothing to resolve to. This service pins the
// same versions as devDependencies; when a package's own install is missing,
// its upstream SDK imports resolve from here instead. A package install, when
// present, still wins.
import { existsSync } from "node:fs";
import { registerHooks } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SERVICE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const SERVICE_PARENT = pathToFileURL(join(SERVICE_ROOT, "package.json")).href;
const UPSTREAM_SDK = /^posthog-(js|node|react-native)(\/|$)/;

registerHooks({
  resolve(specifier, context, next) {
    if (!UPSTREAM_SDK.test(specifier)) return next(specifier, context);
    try {
      return next(specifier, context);
    } catch (e) {
      if ((e as { code?: string }).code !== "ERR_MODULE_NOT_FOUND") throw e;
      return next(specifier, { ...context, parentURL: SERVICE_PARENT });
    }
  },
});

/** The browser SDK's ESM build: the package's own install, else this service's. */
export function sdkEsmPath(pkgDir: string): string {
  const own = join(pkgDir, "node_modules/posthog-js/dist/module.mjs");
  return existsSync(own) ? own : join(SERVICE_ROOT, "node_modules/posthog-js/dist/module.mjs");
}
