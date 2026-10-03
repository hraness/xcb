/** Internal mutations invoked by the credentials provider through string
 * references (`relayInternal:<name>`). Keep this module's name aligned
 * with `paths.internal` in the relay config. */

import { relay } from "./relay";

export const reserveEmailAttempt = relay.internal.reserveEmailAttempt;
export const storeOtpChallenge = relay.internal.storeOtpChallenge;
export const recordOtpDelivery = relay.internal.recordOtpDelivery;
export const consumeOtpChallenge = relay.internal.consumeOtpChallenge;
