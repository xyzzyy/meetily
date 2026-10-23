export type Theme = 'light' | 'dark' | 'system';
export type ResolvedTheme = Exclude<Theme, 'system'>;

export const THEME_STORAGE_KEY = 'meetily-theme';
export const SYSTEM_THEME_QUERY = '(prefers-color-scheme: dark)';

export function isTheme(value: unknown): value is Theme {
  return value === 'light' || value === 'dark' || value === 'system';
}

// Apply the saved theme before the first paint, including when storage is blocked.
export const themeInitScript = `
(() => {
  let theme = 'system';
  try {
    const saved = localStorage.getItem('${THEME_STORAGE_KEY}');
    if (saved === 'light' || saved === 'dark' || saved === 'system') theme = saved;
  } catch {}
  const dark = theme === 'dark' || (theme === 'system' && matchMedia('${SYSTEM_THEME_QUERY}').matches);
  document.documentElement.classList.toggle('dark', dark);
  document.documentElement.style.colorScheme = dark ? 'dark' : 'light';
})();
`;
