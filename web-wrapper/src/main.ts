import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process'
import { createWriteStream, mkdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { app, BrowserWindow, dialog } from 'electron'

const wrapperDir = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const repoDir = resolve(wrapperDir, '..')
const dataDir = join(wrapperDir, 'data')
const logDir = join(dataDir, 'logs')
const dshHome = join(dataDir, 'dsh-home')
const agentsHome = join(dataDir, 'agents-home')
const loadingFile = join(wrapperDir, 'loading.html')
const readyUrlPattern = /^dsh web:\s+(https?:\/\/\S+)/u

let harness: ChildProcessWithoutNullStreams | undefined
let mainWindow: BrowserWindow | undefined
let pendingUrl: string | undefined
let sawReadyUrl = false

function ensureDirectories(): void {
  for (const directory of [dataDir, logDir, dshHome, agentsHome]) {
    mkdirSync(directory, { recursive: true })
  }
}

function logFileName(): string {
  const stamp = new Date().toISOString().replace(/[:.]/gu, '-')
  return join(logDir, `harness-${stamp}.log`)
}

function harnessCommand(): { command: string; args: string[] } {
  return {
    command: process.execPath,
    args: [
      join(repoDir, 'node_modules', 'pnpm', 'bin', 'pnpm.cjs'),
      'dsh',
      'web',
      '--no-open',
      '--port',
      '0',
    ],
  }
}

function spawnEnvironment(): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {}
  for (const [key, value] of Object.entries(process.env)) {
    if (value !== undefined) env[key] = value
  }
  env.DSH_HOME = dshHome
  env.DSH_AGENTS_HOME = agentsHome
  return env
}

async function showStartupFailure(message: string): Promise<void> {
  if (mainWindow !== undefined && !mainWindow.isDestroyed()) {
    await dialog.showMessageBox(mainWindow, {
      buttons: ['Close'],
      message: 'DeepSeek Harness did not start',
      detail: message,
      type: 'error',
    })
  } else {
    await dialog.showErrorBox('DeepSeek Harness did not start', message)
  }
  app.quit()
}

function startHarness(): void {
  const log = createWriteStream(logFileName(), { flags: 'a' })
  const command = harnessCommand()
  harness = spawn(command.command, command.args, {
    cwd: repoDir,
    env: spawnEnvironment(),
    windowsHide: true,
  })

  const onOutput = (chunk: Buffer): void => {
    const text = chunk.toString('utf8')
    log.write(text)
    for (const line of text.split(/\r?\n/u)) {
      const match = readyUrlPattern.exec(line)
      if (match === null || sawReadyUrl) continue
      sawReadyUrl = true
      pendingUrl = match[1]
      void mainWindow?.loadURL(match[1])
    }
  }

  harness.stdout.on('data', onOutput)
  harness.stderr.on('data', onOutput)
  harness.once('error', (error) => {
    log.write(`\nwrapper: failed to launch pnpm: ${error.message}\n`)
    void showStartupFailure(`Could not launch pnpm from ${repoDir}. Run setup.ps1 first, then try again.\n\n${error.message}`)
  })
  harness.once('exit', (code, signal) => {
    log.end(`\nwrapper: harness exited with code ${String(code)} signal ${String(signal)}\n`)
    if (!sawReadyUrl) {
      void showStartupFailure(`The Harness process exited before publishing its Web UI URL. See ${log.path.toString()} for details.`)
    }
  })
}

function stopHarness(): void {
  const runningHarness = harness
  harness = undefined
  if (runningHarness === undefined || runningHarness.killed) return
  runningHarness.kill()
}

app.on('window-all-closed', () => {
  stopHarness()
  app.quit()
})

ensureDirectories()
startHarness()

await app.whenReady()

mainWindow = new BrowserWindow({
  height: 900,
  show: false,
  title: 'DeepSeek Harness',
  width: 1400,
  webPreferences: {
    nodeIntegration: false,
    contextIsolation: true,
    sandbox: true,
  },
})
mainWindow.once('ready-to-show', () => mainWindow?.show())
mainWindow.on('closed', () => {
  mainWindow = undefined
})
await mainWindow.loadURL(pendingUrl ?? pathToFileURL(loadingFile).href)
