// The service as its own process for the A3 capacity run: the real app,
// writer, scheduler and A3 jobs (exports every 30 s), recordings written to a
// directory store, one app with recording on.
import { serve } from "@hono/node-server";
import { createService } from "../../src/app.ts";
import { ensurePartitions } from "../../src/jobs/retention.ts";
import { silentLogger } from "../../src/log.ts";
import { MemorySiteDirectory } from "../../src/tenancy/site-directory.ts";

const site = JSON.parse(process.env.CAPACITY_SITE!);
const svc = createService({ databaseUrl: process.env.DATABASE_URL!, sites: new MemorySiteDirectory([site]), log: silentLogger, consoleSecret: process.env.CAPACITY_CONSOLE_SECRET! });
await ensurePartitions(svc.deps.db, new Date());
svc.deps.writer.start();
svc.scheduler.start();
if (process.env.CAPACITY_MEMLOG) {
  // Where the memory is: V8 heap, external (Buffers, ArrayBuffers) and the rest.
  setInterval(() => {
    const m = process.memoryUsage();
    const mib = (n: number) => Math.round((n / 1048576) * 10) / 10;
    process.stderr.write(`${JSON.stringify({ rss: mib(m.rss), heapTotal: mib(m.heapTotal), heapUsed: mib(m.heapUsed), external: mib(m.external), arrayBuffers: mib(m.arrayBuffers) })}\n`);
  }, 15_000).unref();
}
serve({ fetch: svc.app.fetch, port: Number(process.env.PORT), hostname: "127.0.0.1" }, () => process.send?.("ready"));
process.on("SIGTERM", async () => {
  await svc.close();
  process.exit(0);
});
