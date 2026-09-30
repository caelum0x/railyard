// The service as its own process for the capacity run: the real app, writer
// and scheduler, a memory site directory with one app, on a given port.
import { serve } from "@hono/node-server";
import { createService } from "../../src/app.ts";
import { ensurePartitions } from "../../src/jobs/retention.ts";
import { silentLogger } from "../../src/log.ts";
import { MemorySiteDirectory } from "../../src/tenancy/site-directory.ts";

const site = JSON.parse(process.env.CAPACITY_SITE!);
const svc = createService({ databaseUrl: process.env.DATABASE_URL!, sites: new MemorySiteDirectory([site]), log: silentLogger });
await ensurePartitions(svc.deps.db, new Date());
svc.deps.writer.start();
svc.scheduler.start();
serve({ fetch: svc.app.fetch, port: Number(process.env.PORT), hostname: "127.0.0.1" }, () => process.send?.("ready"));
process.on("SIGTERM", async () => {
  await svc.close();
  process.exit(0);
});
