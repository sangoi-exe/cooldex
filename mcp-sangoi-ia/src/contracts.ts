import { z } from 'zod';
import { zodToJsonSchema } from 'zod-to-json-schema';

export const urbanInfoParseRequestSchema = z.object({
  documentId: z.string().min(1).optional(),
  fileName: z.string().min(1).optional(),
  mimeType: z.string().min(1).optional(),
  extractedText: z.string().min(1),
  projectType: z.string().min(1).optional(),
  sourceSnapshotId: z.string().min(1).optional(),
});

export type UrbanInfoParseRequest = z.infer<typeof urbanInfoParseRequestSchema>;

export const urbanInfoParseResultSchema = z.object({
  status: z.enum(['complete', 'partial', 'needs_review']),
  cadastralSupportProfile: z.enum([
    'urban_info',
    'iptu_sheet',
    'property_registry',
    'mixed',
    'unknown',
  ]),
  supportLevel: z.enum(['structured', 'partial', 'unstructured']),
  zoneCode: z.string().min(1).nullable(),
  frontSetbackMeters: z.number().finite().nonnegative().nullable(),
  sideSetbackRule: z.string().min(1).nullable(),
  heightRule: z.string().min(1).nullable(),
  occupancyIndex: z.number().finite().nonnegative().nullable(),
  floorAreaIndex: z.number().finite().nonnegative().nullable(),
  residentialUseSignal: z.boolean().nullable(),
  evidenceSnippets: z.array(z.string().min(1)).max(8),
  warnings: z.array(z.string().min(1)).max(8),
});

export type UrbanInfoParseResult = z.infer<typeof urbanInfoParseResultSchema>;

export const urbanInfoParseResponseSchema = z.object({
  parser: z.literal('codex-exec'),
  durationMs: z.number().int().nonnegative(),
  result: urbanInfoParseResultSchema,
});

export type UrbanInfoParseResponse = z.infer<typeof urbanInfoParseResponseSchema>;

const urbanInfoOutputJsonSchemaDocument = zodToJsonSchema(urbanInfoParseResultSchema, {
  name: 'UrbanInfoParseResult',
  target: 'openAi',
  $refStrategy: 'none',
});

function isObjectJsonSchema(value: unknown): value is Record<string, unknown> & { type: 'object' } {
  return typeof value === 'object' && value !== null && 'type' in value && value.type === 'object';
}

const urbanInfoDefinition = 'definitions' in urbanInfoOutputJsonSchemaDocument
  ? urbanInfoOutputJsonSchemaDocument.definitions?.UrbanInfoParseResult
  : undefined;

if (!isObjectJsonSchema(urbanInfoDefinition)) {
  throw new Error('urban-info output schema did not resolve to an object schema');
}

export const urbanInfoOutputJsonSchema = urbanInfoDefinition;
