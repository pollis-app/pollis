import { execFileSync } from 'child_process'
import { existsSync, mkdirSync, readFileSync, readdirSync, statSync } from 'fs'
import path from 'path'
import type { Plugin } from 'vite'

// `virtual:oss-notices` (#1832): the third-party licence notices the Open source
// licenses page renders, generated from the lockfiles by
// scripts/oss-inventory.mjs, so the list always matches what this build ships.
// Regenerated only when an input is newer than the cached output.

const MODULE_ID = 'virtual:oss-notices'
const RESOLVED_ID = `\0${MODULE_ID}`

const repoRoot = path.resolve(__dirname, '..')
const output = path.resolve(__dirname, 'node_modules/.cache/oss-notices.json')
const fixture = path.resolve(__dirname, 'src/__mocks__/oss-notices.json')

function newestInput(): number {
  const files = [
    path.join(repoRoot, 'Cargo.lock'),
    path.join(repoRoot, 'pnpm-lock.yaml'),
    path.join(repoRoot, 'scripts/oss-inventory.mjs'),
    path.join(__dirname, 'package.json'),
  ]
  const texts = path.join(repoRoot, 'supply-chain/license-texts')
  for (const f of readdirSync(texts)) {
    files.push(path.join(texts, f))
  }
  return Math.max(...files.map((f) => statSync(f).mtimeMs))
}

function generate(): void {
  if (existsSync(output) && statSync(output).mtimeMs >= newestInput()) {
    return
  }
  mkdirSync(path.dirname(output), { recursive: true })
  execFileSync('node', [path.join(repoRoot, 'scripts/oss-inventory.mjs'), 'desktop', '--notices', output], {
    cwd: repoRoot,
    stdio: ['ignore', 'inherit', 'inherit'],
  })
}

export function ossNotices(options: { playwright: boolean }): Plugin {
  let isBuild = false
  return {
    name: 'pollis-oss-notices',
    configResolved(config) {
      isBuild = config.command === 'build'
    },
    resolveId(id) {
      if (id === MODULE_ID) {
        return RESOLVED_ID
      }
      return null
    },
    load(id) {
      if (id !== RESOLVED_ID) {
        return null
      }
      // The browser e2e suite mocks Tauri and has no Rust toolchain; it gets a
      // small fixed list, like its other mocks.
      if (options.playwright) {
        return `export default ${readFileSync(fixture, 'utf8')}`
      }
      try {
        generate()
      } catch (err) {
        // A release must never ship without its notices.
        if (isBuild) {
          throw err
        }
        this.warn(`open source notices unavailable in this dev server: ${String(err)}`)
        return 'export default null'
      }
      return `export default ${readFileSync(output, 'utf8')}`
    },
  }
}
