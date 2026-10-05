import { cookies } from "next/headers";
import { redirect } from "next/navigation";
import { currentSession } from "../lib/current.ts";
import { EMAIL_COOKIE, oauthProvidersFrom } from "../lib/scribase-auth.ts";

export const dynamic = "force-dynamic";

const LABELS = { google: "Continue with Google", github: "Continue with GitHub" } as const;

function notice(signin?: string): string | null {
  if (signin === "email") return "Type a full email address.";
  if (signin === "code") return "That code did not work. Check the newest email and try again.";
  if (signin) return "Sign-in did not finish. Try again.";
  return null;
}

export default async function SignIn({ searchParams }: { searchParams: { signin?: string; step?: string } }) {
  if (await currentSession()) redirect("/apps");
  const providers = oauthProvidersFrom(process.env.RAILYARD_OAUTH_PROVIDERS);
  const codeStep = searchParams.step === "code" && !!cookies().get(EMAIL_COOKIE)?.value;
  const message = notice(searchParams.signin);
  return (
    <main className="center">
      <section className="card" style={{ width: 380, textAlign: "center" }}>
        <h1>Railyard</h1>
        <p className="muted">Ship it, see who uses it, from your coding agent.</p>
        {message ? <p className="error">{message}</p> : null}
        {codeStep ? (
          <form method="post" action="/auth/email/verify" style={{ display: "grid", gap: 10, marginTop: 18 }}>
            <p className="muted">We sent a sign-in code to your email.</p>
            <input type="text" name="code" inputMode="text" autoComplete="one-time-code" placeholder="Code" required aria-label="Sign-in code" />
            <button className="primary" style={{ justifyContent: "center" }} type="submit">Sign in</button>
            <a className="ghost" style={{ justifyContent: "center" }} href="/">Use another address</a>
          </form>
        ) : (
          <div style={{ display: "grid", gap: 10, marginTop: 18 }}>
            {providers.map((p, i) => (
              <a key={p} className={i === 0 ? "primary" : "ghost"} style={{ justifyContent: "center" }} href={`/auth/start?provider=${p}`}>{LABELS[p]}</a>
            ))}
            <form method="post" action="/auth/email" style={{ display: "grid", gap: 10 }}>
              <input type="email" name="email" autoComplete="email" placeholder="you@company.com" required aria-label="Email" />
              <button className={providers.length ? "ghost" : "primary"} style={{ justifyContent: "center" }} type="submit">Email me a sign-in code</button>
            </form>
          </div>
        )}
      </section>
    </main>
  );
}
