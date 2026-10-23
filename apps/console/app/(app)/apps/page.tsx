import { AddApp } from "../../../components/AddApp.tsx";
import { AppBillingAccount } from "../../../components/AppBillingAccount.tsx";
import { installSnippet } from "../../../lib/apps.ts";
import { currentSession } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { access } from "../../../lib/ladder.ts";

export default async function AppsPage() {
  const { repo, cfg } = deps();
  const s = (await currentSession())!;
  const [apps, ent, deploys] = await Promise.all([repo.listApps(s.org), repo.getEntitlement(s.org), repo.listDeploys(s.org)]);
  const names = new Map(apps.map((x) => [x.id, x.name]));
  const a = access(ent, new Date());
  return (
    <>
      <h1>Apps</h1>
      <section className="card">
        {a.canAddApps ? (
          <AddApp remaining={Math.max(0, a.appsLimit - apps.length)} />
        ) : (
          <div className="row">
            <a className="primary" href="/billing">{a.state === "paused" ? "Resume Railyard" : "Add a card to start ($1, credited back)"}</a>
          </div>
        )}
      </section>
      {apps.length ? (
        <section className="card">
          <ul className="list">
            {apps.map((app) => {
              const sn = installSnippet(app, cfg.insightsUrl);
              return (
                <li key={app.id}>
                  <strong>{app.name}</strong>
                  <code className="muted">{sn.deploy}</code>
                  <AppBillingAccount appId={app.id} mintAccountId={app.mint_account_id} />
                </li>
              );
            })}
          </ul>
        </section>
      ) : null}
      {deploys.length ? (
        <section className="card">
          <h2>Recent deploys</h2>
          <ul className="list">
            {deploys.slice(0, 20).map((d) => (
              <li key={d.id}>
                <span>
                  <strong>{names.get(d.app_id)}</strong> <span className="muted">{d.branch}</span>
                </span>
                {d.status === "succeeded" ? <a href={d.url}>{d.url.replace(/^https:\/\//, "")}</a> : <span className="error">{d.status}</span>}
              </li>
            ))}
          </ul>
        </section>
      ) : null}
    </>
  );
}
