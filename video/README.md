# xcb launch film

A captioned film with no narration, built from the Slopcamera `launch-film`
template. The scenes draw the same illustrations as the site: `mockups.tsx`
renders `site/app/mockups/surfaces.tsx` with the fixtures in
`site/app/mockups/fixtures.ts`, and `copy.ts` takes each number from `site/app/launch/facts.ts`.

The earlier three-provider film remains an unchanged historical asset in
`site/public/media/`, but `launchFilm` is disabled so the current post uses the
router illustration. Update and inspect the film against the current
agent-first interface and Claude/Codex support before enabling it again.
Keep an archived film's captions consistent with its original picture.

Run these in `video/`, one at a time; each render is heavy.

```sh
bun install
bun run still             # landscape stills to out/stills
bun run still:portrait    # native 9:16 stills to out/portrait/stills
bun run render            # landscape MP4, writes out/export.json
bun run deliver           # MP4, WebM, poster, social still, 1:1 cut, per-beat clips
bun run render:portrait   # native 9:16 film
bun run deliver:portrait
```

Copy the delivered files into `site/public/media/` with the `xcb-launch`
basename, then set `launchFilm` in `site/app/launch/film.ts`. Until then the
launch post opens with the interactive router illustration and never points at
a missing file.
