export const dynamic = "force-dynamic";

const PROVIDERS = [
  { id: "anthropic", label: "Anthropic" },
  { id: "openai", label: "OpenAI" },
  { id: "groq", label: "Groq" },
  { id: "openrouter", label: "OpenRouter" },
];

// The secure connect page for the AI gateway (PY1.4). The link from the
// terminal works once for 15 minutes. The provider key is sealed before it is
// stored and is never shown again; your provider bills you directly.
export default function ConnectAiPage({ searchParams }: { searchParams: { t?: string; done?: string; hint?: string; error?: string } }) {
  const done = PROVIDERS.find((p) => p.id === searchParams.done);
  return (
    <main className="center">
      <section className="card" style={{ width: 460 }}>
        <h1>Connect an AI provider</h1>
        {done ? (
          <p>
            {done.label} is connected (key ending {searchParams.hint ?? ""}). Go back to your terminal. Your provider bills you directly.
          </p>
        ) : searchParams.t ? (
          <form method="post" action="/api/connect/ai" className="stack" autoComplete="off">
            <input type="hidden" name="t" value={searchParams.t} />
            <label>
              Provider
              <select name="provider" required defaultValue="anthropic">
                {PROVIDERS.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.label}
                  </option>
                ))}
              </select>
            </label>
            <label>
              Provider key
              <input type="password" name="key" required minLength={8} autoComplete="off" spellCheck={false} aria-label="Provider key" />
            </label>
            <button className="primary" type="submit">Connect</button>
            <p className="muted">Sealed before it is stored and never shown again. Calls use only this key; your provider bills you directly.</p>
          </form>
        ) : (
          <p className="muted">Open this page from the link your terminal printed.</p>
        )}
        {searchParams.error === "expired" ? <p className="error">This link was used already or expired. Start again from your terminal.</p> : null}
        {searchParams.error === "invalid" ? <p className="error">That did not work. Check the provider and the key, then try again.</p> : null}
        {searchParams.error === "not_configured" ? <p className="error">The AI gateway is not set up on this deployment yet.</p> : null}
      </section>
    </main>
  );
}
