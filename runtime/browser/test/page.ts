// A page for the tests: just enough of `window` and `document` (events, elements, focus) for the
// input services (listen.ts) and the accessibility overlay (assist.ts). There's no DOM library
// here; each test swaps these in and puts the real globals back.

/** An element: events, attributes, children, style, focus and a few properties. */
export class FakeElement extends EventTarget {
  readonly attributes = new Map<string, string>();
  readonly children: FakeElement[] = [];
  readonly style: Record<string, string> & { cssText?: string } = {};
  readonly classes = new Set<string>();
  readonly dataset: Record<string, string> = {};
  parent: FakeElement | null = null;
  id = "";
  className = "";
  textContent = "";
  tabIndex = -1;
  value = "";
  autocomplete = "";
  spellcheck = true;
  constructor(readonly page: FakePage, readonly tagName: string) {
    super();
  }
  setAttribute(k: string, v: string): void {
    this.attributes.set(k, v);
  }
  getAttribute(k: string): string | null {
    return this.attributes.get(k) ?? null;
  }
  removeAttribute(k: string): void {
    this.attributes.delete(k);
  }
  append(...xs: FakeElement[]): void {
    for (const x of xs) {
      x.parent?.children.splice(x.parent.children.indexOf(x), 1);
      x.parent = this;
      this.children.push(x);
    }
  }
  remove(): void {
    this.parent?.children.splice(this.parent.children.indexOf(this), 1);
    this.parent = null;
  }
  contains(x: FakeElement | null): boolean {
    for (let e = x; e !== null; e = e.parent) if (e === this) return true;
    return false;
  }
  get childElementCount(): number {
    return this.children.length;
  }
  querySelector(tag: string): FakeElement | null {
    for (const c of this.children) {
      if (c.tagName === tag.toUpperCase()) return c;
      const found = c.querySelector(tag);
      if (found) return found;
    }
    return null;
  }
  get classList() {
    return { toggle: (c: string, on: boolean) => (on ? this.classes.add(c) : this.classes.delete(c)) };
  }
  focus(): void {
    this.page.activeElement = this;
    this.dispatchEvent(new Event("focus"));
  }
  click(): void {
    this.dispatchEvent(new Event("click"));
  }
  getBoundingClientRect() {
    return { left: 0, top: 0, width: 100, height: 100 } as DOMRect;
  }
  setPointerCapture(): void {}
}

/** `window` and `document` together. */
export class FakePage extends EventTarget {
  activeElement: FakeElement | null = null;
  readonly body = new FakeElement(this, "BODY");
  readonly head = new FakeElement(this, "HEAD");
  /** Animation-frame callbacks waiting: `frame()` runs them. */
  readonly frames: (() => void)[] = [];
  createElement(tag: string): FakeElement {
    return new FakeElement(this, tag.toUpperCase());
  }
  frame(): void {
    for (const f of this.frames.splice(0)) f();
  }
}

/** Runs `body` with a fake page as `window` and `document` (and its animation frames), then
 * puts the real ones back. */
export function onPage<T>(body: (page: FakePage) => T): T {
  const names = ["window", "document", "requestAnimationFrame"] as const;
  const saved = names.map((n) => Object.getOwnPropertyDescriptor(globalThis, n));
  const page = new FakePage();
  Object.defineProperty(globalThis, "window", { value: page, configurable: true });
  Object.defineProperty(globalThis, "document", { value: page, configurable: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", { value: (f: () => void) => page.frames.push(f), configurable: true });
  try {
    return body(page);
  } finally {
    names.forEach((n, i) => {
      const d = saved[i];
      if (d) Object.defineProperty(globalThis, n, d);
      else delete (globalThis as Record<string, unknown>)[n];
    });
  }
}

/** A DOM event with these fields (`preventDefault` sets `defaultPrevented`, as a real one does). */
export function domEvent(type: string, fields: Record<string, unknown> = {}): Event {
  return Object.assign(new Event(type, { cancelable: true }), {
    shiftKey: false, ctrlKey: false, altKey: false, metaKey: false, isComposing: false, keyCode: 0, ...fields,
  });
}
