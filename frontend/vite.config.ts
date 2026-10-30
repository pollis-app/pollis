import path from 'path'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

const isPlaywright = process.env.VITE_PLAYWRIGHT === 'true'

export default defineConfig({
  plugins: [react()],
  // Relative asset URLs, so a `file://`-loaded index.html resolves them.
  base: './',
  // VITE_* env vars come from the monorepo root.
  envDir: '..',
  // Reproducible bundle (docs/verifiable-builds-design.md §1.4): no source maps (host paths), content-hashed names.
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
          // #1611: the blocked screen's Update now / download link, scripted by the spec.
          '@tauri-apps/plugin-updater': path.resolve(__dirname, './src/__mocks__/tauri-updater.ts'),
          '@tauri-apps/plugin-process': path.resolve(__dirname, './src/__mocks__/tauri-process.ts'),
          '@tauri-apps/plugin-shell': path.resolve(__dirname, './src/__mocks__/tauri-shell.ts'),
        },
      }
    : undefined,
  server: {
    hmr: {
      host: 'localhost',
    },
  },
})
