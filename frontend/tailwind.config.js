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
        // Active/selected overlay (accent-tinted) → `bg-active`.
        active: 'var(--c-active)',
        // Error / destructive text and borders → `text-danger`, `border-danger`.
        danger: 'var(--c-danger)',
        // "In a voice room" green. Not re-skinned, like the accents.
        connected: 'var(--c-voice-connected)',
      },
      // `--bar-h` is the shared chrome-bar height and `--side-w` the sidebar /
      // right-panel measure (both rem ⇒ font-scalable). Exposed via spacing so
      // `h-bar` / `min-h-bar` / `py-bar` / `w-side` all work.
      //
      // The message-log rhythm tokens sit here too: they are per-skin density
      // knobs the log applies as padding and margin, and without a utility
      // every row and divider had to spell them as an inline style.
      spacing: {
        bar: 'var(--bar-h)',
        composer: 'var(--composer-h)',
        side: 'var(--side-w)',
        'msg-header': 'var(--msg-header-gap)',
        'msg-group': 'var(--msg-group-gap)',
        'msg-divider': 'var(--msg-divider-gap)',
        'msg-row': 'var(--msg-row-pad-y)',
      },
      fontFamily: {
        mono: ['"DM Mono"', 'ui-monospace', 'monospace'],
        sans: ['"Atkinson Hyperlegible Next"', 'system-ui', 'sans-serif'],
      },
      lineHeight: {
        // Per-skin body leading. `leading-msg` on the message body.
        msg: 'var(--lh)',
      },
      borderRadius: {
        panel: '10px',
        // The two per-skin radii from index.css — terminal is nearly square
        // (2px/4px), refined is rounded (6px/8px).
        chip: 'var(--radius-chip)',
        control: 'var(--radius-control)',
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
