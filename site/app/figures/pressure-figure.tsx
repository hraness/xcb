/**
 * The intelligence budget: the pool of workers grows and shrinks with the
 * pressure xcb measures. Thresholds are the defaults in docs/unattended.md
 * (disk warning 24 GiB, pause 8 GiB, resume 12 GiB; memory pressure that
 * lasts 60 seconds; samples every 30 seconds). The worker counts illustrate
 * one host and are not a measurement.
 */
const signals = [
  { label: "Memory pressure", value: "normal", level: 28, note: "defers new workers after 60 s of warning or critical pressure" },
  { label: "Disk, state volume", value: "41 GiB free", level: 18, note: "advisory at 24 GiB, pause at 8 GiB, resume above 12 GiB" },
  { label: "Quota across the pool", value: "3 of 4 accounts able to take work", level: 75, note: "an account at a known limit is skipped until it resets" },
] as const;

export function PressureFigure() {
  return (
    <figure className="xcb-pressure" aria-labelledby="pressure-caption">
      <div className="xcb-pressure__grid">
        <dl className="xcb-pressure__signals">
          {signals.map((signal) => (
            <div key={signal.label} className="xcb-pressure__signal">
              <dt>{signal.label}</dt>
              <dd>
                <span className="xcb-pressure__value">{signal.value}</span>
                <span className="xcb-pressure__bar" aria-hidden="true"><span style={{ inlineSize: `${signal.level}%` }} /></span>
                <span className="xcb-pressure__note">{signal.note}</span>
              </dd>
            </div>
          ))}
        </dl>
        <div className="xcb-pressure__pool" aria-hidden="true">
          <span className="xcb-pressure__pool-label">workers</span>
          <span className="xcb-pressure__pool-count">6</span>
          <span className="xcb-pressure__pool-dots">
            {Array.from({ length: 8 }, (_, index) => <span key={index} data-active={index < 6 ? "true" : "false"} />)}
          </span>
          <span className="xcb-pressure__pool-note">samples every 30 s</span>
        </div>
      </div>
      <figcaption id="pressure-caption">With resource protection on, xcb samples memory, swap, and disk every 30 seconds. When a threshold trips, it stops starting new workers and resumes after three healthy samples. The worker count here is an illustration.</figcaption>
    </figure>
  );
}
