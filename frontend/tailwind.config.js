/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./index.html",
    "./src/**/*.{js,ts,jsx,tsx}",
  ],
  theme: {
    extend: {
      // Design tokens live as CSS custom properties in index.css (single
      // source of truth — themeable + font-scalable). Surface them as
      // semantic Tailwind utilities so call sites write `text-muted` /
      // `border-line` / `h-bar` instead of `[var(--c-text-muted)]` or an
      // inline style. See CLAUDE.md → "Styling" for the convention.
      colors: {
        bg: 'var(--c-bg)',
        surface: {
          DEFAULT: 'var(--c-surface)',
          raised:  'var(--c-surface-raised)',
          high:    'var(--c-surface-high)',
        },
        accent: {
          DEFAULT: 'var(--c-accent)',
          bright:  'var(--c-accent-bright)',
          dim:     'var(--c-accent-dim)',
          muted:   'var(--c-accent-muted)',
        },
        // Foreground / text roles → `text-fg` `text-dim` `text-muted`.
        fg:    'var(--c-text)',
        dim:   'var(--c-text-dim)',
        muted: 'var(--c-text-muted)',
        // Hairline borders / dividers → `border-line` `border-line-strong`,
        // also `bg-line` for the rare hairline fill (e.g. tray separator).
        line: {
          DEFAULT: 'var(--c-border)',
          strong:  'var(--c-border-active)',
        },
        // Hover overlay (accent @ low alpha) → `hover:bg-hover`.
        hover: 'var(--c-hover)',
      },
      // `--bar-h` is the shared chrome-bar height (rem ⇒ font-scalable).
      // Exposed via spacing so `h-bar` / `min-h-bar` / `py-bar` all work.
      spacing: {
        bar: 'var(--bar-h)',
      },
      fontFamily: {
        mono: ['"DM Mono"', 'ui-monospace', 'monospace'],
        sans: ['"Atkinson Hyperlegible Next"', 'system-ui', 'sans-serif'],
      },
      borderRadius: {
        panel: '10px',
      },
      fontSize: {
        '2xs': ['0.733rem', { lineHeight: '1.1rem'  }],
        'xs':  ['0.867rem', { lineHeight: '1.3rem'  }],
        'sm':  ['1rem',     { lineHeight: '1.5rem'  }],
        'base':['1rem',     { lineHeight: '1.6rem'  }],
      },
    },
  },
  plugins: [],
}
