import { relay } from "./relay";

export const register = relay.devices.register;
export const beginBind = relay.devices.beginBind;
export const finishBind = relay.devices.finishBind;
export const beginReauth = relay.devices.beginReauth;
export const finishReauth = relay.devices.finishReauth;
export const reauthStatus = relay.devices.reauthStatus;
export const list = relay.devices.list;
export const connect = relay.devices.connect;
export const heartbeat = relay.devices.heartbeat;
export const disconnect = relay.devices.disconnect;
export const revoke = relay.devices.revoke;
