import snapshot from "../portfolio-messaging.generated.json";

if (snapshot.contract !== "hraness.product-messaging/v1" || snapshot.productId !== "xcb") {
  throw new Error("The website requires the canonical xcb messaging snapshot.");
}

export const productMessaging = snapshot.messaging;
export const productName = productMessaging.names.name;
export const productCanonicalUrl = snapshot.canonicalUrl;
