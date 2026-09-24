/**
 * The site's one social card, served by `opengraph-image.tsx`. Pages that set
 * their own `openGraph` metadata replace the inherited card, so they list it
 * explicitly through these constants.
 */
export const socialImageAlt = "Social card with the xcb mark and the words “One router for your Claude, Codex, and Devin subscriptions”";

export const socialImages = [{ url: "/opengraph-image", width: 1200, height: 630, alt: socialImageAlt }];
