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
export const agentPrompt = `Install xcb on this machine. On macOS or Linux, run \`${installCommand}\`; if the shell can't find \`xcb\` afterwards, add ~/.local/bin to my PATH in my shell profile. On native Windows, run \`${windowsInstallCommand}\` in PowerShell instead; if \`xcb\` isn't found, add %LOCALAPPDATA%\\Programs\\xcb\\bin to my user PATH. On native Windows the providers run in WSL2. Make sure \`xcb --version\` works, then run \`xcb doctor\` and tell me which providers are ready and the one command I should run next. Don't sign in for me.`;
