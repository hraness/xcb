export const modules = {
  "./_generated/server.ts": async () => await import("./server"),
  "./relay.ts": async () => await import("./relay"),
  "./relayAuth.ts": async () => await import("./relayAuth"),
  "./relayCommands.ts": async () => await import("./relayCommands"),
  "./relayDevices.ts": async () => await import("./relayDevices"),
  "./relayEnvelopes.ts": async () => await import("./relayEnvelopes"),
  "./relayInternal.ts": async () => await import("./relayInternal"),
  "./relayInvites.ts": async () => await import("./relayInvites"),
  "./relayMaintenance.ts": async () => await import("./relayMaintenance"),
  "./relayProjections.ts": async () => await import("./relayProjections"),
};
