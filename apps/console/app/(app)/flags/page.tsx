import { FlagRules, FlagSegments, type RuleView } from "../../../components/FlagRules.tsx";
import { FlagsPanel } from "../../../components/FlagsPanel.tsx";
import { currentSession } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { listFlags, listSegments } from "../../../lib/flags/service.ts";

// Flags per app environment (Wave P PY2.1, RYP-E2). A flip reaches apps on
// their next read (the SDK reads every 30 s; reads are cached 15 s). Rules
// and % rollouts per flag, and the environment's segments (QY1.3).
export default async function FlagsPage() {
  const { repo, flags, cfg } = deps();
  const s = (await currentSession())!;
  const [envs, apps] = await Promise.all([flags.store.listEnvs(s.org), repo.listApps(s.org)]);
  const withFlags = await Promise.all(
    envs.map(async (env) => ({ env, flags: await listFlags(flags, env).catch(() => null), segments: await listSegments(flags, env).catch(() => []) })),
  );
  const unset = apps.filter((a) => !envs.some((e) => e.app === a.name));
  return (
    <>
      <h1>Flags</h1>
      {withFlags.length === 0 && unset.length === 0 ? (
        <section className="card">
          <p className="muted">No apps yet. Add an app first, or run the add command in a Scribase project folder.</p>
        </section>
      ) : null}
      {withFlags.map(({ env, flags: list, segments }) => (
        <section className="card" key={env.id}>
          <h2>
            {env.app} <span className="muted">{env.env}</span>
          </h2>
          <p className="muted">
            Flags key for this environment: <code>{env.client_key}</code>
            {env.first_read_at ? ` · first read by the app ${env.first_read_at.slice(0, 16).replace("T", " ")} UTC` : " · not read by the app yet"}
          </p>
          {list ? <FlagsPanel app={env.app} env={env.env} flags={list.map((f) => ({ key: f.key, enabled: f.enabled, value: f.value }))} /> : <p className="error">The flags service did not answer. Try again shortly.</p>}
          {list?.length ? (
            <details>
              <summary>Who gets each flag</summary>
              {list.map((f) => (
                <div key={f.key}>
                  <h3>{f.key}</h3>
                  <FlagRules app={env.app} env={env.env} flag={{ key: f.key, enabled: f.enabled, rules: (f.targeting?.rules as RuleView[] | undefined) ?? null }} segments={segments.map((s) => s.key)} groups={cfg.replayLive} />
                </div>
              ))}
              <FlagSegments app={env.app} env={env.env} segments={segments.map((s) => ({ key: s.key, name: s.name, conditions: s.conditions as RuleView["conditions"] }))} />
            </details>
          ) : null}
        </section>
      ))}
      {unset.map((a) => (
        <section className="card" key={a.id}>
          <h2>
            {a.name} <span className="muted">production</span>
          </h2>
          <FlagsPanel app={a.name} env="production" flags={[]} />
        </section>
      ))}
    </>
  );
}
