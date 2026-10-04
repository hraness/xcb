import type { ArticleVideoRecord } from "@hraness/design-kit";

/**
 * The launch film embedded at the top of the launch post. The source is in
 * video/story/ (story.config.ts); `bash render-all.sh xcb-launch` there, then
 * `bun publish.ts`, writes the files below into public/media/. The 1:1 cut
 * (`/media/xcb-launch-1x1.mp4`) is for social posts and is not embedded.
 * tests/launch.test.ts fails when any file named here is missing.
 */
export const launchFilm: Readonly<{ video: ArticleVideoRecord }> | null = {
  video: {
    name: "Introducing Excalibur",
    description:
      "A captioned introduction to xcb: two coding plans with separate logins and limits, one task sent to the account with unused quota closest to its reset, tasks moved at a usage limit, and how to ask your agent to install it.",
    sources: [
      { src: "/media/xcb-launch.webm", type: "video/webm" },
      { src: "/media/xcb-launch.mp4", type: "video/mp4" },
    ],
    poster: "/media/xcb-launch-poster.jpg",
    captions: "/media/xcb-launch.vtt",
    width: 1920,
    height: 1080,
    duration: "PT33.6S",
    uploadDate: "2026-10-04",
  },
};
