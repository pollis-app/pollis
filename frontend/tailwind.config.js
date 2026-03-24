/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./index.html",
    "./src/**/*.{js,ts,jsx,tsx}",
  ],
  theme: {
    extend: {
      colors: {
        accent: {
          DEFAULT: 'var(--c-accent)',
          bright:  'var(--c-accent-bright)',
          dim:     'var(--c-accent-dim)',
          muted:   'var(--c-accent-muted)',
          ghost:   'var(--c-hover)',
          subtle:  'var(--c-active)',
          border:  'var(--c-border)',
          active:  'var(--c-border-active)',
        },
        surface: {
          DEFAULT: 'var(--c-surface)',
          raised:  'var(--c-surface-raised)',
          high:    'var(--c-surface-high)',
        },
        bg: 'var(--c-bg)',
      },
      fontFamily: {
        mono: ['"Atkinson Hyperlegible Mono"', 'ui-monospace', 'monospace'],
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
