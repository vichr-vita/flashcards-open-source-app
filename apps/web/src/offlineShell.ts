/** Install after the page loads so precaching does not delay the initial app render. */
export function registerOfflineShell(): void {
  if (!import.meta.env.PROD || !("serviceWorker" in navigator)) return;
  const register = () => {
    void navigator.serviceWorker.register("/sw.js", { scope: "/", updateViaCache: "none" }).catch(() => {
      // A failed installation leaves the online app usable; the browser retries on the next load.
    });
  };
  if (document.readyState === "complete") register();
  else window.addEventListener("load", register, { once: true });
}
