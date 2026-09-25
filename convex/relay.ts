/** The xcb relay deployment: namespace `xcb.relay.v1`, daemon + controller
 * device classes, the frozen command union, closed sign-up gated by a
 * one-shot bootstrap capability, and env-selected email transport.
 *
 * The bootstrap token lives in `XCB_RELAY_BOOTSTRAP` on the deployment and
 * admits the first owner exactly once — it dies the moment a subject
 * verifies. OTP delivery defaults to `log` (the only mode an anonymous
 * local backend supports); setting `XCB_RELAY_EMAIL=resend` switches to
 * Resend using `XCB_RESEND_API_KEY` / `XCB_RESEND_FROM`. */

import { defineRelay } from "@hraness/relay/backend";
import type { EmailTransport } from "@hraness/relay/wire";

const email: EmailTransport = process.env.XCB_RELAY_EMAIL === "resend"
  ? { mode: "resend", keyEnv: "XCB_RESEND_API_KEY", fromEnv: "XCB_RESEND_FROM" }
  : { mode: "log" };

export const relay = defineRelay({
  namespace: "xcb.relay.v1",
  commandKinds: [
    "task_dispatch",
    "task_steer",
    "task_cancel",
    "attention_answer",
    "daemon_send",
    "projection_refresh",
  ],
  deviceClasses: ["daemon", "controller"],
  executorClass: "daemon",
  email,
  openSignup: false,
  bootstrapInviteEnv: "XCB_RELAY_BOOTSTRAP",
  authProviderId: "xcb-otp-v1",
});
