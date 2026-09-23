import type { Metadata } from "next";
import { MarketingCallToAction, MarketingPage, MarketingSection, ProductHero } from "@hraness/design-kit/react/server";
import { AskAiAboutThis } from "@hraness/ui";
import { publishedRelease } from "../publication";
import { CompatibilityArchive, NativeDownloads } from "../release-state";
import { SiteHeader } from "../site-header";
import { socialImages } from "../social";

const title = "Download xcb · Verified releases and the source build";
const description = publishedRelease === null
  ? "Build xcb, the router for your Claude, Codex, and Devin subscriptions, from source. Verified macOS ARM64 and Linux x86_64 archives appear here once published."
  : `Download xcb v${publishedRelease.version}, the router for your Claude, Codex, and Devin subscriptions, with checksums and a public verification run, or build it from source.`;

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/download" },
  openGraph: { title, description, siteName: "xcb", type: "website", url: "/download", images: socialImages },
  twitter: { card: "summary_large_image", title, description, images: socialImages },
};

export default function Download() {
  const release = publishedRelease;
  return (
    <div data-hraness-marketing-preset="editorial" className="xcb-download-page">
      <SiteHeader />
      <main id="main" tabIndex={-1}>
        <MarketingPage>
          <ProductHero
            align="start"
            className="xcb-download-hero"
            eyebrow="Download"
            name=""
            heading={release === null ? "Build xcb from source." : `Get xcb v${release.version}.`}
            headingId="download-title"
            summary={release === null
              ? "Build xcb from the repository with its installer script. Verified release archives appear here once they are published."
              : "Download the verified native archive for your platform, or build the same version from source. Every archive ships with its SHA-256 checksum and a public verification run."}
            actions={[{ href: "#release", label: release === null ? "Release status" : "Download" }, { href: "/docs/getting-started", label: "Read the setup guide" }]}
            boundary="Source preview · MIT licensed · release targets: macOS ARM64 and Linux x86_64"
          />

          <MarketingSection
            id="release"
            heading={release === null ? "Latest verified release: none yet." : `Latest verified release: v${release.version}.`}
            headingId="release-title"
            summary="This page lists a release only after its archives pass the public verification run."
          >
            <NativeDownloads release={release} />
            <CompatibilityArchive release={release} />
          </MarketingSection>

          <MarketingSection
            id="install"
            heading="Install from source."
            headingId="install-title"
            summary="You need Git, Rust 1.97.1, and platform build tools. The installer compiles with the lockfile and places xcb in ~/.local/bin."
          >
            <pre className="install-command" tabIndex={0}><code>{`git clone https://github.com/hraness/xcb.git
cd xcb
rustup toolchain install 1.97.1 --profile minimal
./scripts/install-native.sh`}</code></pre>
            <p>The installer records its method and keeps a verified helper beside the binary for future upgrades. Use <code>XCB_INSTALL_PREFIX</code> to choose another prefix. The TypeScript compatibility CLI installs as <code>xcb-compat</code>, so it does not shadow the native command; if an older compatibility install still answers to <code>xcb</code>, <code>command -v xcb</code> shows which binary is active.</p>
          </MarketingSection>

          <MarketingSection
            id="platforms"
            heading="What runs where."
            headingId="platforms-title"
            layout="split"
            summary="Release builds cover two platforms. Provider support is narrower and rests on specific tested builds, not every machine."
          >
            <div className="xcb-download-platforms">
              <div><h3>Release binaries</h3><p>Built for macOS ARM64 (<code>darwin-aarch64</code>) and Linux x86_64 (<code>linux-x86_64</code>) as <code>xcb-&lt;version&gt;-&lt;platform&gt;.tar.gz</code> with an adjacent checksum. Other hosts build from source with the same installer.</p></div>
              <div><h3>macOS ARM64</h3><p>The tested platform: Claude and Codex coding workflows passed, and the isolated Linux command runner is available. Claude also runs under a supported Linux <code>bwrap</code> sandbox configuration.</p></div>
              <div><h3>Codex &amp; Devin</h3><p>Both currently require macOS and the exact supported provider builds. The supported Devin builds pass xcb’s sandbox checks without signing in, but a coding session on a signed-in Devin account hasn’t been confirmed. xcb checks your account’s model list when a Devin turn starts.</p></div>
              <div><h3>Staying current</h3><p>Updates are user-level and release-based. <code>xcb update check</code> and <code>xcb upgrade</code> work on both release platforms; the daily scheduler behind <code>xcb update enable --policy notify</code> is macOS-only, and <code>auto</code> installs only an exact stable archive with its adjacent checksum. On any other host, or when no verified release exists, the updater installs nothing and leaves a source install untouched.</p></div>
              <a className="xcb-text-link" href="/docs/workspace">Command-runner requirements →</a>
            </div>
          </MarketingSection>

          <MarketingSection
            id="lineage"
            heading="Coming from HRA or Oompa?"
            headingId="lineage-title"
            summary="xcb is the current name of the Hraness coding-agent workspace, previously released as HRA and then Oompa."
          >
            <p>xcb keeps the same idea as HRA and Oompa: accounts, local sessions, model choice, and usage in one terminal. It is a fresh Rust codebase, so it does not upgrade an existing install in place. Existing HRA or Oompa installs keep working as installed; new work starts from the xcb repository.</p>
          </MarketingSection>

          <MarketingCallToAction
            heading="Start with the guide."
            headingId="cta-title"
            summary="Connect a provider account, choose a model, and open your first project."
            actions={[{ href: "/docs/getting-started", label: "Get started" }, { href: "/docs/providers", label: "Provider requirements" }]}
            footnote="xcb / Excalibur · Built by Hraness · MIT licensed"
          />
        </MarketingPage>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh/download" />
    </div>
  );
}
