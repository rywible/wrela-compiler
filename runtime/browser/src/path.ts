// ABI paths: what's wrong with one, and a checked one's URL. Checked paths name literal files.
// URL parsing must not trim their spaces/control characters or interpret percent escapes; only
// slash separators and dot parts resolve.

export const pathUrl = (base: string, path: string): URL =>
  new URL(path.split("/").map(encodeURIComponent).join("/"), base);

/**
 * What's wrong with a storage path or a fetched URL, if anything: it's relative to the program's
 * storage (or its build), with `/` between its parts, and none of them empty, `.` or `..`.
 */
export function pathProblem(path: string): string | undefined {
  if (path === "") return "is empty";
  if (path.startsWith("/") || /[\\:?#]/.test(path)) return "must be relative, with `/` between its parts";
  if (path.split("/").some((p) => p === "" || p === "." || p === "..")) return "has an empty, `.` or `..` part";
  return undefined;
}

/**
 * What's wrong with the path of a build to hand over to, if anything: as `pathProblem`'s, except
 * that it may start with one `..` part, so a build can name one beside it (runtime/abi's
 * `build_path_problem`).
 */
export function buildPathProblem(path: string): string | undefined {
  const rest = path.startsWith("../") ? path.slice(3) : path;
  if (rest === ".." || rest.startsWith("../")) return "climbs more than one directory: a build names its own or one beside it";
  return pathProblem(rest);
}
