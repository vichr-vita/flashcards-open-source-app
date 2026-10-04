import { databaseName, databaseVersion } from "./databaseSchema";

const missingCursorMessage = "Attempt to iterate a cursor that doesn't exist";

/** Retains native cursor failure details without including any stored card values. */
export class IndexedDbCursorError extends Error {
  readonly indexedDbOperation = "cursor";
  readonly databaseName = databaseName;
  readonly databaseVersion = databaseVersion;
  readonly indexedDbErrorName: string | null;
  readonly indexedDbErrorCode: number | null;
  readonly indexedDbCursorIndexName: string;
  readonly indexedDbCursorDirection: IDBCursorDirection;

  constructor(
    prefix: string,
    sourceError: unknown,
    indexName: string,
    direction: IDBCursorDirection,
  ) {
    const message = sourceError instanceof Error && sourceError.message !== ""
      ? sourceError.message
      : "unknown error";
    super(`${prefix}: ${message}`, { cause: sourceError });
    this.name = sourceError instanceof DOMException ? sourceError.name : "Error";
    this.indexedDbErrorName = sourceError instanceof DOMException ? sourceError.name : null;
    this.indexedDbErrorCode = sourceError instanceof DOMException ? sourceError.code : null;
    this.indexedDbCursorIndexName = indexName;
    this.indexedDbCursorDirection = direction;
  }
}

export function isIndexedDbMissingCursorError(error: unknown): error is IndexedDbCursorError {
  return error instanceof IndexedDbCursorError
    && error.cause instanceof DOMException
    && error.cause.name === "UnknownError"
    && error.cause.message === missingCursorMessage;
}
