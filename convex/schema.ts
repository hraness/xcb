import { relaySchema } from "@hraness/relay/backend";
import { defineSchema, defineTable } from "convex/server";
import { v } from "convex/values";

// The product's bounded opt-in availability rows are independent of relay
// users, devices, keys and retention. Every relay table is retained unchanged.
export default defineSchema({
  ...relaySchema().tables,
  xcbHostStatus: defineTable({
    id: v.string(),
    keyGeneration: v.string(),
    sequence: v.number(),
    lastReceivedAt: v.number(),
    health: v.union(v.literal("ok"), v.literal("degraded"), v.literal("unknown")),
    sampleAgeSeconds: v.number(),
    tasks: v.optional(v.object({ running: v.number(), queued: v.number(), needsInput: v.number(), uncertain: v.number() })),
    resources: v.optional(v.object({ pressure: v.union(v.literal("normal"), v.literal("warning"), v.literal("critical"), v.literal("unknown")), swapUsedBytes: v.number(), physicalTotalBytes: v.number(), disksFreeBytes: v.array(v.number()) })),
  }).index("by_alias", ["id"]),
  xcbHostStatusHistory: defineTable({
    id: v.string(),
    observedAt: v.number(),
    running: v.number(),
    queued: v.number(),
    needsInput: v.number(),
    uncertain: v.number(),
    pressure: v.union(v.literal("normal"), v.literal("warning"), v.literal("critical"), v.literal("unknown")),
    swapUsedBytes: v.number(),
    physicalTotalBytes: v.number(),
    disksFreeBytes: v.array(v.number()),
  }).index("by_alias_time", ["id", "observedAt"]),
});
