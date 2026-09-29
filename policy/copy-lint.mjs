// Copy-denylist matcher for Railyard (gtm/policy/CLAIMS-RULES.md section 4).
//
// Zero dependencies, so the console, the CLI, the MCP server and the skill
// README tests all share one implementation of the normative matching steps:
// normalise, product and surface filter, path allowlist, flags, except tokens,
// match, source escape, severity. The vendored JSON next to this file is never
// edited here; change gtm/policy/copy-denylist.json and re-vendor.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
export const DENYLIST_PATH = join(HERE, "copy-denylist.json");
export const PINNED_VERSION = "2026-09-29.2";
export const PRODUCT = "railyard";

export function loadDenylist(path = DENYLIST_PATH) {
  return JSON.parse(readFileSync(path, "utf8"));
}

export function normalise(text) {
  return String(text)
    .replace(/[‘’]/g, "'")
    .replace(/[“”]/g, '"')
    .replace(/ /g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

function escapeRegExp(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// Glob with `**` crossing folders and `*` staying in one segment (section 4.3 step 3).
export function globToRegExp(glob) {
  let out = "";
  for (let i = 0; i < glob.length; i += 1) {
    const ch = glob[i];
    if (ch === "*" && glob[i + 1] === "*") {
      out += ".*";
      i += 1;
      if (glob[i + 1] === "/") i += 1;
    } else if (ch === "*") {
      out += "[^/]*";
    } else if (ch === "[" || ch === "]") {
      out += ch;
    } else {
      out += escapeRegExp(ch);
    }
  }
  return new RegExp(`^${out}$`);
}

export function compileRules(denylist, { product = PRODUCT, flags = {} } = {}) {
  const defaults = denylist.defaults ?? {};
  const out = [];
  for (const raw of denylist.rules) {
    if (!(raw.products ?? []).includes(product)) continue;
    const rule = { ...defaults, ...raw };
    // Step 4: a rule is off when any of its flags is on in the real config.
    const off = (rule.unlessFlags ?? []).some((f) => ["1", "true"].includes(String(flags[f] ?? "")));
    if (off) continue;
    if (rule.match.type === "regex") {
      const cs = rule.match.caseSensitive ?? rule.caseSensitive ?? false;
      rule._re = new RegExp(rule.match.value, cs ? "" : "i");
    } else if (rule.match.type === "codepoints") {
      rule._ranges = rule.match.ranges.map(([lo, hi]) => [parseInt(lo, 16), parseInt(hi, 16)]);
    } else {
      throw new Error(`unknown match type ${rule.match.type} in ${rule.id}`);
    }
    rule._allow = (rule.allowPaths ?? []).map(globToRegExp);
    out.push(rule);
  }
  return out;
}

// Steps 1, 5, 6 and 7 for one unit of text.
export function hits(rule, text) {
  let unit = normalise(text);
  for (const tok of rule.exceptTokens ?? []) {
    unit = unit.replace(new RegExp(escapeRegExp(tok), "gi"), " ");
  }
  let hit;
  if (rule._re) {
    hit = rule._re.test(unit);
  } else {
    hit = [...unit].some((ch) => {
      const cp = ch.codePointAt(0);
      return rule._ranges.some(([lo, hi]) => cp >= lo && cp <= hi);
    });
  }
  if (hit && rule.sourceEscape && /copy-source:\s*\S|data-copy-source="[^"]+"/.test(String(text))) {
    return false;
  }
  return hit;
}

export function selfTest(rules) {
  const errors = [];
  for (const r of rules) {
    for (const ex of r.examples?.hit ?? []) if (!hits(r, ex)) errors.push(`self-test: ${r.id} should hit ${JSON.stringify(ex)}`);
    for (const ex of r.examples?.miss ?? []) if (hits(r, ex)) errors.push(`self-test: ${r.id} should miss ${JSON.stringify(ex)}`);
  }
  return errors;
}

/**
 * Lint units of text. Each unit is { text, surface, where, path? }.
 * Returns findings [{ id, severity, where, excerpt }]; excerpts are omitted for
 * secret-surface rules (section 4.5).
 */
export function lintUnits(units, opts = {}) {
  const denylist = opts.denylist ?? loadDenylist();
  const rules = opts.rules ?? compileRules(denylist, opts);
  const findings = [];
  for (const u of units) {
    for (const r of rules) {
      if (!(r.surfaces ?? []).includes(u.surface)) continue;
      if (u.path && r._allow.some((re) => re.test(u.path))) continue;
      if (!hits(r, u.text)) continue;
      findings.push({
        id: r.id,
        severity: r.severity ?? "error",
        where: u.where ?? u.path ?? "",
        excerpt: r.category === "secret-surface" ? "" : normalise(u.text).slice(0, 60),
      });
    }
  }
  return findings;
}

// Markdown to units: one line per unit, code fences and inline code skipped
// (commands and identifiers are not copy), link targets dropped.
export function markdownUnits(md, { surface = "docs", where = "md" } = {}) {
  const units = [];
  let fenced = false;
  md.split(/\r?\n/).forEach((line, i) => {
    if (/^\s*```/.test(line)) {
      fenced = !fenced;
      return;
    }
    if (fenced) return;
    const text = line
      .replace(/`[^`]*`/g, " ")
      .replace(/\]\([^)]*\)/g, "]")
      .replace(/<[^>]+>/g, " ");
    if (text.trim()) units.push({ text, surface, where: `${where}:${i + 1}`, raw: line });
  });
  return units;
}

export function errors(findings) {
  return findings.filter((f) => f.severity === "error");
}

export function formatFindings(findings) {
  return findings
    .map((f) => `${f.id} (${f.severity}): ${f.where}${f.excerpt ? `: ${JSON.stringify(f.excerpt)}` : ""}`)
    .join("\n");
}
