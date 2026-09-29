import { socialCardAlt } from "../social-cards";
import { socialImageFor } from "../social-image";

export { contentType, size } from "../social-image";
export const alt = socialCardAlt("/install");

export default function OpengraphImage() {
  return socialImageFor("/install");
}
