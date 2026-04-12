import Fastify from 'fastify';
import { readConfig } from './config.js';
import {
  urbanInfoOutputJsonSchema,
  urbanInfoParseRequestSchema,
  urbanInfoParseResponseSchema,
} from './contracts.js';
import { runCodexStructuredPrompt, CodexExecError } from './codex-exec.js';
import { buildUrbanInfoPrompt } from './prompts.js';

export function createApp() {
  const config = readConfig(process.env);
  const app = Fastify({ logger: true });

  app.get('/healthz', async () => ({ ok: true }));

  app.post('/api/sangoi/v1/parse/urban-info', async (request, reply) => {
    const parsedBody = urbanInfoParseRequestSchema.safeParse(request.body);
    if (!parsedBody.success) {
      return reply.code(400).send({
        code: 'invalid_request',
        issues: parsedBody.error.issues,
      });
    }

    const startedAt = Date.now();

    try {
      const codexRequest = {
        prompt: buildUrbanInfoPrompt(parsedBody.data),
        outputSchema: urbanInfoOutputJsonSchema as Record<string, unknown>,
        timeoutMs: config.SANGOI_CODEX_TIMEOUT_MS,
        workingDirectory: config.SANGOI_CODEX_WORKDIR,
      } as const;

      const result = await runCodexStructuredPrompt({
        ...codexRequest,
        ...(config.SANGOI_CODEX_MODEL
          ? { model: config.SANGOI_CODEX_MODEL }
          : {}),
      });

      const parsedResult = urbanInfoParseResponseSchema.shape.result.safeParse(result);
      if (!parsedResult.success) {
        request.log.error({ issues: parsedResult.error.issues }, 'codex returned invalid urban-info payload');
        return reply.code(502).send({
          code: 'invalid_codex_response',
          issues: parsedResult.error.issues,
        });
      }

      return reply.code(200).send(
        urbanInfoParseResponseSchema.parse({
          parser: 'codex-exec',
          durationMs: Date.now() - startedAt,
          result: parsedResult.data,
        }),
      );
    } catch (error) {
      if (error instanceof CodexExecError) {
        request.log.error(
          {
            exitCode: error.exitCode,
            stderr: error.stderr.slice(0, 4000),
            stdout: error.stdout.slice(0, 4000),
          },
          'codex exec failed during urban-info parse',
        );

        return reply.code(502).send({
          code: 'codex_exec_failed',
          message: error.message,
          exitCode: error.exitCode,
        });
      }

      throw error;
    }
  });

  return { app, config };
}

async function main() {
  const { app, config } = createApp();
  await app.listen({
    host: config.SANGOI_CODEX_MCP_HOST,
    port: config.SANGOI_CODEX_MCP_PORT,
  });
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
