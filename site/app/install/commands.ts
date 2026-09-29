/** The one-line installer that xcb.sh serves at /install.sh. */
export const installCommand = "curl -fsSL https://xcb.sh/install.sh | sh";

/** The PowerShell line that xcb.sh serves at /install.ps1, for native Windows. */
export const windowsInstallCommand = "irm https://xcb.sh/install.ps1 | iex";

/** The shell line that puts the installer's default prefix on PATH. */
export const pathCommand = 'export PATH="$HOME/.local/bin:$PATH"';

/**
 * A prompt a person can paste into Claude Code, Codex, or another coding
 * agent. It installs and checks xcb but leaves sign-in to the person, because
 * sign-in opens a browser and uses their account.
 */
export const agentPrompt = `Install xcb on this machine. Run \`${installCommand}\`, then make sure \`xcb --version\` works; if the shell can't find it, add ~/.local/bin to my PATH in my shell profile. Then run \`xcb doctor\` and tell me which providers are ready and the one command I should run next. Don't sign in for me.`;
