import { z } from 'zod';

const envSchema = z.object({
  SANGOI_CODEX_RUNTIME_MODE: z.enum(['dev', 'prod']),
  SANGOI_CODEX_MCP_HOST: z.string().min(1).default('127.0.0.1'),
  SANGOI_CODEX_MCP_PORT: z.coerce.number().int().positive().default(7788),
  SANGOI_CODEX_TIMEOUT_MS: z.coerce.number().int().positive().default(120000),
  SANGOI_CODEX_WORKDIR: z.string().min(1).default(process.cwd()),
  SANGOI_CODEX_MODEL: z.string().optional(),
  SANGOI_CODEX_PROD_AUTH_MODE: z.string().optional(),
  SANGOI_CODEX_PROD_AUTH_BACKEND_URL: z.string().optional(),
  SANGOI_CODEX_PROD_RUNTIME_JWT: z.string().optional(),
});

export type AppConfig = z.infer<typeof envSchema>;

export function readConfig(env: NodeJS.ProcessEnv): AppConfig {
  if (!env.SANGOI_CODEX_RUNTIME_MODE) {
    throw new Error(
      'SANGOI_CODEX_RUNTIME_MODE must be set explicitly. Use dev only for local operator launches, or prod once the production-owned Codex runtime auth path exists.',
    );
  }

  return envSchema.parse(env);
}
