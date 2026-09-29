import { redirect } from "next/navigation";
import { currentSession } from "../lib/current.ts";

export const dynamic = "force-dynamic";

export default async function SignIn({ searchParams }: { searchParams: { signin?: string } }) {
  if (await currentSession()) redirect("/apps");
  return (
    <main className="center">
      <section className="card" style={{ width: 380, textAlign: "center" }}>
        <h1>Railyard</h1>
        <p className="muted">Ship it, see who uses it, from your coding agent.</p>
        {searchParams.signin ? <p className="error">Sign-in did not finish. Try again.</p> : null}
        <div style={{ display: "grid", gap: 10, marginTop: 18 }}>
          <a className="primary" style={{ justifyContent: "center" }} href="/auth/start?provider=google">Continue with Google</a>
          <a className="ghost" style={{ justifyContent: "center" }} href="/auth/start?provider=github">Continue with GitHub</a>
        </div>
      </section>
    </main>
  );
}
