import {
  AIEngine,
  SessionInfo,
  UnloadResult,
  chatCompletion,
  chatCompletionChunk,
  chatCompletionRequest,
  modelInfo,
  type SettingComponentProps,
} from '@janhq/core'
import { fetch } from '@tauri-apps/plugin-http'
import { info, warn, error as logError } from '@tauri-apps/plugin-log'
import { invoke } from '@tauri-apps/api/core'
import './env.d'

const logger = {
  info: (...args: unknown[]) => {
    console.log(...args)
    info(args.map((arg) => ` ${arg}`).join(' '))
  },
  warn: (...args: unknown[]) => {
    console.warn(...args)
    warn(args.map((arg) => ` ${arg}`).join(' '))
  },
  error: (...args: unknown[]) => {
    console.error(...args)
    logError(args.map((arg) => ` ${arg}`).join(' '))
  },
}

type CodexShimConfig = {
  host: string
  port: number
  toolMode: string
  modelId?: string
  codexAppServerPath?: string
}

export default class CodexAppServerExtension extends AIEngine {
  provider: string = 'codex-app-server'
  readonly providerId: string = 'codex-app-server'
  inferenceUrl: string = ''

  private config: Record<string, string | boolean | number> = {}
  private cachedModels: modelInfo[] = []

  override async onLoad(): Promise<void> {
    super.onLoad()

    const settings = structuredClone(SETTINGS) as SettingComponentProps[]
    await this.registerSettings(settings as unknown as any)

    const loadedConfig: Record<string, string | boolean | number> = {}
    for (const item of settings) {
      const defaultValue = item.controllerProps.value
      loadedConfig[item.key] = await this.getSetting<typeof defaultValue>(
        item.key,
        defaultValue
      )
    }
    this.config = loadedConfig
    this.updateInferenceUrl()

    if (this.config.auto_start === true) {
      await this.startShim()
    }
  }

  override async onUnload(): Promise<void> {}

  onSettingUpdate<T>(key: string, value: T): void {
    this.config[key] = value as string | boolean | number

    if (key === 'shim_host' || key === 'shim_port') {
      this.updateInferenceUrl()
      this.startShim().catch((error) =>
        logger.warn('Failed to apply Codex shim binding:', error)
      )
    }

    if (key === 'model_id' || key === 'tool_mode') {
      this.startShim().catch((error) =>
        logger.warn('Failed to apply Codex shim setting:', error)
      )
    }

    if (key === 'start_shim' && value === true) {
      this.startShim()
        .catch((error) => logger.warn('Failed to start Codex shim:', error))
        .finally(() => {
          this.updateSettings([
            { key: 'start_shim', controllerProps: { value: false } },
          ]).catch((error) =>
            logger.warn('Failed to reset Start shim setting:', error)
          )
        })
      return
    }

    if (key === 'auto_start' && value === true) {
      this.startShim().catch((error) =>
        logger.warn('Failed to auto-start Codex shim:', error)
      )
    }
  }

  private updateInferenceUrl() {
    const host = this.getHost()
    const port = this.getPort()
    this.inferenceUrl = `http://${host}:${port}/v1/chat/completions`
  }

  private getHost() {
    return (this.config.shim_host as string) || '127.0.0.1'
  }

  private getPort() {
    const portValue = this.config.shim_port
    if (typeof portValue === 'number') return portValue
    const parsed = Number(portValue)
    return Number.isFinite(parsed) ? parsed : 51327
  }

  private getToolMode() {
    return (this.config.tool_mode as string) || 'jan'
  }

  private getModelId() {
    const value = this.config.model_id
    return typeof value === 'string' && value.trim().length > 0
      ? value.trim()
      : undefined
  }

  private getCodexAppServerPath() {
    const value = this.config.codex_app_server_path
    return typeof value === 'string' && value.trim().length > 0
      ? value.trim()
      : undefined
  }

  private async startShim() {
    const config: CodexShimConfig = {
      host: this.getHost(),
      port: this.getPort(),
      toolMode: this.getToolMode(),
      modelId: this.getModelId(),
      codexAppServerPath: this.getCodexAppServerPath(),
    }

    try {
      await invoke('start_codex_app_server_shim', { config })
    } catch (error) {
      logger.warn('Failed to start Codex App Server shim:', error)
    }
  }

  private async stopShim() {
    try {
      await invoke('stop_codex_app_server_shim')
    } catch (error) {
      logger.warn('Failed to stop Codex App Server shim:', error)
    }
  }

  private async fetchModels(): Promise<modelInfo[]> {
    const host = this.getHost()
    const port = this.getPort()
    const url = `http://${host}:${port}/v1/models`

    try {
      const response = await fetch(url, { method: 'GET' })
      if (!response.ok) {
        logger.warn('Failed to fetch models from Codex shim:', response.status)
        return []
      }
      const data = await response.json()
      if (data?.data && Array.isArray(data.data)) {
        return data.data
          .map((model: { id: string; name?: string }) => ({
            id: model.id,
            name: model.name ?? model.id,
            providerId: this.provider,
            port: port,
            sizeBytes: 0,
          })) as modelInfo[]
      }
      return []
    } catch (error) {
      logger.warn('Failed to fetch models from Codex shim:', error)
      return []
    }
  }

  override async list(): Promise<modelInfo[]> {
    await this.startShim()
    const models = await this.fetchModels()
    if (models.length > 0) {
      this.cachedModels = models
      return models
    }

    if (this.cachedModels.length > 0) {
      return this.cachedModels
    }

    return [
      {
        id: 'codex-app-server',
        name: 'Codex App Server',
        providerId: this.provider,
        port: this.getPort(),
        sizeBytes: 0,
      },
    ]
  }

  override async get(modelId: string): Promise<modelInfo | undefined> {
    const models = await this.list()
    return models.find((model) => model.id === modelId)
  }

  override async load(modelId: string): Promise<SessionInfo> {
    await this.startShim()
    return {
      pid: 0,
      port: this.getPort(),
      model_id: modelId,
      model_path: '',
      is_embedding: false,
      api_key: '',
    }
  }

  override async unload(): Promise<UnloadResult> {
    await this.stopShim()
    return { success: true }
  }

  override async chat(
    _opts: chatCompletionRequest
  ): Promise<chatCompletion | AsyncIterable<chatCompletionChunk>> {
    throw new Error('Codex App Server shim chat is handled via OpenAI-compatible API')
  }

  override async delete(): Promise<void> {}

  override async update(): Promise<void> {}

  override async import(): Promise<void> {}

  override async abortImport(): Promise<void> {}

  override async getLoadedModels(): Promise<string[]> {
    return []
  }

  override async isToolSupported(_modelId: string): Promise<boolean> {
    return this.getToolMode() === 'jan'
  }
}
