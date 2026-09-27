import { permanentRedirect } from "next/navigation";

/** Downloads moved to the install page: xcb installs with one command. */
export default function Download(): never {
  permanentRedirect("/install");
}
