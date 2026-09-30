// Matching autocaptured clicks by CSS selector, tag, text and link against
// the stored elements chain. Ported from the upstream product (MIT, commit
// cbb1e9253027; see src/insights/NOTICE):
//   posthog/models/event/event.py        Selector, SelectorPart (parsing)
//   posthog/models/property/util.py      build_selector_regex, _chain_escaped_value
//   posthog/hogql/property.py            tag_name_to_expr, selector_to_expr, steps_to_expr
//   posthog/models/event/sql.py          elements_chain_href / _texts extraction patterns
// Re-expressed for Postgres regular expressions (ARE); the regex text is
// always a bound parameter.

/** Escapes every ASCII character except letters, digits and underscore, as re.escape does. */
export function reEscape(s: string): string {
  return s.replace(/[\x20-\x2f\x3a-\x40\x5b-\x5e\x60\x7b-\x7e]/g, (c) => `\\${c}`);
}

const SELECTOR_ATTRIBUTE_REGEX = /([a-zA-Z]*)\[(.*)=['|"](.*)['|"]\]/;

export interface SelectorPart {
  directDescendant: boolean;
  tagName?: string;
  classes?: string[];
  chAttributes: Record<string, string>;
}

function parsePart(tagIn: string, directDescendant: boolean): SelectorPart {
  let tag = tagIn;
  const part: SelectorPart = { directDescendant, chAttributes: {} };
  const result = SELECTOR_ATTRIBUTE_REGEX.exec(tag);
  if (result && tag.includes("[id=")) {
    part.chAttributes.attr_id = result[3];
    tag = result[1];
  }
  if (result && tag.includes("[")) {
    part.chAttributes[result[2]] = result[3];
    tag = result[1];
  }
  if (tag.includes(":nth-child(")) {
    const [head, rest] = tag.split(":nth-child(");
    part.chAttributes["nth-child"] = rest.replace(")", "");
    tag = head;
  }
  if (tag.includes(".")) {
    // Dots that are not inside square brackets (arbitrary-value class names).
    const pieces = tag.split(/\.(?![^[]*\])/);
    part.classes = pieces.slice(1);
    tag = pieces[0];
  }
  if (tag.includes("#")) {
    const pieces = tag.split("#");
    if (pieces.length > 1) part.chAttributes.attr_id = pieces[1];
    tag = pieces[0];
  }
  if (tag) part.tagName = tag;
  return part;
}

function* splitSelector(selector: string): Generator<string> {
  let inAttr = false;
  let inQuotes: string | null = null;
  let buf = "";
  for (const ch of selector) {
    if (ch === "[" && inQuotes === null) inAttr = true;
    if (ch === "]" && inQuotes === null) inAttr = false;
    if (ch === '"' || ch === "'") {
      if (inQuotes !== null) {
        if (inQuotes === ch) inQuotes = null;
      } else inQuotes = ch;
    }
    if (ch === " " && !inAttr) {
      yield buf;
      buf = "";
    } else buf += ch;
  }
  yield buf;
}

export function parseSelector(selectorIn: string): SelectorPart[] {
  const selector = selectorIn.replaceAll("> * > ", "").replaceAll("> *", "").replaceAll("\\:", ":").trim();
  const tags = [...splitSelector(selector)].reverse();
  const parts: SelectorPart[] = [];
  tags.forEach((tag, i) => {
    if (tag === ">" || tag === "") return;
    parts.push(parsePart(tag, i > 0 && tags[i - 1] === ">"));
  });
  return parts;
}

function chainEscapedValue(v: string): string {
  return v.replaceAll('\\"', '"').replaceAll('"', '\\"');
}

/** The regex an elements chain must match for the selector (empty string: no constraint). */
export function selectorRegex(selector: string): string {
  let regex = "";
  for (const tag of parseSelector(selector)) {
    if (tag.tagName && tag.tagName !== "*") regex += reEscape(tag.tagName);
    if (tag.classes?.length) regex += ".*?\\." +[...tag.classes].sort().map(reEscape).join("\\..*?");
    const keys = Object.keys(tag.chAttributes).sort();
    if (keys.length) {
      regex += ".*?";
      for (const k of keys) regex += `${reEscape(k)}="${reEscape(chainEscapedValue(String(tag.chAttributes[k])))}".*?`;
    }
    regex += "[^;]*?($|;|:([^;^\\s]*(;|$|\\s)))";
    if (tag.directDescendant) regex += ".*";
  }
  return regex ? `(^|;)${regex}` : "";
}

export function tagNameRegex(tag: string): string {
  return `(^|;)${reEscape(tag)}(\\.|$|;|:)`;
}

/** Postgres patterns extracting the first link and every text of a chain (upstream materialized columns). */
export const HREF_PATTERN = '(?::|")href="(.*?)"';
export const TEXT_PATTERN = '(?::|")text="(.*?)"';
