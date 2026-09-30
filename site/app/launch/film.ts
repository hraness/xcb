import type { ArticleVideoRecord } from "@hraness/design-kit";

/**
 * The launch film embedded at the top of the launch post. The source is in
 * video/ at the repo root; video/README.md has the render and deliver
 * commands, which write the files below into public/media/. The 1:1 cut
 * (`/media/xcb-launch-1x1.mp4`) is for social posts and is not embedded.
 * tests/launch.test.ts fails when any file named here is missing.
 */
export const launchFilm: Readonly<{ video: ArticleVideoRecord }> | null = {
  video: {
    name: "Introducing Excalibur",
    description:
      "A 42-second film with captions and no narration. It opens on one plan at its limit while the others sit unused, then the title card. An xcb thread picks the project and the account; the Claude account whose quota resets first takes the task; a limit mid-task moves the task to another account; xcb tasks and xcb attention show work running in the background; and xcb fleet sends a task from a laptop to a desktop. It closes on what xcb does not do: it raises no usage limit.",
    sources: [
      { src: "/media/xcb-launch.webm", type: "video/webm" },
      { src: "/media/xcb-launch.mp4", type: "video/mp4" },
    ],
    poster: "/media/xcb-launch-poster.jpg",
    captions: "/media/xcb-launch.vtt",
    width: 1920,
    height: 1080,
    duration: "PT42.1S",
    uploadDate: "2026-09-29",
  },
};
