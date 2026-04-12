import { z } from 'zod';

const envSchema = z.object({
  SANGOI_CODEX_MCP_HOST: z.string().min(1).default('127.0.0.1'),
  SANGOI_CODEX_MCP_PORT: z.coerce.number().int().positive().default(7788),
  SANGOI_CODEX_TIMEOUT_MS: z.coerce.number().int().positive().default(120000),
  SANGOI_CODEX_WORKDIR: z.string().min(1).default(process.cwd()),
  SANGOI_CODEX_MODEL: z.string().optional(),
});

export type AppConfig = z.infer<typeof envSchema>;

export function readConfig(env: NodeJS.ProcessEnv): AppConfig {
  return envSchema.parse(env);
}
