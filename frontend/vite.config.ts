import path from 'path'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

const isPlaywright = process.env.VITE_PLAYWRIGHT === 'true'

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react()],
  // Emit relative asset URLs in the built index.html. Electron loads the
  // packaged HTML via `file://`, where absolute paths like `/assets/x.js`
  // resolve to `file:///assets/x.js` and 404 — the window then shows just
  // the title bar with no rendered content. Relative (`./assets/x.js`)
  // works under both `file://` (packaged) and the Vite dev server.
  base: './',
  // Load VITE_* env vars from the monorepo root so we only need a single .env.development
  envDir: '..',
  // Reproducible frontend bundle (docs/verifiable-builds-design.md §1.4). The
  // Vite/Rollup output that `tauri build` embeds is an input to the hashed
  // Linux payload, so it must be a pure function of source + the frozen
  // lockfile — no host-specific or wall-clock inputs:
  //   * sourcemap: false — source maps bake absolute host paths into their
  //     `sources`/`sourceRoot`, leaking the builder's filesystem into the
  //     shipped bundle and differing host-to-host.
  //   * asset file names use Rollup's content hash (its default) — a pure
  //     function of chunk bytes. Stated explicitly so it is never "tuned" into
  //     a timestamp/counter scheme later.
  // Chunk ordering + hashing are otherwise already deterministic given the
  // frozen lockfile, and this config injects no `Date.now()`/random `define`.
  build: {
    sourcemap: false,
    rollupOptions: {
      output: {
        entryFileNames: 'assets/[name].[hash].js',
        chunkFileNames: 'assets/[name].[hash].js',
        assetFileNames: 'assets/[name].[hash][extname]',
      },
    },
  },
  resolve: isPlaywright
    ? {
        alias: {
          '@tauri-apps/api/core': path.resolve(__dirname, './src/__mocks__/tauri-core.ts'),
          '@tauri-apps/api/event': path.resolve(__dirname, './src/__mocks__/tauri-event.ts'),
        },
      }
    : undefined,
  server: {
    // Handle client-side routing for desktop auth pages
    // When browser opens to /desktop-auth, serve index.html
    // so React Router (or our pathname-based routing) can handle it

    hmr: {
      host: 'localhost',
    },
  },
})
