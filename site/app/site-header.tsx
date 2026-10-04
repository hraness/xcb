import { MarketingSiteHeader } from "@hraness/design-kit/react/server";
import { ThemeMenuButton } from "@hraness/design-kit/react";

import { productMessaging, productName } from "./messaging";

export function SiteHeader({ active }: Readonly<{ active?: "home" | "docs" | "compare" | "blog" | "install" }>) {
  return (
    <div data-hraness-marketing-preset="minimal" className="xcb-header-wrap">
      <a className="skip-link" href="#main">Skip to content</a>
      <MarketingSiteHeader
        ariaLabel="Primary"
        className="hraness-material-chrome"
        brand={productName}
        brandMark="/marks/xcb.svg"
        brandLabel={`${productName} home`}
        links={[
          { href: "/docs", label: "Docs", current: active === "docs" },
          { href: "/compare", label: "Compare", current: active === "compare" },
          { href: "/blog", label: "Blog", current: active === "blog" },
          { href: "https://github.com/hraness/xcb", label: "GitHub ↗" },
        ]}
        action={{ href: "/install", label: productMessaging.hero.primaryAction, emphasis: "secondary" }}
        trailing={<ThemeMenuButton aria-label="Appearance" />}
      />
    </div>
  );
}
