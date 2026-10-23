// The page's side of handing the build's first files to the render worker: the page preloads
// them as soon as it is parsed (the build writes `<link rel="preload" as="fetch">` for its
// manifest, WASM and shaders into its page), and the worker takes them (loader.ts `Handed`)
// instead of asking only once its own script and the manifest have come over the network.

import type { ToWorker } from "./messages.ts";

/** The build's files the page preloads (`<link rel="preload" as="fetch">`, which the build writes
 * into its page): their URLs. */
export function preloaded(page: ParentNode): string[] {
  return [...page.querySelectorAll<HTMLLinkElement>('link[rel="preload"][as="fetch"]')].map((link) => link.href);
}

/** Fetches `urls`, taking the page's preloads of them, and hands each one's bytes to the worker as
 * it arrives (null if it fails: the worker then fetches it itself). The page asks for them as
 * soon as it is parsed; the worker would ask only once its script and the manifest had come. */
export function handOver(worker: Pick<Worker, "postMessage">, urls: readonly string[], fetchFn: typeof fetch = fetch): void {
  for (const url of urls) {
    fetchFn(url)
      .then((r) => (r.ok ? r.arrayBuffer() : null))
      .catch(() => null)
      .then((b) => {
        const bytes = b === null ? null : new Uint8Array(b);
        worker.postMessage({ type: "file", url, bytes } satisfies ToWorker, bytes === null ? [] : [bytes.buffer]);
      });
  }
}

