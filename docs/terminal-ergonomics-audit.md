# Terminal ergonomics audit

Scope: native account setup and terminal pickers, reviewed against the
0.14.1 source. This audit follows the reported experience of having to copy
an account ID between account creation and sign-in.

| Friction | Change |
| --- | --- |
| Adding an account stops after printing an internal ID | Interactive Claude and Codex account creation continues through sign-in and model loading. JSON and piped commands retain create-only behavior. |
| Setup silently chooses an existing account | Terminal setup offers existing accounts, another account, and cancellation. `--new` and `--account` select directly. |
| Pickers start at the first row | Pickers select and mark the current account, model, session, conversation, or pane when present. |
| Empty lists and filters look alike | Empty lists give a relevant next step; unmatched filters explain how to clear the query. |
| Codex device sign-in requires manually transferring a link and code | Show and copy the short device code on macOS when clipboard access works, then wait for Enter before opening the official sign-in page. Keep manual instructions when either helper fails. |
| A disabled account cannot be selected, without a usable recovery step | The message includes the exact command to enable the account. |

Provider sign-in, model discovery, account holds, and supported-build checks
remain in their existing runtime paths. Creating a new account does not release
an existing account with unfinished work.

Remaining opportunity: account creation and reconnection inside the full-screen
TUI currently require leaving it for the CLI. A future TUI flow should hand
terminal ownership to the provider's sign-in process and restore the draft and
selection afterward. This audit does not add that subprocess lifecycle.
