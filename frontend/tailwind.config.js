/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./index.html",
    "./src/**/*.{js,ts,jsx,tsx}",
  ],
  theme: {
    extend: {
      // Semantic utilities over the colour tokens (design/theme.ts, painted by utils/colorUtils.ts) and index.css; see docs/design/.
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
          faint:   'var(--c-accent-faint)',
          soft:    'var(--c-accent-soft)',
          mid:     'var(--c-accent-mid)',
        },
        // Ink on an accent / danger fill.
        'on-accent': 'var(--c-on-accent)',
        'on-danger': 'var(--c-on-danger)',
        fg:    'var(--c-text)',
        dim:   'var(--c-text-dim)',
        muted: 'var(--c-text-muted)',
        line: {
          DEFAULT: 'var(--c-border)',
          soft:    'var(--c-border-soft)',
          // A control's edge: 3:1 against the surface it sits on.
          strong:  'var(--c-border-active)',
        },
        hover: 'var(--c-hover)',
        active: 'var(--c-active)',
        danger: 'var(--c-danger)',
        warning: 'var(--c-warning)',
        // "In a voice room" green.
        connected: 'var(--c-voice-connected)',
        // Fixed dark text for a bright runtime fill (admin name chip) → `text-ink`.
        ink: 'var(--c-ink)',
      },
      // `--bar-h`, `--side-w` and the per-skin log rhythm tokens, as spacing utilities.
      spacing: {
        bar: 'var(--bar-h)',
        composer: 'var(--composer-h)',
        'composer-flush': 'var(--composer-flush-h)',
        side: 'var(--side-w)',
        'msg-header': 'var(--msg-header-gap)',
        'msg-group': 'var(--msg-group-gap)',
        'msg-divider': 'var(--msg-divider-gap)',
        'msg-row': 'var(--msg-row-pad-y)',
      },
      fontFamily: {
        // The stacks live in index.css so the per-script fallback (#1725) and the skins apply.
        mono: ['var(--font-mono)'],
        sans: ['var(--font-sans)'],
      },
      lineHeight: {
        msg: 'var(--lh)',
      },
      borderRadius: {
        panel: '10px',
        // Per-skin radii from index.css.
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
