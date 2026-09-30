import { httpRouter } from "convex/server";

import { auth } from "./auth";
import { heartbeat, status } from "./hostStatus";

const http = httpRouter();
auth.addHttpRoutes(http);
http.route({ path: "/host-status/heartbeat", method: "POST", handler: heartbeat });
http.route({ path: "/host-status", method: "GET", handler: status });

export default http;
