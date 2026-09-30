import { relaySchema } from "@hraness/relay/backend";
import { defineSchema, defineTable } from "convex/server";
import { v } from "convex/values";

// The product's two opt-in availability rows are independent of relay users,
// devices, keys and retention. Every relay table is retained unchanged.
export default defineSchema({
  ...relaySchema().tables,
  xcbHostStatus: defineTable({
    id: v.union(v.literal("laptop-1"), v.literal("laptop-2")),
    keyGeneration: v.string(),
    sequence: v.number(),
    lastReceivedAt: v.number(),
    health: v.union(v.literal("ok"), v.literal("degraded"), v.literal("unknown")),
    sampleAgeSeconds: v.number(),
  }).index("by_alias", ["id"]),
});
