"use client";
import { usePathname } from "next/navigation";
import { SignOutIcon } from "./Icons.tsx";

const LINKS = [
  { href: "/apps", label: "Apps" },
  { href: "/insights", label: "Insights" },
  { href: "/product-analytics", label: "Product analytics" },
  { href: "/usage", label: "Usage" },
  { href: "/billing", label: "Billing" },
];

export function Nav() {
  const path = usePathname();
  return (
    <nav className="nav" aria-label="Main">
      <div className="brand">Railyard</div>
      {LINKS.map((l) => (
        <a key={l.href} href={l.href} aria-current={path?.startsWith(l.href) ? "page" : undefined}>
          {l.label}
        </a>
      ))}
      <form method="post" action="/auth/signout" style={{ marginTop: "auto" }}>
        <button className="icon" type="submit" aria-label="Sign out" title="Sign out">
          <SignOutIcon />
        </button>
      </form>
    </nav>
  );
}
