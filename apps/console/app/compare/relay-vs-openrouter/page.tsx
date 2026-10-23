// Public, numbers-backed comparison page (backlog item 2): "$0 markup"
// Relay (Railyard's TensorZero-based LLM gateway) vs. OpenRouter's published
// fee structure. No auth required (outside the (app) route group, same as
// the root sign-in page) -- meant to be submitted to awesome-llm-gateway
// lists, r/LocalLLaMA and HN, and to convert into Console signups via the
// same cross-sell bridge as the Insights banner (lib/session.ts SRC_COOKIE).
import { deps } from "../../../lib/deps.ts";

export const dynamic = "force-dynamic";

const SRC = "compare-relay-openrouter";

// Spend tiers x OpenRouter's two published fee paths (openrouter.ai/docs/faq):
// Stripe card top-ups and BYOK usage both carry a percentage fee; Stripe
// top-ups also carry a flat per-charge fee, which is why the *effective*
// rate on smaller/frequent top-ups runs toward the high end of the range
// rather than the flat percentage alone. Relay has neither: you bring your
// own provider key and Relay adds $0 on top, whether self-hosted or run
// through Railyard.
const TIERS: { monthlySpend: number; orLow: number; orHigh: number }[] = [
  { monthlySpend: 500, orLow: 0.055, orHigh: 0.08 },
  { monthlySpend: 2_000, orLow: 0.055, orHigh: 0.075 },
  { monthlySpend: 10_000, orLow: 0.05, orHigh: 0.07 },
  { monthlySpend: 50_000, orLow: 0.05, orHigh: 0.065 },
];

const usd = (n: number) => `$${n.toLocaleString("en-US", { maximumFractionDigits: n < 100 ? 2 : 0 })}`;

export default async function CompareRelayOpenRouter() {
  const { cfg } = deps();
  const signupHref = `${cfg.consoleUrl}/auth/start?src=${SRC}`;
  return (
    <main style={{ maxWidth: 880, margin: "0 auto", padding: "48px 24px 80px" }}>
      <p className="muted" style={{ marginBottom: 6 }}>Railyard Relay vs. OpenRouter</p>
      <h1 style={{ fontSize: 32, marginBottom: 10 }}>$0 markup vs. 5.5-8%: what OpenRouter's fee actually costs you</h1>
      <p className="muted" style={{ maxWidth: 640 }}>
        Relay is Railyard's open-source, OpenAI-compatible LLM gateway (a TensorZero fork): one API, your own provider
        keys, automatic fallbacks, caching and per-key usage metering -- and no fee on top of what your model provider
        already charges. OpenRouter is a closed-source hosted router that adds a published fee to every credit
        purchase and, above its free monthly allowance, to bring-your-own-key usage too.
      </p>
      <section className="card">
        <h2>The fee, from OpenRouter's own docs</h2>
        <ul>
          <li>Credit top-ups by card: a percentage fee plus a flat per-charge fee (openrouter.ai/docs/faq).</li>
          <li>Credit top-ups by crypto: a percentage fee, no flat charge.</li>
          <li>Bring-your-own-key usage above the free monthly allowance: 5% of what the same call would have cost on OpenRouter.</li>
          <li>Relay: 0% in every case above. You call your own provider (or Railyard's metered relay) directly; the only money that moves is to the model provider.</li>
        </ul>
        <p className="muted" style={{ fontSize: 13 }}>
          The flat per-charge fee is why a 5.5% headline rate becomes a higher *effective* rate on smaller or more
          frequent top-ups -- the fixed cents are a bigger share of a small charge. The ranges below bracket that:
          low end is a large, infrequent top-up; high end is frequent, smaller top-ups at the same monthly spend.
        </p>
      </section>
      <section className="card">
        <h2>What that costs by monthly LLM spend</h2>
        <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 14 }}>
          <thead>
            <tr style={{ textAlign: "left", borderBottom: "1px solid var(--line)" }}>
              <th style={{ padding: "8px 10px" }}>Monthly spend</th>
              <th style={{ padding: "8px 10px" }}>OpenRouter fee (est.)</th>
              <th style={{ padding: "8px 10px" }}>Relay fee</th>
              <th style={{ padding: "8px 10px" }}>You keep with Relay</th>
            </tr>
          </thead>
          <tbody>
            {TIERS.map((t) => {
              const low = t.monthlySpend * t.orLow;
              const high = t.monthlySpend * t.orHigh;
              return (
                <tr key={t.monthlySpend} style={{ borderBottom: "1px solid var(--line)" }}>
                  <td style={{ padding: "8px 10px" }}>{usd(t.monthlySpend)}/mo</td>
                  <td style={{ padding: "8px 10px" }}>{usd(low)}-{usd(high)}/mo</td>
                  <td style={{ padding: "8px 10px" }}>$0</td>
                  <td style={{ padding: "8px 10px" }}><strong>{usd(low)}-{usd(high)}/mo</strong></td>
                </tr>
              );
            })}
          </tbody>
        </table>
        <p className="muted" style={{ fontSize: 13, marginTop: 10 }}>
          At $10k/mo in model spend, that is roughly $6,000-$8,400 a year OpenRouter's fee alone would have cost --
          money that stays with your model provider (or your margin) when you route through Relay instead.
        </p>
      </section>
      <section className="card">
        <h2>Same inference API, different billing</h2>
        <ul>
          <li>Both offer a unified, OpenAI-compatible inference API across providers with automatic fallbacks.</li>
          <li>Relay adds built-in observability, caching, per-key spend caps and usage metering to your own database --
            OpenRouter does not offer observability or caching.</li>
          <li>OpenRouter offers dynamic cost/latency-based routing across providers; Relay's routing is a declared,
            auditable fallback order -- a deliberate trade for predictable billing.</li>
          <li>Run Relay yourself (Apache 2.0, Docker) for the $0 markup above, or let Railyard host and meter it for
            you with the same guarantee: no fee layered on top of provider cost, ever.</li>
        </ul>
      </section>
      <section className="card" style={{ textAlign: "center" }}>
        <h2>Get a Relay key</h2>
        <p className="muted">Railyard Console issues a Relay key, meters usage per project and shows exact
          provider cost -- no markup, no separate OpenRouter-style billing relationship.</p>
        <a className="primary" style={{ justifyContent: "center", display: "inline-flex", marginTop: 10 }} href={signupHref}>
          Start free on Railyard Console
        </a>
      </section>
    </main>
  );
}
