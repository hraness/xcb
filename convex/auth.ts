import { relay } from "./relay";

export const auth = relay.auth.auth;
export const isAuthenticated = relay.auth.isAuthenticated;
export const signIn = relay.auth.signIn;
export const signOut = relay.auth.signOut;
export const store = relay.auth.store;
export const currentSubject = relay.auth.queries.currentSubject;
