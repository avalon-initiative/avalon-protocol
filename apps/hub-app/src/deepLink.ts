// Epic #623, issue #640: routes an `avalon://` deep link (registered via
// `src-tauri/src/lib.rs`'s `tauri_plugin_deep_link` + `tauri.conf.json`'s
// `plugins.deep-link.desktop.schemes`) to the matching in-app route. Only
// `cross-node-login` is handled today — every other host is ignored rather
// than erroring, so this stays forward-compatible with a future deep-link
// destination this app doesn't know about yet.
import type { Router } from 'vue-router'

/** Parses one deep-link URL and pushes the matching route, if recognized.
 * Never throws on a malformed URL — a bad/unexpected link is silently
 * ignored rather than crashing app startup. */
export function routeDeepLink(router: Router, rawUrl: string): void {
  let url: URL
  try {
    url = new URL(rawUrl)
  } catch {
    return
  }

  // A custom-scheme URL's authority component (`cross-node-login` in
  // `avalon://cross-node-login?...`) parses as `hostname`, not `pathname` —
  // there's no `/` before the first `?`.
  if (url.hostname !== 'cross-node-login') return

  const node = url.searchParams.get('node')
  const userCode = url.searchParams.get('user_code')
  if (!node || !userCode) return

  router.push({ name: 'cross-node-login', query: { node, user_code: userCode } })
}

/** Wires both halves of the deep-link plugin's API: `getCurrent` for a URL
 * this app was cold-launched with, `onOpenUrl` for one that arrives while
 * already running (platform support varies — see `src-tauri/src/lib.rs`'s
 * own doc comment on the real, not-yet-closed gaps). Best-effort: any
 * failure to even load the plugin (e.g. running in a plain browser during
 * `npm run dev` with no Tauri runtime) is swallowed rather than breaking
 * app startup. */
export async function setUpDeepLinkHandling(router: Router): Promise<void> {
  try {
    const { getCurrent, onOpenUrl } = await import('@tauri-apps/plugin-deep-link')
    const startupUrls = await getCurrent()
    if (startupUrls) {
      for (const url of startupUrls) routeDeepLink(router, url)
    }
    await onOpenUrl((urls) => {
      for (const url of urls) routeDeepLink(router, url)
    })
  } catch {
    // No Tauri deep-link runtime available (e.g. plain browser dev mode) —
    // manual entry on the cross-node-login screen still works.
  }
}
