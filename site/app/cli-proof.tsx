/** Exact help excerpt from the SHA-256-verified v0.10.1 macOS ARM64 archive. */
export function CliProof() {
  return (
    <figure className="xcb-cli-proof">
      <figcaption>Preview a route before reserving an account</figcaption>
      <pre tabIndex={0}><code>{`$ xcb models route --help
Preview managed routing for --cwd without reserving an account. Uses the configured judge when
enabled; selection may change before execution

Usage: xcb models route [OPTIONS] --task <TASK>`}</code></pre>
      <p>Help excerpt from xcb 0.10.1.</p>
      <nav className="xcb-cli-proof__links" aria-label="CLI proof references">
        <a href="https://github.com/hraness/xcb/releases/tag/v0.10.1">Inspect the release</a>
        <a href="/docs/route">Read the routing contract</a>
      </nav>
    </figure>
  );
}
