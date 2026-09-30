// Formulas over trend series ("A / B * 100"): parsed here into a tiny
// expression tree and evaluated per interval in JavaScript, never in SQL.
// Letters are series in order (A is the first). Division by zero gives 0,
// as the upstream trends formula does.

import { QueryError } from "./types.ts";

type Node = { n: number } | { v: number } | { op: "+" | "-" | "*" | "/"; a: Node; b: Node } | { neg: Node };

const MAX_LEN = 200;

export function parseFormula(src: string, seriesCount: number): Node {
  if (src.length > MAX_LEN) throw new QueryError("Keep the formula under 200 characters.");
  const tokens = src.match(/\s*([A-Za-z]|\d+(?:\.\d+)?|[()+\-*/])\s*/g);
  if (!tokens || tokens.join("").replace(/\s/g, "") !== src.replace(/\s/g, "")) throw new QueryError("Use series letters, numbers, + - * / and brackets in the formula.");
  const t = tokens.map((x) => x.trim());
  let i = 0;
  const peek = () => t[i];
  const eat = () => t[i++];
  const primary = (): Node => {
    const tok = eat();
    if (tok === undefined) throw new QueryError("The formula ends too early.");
    if (tok === "(") {
      const e = expr();
      if (eat() !== ")") throw new QueryError("A bracket in the formula is not closed.");
      return e;
    }
    if (tok === "-") return { neg: primary() };
    if (/^[A-Za-z]$/.test(tok)) {
      const idx = tok.toUpperCase().charCodeAt(0) - 65;
      if (idx >= seriesCount) throw new QueryError(`The formula uses ${tok.toUpperCase()}, but there are only ${seriesCount} series.`);
      return { v: idx };
    }
    if (/^\d/.test(tok)) return { n: Number(tok) };
    throw new QueryError("Use series letters, numbers, + - * / and brackets in the formula.");
  };
  const term = (): Node => {
    let a = primary();
    while (peek() === "*" || peek() === "/") {
      const op = eat() as "*" | "/";
      a = { op, a, b: primary() };
    }
    return a;
  };
  const expr = (): Node => {
    let a = term();
    while (peek() === "+" || peek() === "-") {
      const op = eat() as "+" | "-";
      a = { op, a, b: term() };
    }
    return a;
  };
  const root = expr();
  if (i !== t.length) throw new QueryError("The formula has something extra at the end.");
  return root;
}

export function evalFormula(node: Node, values: number[]): number {
  if ("n" in node) return node.n;
  if ("v" in node) return values[node.v] ?? 0;
  if ("neg" in node) return -evalFormula(node.neg, values);
  const a = evalFormula(node.a, values);
  const b = evalFormula(node.b, values);
  switch (node.op) {
    case "+":
      return a + b;
    case "-":
      return a - b;
    case "*":
      return a * b;
    case "/":
      return b === 0 ? 0 : a / b;
  }
}
