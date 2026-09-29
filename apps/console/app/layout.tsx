import "./globals.css";
import type { ReactNode } from "react";

export const metadata = { title: "Railyard", description: "Ship it, see who uses it, from your coding agent." };

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
