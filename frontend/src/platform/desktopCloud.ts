/** Standalone Desktop builds can omit the cloud account features. */
export function desktopCloudEnabled(): boolean {
  return import.meta.env.VITE_DESKTOP_CLOUD !== '0'
}
