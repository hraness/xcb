/** The xcb relay deployment: namespace `xcb.relay.v1`, daemon + controller
 * device classes, the frozen command union, closed sign-up gated by a
 * one-shot bootstrap capability, and env-selected email transport.
 *
 * The bootstrap token lives in `XCB_RELAY_BOOTSTRAP` on the deployment and
 * admits the first owner exactly once — it dies the moment a subject
 * verifies. OTP delivery defaults to `log`; `XCB_RELAY_EMAIL=resend` uses
 * Resend via `XCB_RESEND_API_KEY` / `XCB_RESEND_FROM`, `sendgrid` uses the
 * SendGrid v3 API via `XCB_SENDGRID_API_KEY` / `XCB_SENDGRID_FROM`, and
 * `webhook` POSTs the code to `XCB_OTP_WEBHOOK_URL` with bearer
 * `XCB_OTP_WEBHOOK_TOKEN`. */

import { defineRelay } from "@hraness/relay/backend";
import type { EmailTransport } from "@hraness/relay/wire";

const email: EmailTransport = process.env.XCB_RELAY_EMAIL === "resend"
  ? { mode: "resend", keyEnv: "XCB_RESEND_API_KEY", fromEnv: "XCB_RESEND_FROM" }
  : process.env.XCB_RELAY_EMAIL === "sendgrid"
    ? { mode: "sendgrid", keyEnv: "XCB_SENDGRID_API_KEY", fromEnv: "XCB_SENDGRID_FROM" }
    : process.env.XCB_RELAY_EMAIL === "webhook"
      ? { mode: "webhook", urlEnv: "XCB_OTP_WEBHOOK_URL", tokenEnv: "XCB_OTP_WEBHOOK_TOKEN" }
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
