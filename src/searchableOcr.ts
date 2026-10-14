import type { OcrCancelAck, SavedCopy } from './bridge';

export type SearchableOcrCapability = {
  searchablePdfAvailable: boolean;
  searchablePdfReason: string | null;
  searchablePdfDpi: number | null;
  searchablePdfMaxPages: number;
  searchablePdfMaxPixels: number;
  searchablePdfMaxWords: number;
  searchablePdfMaxTextBytes: number;
  searchablePdfCharacters: string | null;
};
export type SearchableOcrRequest = { requestId: string; documentId: number; revision: number };
export type SearchableOcrTarget = Omit<SearchableOcrRequest, 'requestId'>;
export type CreateSearchableOcrCopy = (request: SearchableOcrRequest) => Promise<SavedCopy | null>;
export type CancelSearchableOcr = (requestId: string) => Promise<OcrCancelAck>;

export function validateSearchableOcrCapability(value: SearchableOcrCapability): string | null {
  if (!value.searchablePdfAvailable) return value.searchablePdfReason?.trim() || 'Searchable OCR is unavailable in this build.';
  if (value.searchablePdfReason !== null || value.searchablePdfDpi !== 150 || value.searchablePdfMaxPages !== 32 || value.searchablePdfMaxPixels !== 33_554_432 || value.searchablePdfMaxWords !== 100_000 || value.searchablePdfMaxTextBytes !== 8_388_608 || value.searchablePdfCharacters !== 'Unicode words without control or whitespace characters') return 'Searchable OCR capability does not match the supported fixed profile.';
  return null;
}

export function validateSearchableOcrRequest(request: SearchableOcrRequest): string | null {
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(request.requestId) || !Number.isSafeInteger(request.documentId) || request.documentId < 0 || !Number.isSafeInteger(request.revision) || request.revision < 0) return 'Searchable OCR requires an exact document, revision, and request UUID.';
  return null;
}
