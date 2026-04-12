import type { UrbanInfoParseRequest } from './contracts.js';

export function buildUrbanInfoPrompt(request: UrbanInfoParseRequest): string {
  const metadata = {
    documentId: request.documentId ?? null,
    fileName: request.fileName ?? null,
    mimeType: request.mimeType ?? null,
    projectType: request.projectType ?? null,
    sourceSnapshotId: request.sourceSnapshotId ?? null,
  };

  return [
    'You are a bounded parser for Sangoi, a municipal dossier-review product for architects.',
    'Your only job is to parse support-document text that may correspond to Santa Maria urban information.',
    'Do not decide legal compliance. Do not invent missing values. Do not use tools. Do not browse. Do not run shell commands.',
    'Return only data that is explicitly supported by the text.',
    'If the text is not clearly an urban-information style support document, say so through profile/status/supportLevel instead of guessing.',
    'Use these rules:',
    '- status=complete only when the document clearly looks like urban information and the main regime fields are materially present.',
    '- status=partial when the document likely belongs to one support profile but important fields are missing.',
    '- status=needs_review when the text is ambiguous, mixed, contradictory, or too weak for reliable extraction.',
    '- supportLevel=structured when the text clearly exposes labeled regime/use fields.',
    '- supportLevel=partial when some structure exists but important fields are incomplete or noisy.',
    '- supportLevel=unstructured when the text is too messy or indirect for structured extraction.',
    '- Preserve zone formats like 12.d, 1.1.a, 6.c PS1, or 19 exactly enough to remain faithful to the text.',
    '- Convert decimal comma to decimal point for numeric fields.',
    '- occupancyIndex should be a decimal like 0.65, not a percentage string.',
    '- floorAreaIndex should be a decimal like 1.1.',
    '- evidenceSnippets must quote short exact snippets from the text that justify the extracted fields.',
    '- warnings should be short parser caveats, not essays.',
    '',
    '<metadata>',
    JSON.stringify(metadata, null, 2),
    '</metadata>',
    '',
    '<document_text>',
    request.extractedText,
    '</document_text>',
  ].join('\n');
}
