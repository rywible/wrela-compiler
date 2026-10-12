// Checked ABI paths name literal files. URL parsing must not trim their spaces/control
// characters or interpret percent escapes; only slash separators and dot parts resolve.

export const pathUrl = (base: string, path: string): URL =>
  new URL(path.split("/").map(encodeURIComponent).join("/"), base);
