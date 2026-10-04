import { createSocialImageResponse, socialImageSiteDetails } from "@hraness/web-discovery/social-image";
import { socialCard } from "./social-cards";
import { socialSite } from "./social";

export { socialImageContentType as contentType, socialImageSize as size } from "@hraness/web-discovery/social-image";

/**
 * Renders the share card declared for `path`. `strict` makes a card whose copy
 * would be cut, shrunk, or stripped fail the build instead of shipping.
 */
export function socialImageFor(path: string) {
  return createSocialImageResponse({ ...socialImageSiteDetails(socialSite, socialCard(path)), strict: true });
}
