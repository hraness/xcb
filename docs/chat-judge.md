# Optional API judges

Excalibur (xcb) can ask an API model to classify a bounded state and questions.
The result advises routing, continuation and compaction. Existing permissions,
account checks and process recovery rules still decide whether work may run.
The default judge configuration remains disabled.

Select direct xAI with Grok 4.7:

```sh
xcb judge select xai
xcb judge test
xcb judge test --live
xcb judge enable
xcb judge status --json
```

The model defaults to `grok-4.7`, served at
`https://api.x.ai/v1/chat/completions`. `XAI_API_KEY` supplies the credential.
For a service that must survive reboot without an interactive shell, pipe the
key from your trusted secret loader to `xcb judge token --provider xai`.
The command accepts stdin only and saves a separate private file under the
xcb state directory. It refuses to overwrite an existing key. Rotate it with
`xcb judge logout --provider xai`, then import the replacement. Environment
keys take precedence; an invalid environment key fails instead of silently
using a stored key. Avoid placing keys in shell arguments, launchd plists,
project files or provider account homes.

Vercel AI Gateway uses a separate credential and endpoint:

```sh
xcb judge select vercel
xcb judge test
```

Its default model is `spacexai/grok-4.7`, endpoint is
`https://ai-gateway.vercel.sh/v1/chat/completions`, and key variable is
`AI_GATEWAY_API_KEY`. `xcb judge token --provider vercel` stores a separate
key for that canonical endpoint. A direct xAI key does not authenticate the
gateway. Configure gateway billing or provider credentials separately.

Other compatible services require an explicit full HTTPS endpoint, model
and environment variable name:

```sh
xcb judge select openai-compatible --model model-id --endpoint https://judge.example/v1/chat/completions --key-env CUSTOM_JUDGE_KEY
```

Custom services read only that named variable and cannot access the xAI,
gateway or legacy System One vault. Their host service must securely load the
variable on every start. Known vendor variable names cannot be redirected to
another vendor's endpoint. Named xAI and gateway endpoints are fixed.

Selection saves the configuration with the judge disabled. `test` checks the
configuration and credential locally; `test --live` sends exactly one synthetic
arithmetic request without project context. Enabling the judge permits callers
to send their bounded task state, so choose a provider appropriate for that data.
No command here creates a recurring schedule or starts a background service.

Each call has a 45-second deadline, a 2,048-token output cap and 256 KiB request
and response limits. Calls use JSON output without tools, redirects, automatic
retries or alternate-provider fallback. Truncated output, extra answer fields,
missing questions, out-of-range scores and unknown choices fail validation.
Callers retain their deterministic behavior when judgment is unavailable.
These are separately billed API requests; subscription coding usage does not
cover them. The adapter does not maintain a monthly API spending limit.

Clef and legacy System One configurations keep their existing behavior.
Their credentials remain separate from chat providers. API keys stored by
xcb are private local files, not OS-keychain entries; protect the state directory
and its recovery copies as credentials. `status` reports the key source without
printing the key.
