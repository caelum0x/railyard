import { AddApp } from "../../../components/AddApp.tsx";
import { installSnippet } from "../../../lib/apps.ts";
import { currentSession } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { access } from "../../../lib/ladder.ts";

export default async function AppsPage() {
  const { repo, cfg } = deps();
  const s = (await currentSession())!;
  const [apps, ent] = await Promise.all([repo.listApps(s.org), repo.getEntitlement(s.org)]);
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
                </li>
              );
            })}
          </ul>
        </section>
      ) : null}
    </>
  );
}
