// The SQLite storage API binds a BLOB as an ArrayBuffer; bun:sqlite, which the tests stand in for
// it with, takes a Uint8Array (an ArrayBuffer binds as NULL there).

export const sqliteBindings = (bindings: unknown[]): unknown[] => bindings.map((b) => (b instanceof ArrayBuffer ? new Uint8Array(b) : b));
