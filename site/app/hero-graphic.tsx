"use client";

import { useEffect, useRef, useState } from "react";

const SWORD_FRAMES = 12;

function swordFrame(index: number) {
  return `/sword/frame-${String(index).padStart(2, "0")}.png`;
}

/** Rendered 3D sword; the pointer scrubs turntable frames so it reads as interactive. */
function Sword() {
  const [frame, setFrame] = useState(0);
  const loaded = useRef(false);

  useEffect(() => {
    if (loaded.current || window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    loaded.current = true;
    for (let i = 1; i < SWORD_FRAMES; i += 1) {
      const image = new Image();
      image.src = swordFrame(i);
    }
  }, []);

  return (
    <div
      className="xcb-sword"
      aria-hidden="true"
      onPointerMove={(event) => {
        const rect = event.currentTarget.getBoundingClientRect();
        const progress = (event.clientX - rect.left) / rect.width;
        setFrame(Math.min(SWORD_FRAMES - 1, Math.max(0, Math.floor(progress * SWORD_FRAMES))));
      }}
    >
      <img src={swordFrame(frame)} alt="" width="240" height="540" draggable="false" />
    </div>
  );
}

export function HeroGraphic() {
  return (
    <figure className="xcb-hero-visual">
      <div className="xcb-hero-providers" aria-hidden="true">
        <span>Claude</span>
        <span>Codex</span>
        <span>Devin</span>
      </div>
      <svg className="xcb-hero-connectors" viewBox="0 0 320 44" aria-hidden="true" preserveAspectRatio="none">
        <path d="M40 0 C 90 22, 120 30, 152 44" />
        <path d="M160 0 L 160 44" />
        <path d="M280 0 C 230 22, 200 30, 168 44" />
      </svg>
      <div className="xcb-hero-stage">
        <div className="xcb-terminal hraness-material-pane">
          <div className="xcb-terminal-bar"><span><img className="xcb-terminal-mark" src="/marks/xcb.svg" alt="" width="14" height="14" /> xcb <span className="xcb-terminal-path">/ your-project</span></span><span className="xcb-terminal-local">subscription router</span></div>
          <div className="xcb-terminal-body xcb-hero-terminal">
            <p className="xcb-terminal-prompt">Refactor the auth module. Use whichever account is free.</p>
            <ol className="xcb-lanes">
              <li>
                <strong>claude</strong><span className="xcb-lane-plan">max · sonnet 4.6</span>
                <span className="xcb-lane-state xcb-lane-live">working</span>
                <small>auth refactor</small>
              </li>
              <li>
                <strong>codex</strong><span className="xcb-lane-plan">plus · gpt-5.2-codex</span>
                <span className="xcb-lane-state xcb-lane-done">settled</span>
                <small>test repair</small>
              </li>
              <li>
                <strong>devin</strong><span className="xcb-lane-plan">—</span>
                <span className="xcb-lane-state">quota window</span>
                <small>resumes at reset</small>
              </li>
            </ol>
            <div className="xcb-terminal-input"><span aria-hidden="true">›</span> route → claude/max <span>lease held until exit proven</span></div>
          </div>
        </div>
        <Sword />
      </div>
      <figcaption>Illustrative router · no live provider calls.</figcaption>
    </figure>
  );
}
