"use client";
// Frame of every product analytics screen: title, the five screens, the app
// picker, and (on live screens) a quiet refresh every 10 seconds.
import { useEffect, type ReactNode } from "react";
import { usePathname, useRouter } from "next/navigation";

const SCREENS = [
  { href: "/product-analytics", label: "Events" },
  { href: "/product-analytics/web", label: "Web" },
  { href: "/product-analytics/persons", label: "People" },
  { href: "/product-analytics/setup", label: "Setup" },
  { href: "/product-analytics/privacy", label: "Privacy" },
];

export const REFRESH_MS = 10_000;

export function Shell({ title, apps, app, live = false, children }: { title: string; apps: string[]; app: string | null; live?: boolean; children: ReactNode }) {
  const path = usePathname();
  const router = useRouter();
  useEffect(() => {
    if (!live) return;
    const t = setInterval(() => router.refresh(), REFRESH_MS);
    return () => clearInterval(t);
  }, [live, router]);
  const q = app ? `?app=${encodeURIComponent(app)}` : "";
  return (
    <>
      <h1>{title}</h1>
      <nav className="row" aria-label="Product analytics" style={{ marginBottom: 18 }}>
        {SCREENS.map((s) => (
          <a key={s.href} href={`${s.href}${q}`} aria-current={path === s.href ? "page" : undefined} className="muted">
            {s.label}
          </a>
        ))}
        {apps.length > 1 ? (
          <span className="row" style={{ marginLeft: "auto" }}>
            {apps.map((name) => (
              <a key={name} href={`${path}?app=${encodeURIComponent(name)}`} aria-current={name === app ? "true" : undefined}>
                {name}
              </a>
            ))}
          </span>
        ) : null}
      </nav>
      {children}
    </>
  );
}
