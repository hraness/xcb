import { MarketingSiteHeader } from "@hraness/design-kit/react/server";
import { ThemeMenuButton } from "@hraness/design-kit/react";

function BrandMark() {
  // eslint-disable-next-line @next/next/no-img-element -- fixed-size authored SVG; no raster optimization needed
  return <img src="/marks/xcb.svg" alt="" aria-hidden="true" width="22" height="22" />;
}

export function SiteHeader({ active }: Readonly<{ active?: "home" | "docs" | "compare" }>) {
  return (
    <div data-hraness-marketing-preset="editorial" className="xcb-header-wrap">
      <a className="skip-link" href="#main">Skip to content</a>
      <MarketingSiteHeader
        ariaLabel="Primary"
        className="hraness-material-chrome"
        brand={<><BrandMark />xcb</>}
        brandLabel="xcb home"
        links={[
          { href: "/#workspace", label: "Why xcb" },
          { href: "/compare", label: "Compare", current: active === "compare" },
          { href: "/docs", label: "Docs", current: active === "docs" },
          { href: "https://github.com/hraness/xcb", label: "GitHub" },
        ]}
        action={{ href: "/docs/getting-started", label: "Get started" }}
        trailing={<ThemeMenuButton aria-label="Appearance" />}
      />
    </div>
  );
}
