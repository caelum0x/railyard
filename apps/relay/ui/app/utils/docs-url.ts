/**
 * Links from the operator UI to feature documentation.
 *
 * Relay's observability, evaluation and optimization screens come from the
 * TensorZero engine (Apache-2.0). Their reference pages ship in this repository
 * under `apps/relay/docs/` as MDX. By default the UI links to the same pages in
 * the archived upstream repository, pinned to the fork baseline tag, so the
 * link always matches the engine Relay runs and never depends on a hosted
 * upstream docs site.
 *
 * Operators who publish their own docs set `VITE_RELAY_DOCS_URL` at build time;
 * paths are then appended as `<base>/<path>` (no file extension).
 */
export const RELAY_FORK_BASELINE_TAG = "2026.6.0";

export const DEFAULT_DOCS_SOURCE_BASE = `https://github.com/tensorzero/tensorzero/blob/${RELAY_FORK_BASELINE_TAG}/docs`;

function normalizePath(path: string): { page: string; hash: string } {
  const [rawPage, ...rest] = path.split("#");
  const page = rawPage.replace(/^\/+/, "").replace(/\/+$/, "");
  const hash = rest.length > 0 ? `#${rest.join("#")}` : "";
  return { page, hash };
}

/**
 * Build a docs URL for a path like `gateway/guides/episodes` or
 * `gateway/create-a-prompt-template#some-heading`.
 */
export function buildDocsUrl(path: string, customBase?: string | null): string {
  const { page, hash } = normalizePath(path);
  const base = customBase?.trim();
  if (base) {
    return `${base.replace(/\/+$/, "")}/${page}${hash}`;
  }
  // GitHub renders MDX headings with the same slug anchors Mintlify used.
  return `${DEFAULT_DOCS_SOURCE_BASE}/${page}.mdx${hash}`;
}

export function docsUrl(path: string): string {
  return buildDocsUrl(
    path,
    import.meta.env.VITE_RELAY_DOCS_URL as string | undefined,
  );
}
