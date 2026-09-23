import type { Metadata } from "next";
import { MarketingCallToAction, MarketingPage, MarketingSection, ProductHero } from "@hraness/design-kit/react/server";
import { AskAiAboutThis } from "@hraness/ui";
import { SiteHeader } from "../site-header";

const title = "Download xcb — build the native source preview";
const description = "Install xcb, the open-source terminal workspace for coding agents, from the verified repository. Requirements, exact build commands, and current platform limits.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/download" },
  openGraph: { title, description, siteName: "xcb", type: "website", url: "/download" },
  twitter: { card: "summary_large_image", title, description },
};

export default function Download() {
  return (
    <div data-hraness-marketing-preset="editorial" className="xcb-download-page">
      <SiteHeader active="docs" />
      <main id="main" tabIndex={-1}>
        <MarketingPage>
          <ProductHero
            align="start"
            className="xcb-download-hero"
            eyebrow="Download"
            name=""
            heading="Build xcb from the verified source."
            headingId="download-title"
            summary="No packaged release exists yet. The native Rust app installs from the repository with one build command."
            actions={[{ href: "/docs/getting-started", label: "Read the setup guide" }, { href: "https://github.com/hraness/xcb/releases", label: "Check release assets" }]}
            boundary="Source preview · MIT licensed · no published binary or npm package"
          />

          <MarketingSection
            id="install"
            heading="Install in four commands."
            headingId="install-title"
            summary="You need Git, Rust 1.97.1, and platform build tools. The installer compiles with the lockfile and places xcb in ~/.local/bin."
          >
            <pre className="install-command" tabIndex={0}><code>{`git clone https://github.com/hraness/xcb.git
cd xcb
rustup toolchain install 1.97.1 --profile minimal
./scripts/install-native.sh`}</code></pre>
            <p>The installer records its method and keeps a verified helper beside the binary for future upgrades. Use <code>XCB_INSTALL_PREFIX</code> to choose another prefix. If an older TypeScript CLI is installed under the same command name, run <code>command -v xcb</code> to see which one answers.</p>
          </MarketingSection>

          <MarketingSection
            id="platforms"
            heading="What runs where."
            headingId="platforms-title"
            layout="split"
            summary="The source preview covers specific tested builds, not every machine."
          >
            <div className="xcb-download-platforms">
              <div><h3>macOS ARM64</h3><p>The tested platform: Claude and Codex coding workflows passed, and the isolated Linux command runner is available. Claude also runs under an admitted Linux <code>bwrap</code> configuration.</p></div>
              <div><h3>Codex &amp; Devin</h3><p>The provider candidates currently require macOS and exact admitted builds. Devin’s credential-free boundary checks are separate from authenticated coding acceptance; account model availability is checked at launch.</p></div>
              <div><h3>Staying current</h3><p>Updates are user-level and release-based. <code>xcb update enable --policy notify</code> checks daily without replacing anything; <code>auto</code> installs only an exact stable archive with its adjacent checksum. No native release is published yet, so checks fail closed and leave a source install untouched.</p></div>
              <a className="xcb-text-link" href="/docs/workspace">Command-runner requirements ↗</a>
            </div>
          </MarketingSection>

          <MarketingSection
            id="lineage"
            heading="Coming from HRA or Oompa?"
            headingId="lineage-title"
            summary="xcb is the current name of the Hraness coding-agent workspace, previously released as HRA and then Oompa."
          >
            <p>The product lineage is continuous — accounts, local sessions, model choice, and usage in one terminal — but xcb is a fresh Rust codebase, not an in-place upgrade. Existing HRA or Oompa installs keep working as installed; new work starts from the xcb repository.</p>
          </MarketingSection>

          <MarketingCallToAction
            heading="Start with the guide."
            headingId="cta-title"
            summary="Connect a provider account, choose a model, and open your first project."
            actions={[{ href: "/docs/getting-started", label: "Get started ↗" }, { href: "/docs/providers", label: "Provider requirements" }]}
            footnote="xcb / Excalibur · Built by Hraness · MIT licensed"
          />
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh/download" />
    </div>
  );
}
