import path from 'path'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

const isPlaywright = process.env.VITE_PLAYWRIGHT === 'true'

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react()],
  // Load VITE_* env vars from the monorepo root so we only need a single .env.development
  envDir: '..',
  // Emit relative asset URLs in the built index.html. Electron loads the
  // packaged HTML via `file://`, where absolute paths like `/assets/x.js`
  // resolve to `file:///assets/x.js` and 404 — the window then shows just
  // the title bar with no rendered content. Relative (`./assets/x.js`)
  // works under both `file://` (packaged) and the Vite dev server.
  base: './',
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
