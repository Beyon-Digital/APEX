import { getAppSettings } from './tauri';

/** Apply theme + density to <html> data attributes (tokens.css hooks). */
export function applyAppearance(theme?: string, density?: string) {
  const el = document.documentElement;
  const resolved = theme === 'light' ? 'light' : theme === 'dark' ? 'dark' : 'terminal';
  el.dataset.theme = resolved;
  // The terminal theme is built for dense screens, so it defaults to compact.
  el.dataset.density = density === 'comfortable' ? 'comfortable' : 'compact';
}

/** Load persisted appearance from settings and apply it. */
export async function syncAppearance() {
  try {
    const settings = await getAppSettings();
    applyAppearance(settings.appearance.theme, settings.appearance.density);
  } catch {
    // defaults stay
  }
}
