import type { Metadata, Viewport } from "next";
import { getDesignPaletteTheme } from "@hraness/design-kit";
import { DesignPaletteProvider, ThemeColorSync } from "@hraness/design-kit/react";
import { HranessSiteFooter } from "@hraness/site-footer/react";
import { siteDefaultPalette } from "../palette";
import { supportProfile } from "../../src/support-profile";
import { productMessaging, productName } from "./messaging";
import "./globals.css";
import "./docs/docs.css";
import "./compare/compare.css";
import "./blog/blog.css";
import "./calm.css";

/**
 * Tokyo Night is the site's own palette; the initial class supplies its compiled
 * values and the blocking bootstrap adds a concrete `data-theme` before
 * paint. With JavaScript disabled no `data-theme` is rendered, so the palette's
 * light-dark() colors keep following the operating system.
 */
const initialPalette = getDesignPaletteTheme("tokyo-night", "light");

const title = `${productName} · ${productMessaging.hero.heading.replace(/\.$/u, "")}`;
const description = productMessaging.meta;

export const metadata: Metadata = {
  metadataBase: new URL("https://xcb.sh"),
  title,
  description,
  alternates: { canonical: "/" },
  icons: {
    icon: [{ type: "image/svg+xml", url: "/favicon.svg" }],
    apple: [{ url: "/apple-icon.png", sizes: "180x180" }],
  },
  openGraph: {
    title,
    description,
    siteName: productName,
    type: "website",
    url: "/",
  },
  twitter: {
    card: "summary_large_image",
    title,
    description,
  },
};

export const viewport: Viewport = {
  themeColor: [
    { color: "#e1e2e7", media: "(prefers-color-scheme: light)" },
    { color: "#1a1b26", media: "(prefers-color-scheme: dark)" },
  ],
};

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html
      lang="en"
      data-hraness-theme="paper"
      data-hraness-material="lantern"
      data-palette="tokyo-night"
      data-hraness-pattern="none"
      className={initialPalette.className}
      suppressHydrationWarning
    >
      <head>
        {/* The blocking external bootstrap applies a saved palette before first paint. */}
        {/* eslint-disable-next-line @next/next/no-sync-scripts */}
        <script src="/theme-bootstrap.js" />
      </head>
      <body>
        <DesignPaletteProvider defaultPreference={siteDefaultPalette}>
          <ThemeColorSync />
          {children}
          <div className="network-footer">
            <HranessSiteFooter mailingList={{ kind: "none" }} support={supportProfile} />
          </div>
        </DesignPaletteProvider>
      </body>
    </html>
  );
}
