import { readFileSync } from "node:fs";
import { join } from "node:path";
import { defineSocialImageSite } from "@hraness/web-discovery/social-image/card";

import { productMessaging, productName } from "./messaging";

/**
 * The site's one social-image declaration. Every share card comes from the
 * shared @hraness/web-discovery template through this record; each route's
 * `opengraph-image` passes only its page copy from `social-cards.ts`. The icon
 * is the header's crossed-swords artwork, read from the repo so the card and
 * the header never drift. The theme is Tokyo Night light with a crimson
 * "blade" wash, which keeps xcb's card distinct from SWFT's pale blue.
 */
const markSvg = readFileSync(join(process.cwd(), "public/marks/xcb.svg"));

export const socialSite = defineSocialImageSite({
  description: productMessaging.tagline,
  domain: "xcb.sh",
  // Product names a card must not break across lines.
  keepTogether: ["Claude Code Router"],
  icon: { kind: "mark", src: `data:image/svg+xml;base64,${markSvg.toString("base64")}` },
  name: productName,
  theme: {
    accent: "#2e7de9",
    background: "#e1e2e7",
    foreground: "#3760bf",
    muted: "#6172b0",
    wash: "#E0061C",
  },
});
