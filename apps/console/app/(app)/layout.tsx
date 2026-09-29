import { redirect } from "next/navigation";
import type { ReactNode } from "react";
import { Nav } from "../../components/Nav.tsx";
import { currentSession } from "../../lib/current.ts";

export const dynamic = "force-dynamic";

export default async function AppLayout({ children }: { children: ReactNode }) {
  if (!(await currentSession())) redirect("/");
  return (
    <div className="shell">
      <Nav />
      <main className="main">{children}</main>
    </div>
  );
}
