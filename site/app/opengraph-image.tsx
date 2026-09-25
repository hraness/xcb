import {
  createSocialImageResponse,
  socialImageContentType as contentType,
  socialImageSize as size,
} from "@hraness/web-discovery/social-image";

export { socialImageAlt as alt } from "./social";
export { contentType, size };

function XcbMark() {
  return (
    <svg aria-label="xcb mark" fill="none" height="42" role="img" viewBox="0 0 42 42" width="42">
      <path d="M21 4v33M9 15h24" stroke="currentColor" strokeLinecap="round" strokeWidth="4" />
    </svg>
  );
}

export default function OpengraphImage() {
  return createSocialImageResponse({
    description: "One terminal for your Claude, Codex, and Devin accounts. Each task runs on an account that is signed in, idle, and not at a known limit.",
    domain: "xcb.sh",
    eyebrow: "xcb",
    mark: <XcbMark />,
    theme: {
      accent: "#8A5A28",
      background: "#F8F7F4",
      foreground: "#1C1A18",
      muted: "#6A655E",
    },
    title: "Keep coding when one subscription hits its limit.",
  });
}
