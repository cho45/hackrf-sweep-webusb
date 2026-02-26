import { spawn } from 'node:child_process'
import type { Plugin } from 'vite'

/**
 * Rust ファイルの変更を監視し、WASM を自動ビルドする Vite プラグイン
 */
export function rustWatchPlugin(): Plugin {
  let isBuilding = false

  return {
    name: 'vite-plugin-rust-watch',

    configureServer(server) {
      const rustFiles = 'hackrf-dsp/src/**/*.rs'

      // chokidar を使用して Rust ファイルを監視
      server.watcher.add(rustFiles)

      server.watcher.on('change', async (filePath: string) => {
        // Rust ファイル以外は無視
        if (!filePath.endsWith('.rs')) {
          return
        }

        // 既にビルド中ならスキップ
        if (isBuilding) {
          return
        }

        isBuilding = true
        console.log(`\n[Rust Watch] ${filePath} が変更されました。WASM をビルドします...`)

        try {
          await runBuildWasm()
          console.log('[Rust Watch] WASM ビルド完了')
          // 全クライアントにフルリロードを通知
          server.ws.send({ type: 'full-reload' })
        } catch (error) {
          console.error('[Rust Watch] WASM ビルドエラー:', error)
        } finally {
          isBuilding = false
        }
      })
    },
  }
}

/**
 * npm run build:wasm を実行
 */
function runBuildWasm(): Promise<void> {
  return new Promise((resolve, reject) => {
    const child = spawn('npm', ['run', 'build:wasm'], {
      stdio: 'inherit',
      shell: true,
    })

    child.on('close', (code) => {
      if (code === 0) {
        resolve()
      } else {
        reject(new Error(`npm run build:wasm がコード ${code} で終了しました`))
      }
    })

    child.on('error', (error) => {
      reject(error)
    })
  })
}
