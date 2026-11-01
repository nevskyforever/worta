import { copyFileSync, mkdirSync, rmSync } from 'node:fs'
import { resolve } from 'node:path'
import { fileURLToPath, URL } from 'node:url'

import vue from '@vitejs/plugin-vue'
import type { Plugin } from 'vite'
import { defineConfig } from 'vitest/config'
import { enableE2eLegalGateSkip } from './scripts/e2eLegalGate.ts'

function syncMindMapAssets(): Plugin {
  const source = fileURLToPath(new URL('../mindmap_assets/', import.meta.url))
  const target = fileURLToPath(new URL('./public/mindmap-assets/', import.meta.url))
  const webAssets = [
    'index.html',
    'app.js',
    'i18n.js',
    'MindElixir.js',
    'MindElixir.css',
    'LICENSE.txt',
    'NOTICE.txt',
  ]

  return {
    name: 'nfprogress-mindmap-assets',
    configResolved() {
      rmSync(target, { recursive: true, force: true })
      mkdirSync(target, { recursive: true })
      for (const fileName of webAssets) {
        copyFileSync(resolve(source, fileName), resolve(target, fileName))
      }
    },
  }
}

export default defineConfig(({ command, mode }) => ({
  define: {
    __WORTA_E2E_SKIP_LEGAL_GATE__: JSON.stringify(enableE2eLegalGateSkip(command, mode, process.env)),
  },
  plugins: [syncMindMapAssets(), vue()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  server: {
    host: '127.0.0.1',
    port: 5173,
    proxy: {
      '/api': 'http://127.0.0.1:8000',
      '/health': 'http://127.0.0.1:8000',
    },
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    css: true,
    restoreMocks: true,
  },
}))
