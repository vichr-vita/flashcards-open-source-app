import type { ReactElement } from "react";

/** Logo and wordmark for the app header. The adjacent text names the decorative image. */
export function Brand(): ReactElement {
  return (
    <>
      <img className="brand-logo" src="/logo.svg" width="32" height="32" alt="" />
      <span className="brand-wordmark">lingvichr</span>
    </>
  );
}
