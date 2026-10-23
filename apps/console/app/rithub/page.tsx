// Public landing page for Rithub (apps/gitea), backlog item 4: Rithub is
// live (git.ereboslab.com) with zero discovery/signup surface today. Signed
// out, the CTA goes through the same Console sign-in as the Insights
// cross-sell bridge (item 1); signed in, one click provisions a dedicated
// Gitea org + first repo via RithubProvision -- one sign-in across Console
// and Rithub, closed for real by the OIDC provider (item 5).
import { currentSession } from "../../lib/current.ts";
import { deps } from "../../lib/deps.ts";
import { RithubProvision } from "../../components/RithubProvision.tsx";

export const dynamic = "force-dynamic";

const SRC = "rithub-landing";

export default async function RithubLanding() {
  const { cfg } = deps();
  const session = await currentSession();
  const host = cfg.gitUrl.replace(/^https?:\/\//, "");
  const signupHref = `${cfg.consoleUrl}/auth/start?src=${SRC}`;
  return (
    <main style={{ maxWidth: 880, margin: "0 auto", padding: "48px 24px 80px" }}>
      <p className="muted" style={{ marginBottom: 6 }}>Rithub, by Railyard</p>
      <h1 style={{ fontSize: 32, marginBottom: 10 }}>Git hosting, issues, PRs and CI -- one sign-in, not a second account</h1>
      <p className="muted" style={{ maxWidth: 640 }}>
        Rithub is Railyard&apos;s code-hosting rail (go-gitea/gitea, MIT): push over HTTPS or SSH, open issues and
        pull requests with review and protected branches, run CI on a registered Actions runner, publish npm
        packages, track Git LFS binaries and keep a wiki -- live today at {host}. Sign in with your Railyard
        account and it provisions your own private org and first repo for you, instead of a separate native Gitea
        signup.
      </p>
      <section className="card">
        <h2>What you get</h2>
        <ul>
          <li>A private org and a first repo, created the moment you sign in -- no separate signup form.</li>
          <li>Issues and pull requests (merge, squash, rebase), code review, protected branches.</li>
          <li>Actions CI with a registered runner -- push a workflow, it runs.</li>
          <li>Git LFS, an npm package registry, and a wiki per repo.</li>
          <li>HTTPS and SSH push, the same Railyard account as Console, Relay and the rest of the rail.</li>
        </ul>
      </section>
      <section className="card" style={{ textAlign: "center" }}>
        <h2>Create your repo</h2>
        {session ? (
          <RithubProvision />
        ) : (
          <>
            <p className="muted">Sign in with Railyard to provision your org and first repo on Rithub.</p>
            <a className="primary" style={{ justifyContent: "center", display: "inline-flex", marginTop: 10 }} href={signupHref}>
              Sign in and create your repo
            </a>
          </>
        )}
      </section>
    </main>
  );
}
