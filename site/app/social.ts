import { readFileSync } from "node:fs";
import { join } from "node:path";
import { defineSocialImageSite, socialImageAlt as altFor } from "@hraness/web-discovery/social-image/card";

/**
 * The site's one social-image declaration. Every share card comes from the
 * shared @hraness/web-discovery template through this record; pages pass copy
 * only. The icon is the header's crossed-swords artwork, read from the repo
 * so the card and the header never drift. The theme is Tokyo Night light.
 */
const markSvg = readFileSync(join(process.cwd(), "public/marks/xcb.svg"));

export const socialSite = defineSocialImageSite({
  description: "Route coding tasks across the Claude, Codex, and Devin plans you pay for.",
  domain: "xcb.sh",
  icon: { kind: "mark", src: `data:image/svg+xml;base64,${markSvg.toString("base64")}` },
  name: "xcb",
  theme: {
    accent: "#2e7de9",
    background: "#e1e2e7",
    foreground: "#3760bf",
    muted: "#6172b0",
  },
});

export const socialImageAlt = altFor(socialSite);

/**
 * Pages that set their own `openGraph` metadata replace the inherited card, so
 * they list it explicitly through this constant.
 */
export const socialImages = [{ url: "/opengraph-image", width: 1200, height: 630, alt: socialImageAlt }];
