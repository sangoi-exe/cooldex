import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';

export interface RunCodexStructuredPromptOptions {
  prompt: string;
  outputSchema: Record<string, unknown>;
  timeoutMs: number;
  workingDirectory: string;
  model?: string;
}

export class CodexExecError extends Error {
  readonly exitCode: number | null;
  readonly stdout: string;
  readonly stderr: string;

  constructor(message: string, exitCode: number | null, stdout: string, stderr: string) {
    super(message);
    this.name = 'CodexExecError';
    this.exitCode = exitCode;
    this.stdout = stdout;
    this.stderr = stderr;
  }
}

export async function runCodexStructuredPrompt<T>(
  options: RunCodexStructuredPromptOptions,
): Promise<T> {
  const tempDirectory = await mkdtemp(join(tmpdir(), 'sangoi-codex-'));
  const schemaPath = join(tempDirectory, 'output-schema.json');
  const outputPath = join(tempDirectory, 'last-message.json');

  await writeFile(schemaPath, JSON.stringify(options.outputSchema, null, 2), 'utf8');

  const args = [
    'exec',
    '--ephemeral',
    '--skip-git-repo-check',
    '--sandbox',
    'read-only',
    '--cd',
    options.workingDirectory,
    '--output-schema',
    schemaPath,
    '--output-last-message',
    outputPath,
    '-',
  ];

  if (options.model && options.model.length > 0) {
    args.splice(1, 0, '--model', options.model);
  }

  let stdout = '';
  let stderr = '';

  try {
    await new Promise<void>((resolve, reject) => {
      const child = spawn('codex', args, {
        env: process.env,
        stdio: ['pipe', 'pipe', 'pipe'],
      });

      const timeoutHandle = setTimeout(() => {
        child.kill('SIGKILL');
        reject(new CodexExecError('codex exec timed out', null, stdout, stderr));
      }, options.timeoutMs);

      child.stdout.setEncoding('utf8');
      child.stderr.setEncoding('utf8');

      child.stdout.on('data', (chunk: string) => {
        stdout += chunk;
      });
      child.stderr.on('data', (chunk: string) => {
        stderr += chunk;
      });

      child.on('error', (error) => {
        clearTimeout(timeoutHandle);
        reject(new CodexExecError(error.message, null, stdout, stderr));
      });

      child.on('exit', (code) => {
        clearTimeout(timeoutHandle);
        if (code === 0) {
          resolve();
          return;
        }

        reject(new CodexExecError('codex exec failed', code, stdout, stderr));
      });

      child.stdin.end(options.prompt, 'utf8');
    });

    const rawOutput = await readFile(outputPath, 'utf8');
    return JSON.parse(rawOutput) as T;
  } finally {
    await rm(tempDirectory, { recursive: true, force: true });
  }
}
