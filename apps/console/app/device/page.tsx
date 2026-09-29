import { redirect } from "next/navigation";
import { currentSession } from "../../lib/current.ts";

export const dynamic = "force-dynamic";

export default async function DevicePage({ searchParams }: { searchParams: { code?: string; approved?: string; error?: string } }) {
  if (!(await currentSession())) redirect("/");
  return (
    <main className="center">
      <section className="card" style={{ width: 420 }}>
        <h1>Connect the Railyard CLI</h1>
        {searchParams.approved ? (
          <p>Done. Go back to your terminal.</p>
        ) : (
          <form method="post" action="/api/device/approve" className="row">
            <input type="text" name="code" defaultValue={searchParams.code ?? ""} placeholder="ABCD-EFGH" aria-label="Code from your terminal" required />
            <button className="primary" type="submit">Connect</button>
          </form>
        )}
        {searchParams.error ? <p className="error">That code is not valid any more. Run railyard login again.</p> : null}
      </section>
    </main>
  );
}
