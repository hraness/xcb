import { cronJobs, makeFunctionReference } from "convex/server";

const crons = cronJobs();

const sweep = makeFunctionReference<"mutation">("relayMaintenance:sweep");

// The retention sweep runs hourly; each run is bounded and idempotent.
crons.interval("relay retention sweep", { hours: 1 }, sweep);

export default crons;
