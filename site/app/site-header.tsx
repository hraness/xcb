import { MarketingSiteHeader } from "@hraness/design-kit/react/server";
import { ThemeMenuButton } from "@hraness/design-kit/react";

export function SiteHeader({ active }: Readonly<{ active?: "home" | "docs" | "compare" }>) {
  return (
    <div data-hraness-marketing-preset="editorial" className="xcb-header-wrap">
      <a className="skip-link" href="#main">Skip to content</a>
      <MarketingSiteHeader
        ariaLabel="Primary"
        className="hraness-material-chrome"
        brand="xcb"
        brandMark="/marks/xcb.svg"
        brandLabel="xcb home"
        links={[
          { href: "/#router", label: "The router" },
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
