import {
  createSiteSocialImageResponse,
  socialImageContentType as contentType,
  socialImageSize as size,
} from "@hraness/web-discovery/social-image";
import { socialSite } from "./social";

export { socialImageAlt as alt } from "./social";
export { contentType, size };

export default function OpengraphImage() {
  return createSiteSocialImageResponse(socialSite);
}
