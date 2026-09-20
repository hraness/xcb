# Native Codex boundary qualification

The Rust adapter admits the exact macOS Codex executable identified in
`crates/xcb-runtime/src/codex/config.rs`. The inventory in
`codex-0.155.0-alpha.2.6-inventory.json` records synthetic model requests and
callback checks. These separate probes exercise the production Seatbelt policy.
Neither probe logs in, reads existing credentials, or sends a model request.

Run on macOS with Python 3 and the admitted Codex executable:

```sh
python3 qualification/codex-native.py --executable /absolute/path/to/codex --output /absolute/private/evidence
python3 qualification/codex-kernel.py --output /absolute/private/evidence
```

On hosts using a resource scheduler, run both through its Mac-native lane. Each
invocation creates a unique disposable fixture directory and retains a JSON
receipt. The provider fixture reads the model catalog from the hash-checked
executable, reconstructs the current Rust configuration, and extracts the current
production policy. It fails if that source shape changes unexpectedly. It does
not read the user's Codex model cache or configuration.

The provider fixture verifies filesystem and process checks through the exact
app-server executable; the receipt records the observed checks and their results. Workspace, other-account and ambient-config canaries sit
outside the provider's disposable home. Direct paths and symlinks are exercised;
protected configuration/catalog writes, replacements and deletion are denied.
The run-owned public CA bundle lives outside writable scratch and is selected
only through the fixed `SSL_CERT_FILE` environment variable. The fixture copies
the OS-owned public `/private/etc/ssl/cert.pem` after ownership, mode, link-count,
size and stable-read checks; it reads no user trust store. Direct and symlink CA
reads must succeed with exact bytes, while writes, replacement and deletion must
fail. Protected file bytes, identity, permissions and link counts must remain
unchanged, including ctime, size and file flags. The fixture waits for bounded
macOS provenance bookkeeping to settle before recording its launch baseline;
setup snapshots are retained separately. The extracted policy must retain its denial of ambient Keychain and
trustd access. Provider and helper process execution must be denied. The receipt records process
exit, drained streams, process-group absence, immutable executable bytes and
unchanged policy source.

The kernel fixture applies that same policy to a trusted, already-loaded Python
process. This is a separate syscall test, not a substitute provider runtime. It
checks creation of hard links after sandbox entry, symlink writes, chmod and
rename, including direct and aliased CA mutation attempts. Scratch-only link operations serve as observational controls.

## Preplanted aliases

The optional `--preplanted-aliases` provider run deliberately creates an invalid
launch state. On the qualified macOS host, the OS alone permits writes through
those existing hard-link aliases; this negative control is expected to fail.
Production rejects protected launch files with more than one link before
protocol initialization. The Rust regression
`preplanted_config_or_catalog_hardlinks_are_rejected_before_initialization`
checks that rejection, and the kernel fixture verifies the sandbox cannot create
new protected-file aliases afterward. Both controls are necessary.

## Scope

Passing boundary checks is not proof of authenticated TLS transport, credential
refresh, live model behavior, or daily-driver readiness. Those require separate
bounded live checks. The current adapter exposes only the host workspace broker;
shell commands, builds, test execution and Git commands are not available.
