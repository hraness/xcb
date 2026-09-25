# Native Codex boundary qualification

The Rust adapter admits the exact macOS Codex executable identified in
`crates/xcb-runtime/src/codex/config.rs`. The inventory in
`codex-0.156.1-inventory.json` records synthetic model requests and callback
checks, and `codex-0.156.1-boundary.json` records the filesystem, process and
kernel checks. These probes exercise the production Seatbelt policy. None of
them logs in, reads existing credentials, or sends a model request.

Run on macOS with Python 3 and the admitted Codex executable:

```sh
python3 qualification/codex-native.py --executable /absolute/path/to/codex --output /absolute/private/evidence
python3 qualification/codex-kernel.py --output /absolute/private/evidence
python3 qualification/codex-inventory.py --executable /absolute/path/to/codex --output /absolute/private/evidence \
  --inventory qualification/codex-VERSION-inventory.json --wire-trace crates/xcb-runtime/src/codex/wire-echo-frames.json
```

## Admitting a new Codex build

Set `VERSION` and `BINARY_SHA256` in `config.rs` first; every fixture reads its
expected executable from there. The inventory fixture fails with the observed
digest until `SCHEMA_SHA256` matches the build's generated app-server schema.
It serves a loopback Responses endpoint through the production configuration
and a copy of the production policy whose only network allowance is that port,
and runs every catalog reasoning effort of each qualified model. Each case
checks that a thread without host tools sends an empty tool manifest, that a
thread with one host tool sends exactly that tool, that the permitted call
reaches the host and its result returns to the model, and that nine forged
builtin calls (`exec_command`, `apply_patch`, `spawn_agent` and others) are
answered `unsupported call` without reaching the host or leaving an effect.
`--wire-trace` records one permitted-callback turn with stable identifiers for
the Rust replay test. Also compare `codex features list` against
`ACCOUNT_FEATURES` and `EXTRA_FEATURES`: a feature the build turns on by default
must be reviewed, and the manifest checks show whether it changes what the
model can call.

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
only through the fixed `SSL_CERT_FILE` environment variable. The fixture pins
the rest of the provider's environment as well: `CODEX_HOME` points at the
disposable profile, `HOME`, `TMPDIR` and the `XDG_CONFIG_HOME`,
`XDG_DATA_HOME` and `XDG_CACHE_HOME` directories point inside the disposable
scratch home, `PATH` is the system default, `LANG` is fixed, and
`CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED` is set so no ambient user
state is read and the remote-control surface stays off. The fixture copies
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
