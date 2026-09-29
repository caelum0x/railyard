// Types for the shared copy lint (railyard/policy/copy-lint.mjs).
declare module "*/policy/copy-lint.mjs" {
  export interface Unit { text: string; surface: string; where?: string; path?: string }
  export interface Finding { id: string; severity: string; where: string; excerpt: string }
  export function lintUnits(units: Unit[], opts?: Record<string, unknown>): Finding[];
  export function errors(f: Finding[]): Finding[];
  export function formatFindings(f: Finding[]): string;
  export function markdownUnits(md: string, opts?: { surface?: string; where?: string }): Unit[];
}
