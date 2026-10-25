// Los proveedores que configura la app (agents.rs del daemon), registrados en pi-ai. Las claves
// llegan del daemon por stdin o por /providers y viven solo en memoria (InMemoryCredentialStore):
// nunca en disco ni en el entorno del proceso.

import { createModels, createProvider, InMemoryCredentialStore, type MutableModels } from "@earendil-works/pi-ai";
import { openAICompletionsApi } from "@earendil-works/pi-ai/api/openai-completions.lazy";
import { anthropicProvider } from "@earendil-works/pi-ai/providers/anthropic";
import { googleProvider } from "@earendil-works/pi-ai/providers/google";
import { groqProvider } from "@earendil-works/pi-ai/providers/groq";
import { mistralProvider } from "@earendil-works/pi-ai/providers/mistral";
import { openaiProvider } from "@earendil-works/pi-ai/providers/openai";
import { openrouterProvider } from "@earendil-works/pi-ai/providers/openrouter";
import { xaiProvider } from "@earendil-works/pi-ai/providers/xai";

export interface ProviderConfig {
  id: string;
  kind: string;
  baseUrl?: string | null;
  model?: string | null;
  apiKey?: string | null;
}

const BUILTIN: Record<string, () => Parameters<MutableModels["setProvider"]>[0]> = {
  anthropic: anthropicProvider,
  openai: openaiProvider,
  google: googleProvider,
  openrouter: openrouterProvider,
  xai: xaiProvider,
  groq: groqProvider,
  mistral: mistralProvider,
};

/** El id con que pi-ai conoce al proveedor: el tipo para los integrados (uno por tipo), el propio para los compatibles. */
export function piProviderId(p: ProviderConfig): string {
  return p.kind === "openai-compatible" ? `condx-${p.id}` : p.kind;
}

/** Solo pruebas: el proveedor guionado de pi-ai, que main.ts registra aparte. */
const FAUX = "faux";

export class Providers {
  readonly credentials = new InMemoryCredentialStore();
  readonly models: MutableModels = createModels({ credentials: this.credentials });
  #configs = new Map<string, ProviderConfig>();

  /** Cambia el conjunto entero: los que ya no están pierden su clave. */
  async set(list: ProviderConfig[]): Promise<void> {
    for (const old of this.#configs.values()) {
      if (!list.some((p) => p.id === old.id)) await this.credentials.delete(piProviderId(old));
    }
    this.#configs.clear();
    for (const p of list) {
      this.#configs.set(p.id, p);
      const id = piProviderId(p);
      if (p.kind === FAUX) continue;
      if (p.kind === "openai-compatible") {
        if (!p.baseUrl || !p.model) throw new Error(`${p.id}: an OpenAI-compatible provider needs a base URL and a model`);
        this.models.setProvider(
          createProvider({
            id,
            name: p.id,
            baseUrl: p.baseUrl,
            auth: { apiKey: { name: p.id, resolve: async () => ({ auth: p.apiKey ? { apiKey: p.apiKey } : {} }) } },
            models: [{ id: p.model, name: p.model, api: "openai-completions", provider: id, baseUrl: p.baseUrl, reasoning: false, input: ["text"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 128_000, maxTokens: 16_000 }],
            api: openAICompletionsApi(),
          }),
        );
      } else {
        const make = BUILTIN[p.kind];
        if (!make) throw new Error(`unknown provider kind ${p.kind}`);
        this.models.setProvider(make());
      }
      if (p.apiKey) await this.credentials.modify(id, async () => ({ type: "api_key", key: p.apiKey! }));
    }
  }

  get(id: string): ProviderConfig | undefined {
    return this.#configs.get(id);
  }
}
