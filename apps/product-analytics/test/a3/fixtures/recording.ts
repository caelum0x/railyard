// A recorded browsing minute in the shape the browser SDK's recorder sends:
// a meta event and a partly compressed full snapshot of a product page
// (about 400 nodes) when a session starts, then per second mouse movement,
// and over a minute 20 clicks, 6 scrolls, 12 typed inputs and 30 small
// DOM changes (compressed like the recorder does). Deterministic per seed.

import { gzipSync, strFromU8, strToU8 } from "fflate";

const packed = (v: unknown) => strFromU8(gzipSync(strToU8(JSON.stringify(v))), true);

let id = 100;
const el = (tagName: string, attributes: Record<string, string>, childNodes: unknown[] = []) => ({ type: 2, tagName, attributes, childNodes, id: id++ });
const txt = (textContent: string) => ({ type: 3, textContent, id: id++ });

function page(seed: number) {
  id = 100;
  const cards = Array.from({ length: 40 }, (_, i) =>
    el("article", { class: "card product", "data-sku": `sku-${seed}-${i}` }, [
      el("img", { src: `https://cdn.shop.example/p/${i}.jpg`, alt: `Product ${i}`, width: "240", height: "180" }),
      el("h3", { class: "title" }, [txt(`Product number ${i} in the spring collection`)]),
      el("p", { class: "price" }, [txt(`${(19 + i * 3.5).toFixed(2)} EUR`)]),
      el("button", { class: "btn add", type: "button" }, [txt("Add to cart")]),
    ]),
  );
  const form = el("form", { class: "newsletter" }, [el("input", { type: "email", name: "email", value: "" }), el("button", { type: "submit" }, [txt("Subscribe")])]);
  const style = el("style", {}, [txt(".card{display:grid;gap:8px;padding:12px;border:1px solid #ddd;border-radius:8px}".repeat(20))]);
  return { type: 0, id: 1, childNodes: [el("html", { lang: "en" }, [el("head", {}, [style]), el("body", {}, [el("header", {}, [txt("Shop")]), el("main", {}, cards), form])])] };
}

export interface RrwebLike {
  type: number;
  timestamp: number;
  data?: unknown;
  cv?: string;
}

/** Events of one session from `fromMs` for `seconds`, starting with the snapshot when `start` is true. */
export function recordingSlice(seed: number, fromMs: number, seconds: number, start: boolean): RrwebLike[] {
  const out: RrwebLike[] = [];
  if (start) {
    out.push({ type: 4, timestamp: fromMs, data: { href: `https://shop.example/c/${seed % 7}`, width: 1440, height: 900 } });
    out.push({ type: 2, timestamp: fromMs + 5, data: packed({ node: page(seed), initialOffset: { top: 0, left: 0 } }), cv: "2024-10" });
  }
  for (let s = 0; s < seconds; s += 1) {
    const t = fromMs + s * 1000 + 50;
    const k = Math.floor(t / 1000) + seed;
    out.push({ type: 3, timestamp: t, data: { source: 1, positions: Array.from({ length: 10 }, (_, i) => ({ x: (k * 37 + i * 13) % 1400, y: (k * 53 + i * 7) % 880, id: 120 + (i % 40), timeOffset: -i * 50 })) } });
    if (k % 3 === 0) out.push({ type: 3, timestamp: t + 200, data: { source: 2, type: 2, id: 130 + (k % 40), x: (k * 11) % 1400, y: (k * 17) % 880 } });
    if (k % 10 === 0) out.push({ type: 3, timestamp: t + 300, data: { source: 3, id: 1, x: 0, y: (k * 97) % 4000 } });
    if (k % 5 === 0) out.push({ type: 3, timestamp: t + 400, data: { source: 5, text: "*".repeat(8 + (k % 12)), isChecked: false, id: 900 } });
    if (k % 2 === 0) {
      out.push({
        type: 3,
        timestamp: t + 500,
        cv: "2024-10",
        data: { source: 0, texts: packed([{ id: 140 + (k % 40), value: `${(19 + k * 0.5).toFixed(2)} EUR` }]), attributes: packed([{ id: 141 + (k % 40), attributes: { class: k % 4 ? "btn add" : "btn add busy" } }]), removes: packed([]), adds: packed([]) },
      });
    }
  }
  return out;
}
