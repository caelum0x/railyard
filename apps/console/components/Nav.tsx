"use client";
import { usePathname } from "next/navigation";
import { SignOutIcon } from "./Icons.tsx";

const LINKS = [
  { href: "/apps", label: "Apps" },
  { href: "/insights", label: "Insights" },
  { href: "/flags", label: "Flags" },
  { href: "/product-analytics", label: "Product analytics" },
  { href: "/usage", label: "Usage" },
  { href: "/billing", label: "Billing" },
];

/**
 * `watch` adds the Watch screen once RAILYARD_WATCH_LIVE is on (PY2.2).
 * `gitUrl` is always shown as an external "Code" link to Rithub (apps/gitea) —
 * code hosting has no Railyard-session gate of its own yet (native Gitea
 * accounts), so it is a plain outbound link, not an internal route.
 */
export function Nav({ watch = false, gitUrl }: { watch?: boolean; gitUrl?: string }) {
  const path = usePathname();
  const links = watch ? [...LINKS.slice(0, 3), { href: "/watch", label: "Watch" }, ...LINKS.slice(3)] : LINKS;
  return (
    <nav className="nav" aria-label="Main">
      <div className="brand">Railyard</div>
      {links.map((l) => (
        <a key={l.href} href={l.href} aria-current={path?.startsWith(l.href) ? "page" : undefined}>
          {l.label}
        </a>
      ))}
      {gitUrl && (
        <a href={gitUrl} target="_blank" rel="noopener noreferrer">
          Code
        </a>
      )}
      <form method="post" action="/auth/signout" style={{ marginTop: "auto" }}>
        <button className="icon" type="submit" aria-label="Sign out" title="Sign out">
          <SignOutIcon />
        </button>
      </form>
    </nav>
  );
}
