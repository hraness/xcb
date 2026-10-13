import { MarketingProofFrame } from "@hraness/design-kit/react/server";

/**
 * The hero's proof object: the pool as a control panel. Three gauges show the
 * pressure xcb samples (memory, disk on the state volume, quota across the
 * accounts) and a dial shows how many workers the pool is running. The
 * thresholds are the defaults in docs/unattended.md (disk advisory 24 GiB,
 * pause 8 GiB, resume 12 GiB; memory pressure that lasts 60 seconds; a sample
 * every 30 seconds). The readings describe one example host.
 */
const gauges = [
  { id: "memory", label: "Memory pressure", value: "normal", level: 0.28, note: "defers workers after 60 s of pressure" },
  { id: "disk", label: "Disk, state volume", value: "41 GiB free", level: 0.18, note: "pauses at 8 GiB, resumes above 12 GiB" },
  { id: "quota", label: "Quota pressure", value: "3 of 4 able to take work", level: 0.25, note: "an account at its limit is skipped until it resets" },
] as const;

const workers = { running: 6, slots: 8 } as const;

const gaugeRadius = 44;
const gaugeLength = Math.PI * gaugeRadius;
const dialRadius = 54;
const dialLength = 2 * Math.PI * dialRadius;

function Gauge({ gauge }: Readonly<{ gauge: (typeof gauges)[number] }>) {
  return (
    <div className="xcb-pool__gauge" data-gauge={gauge.id}>
      <svg className="xcb-pool__arc" viewBox="0 0 120 66" aria-hidden="true" focusable="false">
        <path className="xcb-pool__track" d="M 16 60 A 44 44 0 0 1 104 60" />
        <path className="xcb-pool__fill" d="M 16 60 A 44 44 0 0 1 104 60" strokeDasharray={`${(gauge.level * gaugeLength).toFixed(1)} ${gaugeLength.toFixed(1)}`} />
        {[0.25, 0.5, 0.75].map((tick) => {
          const angle = Math.PI * (1 - tick);
          const x1 = 60 + Math.cos(angle) * 34;
          const y1 = 60 - Math.sin(angle) * 34;
          const x2 = 60 + Math.cos(angle) * 30;
          const y2 = 60 - Math.sin(angle) * 30;
          return <line className="xcb-pool__tick" key={tick} x1={x1.toFixed(1)} y1={y1.toFixed(1)} x2={x2.toFixed(1)} y2={y2.toFixed(1)} />;
        })}
      </svg>
      <span className="xcb-pool__value">{gauge.value}</span>
      <span className="xcb-pool__label">{gauge.label}</span>
      <span className="xcb-pool__note">{gauge.note}</span>
    </div>
  );
}

export function PoolPanel() {
  const describe = `Example pool panel. Memory pressure normal, 41 GiB free on the state volume, and three of four accounts able to take work, so the pool runs ${workers.running} of ${workers.slots} workers. xcb samples these every 30 seconds.`;
  return (
    <MarketingProofFrame chrome="window" className="xcb-pool-frame" title="Pool pressure">
      <figure className="xcb-pool" role="img" aria-label={describe}>
        <div className="xcb-pool__gauges" aria-hidden="true">
          {gauges.map((gauge) => <Gauge gauge={gauge} key={gauge.id} />)}
        </div>
        <div className="xcb-pool__dial" aria-hidden="true">
          <svg className="xcb-pool__ring" viewBox="0 0 120 120" focusable="false">
            <circle className="xcb-pool__track" cx="60" cy="60" r={dialRadius} />
            <circle className="xcb-pool__fill" cx="60" cy="60" r={dialRadius} strokeDasharray={`${((workers.running / workers.slots) * dialLength).toFixed(1)} ${dialLength.toFixed(1)}`} transform="rotate(-90 60 60)" />
            <text className="xcb-pool__count" x="60" y="66" textAnchor="middle">{workers.running}</text>
          </svg>
          <span className="xcb-pool__dots">
            {Array.from({ length: workers.slots }, (_, index) => <span key={index} data-active={index < workers.running ? "true" : "false"} />)}
          </span>
          <span className="xcb-pool__label">workers running</span>
          <span className="xcb-pool__note">samples every 30 s</span>
        </div>
      </figure>
    </MarketingProofFrame>
  );
}
