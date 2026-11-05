/** Whether the app runs on Windows: WebView2 names Windows in its user agent, WebKitGTK Linux. */
export function isWindows(): boolean {
  return navigator.userAgent.includes("Windows");
}
