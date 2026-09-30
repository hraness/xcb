import { readFileSync } from "node:fs";
import { join } from "node:path";
import { defineSocialImageSite } from "@hraness/web-discovery/social-image/card";

import { productMessaging, productName } from "./messaging";

/**
 * The site's one social-image declaration. Every share card comes from the
 * shared @hraness/web-discovery template through this record; each route's
 * `opengraph-image` passes only its page copy from `social-cards.ts`. The card
 * is a crop of the site's own sticky header and hero: the Tokyo Night palette
 * the layout sets as `data-palette`, and the header's crossed-swords mark
 * (read from the repo so the card and the header never drift) painted in foil
 * beside the product name as the header shows it.
 */
const markSvg = readFileSync(join(process.cwd(), "public/marks/xcb.svg"), "utf8");

export const socialSite = defineSocialImageSite({
  brand: productName,
  brandMark: markSvg,
  description: productMessaging.tagline,
  domain: "xcb.sh",
  // Product names a card must not break across lines.
  keepTogether: ["Claude Code Router"],
  name: productName,
  palette: "tokyo-night",
});
